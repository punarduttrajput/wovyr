//! The workflow execution engine.
//!
//! Drives a [`Definition`] to completion as a durable, event-sourced state machine
//! ([execution model](../../docs/03-workflow-engine/execution-model.md)). After
//! every significant step it appends an event and writes a full checkpoint, so a
//! fresh `Engine` built from the same store can [`resume`](Engine::resume) without
//! re-executing completed activities
//! ([recovery §16](../../docs/03-workflow-engine/execution-model.md)).
//!
//! Scheduling is deterministic: ready activities (all predecessors completed) run
//! in declaration order, one at a time. Parallel/distributed workers are a later
//! slice; correctness and durability come first.

use crate::definition::Definition;
use crate::event::WorkflowEvent;
use crate::executor::{ActivityContext, ActivityError, ActivityExecutor};
use crate::state::{ActivityState, WorkflowState};
use crate::store::{CheckpointStore, EventLog};
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

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

    /// Start a new execution of `def` with id `execution_id` and JSON `input`.
    pub async fn run(
        &self,
        def: &Definition,
        execution_id: &str,
        input: Value,
    ) -> Result<(RunOutcome, ExecutionState)> {
        let mut variables = def.spec.variables.clone();
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

        self.drive(def, state).await
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

    /// The scheduling loop: run ready activities until the workflow ends.
    async fn drive(
        &self,
        def: &Definition,
        mut state: ExecutionState,
    ) -> Result<(RunOutcome, ExecutionState)> {
        loop {
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
    /// completed.
    fn forward_complete(&self, def: &Definition, state: &ExecutionState) -> bool {
        let handlers = def.compensation_targets();
        def.spec
            .activities
            .iter()
            .filter(|a| !handlers.contains(&a.id))
            .all(|a| state.activities[&a.id].state == ActivityState::Completed)
    }

    /// Pick the first activity (in declaration order) that is ready to run:
    /// not yet completed/failed, with every predecessor completed. Compensation
    /// handlers are excluded — they run only during rollback.
    fn next_ready(&self, def: &Definition, state: &ExecutionState) -> Option<String> {
        let handlers = def.compensation_targets();
        for a in &def.spec.activities {
            if handlers.contains(&a.id) {
                continue;
            }
            let record = &state.activities[&a.id];
            let runnable = matches!(
                record.state,
                ActivityState::Created | ActivityState::Ready | ActivityState::Retrying
            );
            if !runnable {
                continue;
            }
            let deps_done = def.predecessors(&a.id).iter().all(|p| {
                state
                    .activities
                    .get(p)
                    .map(|r| r.state == ActivityState::Completed)
                    .unwrap_or(false)
            });
            if deps_done {
                return Some(a.id.clone());
            }
        }
        None
    }

    /// Execute a single activity with retry, persisting progress as it goes.
    async fn run_activity(
        &self,
        def: &Definition,
        state: &mut ExecutionState,
        id: &str,
    ) -> Result<Step> {
        let activity = def.activity(id).expect("activity exists").clone();
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
