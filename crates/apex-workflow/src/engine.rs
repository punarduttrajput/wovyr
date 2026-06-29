//! The workflow execution engine.
//!
//! Drives a [`Definition`] to completion as a durable, event-sourced state machine
//! ([execution model](../../docs/03-workflow-engine/execution-model.md)). After
//! every significant step it appends an event and writes a full checkpoint, so a
//! fresh `Engine` built from the same store can [`resume`](Engine::resume) without
//! re-executing completed activities
//! ([recovery §16](../../docs/03-workflow-engine/execution-model.md)).
//!
//! Scheduling is deterministic: when several activities are ready at once (all
//! predecessors completed), the engine runs them **concurrently** — their executor
//! calls (and retry backoffs) overlap — then commits their results to the event log
//! and checkpoint in declaration order, so the persisted history stays reproducible
//! regardless of real-time completion order. Independent branches share no data edge,
//! so each runs against the same pre-batch variable snapshot. A lone ready activity
//! (or a `wait` suspension point) takes the simple sequential path.

use crate::definition::{ActivityDef, Definition};
use crate::event::WorkflowEvent;
use crate::executor::{ActivityContext, ActivityError, ActivityExecutor};
use crate::state::{ActivityState, WorkflowState};
use crate::store::{CheckpointStore, EventLog};
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::task::JoinSet;

/// Per-activity durable record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRecord {
    /// Current activity state.
    pub state: ActivityState,
    /// Attempts made so far.
    pub attempts: u32,
    /// Output of a completed activity.
    pub output: Option<Value>,
    /// Last error message, if any.
    pub last_error: Option<String>,
}

impl Default for ActivityRecord {
    fn default() -> Self {
        Self {
            state: ActivityState::Created,
            attempts: 0,
            output: None,
            last_error: None,
        }
    }
}

/// The full, serializable execution snapshot — both the live state and the
/// checkpoint payload ([checkpointing §8](../../docs/03-workflow-engine/checkpointing-specification.md)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionState {
    /// Unique execution id.
    pub execution_id: String,
    /// Workflow name and version this execution was started with.
    pub workflow_name: String,
    /// Workflow version.
    pub workflow_version: String,
    /// Current workflow state.
    pub status: WorkflowState,
    /// Mutable workflow variables (seeded from definition + run input; each
    /// completed activity's output is stored under its id).
    pub variables: BTreeMap<String, Value>,
    /// Per-activity records, keyed by activity id.
    pub activities: BTreeMap<String, ActivityRecord>,
    /// Forward activities in completion order — the compensation stack
    /// ([compensation §7](../../docs/03-workflow-engine/compensation-engine.md)).
    #[serde(default)]
    pub completed_order: Vec<String>,
    /// Monotonic state version for optimistic concurrency
    /// ([state machine §13](../../docs/03-workflow-engine/state-machine.md)).
    pub version: u64,
}

/// Terminal result of driving an execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// All activities completed.
    Completed,
    /// An activity failed terminally (non-retryable or retries exhausted).
    Failed(String),
    /// An activity failed and the workflow was rolled back via compensation.
    Compensated(String),
    /// A worker yielded; the execution is durable and can be resumed.
    Interrupted(String),
}

/// Internal per-activity step result.
enum Step {
    Completed,
    Failed(String),
    Interrupted(String),
}

/// The aggregate result of running a batch of independent activities concurrently.
enum BatchStep {
    /// Every activity in the batch settled (completed). The loop continues.
    AllSettled,
    /// At least one activity failed terminally; carries the failure to compensate on.
    Failed(String),
    /// At least one activity suspended durably; the workflow yields after committing
    /// the rest of the batch. Names the suspending activity for the interrupt event.
    Interrupted { activity: String, msg: String },
}

/// The outcome of running one activity to a terminal point in isolation — computed
/// off the shared state (Phase 1) so a batch can run concurrently, then committed in
/// declaration order (Phase 2). `events` are the per-attempt events to replay on
/// commit (`ActivityStarted`/`ActivityRetried`), in order.
struct IsolatedOutcome {
    id: String,
    attempts: u32,
    events: Vec<WorkflowEvent>,
    result: IsolatedResult,
}

/// The terminal disposition of an isolated activity run.
enum IsolatedResult {
    Completed(Value),
    /// Permanent failure or exhausted retries — the message to record/compensate on.
    Failed(String),
    Interrupted(String),
}

/// Run one activity to a terminal point against a fixed variable snapshot, with the
/// same retry semantics as the sequential path — but touching no shared state, so a
/// batch of these can run concurrently on the runtime. Owns its inputs to be
/// `'static` for [`JoinSet`] spawning.
#[tracing::instrument(name = "workflow.activity", skip_all, fields(activity = %id))]
async fn run_attempts(
    executor: Arc<dyn ActivityExecutor>,
    id: String,
    activity: ActivityDef,
    policy: crate::retry::RetryPolicy,
    variables: BTreeMap<String, Value>,
    base_attempts: u32,
) -> IsolatedOutcome {
    let mut events = Vec::new();
    let mut attempt = base_attempts;
    loop {
        attempt += 1;
        events.push(WorkflowEvent::ActivityStarted {
            id: id.clone(),
            attempt,
        });
        let ctx = ActivityContext {
            id: id.clone(),
            activity_type: activity.activity_type.clone(),
            name: activity.name.clone(),
            inputs: activity.inputs.clone(),
            variables: variables.clone(),
            attempt,
        };
        match executor.execute(&ctx).await {
            Ok(output) => {
                return IsolatedOutcome {
                    id,
                    attempts: attempt,
                    events,
                    result: IsolatedResult::Completed(output),
                };
            }
            Err(ActivityError::Interrupted(msg)) => {
                return IsolatedOutcome {
                    id,
                    attempts: attempt,
                    events,
                    result: IsolatedResult::Interrupted(msg),
                };
            }
            Err(ActivityError::Permanent(msg)) => {
                return IsolatedOutcome {
                    id,
                    attempts: attempt,
                    events,
                    result: IsolatedResult::Failed(msg),
                };
            }
            Err(ActivityError::Retryable(msg)) => {
                if attempt >= policy.max_attempts {
                    let msg = format!("retries exhausted after {attempt} attempts: {msg}");
                    return IsolatedOutcome {
                        id,
                        attempts: attempt,
                        events,
                        result: IsolatedResult::Failed(msg),
                    };
                }
                let delay = policy.next_delay(attempt);
                events.push(WorkflowEvent::ActivityRetried {
                    id: id.clone(),
                    attempt,
                    delay_ms: delay.as_millis() as u64,
                    reason: msg,
                });
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Classification of an inbound edge during scheduling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// Source completed and the guard holds — this edge enables the target.
    Live,
    /// Source settled but the edge cannot fire (guard false, or source skipped/failed).
    Dead,
    /// Source not yet settled — the edge's fate is undecided.
    Pending,
}

/// The durable workflow engine.
#[derive(Clone)]
pub struct Engine {
    events: Arc<dyn EventLog>,
    checkpoints: Arc<dyn CheckpointStore>,
    executor: Arc<dyn ActivityExecutor>,
}

impl Engine {
    /// Build an engine over an event log, checkpoint store, and activity executor.
    pub fn new(
        events: Arc<dyn EventLog>,
        checkpoints: Arc<dyn CheckpointStore>,
        executor: Arc<dyn ActivityExecutor>,
    ) -> Self {
        Self {
            events,
            checkpoints,
            executor,
        }
    }

    /// Start a new execution of `def` with id `execution_id` and JSON `input`, and
    /// drive it to completion (or its first suspend point).
    pub async fn run(
        &self,
        def: &Definition,
        execution_id: &str,
        input: Value,
    ) -> Result<(RunOutcome, ExecutionState)> {
        let state = self.start(def, execution_id, input).await?;
        self.drive(def, state).await
    }

    /// Create and durably checkpoint a new execution **without** running any
    /// activities. Returns the initial state. Used by distributed workers: the
    /// submitter calls `start` then enqueues the execution for a worker to drive via
    /// [`resume`](Self::resume).
    pub async fn start(
        &self,
        def: &Definition,
        execution_id: &str,
        input: Value,
    ) -> Result<ExecutionState> {
        let mut variables = def.spec.variables.clone();
        // Expose the run input both as a nested `input` object (so guards can use
        // `input.field`) and flattened at the top level (back-compat).
        variables.insert("input".to_string(), input.clone());
        if let Value::Object(map) = input {
            variables.extend(map);
        }

        let mut state = ExecutionState {
            execution_id: execution_id.to_string(),
            workflow_name: def.metadata.name.clone(),
            workflow_version: def.metadata.version.clone(),
            status: WorkflowState::Created,
            variables,
            activities: def
                .spec
                .activities
                .iter()
                .map(|a| (a.id.clone(), ActivityRecord::default()))
                .collect(),
            completed_order: Vec::new(),
            version: 0,
        };

        self.emit(
            &mut state,
            WorkflowEvent::WorkflowCreated {
                workflow: def.metadata.name.clone(),
                version: def.metadata.version.clone(),
            },
        )
        .await?;
        self.transition(
            &mut state,
            WorkflowState::Validated,
            WorkflowEvent::WorkflowValidated,
        )
        .await?;
        self.transition(
            &mut state,
            WorkflowState::Scheduled,
            WorkflowEvent::WorkflowScheduled,
        )
        .await?;
        self.transition(
            &mut state,
            WorkflowState::Running,
            WorkflowEvent::WorkflowStarted,
        )
        .await?;
        self.checkpoint(&mut state).await?;
        Ok(state)
    }

    /// Resume a previously-started execution from its latest checkpoint.
    pub async fn resume(
        &self,
        def: &Definition,
        execution_id: &str,
    ) -> Result<(RunOutcome, ExecutionState)> {
        let state = self
            .checkpoints
            .latest(execution_id)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!("no checkpoint for execution `{execution_id}`"))
            })?;

        match state.status {
            WorkflowState::Completed => return Ok((RunOutcome::Completed, state)),
            WorkflowState::Failed => {
                return Ok((
                    RunOutcome::Failed("workflow previously failed".into()),
                    state,
                ));
            }
            WorkflowState::Cancelled => {
                return Ok((RunOutcome::Failed("workflow was cancelled".into()), state));
            }
            _ => {}
        }
        self.drive(def, state).await
    }

    /// Deliver a named **event** to a waiting execution and resume it. A `wait`
    /// activity declared `{event: <name>}` observes it and completes with `payload`.
    pub async fn signal_event(
        &self,
        def: &Definition,
        execution_id: &str,
        name: &str,
        payload: Value,
    ) -> Result<(RunOutcome, ExecutionState)> {
        self.deliver(def, execution_id, &format!("event.{name}"), payload)
            .await
    }

    /// Fire a **timer** for a waiting execution and resume it. A `wait` activity
    /// declared `{timer: <id>}` observes it and completes.
    pub async fn fire_timer(
        &self,
        def: &Definition,
        execution_id: &str,
        timer: &str,
    ) -> Result<(RunOutcome, ExecutionState)> {
        self.deliver(
            def,
            execution_id,
            &format!("timer.{timer}"),
            Value::Bool(true),
        )
        .await
    }

    /// Inject a delivered signal into the durable checkpoint, then resume.
    async fn deliver(
        &self,
        def: &Definition,
        execution_id: &str,
        key: &str,
        payload: Value,
    ) -> Result<(RunOutcome, ExecutionState)> {
        let mut state = self
            .checkpoints
            .latest(execution_id)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!("no checkpoint for execution `{execution_id}`"))
            })?;
        state.variables.insert(key.to_string(), payload);
        self.checkpoints.save(&state).await?;
        self.resume(def, execution_id).await
    }

    /// The scheduling loop: run ready activities until the workflow ends.
    async fn drive(
        &self,
        def: &Definition,
        mut state: ExecutionState,
    ) -> Result<(RunOutcome, ExecutionState)> {
        loop {
            // Disable branches whose guards excluded every inbound edge, so they
            // neither block completion nor get scheduled.
            self.apply_skips(def, &mut state).await?;

            if self.forward_complete(def, &state) {
                self.transition(
                    &mut state,
                    WorkflowState::Completed,
                    WorkflowEvent::WorkflowCompleted,
                )
                .await?;
                self.checkpoint(&mut state).await?;
                return Ok((RunOutcome::Completed, state));
            }

            // When more than one independent activity is ready, run the whole batch
            // concurrently (their executor calls + backoffs overlap) and commit the
            // results in declaration order. A single ready activity — or a `wait`
            // suspension point — takes the sequential path below.
            let batch = self.ready_batch(def, &state);
            if batch.len() > 1 {
                match self.run_ready_batch(def, &mut state, &batch).await? {
                    BatchStep::AllSettled => continue,
                    BatchStep::Failed(msg) => {
                        let outcome = self.compensate(def, &mut state, msg).await?;
                        return Ok((outcome, state));
                    }
                    BatchStep::Interrupted { activity, msg } => {
                        self.emit(&mut state, WorkflowEvent::WorkflowInterrupted { activity })
                            .await?;
                        self.checkpoint(&mut state).await?;
                        return Ok((RunOutcome::Interrupted(msg), state));
                    }
                }
            }

            let Some(id) = self.next_ready(def, &state) else {
                // Nothing runnable and not all complete → blocked on a failed activity.
                let err = "workflow is blocked: no runnable activities remain".to_string();
                let outcome = self.compensate(def, &mut state, err).await?;
                return Ok((outcome, state));
            };

            match self.run_activity(def, &mut state, &id).await? {
                Step::Completed => continue,
                Step::Failed(msg) => {
                    let outcome = self.compensate(def, &mut state, msg).await?;
                    return Ok((outcome, state));
                }
                Step::Interrupted(msg) => {
                    self.emit(
                        &mut state,
                        WorkflowEvent::WorkflowInterrupted { activity: id },
                    )
                    .await?;
                    self.checkpoint(&mut state).await?;
                    return Ok((RunOutcome::Interrupted(msg), state));
                }
            }
        }
    }

    /// Whether every *forward* activity (excluding compensation handlers) has
    /// settled — completed or skipped (a skipped branch needs no execution).
    fn forward_complete(&self, def: &Definition, state: &ExecutionState) -> bool {
        let handlers = def.compensation_targets();
        def.spec
            .activities
            .iter()
            .filter(|a| !handlers.contains(&a.id))
            .all(|a| {
                matches!(
                    state.activities[&a.id].state,
                    ActivityState::Completed | ActivityState::Skipped
                )
            })
    }

    /// Pick the first activity (in declaration order) that is ready to run: it is
    /// not yet settled, none of its inbound edges are still undecided, and at least
    /// one inbound edge is *live* (an entry node with no edges is always ready).
    /// Compensation handlers are excluded — they run only during rollback.
    fn next_ready(&self, def: &Definition, state: &ExecutionState) -> Option<String> {
        let handlers = def.compensation_targets();
        def.spec
            .activities
            .iter()
            .find(|a| !handlers.contains(&a.id) && self.is_ready(def, state, &a.id))
            .map(|a| a.id.clone())
    }

    /// The full set of ready, non-`wait` activities (in declaration order) — the
    /// candidates for one concurrent batch. `wait` activities are excluded: they are
    /// suspension points handled by the sequential [`run_wait`](Self::run_wait) path,
    /// not parallel work.
    fn ready_batch(&self, def: &Definition, state: &ExecutionState) -> Vec<String> {
        let handlers = def.compensation_targets();
        def.spec
            .activities
            .iter()
            .filter(|a| {
                !handlers.contains(&a.id)
                    && a.activity_type != "wait"
                    && self.is_ready(def, state, &a.id)
            })
            .map(|a| a.id.clone())
            .collect()
    }

    /// Whether activity `id` is ready to run: not yet settled, none of its inbound
    /// edges still undecided, and at least one inbound edge *live* (an entry node with
    /// no edges is always ready).
    fn is_ready(&self, def: &Definition, state: &ExecutionState, id: &str) -> bool {
        let record = &state.activities[id];
        // `Waiting` is runnable so a resumed wait re-evaluates its condition.
        let runnable = matches!(
            record.state,
            ActivityState::Created
                | ActivityState::Ready
                | ActivityState::Retrying
                | ActivityState::Waiting
        );
        if !runnable {
            return false;
        }
        let inbound = def.inbound(id);
        if inbound.is_empty() {
            return true; // entry node
        }
        let edges: Vec<Edge> = inbound
            .iter()
            .map(|t| self.classify_edge(t, state))
            .collect();
        !edges.contains(&Edge::Pending) && edges.contains(&Edge::Live)
    }

    /// Mark activities whose inbound edges are all *decided and dead* as `Skipped`,
    /// to a fixpoint (skipping one node deadens its outbound edges, possibly
    /// skipping more). Entry nodes are never skipped.
    async fn apply_skips(&self, def: &Definition, state: &mut ExecutionState) -> Result<()> {
        let handlers = def.compensation_targets();
        loop {
            let mut to_skip = None;
            for a in &def.spec.activities {
                if handlers.contains(&a.id) {
                    continue;
                }
                let runnable = matches!(
                    state.activities[&a.id].state,
                    ActivityState::Created | ActivityState::Ready | ActivityState::Retrying
                );
                if !runnable {
                    continue;
                }
                let inbound = def.inbound(&a.id);
                if inbound.is_empty() {
                    continue; // entry nodes always run
                }
                let edges: Vec<Edge> = inbound
                    .iter()
                    .map(|t| self.classify_edge(t, state))
                    .collect();
                // Decided (no pending) and no live edge → this branch is dead.
                if edges.iter().all(|e| *e == Edge::Dead) {
                    to_skip = Some(a.id.clone());
                    break;
                }
            }
            match to_skip {
                Some(id) => {
                    if let Some(record) = state.activities.get_mut(&id) {
                        record.state = ActivityState::Skipped;
                    }
                    self.emit(state, WorkflowEvent::ActivitySkipped { id })
                        .await?;
                    self.checkpoint(state).await?;
                }
                None => return Ok(()),
            }
        }
    }

    /// Classify an inbound edge given the source activity's state and the edge's
    /// guard, evaluated against the current variables.
    fn classify_edge(&self, t: &crate::definition::Transition, state: &ExecutionState) -> Edge {
        match state.activities.get(&t.from).map(|r| r.state) {
            Some(ActivityState::Completed) => {
                let guard = t.when.as_deref().unwrap_or("");
                if crate::condition::evaluate(guard, &state.variables).unwrap_or(false) {
                    Edge::Live
                } else {
                    Edge::Dead
                }
            }
            Some(ActivityState::Skipped) | Some(ActivityState::Failed) => Edge::Dead,
            _ => Edge::Pending,
        }
    }

    /// Execute a single activity with retry, persisting progress as it goes.
    #[tracing::instrument(name = "workflow.activity", skip_all, fields(activity = %id))]
    async fn run_activity(
        &self,
        def: &Definition,
        state: &mut ExecutionState,
        id: &str,
    ) -> Result<Step> {
        let activity = def.activity(id).expect("activity exists").clone();

        // `wait` activities are handled by the engine: they suspend durably until a
        // timer fires or a named event is delivered (see `signal_event`/`fire_timer`).
        if activity.activity_type == "wait" {
            return self.run_wait(state, &activity).await;
        }

        let policy = def.retry_for(id);

        loop {
            let attempt = state.activities[id].attempts + 1;
            self.set_activity(state, id, ActivityState::Running);
            self.emit(
                state,
                WorkflowEvent::ActivityStarted {
                    id: id.to_string(),
                    attempt,
                },
            )
            .await?;

            let ctx = ActivityContext {
                id: id.to_string(),
                activity_type: activity.activity_type.clone(),
                name: activity.name.clone(),
                inputs: activity.inputs.clone(),
                variables: state.variables.clone(),
                attempt,
            };

            match self.executor.execute(&ctx).await {
                Ok(output) => {
                    let record = state.activities.get_mut(id).expect("record exists");
                    record.attempts = attempt;
                    record.state = ActivityState::Completed;
                    record.output = Some(output.clone());
                    state.variables.insert(id.to_string(), output.clone());
                    state.completed_order.push(id.to_string());
                    self.emit(
                        state,
                        WorkflowEvent::ActivityCompleted {
                            id: id.to_string(),
                            output,
                        },
                    )
                    .await?;
                    self.checkpoint(state).await?;
                    return Ok(Step::Completed);
                }
                Err(ActivityError::Interrupted(msg)) => {
                    // Reset to Ready so a resume re-runs this (uncompleted) activity.
                    let record = state.activities.get_mut(id).expect("record exists");
                    record.state = ActivityState::Ready;
                    self.checkpoint(state).await?;
                    return Ok(Step::Interrupted(msg));
                }
                Err(ActivityError::Permanent(msg)) => {
                    return self
                        .terminal_activity_failure(state, id, attempt, msg)
                        .await;
                }
                Err(ActivityError::Retryable(msg)) => {
                    if attempt >= policy.max_attempts {
                        let msg = format!("retries exhausted after {attempt} attempts: {msg}");
                        return self
                            .terminal_activity_failure(state, id, attempt, msg)
                            .await;
                    }
                    let delay = policy.next_delay(attempt);
                    {
                        let record = state.activities.get_mut(id).expect("record exists");
                        record.attempts = attempt;
                        record.state = ActivityState::Retrying;
                        record.last_error = Some(msg.clone());
                    }
                    self.emit(
                        state,
                        WorkflowEvent::ActivityRetried {
                            id: id.to_string(),
                            attempt,
                            delay_ms: delay.as_millis() as u64,
                            reason: msg,
                        },
                    )
                    .await?;
                    self.checkpoint(state).await?;
                    tokio::time::sleep(delay).await;
                    // loop and retry
                }
            }
        }
    }

    /// Handle a `wait` activity: complete it if the awaited timer/event has been
    /// delivered (into `state.variables`), otherwise suspend the workflow durably.
    async fn run_wait(&self, state: &mut ExecutionState, activity: &ActivityDef) -> Result<Step> {
        let id = activity.id.clone();
        let spec = WaitSpec::from_inputs(&activity.inputs)?;

        if let Some(payload) = state.variables.get(&spec.variable_key()).cloned() {
            // The signal arrived → complete, exposing the payload like any output.
            {
                let record = state.activities.get_mut(&id).expect("record exists");
                record.state = ActivityState::Completed;
                record.output = Some(payload.clone());
            }
            state.variables.insert(id.clone(), payload.clone());
            state.completed_order.push(id.clone());
            self.emit(
                state,
                WorkflowEvent::ActivityCompleted {
                    id: id.clone(),
                    output: payload,
                },
            )
            .await?;
            self.checkpoint(state).await?;
            return Ok(Step::Completed);
        }

        // Not yet signalled → mark Waiting and suspend (resumable by delivery).
        self.set_activity(state, &id, ActivityState::Waiting);
        self.emit(
            state,
            WorkflowEvent::ActivityWaiting {
                id: id.clone(),
                waiting_for: spec.describe(),
            },
        )
        .await?;
        self.checkpoint(state).await?;
        Ok(Step::Interrupted(format!(
            "waiting for {}",
            spec.describe()
        )))
    }

    /// Mark an activity as terminally failed and record the event.
    async fn terminal_activity_failure(
        &self,
        state: &mut ExecutionState,
        id: &str,
        attempt: u32,
        msg: String,
    ) -> Result<Step> {
        let record = state.activities.get_mut(id).expect("record exists");
        record.attempts = attempt;
        record.state = ActivityState::Failed;
        record.last_error = Some(msg.clone());
        self.emit(
            state,
            WorkflowEvent::ActivityFailed {
                id: id.to_string(),
                error: msg.clone(),
            },
        )
        .await?;
        self.checkpoint(state).await?;
        Ok(Step::Failed(format!("activity `{id}` failed: {msg}")))
    }

    /// Run a batch of independent ready activities concurrently, then commit their
    /// results to the event log + checkpoint in declaration order, so the persisted
    /// history is deterministic regardless of completion timing. Each activity runs
    /// against the same pre-batch variable snapshot — sound because batch members
    /// share no data edge.
    async fn run_ready_batch(
        &self,
        def: &Definition,
        state: &mut ExecutionState,
        batch: &[String],
    ) -> Result<BatchStep> {
        // Phase 1: execute concurrently, off the shared state (pure w.r.t. the engine).
        let snapshot = state.variables.clone();
        let mut set = JoinSet::new();
        for (idx, id) in batch.iter().enumerate() {
            let executor = self.executor.clone();
            let activity = def.activity(id).expect("activity exists").clone();
            let policy = def.retry_for(id);
            let vars = snapshot.clone();
            let base = state.activities[id].attempts;
            let id = id.clone();
            set.spawn(async move {
                (
                    idx,
                    run_attempts(executor, id, activity, policy, vars, base).await,
                )
            });
        }

        // Restore declaration order — JoinSet yields completions as they finish.
        let mut slots: Vec<Option<IsolatedOutcome>> = (0..batch.len()).map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            let (idx, outcome) =
                joined.map_err(|e| Error::Runtime(format!("activity task panicked: {e}")))?;
            slots[idx] = Some(outcome);
        }

        // Phase 2: commit deterministically in declaration order.
        let mut failure: Option<String> = None;
        let mut interrupt: Option<(String, String)> = None;
        for outcome in slots.into_iter().map(|s| s.expect("every slot filled")) {
            let id = outcome.id.clone();
            let attempts = outcome.attempts;
            self.set_activity(state, &id, ActivityState::Running);
            for event in outcome.events {
                self.emit(state, event).await?;
            }
            match outcome.result {
                IsolatedResult::Completed(output) => {
                    let record = state.activities.get_mut(&id).expect("record exists");
                    record.attempts = attempts;
                    record.state = ActivityState::Completed;
                    record.output = Some(output.clone());
                    state.variables.insert(id.clone(), output.clone());
                    state.completed_order.push(id.clone());
                    self.emit(
                        state,
                        WorkflowEvent::ActivityCompleted {
                            id: id.clone(),
                            output,
                        },
                    )
                    .await?;
                    self.checkpoint(state).await?;
                }
                IsolatedResult::Failed(msg) => {
                    let record = state.activities.get_mut(&id).expect("record exists");
                    record.attempts = attempts;
                    record.state = ActivityState::Failed;
                    record.last_error = Some(msg.clone());
                    self.emit(
                        state,
                        WorkflowEvent::ActivityFailed {
                            id: id.clone(),
                            error: msg.clone(),
                        },
                    )
                    .await?;
                    self.checkpoint(state).await?;
                    if failure.is_none() {
                        failure = Some(format!("activity `{id}` failed: {msg}"));
                    }
                }
                IsolatedResult::Interrupted(msg) => {
                    // Reset to Ready so a resume re-runs this (uncompleted) activity.
                    self.set_activity(state, &id, ActivityState::Ready);
                    self.checkpoint(state).await?;
                    if interrupt.is_none() {
                        interrupt = Some((id, msg));
                    }
                }
            }
        }

        // A failure takes precedence (it drives compensation); otherwise a suspend.
        if let Some(msg) = failure {
            return Ok(BatchStep::Failed(msg));
        }
        if let Some((activity, msg)) = interrupt {
            return Ok(BatchStep::Interrupted { activity, msg });
        }
        Ok(BatchStep::AllSettled)
    }

    /// Handle a terminal failure: transition to `Failed`, then roll back completed
    /// activities (in reverse order) via their compensation handlers
    /// ([compensation §7–8](../../docs/03-workflow-engine/compensation-engine.md)).
    /// Returns `Compensated` if a rollback ran, else `Failed`.
    async fn compensate(
        &self,
        def: &Definition,
        state: &mut ExecutionState,
        error: String,
    ) -> Result<RunOutcome> {
        if state.status == WorkflowState::Running {
            self.transition(
                state,
                WorkflowState::Failed,
                WorkflowEvent::WorkflowFailed {
                    error: error.clone(),
                },
            )
            .await?;
            self.checkpoint(state).await?;
        }

        // Build the rollback plan: completed activities (reverse order) that
        // declare a compensation handler. Only completed work is rolled back.
        let plan: Vec<(String, String)> = state
            .completed_order
            .iter()
            .rev()
            .filter_map(|id| {
                def.activity(id)
                    .and_then(|a| a.compensate.clone())
                    .map(|c| (id.clone(), c))
            })
            .collect();

        if plan.is_empty() {
            return Ok(RunOutcome::Failed(error));
        }

        self.transition(
            state,
            WorkflowState::Compensating,
            WorkflowEvent::CompensationStarted,
        )
        .await?;
        self.checkpoint(state).await?;

        for (activity_id, comp_id) in plan {
            match self.run_compensation(def, state, &comp_id).await? {
                Ok(()) => {
                    self.emit(
                        state,
                        WorkflowEvent::CompensationStepCompleted {
                            activity: activity_id,
                            compensation: comp_id,
                        },
                    )
                    .await?;
                    self.checkpoint(state).await?;
                }
                Err(comp_err) => {
                    self.emit(
                        state,
                        WorkflowEvent::CompensationStepFailed {
                            activity: activity_id,
                            compensation: comp_id,
                            error: comp_err.clone(),
                        },
                    )
                    .await?;
                    self.checkpoint(state).await?;
                    // Rollback could not complete; escalate (stays Compensating).
                    return Ok(RunOutcome::Failed(format!(
                        "compensation failed: {comp_err} (original failure: {error})"
                    )));
                }
            }
        }

        self.transition(
            state,
            WorkflowState::Completed,
            WorkflowEvent::CompensationCompleted,
        )
        .await?;
        self.checkpoint(state).await?;
        Ok(RunOutcome::Compensated(error))
    }

    /// Run a compensation handler activity with retry. Returns `Ok(Ok(()))` on
    /// success, `Ok(Err(msg))` if the handler failed terminally.
    async fn run_compensation(
        &self,
        def: &Definition,
        state: &mut ExecutionState,
        comp_id: &str,
    ) -> Result<std::result::Result<(), String>> {
        let activity = def
            .activity(comp_id)
            .expect("compensation activity exists")
            .clone();
        let policy = def.retry_for(comp_id);
        let mut attempt = 0;

        loop {
            attempt += 1;
            let ctx = ActivityContext {
                id: comp_id.to_string(),
                activity_type: activity.activity_type.clone(),
                name: activity.name.clone(),
                inputs: activity.inputs.clone(),
                variables: state.variables.clone(),
                attempt,
            };
            match self.executor.execute(&ctx).await {
                Ok(output) => {
                    let record = state.activities.get_mut(comp_id).expect("record exists");
                    record.state = ActivityState::Completed;
                    record.attempts = attempt;
                    record.output = Some(output.clone());
                    state.variables.insert(comp_id.to_string(), output);
                    return Ok(Ok(()));
                }
                Err(e) if e.is_retryable() && attempt < policy.max_attempts => {
                    tokio::time::sleep(policy.next_delay(attempt)).await;
                }
                Err(e) => return Ok(Err(e.to_string())),
            }
        }
    }

    /// Set an activity's state (used for non-persisted intermediate transitions).
    fn set_activity(&self, state: &mut ExecutionState, id: &str, to: ActivityState) {
        if let Some(record) = state.activities.get_mut(id) {
            record.state = to;
        }
    }

    /// Validate + apply a workflow state transition, emitting its event.
    async fn transition(
        &self,
        state: &mut ExecutionState,
        to: WorkflowState,
        event: WorkflowEvent,
    ) -> Result<()> {
        if !state.status.can_transition(to) {
            return Err(Error::Runtime(format!(
                "invalid workflow transition {:?} -> {:?}",
                state.status, to
            )));
        }
        state.status = to;
        self.emit(state, event).await
    }

    /// Append an event and bump the state version.
    async fn emit(&self, state: &mut ExecutionState, event: WorkflowEvent) -> Result<()> {
        self.events.append(&state.execution_id, event).await?;
        state.version += 1;
        Ok(())
    }

    /// Persist a full checkpoint of the current state.
    async fn checkpoint(&self, state: &mut ExecutionState) -> Result<()> {
        self.checkpoints.save(state).await
    }
}

/// What a `wait` activity is blocked on, parsed from its `inputs`.
enum WaitSpec {
    /// Waits for `signal_event(<name>)`.
    Event(String),
    /// Waits for `fire_timer(<id>)`.
    Timer(String),
}

impl WaitSpec {
    /// Parse `{event: <name>}` or `{timer: <id>}` from a wait activity's inputs.
    fn from_inputs(inputs: &Value) -> Result<Self> {
        if let Some(name) = inputs.get("event").and_then(Value::as_str) {
            return Ok(WaitSpec::Event(name.to_string()));
        }
        if let Some(timer) = inputs.get("timer").and_then(Value::as_str) {
            return Ok(WaitSpec::Timer(timer.to_string()));
        }
        Err(Error::Invalid(
            "a `wait` activity needs an `event` or `timer` input".into(),
        ))
    }

    /// The workflow variable a delivery writes (and the wait reads).
    fn variable_key(&self) -> String {
        match self {
            WaitSpec::Event(name) => format!("event.{name}"),
            WaitSpec::Timer(timer) => format!("timer.{timer}"),
        }
    }

    /// Human-readable description for events/logs.
    fn describe(&self) -> String {
        match self {
            WaitSpec::Event(name) => format!("event '{name}'"),
            WaitSpec::Timer(timer) => format!("timer '{timer}'"),
        }
    }
}
