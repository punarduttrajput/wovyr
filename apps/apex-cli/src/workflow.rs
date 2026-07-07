//! `apex workflows` commands: validate, run, and approve workflows locally.
//!
//! Bridges the [workflow engine](apex_workflow) to the platform via a
//! [`PlatformExecutor`] that maps activities onto the [tool runtime](apex_tools)
//! (`tool`), the [LLM gateway](apex_provider) (`ai`), pass-throughs (`function`),
//! a full agent run ([`apex_agent::run_agent`], `agent`), and a human-in-the-loop
//! suspend (`human`). Executions persist to a [`FileStore`](apex_workflow::FileStore)
//! under `~/.apex/workflows`, so a `human` task can suspend durably and be resumed
//! by `approve` — the customer-support example
//! ([docs](../../docs/16-examples/customer-support.md)).

use crate::config;
use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_provider::{ChatRequest, Gateway, Message, ModelSelector};
use apex_tools::{ToolContext, ToolError, ToolRegistry, ToolRequest};
use apex_workflow::{
    ActivityContext, ActivityError, ActivityExecutor, ActivityState, CheckpointStore, Clock,
    Definition, DefinitionResolver, Engine, EventLog, ExecutionFilter, FileScheduleStore,
    FileStore, FileTimerStore, RunOutcome, Schedule, ScheduleDispatcher, ScheduleStore,
    SystemClock, TimerDispatcher, TimerStore, WorkflowState, resolve_template,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Executes workflow activities against the platform's tool runtime and gateway.
struct PlatformExecutor {
    registry: ToolRegistry,
    gateway: Gateway,
    /// Directory `agent`-typed activities resolve `name` against, as
    /// `<agents_dir>/<name>.yaml` — the CLI's file-based stand-in for the server's
    /// stored-agent-by-id lookup (there's no local agent store to look up).
    agents_dir: String,
}

#[async_trait]
impl ActivityExecutor for PlatformExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        // Resolve `${path}` references in the inputs against the live variables.
        let inputs = resolve_template(&ctx.inputs, ctx);

        match ctx.activity_type.as_str() {
            // `function` activities are pass-throughs: echo their (resolved) inputs.
            "function" => Ok(inputs),

            // `tool` activities invoke a registered tool by name.
            "tool" => {
                let tool_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent("tool activity requires a `name` (tool id)".into())
                })?;
                let tool = self
                    .registry
                    .get(tool_id)
                    .ok_or_else(|| ActivityError::Permanent(format!("unknown tool `{tool_id}`")))?;

                let tool_ctx = ToolContext {
                    execution_id: ctx.id.clone(),
                    agent_id: "workflow".to_string(),
                    workdir: ".".to_string(),
                    tenant: String::new(),
                    granted_permissions: None,
                };
                let params = if inputs.is_null() {
                    Value::Object(Default::default())
                } else {
                    inputs
                };

                match tool.execute(&tool_ctx, ToolRequest::new(params)).await {
                    Ok(resp) => Ok(resp.payload),
                    Err(ToolError::Validation(m)) | Err(ToolError::PermissionDenied(m)) => {
                        Err(ActivityError::Permanent(m))
                    }
                    Err(ToolError::Network(m)) | Err(ToolError::Internal(m)) => {
                        Err(ActivityError::Retryable(m))
                    }
                }
            }

            // `ai` activities call the model: `prompt` is the system instruction and
            // `text`/`message` the user turn.
            "ai" => {
                let system = inputs
                    .get("prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("You are a helpful support assistant. Draft a concise reply.");
                let user = inputs
                    .get("text")
                    .or_else(|| inputs.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("");

                let model = self.gateway.resolve_model(None, &ModelSelector::default());
                let request =
                    ChatRequest::new(model, vec![Message::system(system), Message::user(user)]);
                match self.gateway.chat(request).await {
                    Ok(resp) => Ok(json!({ "reply": resp.message.content.unwrap_or_default() })),
                    Err(e) => Err(ActivityError::Retryable(e.to_string())),
                }
            }

            // `agent` activities run a full agent (model + tool loop) via
            // `run_agent`, mirroring the server's `ServerExecutor` — but resolved
            // from `<agents_dir>/<name>.yaml` on disk instead of a stored agent id,
            // since the CLI has no server-side agent store to look up.
            "agent" => {
                let agent_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for agent type (the agent's file stem \
                         under --agents-dir)",
                        ctx.id
                    ))
                })?;
                let path = std::path::Path::new(&self.agents_dir).join(format!("{agent_id}.yaml"));
                let def = AgentDefinition::from_file(&path.to_string_lossy()).map_err(|e| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: could not load agent `{agent_id}` from `{}`: {e}",
                        ctx.id,
                        path.display()
                    ))
                })?;
                let input = if inputs.is_null() { json!({}) } else { inputs };
                let mut opts = RunOptions::new(input);
                if let Some(n) = def.spec.max_steps {
                    opts = opts.with_max_steps(n);
                }
                let mut sink = NullSink;
                let output = run_agent(&def, &self.gateway, &self.registry, opts, &mut sink)
                    .await
                    .map_err(|e| ActivityError::Retryable(e.to_string()))?;
                Ok(json!({ "message": output.text, "steps": output.steps }))
            }

            // `human` activities suspend until a decision is injected (see `approve`).
            "human" => match ctx.variables.get(&ctx.id) {
                Some(decision) => Ok(decision.clone()),
                None => {
                    let who = inputs
                        .get("assignee")
                        .and_then(Value::as_str)
                        .unwrap_or("a reviewer");
                    Err(ActivityError::Interrupted(format!(
                        "awaiting approval from {who}"
                    )))
                }
            },

            other => Err(ActivityError::Permanent(format!(
                "unsupported activity type `{other}` in the local runner"
            ))),
        }
    }
}

/// The durable workflow directory under `~/.apex/workflows`.
fn workflows_dir() -> apex_common::Result<std::path::PathBuf> {
    Ok(config::config_dir()?.join("workflows"))
}

/// A durable engine over `~/.apex/workflows`, plus the platform executor. A durable
/// [`FileTimerStore`] is attached so wall-clock `wait` timers fire across `tick`s.
/// `agents_dir` is where `agent`-typed activities resolve `name` against; commands
/// without an `--agents-dir` flag of their own pass `"."`.
fn engine(agents_dir: &str) -> apex_common::Result<Engine> {
    let dir = workflows_dir()?;
    let store = FileStore::new(dir.clone())?;
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let timers: Arc<dyn TimerStore> = Arc::new(FileTimerStore::new(dir)?);
    let executor = Arc::new(PlatformExecutor {
        // `workflows run --local` is a trusted, first-party/local context (SEC-301's
        // documented escape hatch) — shell stays available here, unlike the server's
        // default registry.
        registry: ToolRegistry::with_privileged_builtins(),
        gateway: Gateway::from_env(),
        agents_dir: agents_dir.to_string(),
    });
    Ok(Engine::new(events, checkpoints, executor).with_timer_store(timers))
}

/// Durable checkpoint store at `~/.apex/workflows` (for decision injection).
fn checkpoint_store() -> apex_common::Result<FileStore> {
    FileStore::new(workflows_dir()?)
}

/// Durable timer store at `~/.apex/workflows` (shared with the engine).
fn timer_store() -> apex_common::Result<Arc<dyn TimerStore>> {
    Ok(Arc::new(FileTimerStore::new(workflows_dir()?)?))
}

/// Durable schedule store at `~/.apex/workflows`.
fn schedule_store() -> apex_common::Result<Arc<dyn ScheduleStore>> {
    Ok(Arc::new(FileScheduleStore::new(workflows_dir()?)?))
}

/// A resolver that returns `def` for its own workflow name — the CLI drives one
/// definition file at a time, so `tick` resolves only that workflow.
fn resolver_for(def: &Definition) -> DefinitionResolver {
    let name = def.metadata.name.clone();
    let def = def.clone();
    Arc::new(move |want: &str| (want == name).then(|| def.clone()))
}

/// `apex workflows validate -f <file>` — compile and validate a definition.
pub fn validate_cmd(file: &str) -> apex_common::Result<()> {
    let def = Definition::from_file(file)?;
    println!(
        "ok: workflow '{}' v{} ({} activities)",
        def.metadata.name,
        def.metadata.version,
        def.spec.activities.len()
    );
    Ok(())
}

/// `apex workflows run --local -f <file> --input <json> [--id <id>] [--agents-dir <dir>]`.
pub async fn run_cmd(
    file: &str,
    input: &str,
    local: bool,
    id: Option<String>,
    agents_dir: &str,
) -> apex_common::Result<()> {
    if !local {
        return Err(apex_common::Error::config(
            "the workflow runner supports local execution only; pass --local",
        ));
    }

    let def = Definition::from_file(file)?;
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));
    let exec_id = id.unwrap_or_else(|| format!("wf-{}", def.metadata.name));

    let (outcome, state) = engine(agents_dir)?.run(&def, &exec_id, input_value).await?;
    report(&def, &exec_id, &outcome, &state);

    if let RunOutcome::Failed(_) = outcome {
        return Err(apex_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}

/// `apex workflows approve -f <file> --id <id> --task <activity> [--decision approved]`
/// — inject a human decision into the durable checkpoint and resume.
pub async fn approve_cmd(
    file: &str,
    id: &str,
    task: &str,
    decision: &str,
) -> apex_common::Result<()> {
    let def = Definition::from_file(file)?;

    let store = checkpoint_store()?;
    let mut snapshot = store.latest(id).await?.ok_or_else(|| {
        apex_common::Error::NotFound(format!("no execution `{id}`; run the workflow first"))
    })?;

    // Record the decision under the human activity's id; the executor reads it on
    // resume and a downstream guard (e.g. `${task}.decision == 'approved'`) routes.
    snapshot.variables.insert(
        task.to_string(),
        json!({ "decision": decision, "approved": decision == "approved" }),
    );
    store.save(&snapshot).await?;
    println!("recorded decision '{decision}' for task '{task}' on execution '{id}'");

    let (outcome, state) = engine(".")?.resume(&def, id).await?;
    report(&def, id, &outcome, &state);

    if let RunOutcome::Failed(_) = outcome {
        return Err(apex_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}

/// `apex workflows signal -f <file> --id <id> (--event <name> [--payload json] | --timer <id>)`
/// — deliver a waiting-state signal to a suspended execution and resume it.
pub async fn signal_cmd(
    file: &str,
    id: &str,
    event: Option<String>,
    timer: Option<String>,
    payload: &str,
) -> apex_common::Result<()> {
    let def = Definition::from_file(file)?;
    let engine = engine(".")?;

    let (outcome, state) = match (event, timer) {
        (Some(name), None) => {
            let payload = serde_json::from_str(payload)
                .map_err(|e| apex_common::Error::Invalid(format!("invalid --payload JSON: {e}")))?;
            engine.signal_event(&def, id, &name, payload).await?
        }
        (None, Some(timer)) => engine.fire_timer(&def, id, &timer).await?,
        _ => {
            return Err(apex_common::Error::Invalid(
                "provide exactly one of --event or --timer".into(),
            ));
        }
    };

    report(&def, id, &outcome, &state);
    if let RunOutcome::Failed(_) = outcome {
        return Err(apex_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}

/// `apex workflows status --id <id>` — read an execution's live state without
/// resuming it (a side-effect-free query, G3).
pub async fn status_cmd(id: &str) -> apex_common::Result<()> {
    let summary = engine(".")?.status(id).await?.ok_or_else(|| {
        apex_common::Error::NotFound(format!("no execution `{id}`; run the workflow first"))
    })?;
    println!(
        "execution '{}' — workflow '{}' v{} — {:?}",
        summary.execution_id, summary.workflow_name, summary.workflow_version, summary.status
    );
    for (id, state) in &summary.activities {
        println!("  {id:<16} {state:?}");
    }
    if !summary.waiting_on.is_empty() {
        println!("  waiting on: {}", summary.waiting_on.join(", "));
    }
    Ok(())
}

/// Parse a workflow status name (case-insensitive) into a [`WorkflowState`].
fn parse_status(s: &str) -> apex_common::Result<WorkflowState> {
    let status = match s.to_ascii_lowercase().as_str() {
        "created" => WorkflowState::Created,
        "validated" => WorkflowState::Validated,
        "scheduled" => WorkflowState::Scheduled,
        "running" => WorkflowState::Running,
        "waiting" => WorkflowState::Waiting,
        "resumed" => WorkflowState::Resumed,
        "compensating" => WorkflowState::Compensating,
        "completed" => WorkflowState::Completed,
        "failed" => WorkflowState::Failed,
        "cancelled" | "canceled" => WorkflowState::Cancelled,
        other => {
            return Err(apex_common::Error::Invalid(format!(
                "unknown status `{other}`"
            )));
        }
    };
    Ok(status)
}

/// `apex workflows list [--workflow <name>] [--status <status>] [--limit <n>]` —
/// list executions, optionally filtered (G4 visibility).
pub async fn list_cmd(
    workflow: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
) -> apex_common::Result<()> {
    let filter = ExecutionFilter {
        workflow_name: workflow,
        status: status.as_deref().map(parse_status).transpose()?,
        limit,
    };
    let executions = engine(".")?.list(&filter).await?;
    if executions.is_empty() {
        println!("no executions found");
        return Ok(());
    }
    for e in executions {
        let waits = if e.waiting_on.is_empty() {
            String::new()
        } else {
            format!("  waiting:{}", e.waiting_on.join(","))
        };
        println!(
            "{:<24} {:<16} v{:<8} {:?}{waits}",
            e.execution_id, e.workflow_name, e.workflow_version, e.status
        );
    }
    Ok(())
}

/// `apex workflows show --id <id>` — show an execution's status plus its full event
/// timeline (G4 visibility).
pub async fn show_cmd(id: &str) -> apex_common::Result<()> {
    let engine = engine(".")?;
    let summary = engine.status(id).await?.ok_or_else(|| {
        apex_common::Error::NotFound(format!("no execution `{id}`; run the workflow first"))
    })?;
    println!(
        "execution '{}' — workflow '{}' v{} — {:?}",
        summary.execution_id, summary.workflow_name, summary.workflow_version, summary.status
    );
    for (aid, state) in &summary.activities {
        println!("  {aid:<16} {state:?}");
    }
    println!("\ntimeline:");
    for (i, event) in engine.history(id).await?.iter().enumerate() {
        let line = serde_json::to_string(event).unwrap_or_else(|_| "<unserializable>".into());
        println!("  {:>3}. {line}", i + 1);
    }
    Ok(())
}

/// `apex workflows tick -f <file>` — fire any due wall-clock timers (G1) and start
/// any due schedules (G2) for the given workflow, then report what happened.
/// Caller-driven: run it on a cron/interval to advance time-based work.
pub async fn tick_cmd(file: &str) -> apex_common::Result<()> {
    let def = Definition::from_file(file)?;
    let engine = engine(".")?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let resolver = resolver_for(&def);

    let timers = TimerDispatcher::new(
        engine.clone(),
        timer_store()?,
        clock.clone(),
        resolver.clone(),
    );
    let fired = timers.poll().await?;

    let schedules = ScheduleDispatcher::new(engine, schedule_store()?, clock, resolver);
    let started = schedules.poll().await?;

    println!(
        "tick: {} timer(s) fired, {} schedule run(s) started",
        fired.len(),
        started.len()
    );
    for id in &fired {
        println!("  fired timer '{id}' and resumed its execution");
    }
    for id in &started {
        println!("  started scheduled execution '{id}'");
    }
    Ok(())
}

/// `apex workflows schedule create -f <file> --id <id> (--every <ms> | --cron <expr>)
/// [--input json]` — register a recurring schedule that starts the workflow on an
/// interval or a cron expression (UTC) (G2).
pub async fn schedule_create_cmd(
    file: &str,
    id: &str,
    every_ms: Option<u64>,
    cron: Option<String>,
    input: &str,
) -> apex_common::Result<()> {
    let def = Definition::from_file(file)?;
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));
    let now = SystemClock.now_millis();

    let schedule = match (every_ms, cron) {
        (Some(_), Some(_)) => {
            return Err(apex_common::Error::Invalid(
                "provide exactly one of --every or --cron".into(),
            ));
        }
        (Some(every_ms), None) => {
            if every_ms == 0 {
                return Err(apex_common::Error::Invalid(
                    "--every must be greater than 0".into(),
                ));
            }
            Schedule::every(
                id,
                def.metadata.name.clone(),
                every_ms,
                now.saturating_add(every_ms),
            )
        }
        (None, Some(expr)) => Schedule::cron(id, def.metadata.name.clone(), expr, now)?,
        (None, None) => {
            return Err(apex_common::Error::Invalid(
                "provide one of --every <ms> or --cron <expr>".into(),
            ));
        }
    }
    .with_input(input_value);

    let cadence = schedule
        .cron
        .clone()
        .map(|c| format!("cron '{c}'"))
        .unwrap_or_else(|| format!("every {}ms", schedule.interval_ms));
    schedule_store()?.save(&schedule).await?;

    println!(
        "created schedule '{id}' for workflow '{}' ({cadence}); first fire at {}",
        def.metadata.name, schedule.next_fire_ms
    );
    println!("advance it with: apex workflows tick -f {file}");
    Ok(())
}

/// `apex workflows schedule list` — list registered schedules.
pub async fn schedule_list_cmd() -> apex_common::Result<()> {
    let schedules = schedule_store()?.list().await?;
    if schedules.is_empty() {
        println!("no schedules registered");
        return Ok(());
    }
    for s in schedules {
        let cadence = s
            .cron
            .as_ref()
            .map(|c| format!("cron='{c}'"))
            .unwrap_or_else(|| format!("every={}ms", s.interval_ms));
        println!(
            "{:<16} workflow={} {cadence} next={} paused={} overlap={:?}",
            s.id, s.workflow_name, s.next_fire_ms, s.paused, s.overlap
        );
    }
    Ok(())
}

/// Print the outcome, per-activity states, and any pending human task.
fn report(
    def: &Definition,
    exec_id: &str,
    outcome: &RunOutcome,
    state: &apex_workflow::ExecutionState,
) {
    match outcome {
        RunOutcome::Completed => println!("workflow '{}' completed", def.metadata.name),
        RunOutcome::Compensated(msg) => {
            println!(
                "workflow '{}' rolled back after failure: {msg}",
                def.metadata.name
            )
        }
        RunOutcome::Failed(msg) => println!("workflow '{}' failed: {msg}", def.metadata.name),
        RunOutcome::Interrupted(msg) => {
            println!("workflow '{}' suspended: {msg}", def.metadata.name)
        }
    }
    for activity in &def.spec.activities {
        let record = &state.activities[&activity.id];
        println!("  {:<16} {:?}", activity.id, record.state);
    }

    if let RunOutcome::Interrupted(_) = outcome {
        // Surface pending human tasks and how to approve them.
        for activity in &def.spec.activities {
            if activity.activity_type == "human"
                && state.activities[&activity.id].state == ActivityState::Ready
            {
                println!(
                    "\nto resume: apex workflows approve -f <file> --id {exec_id} --task {} --decision approved",
                    activity.id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_workflow::InMemoryStore;

    /// `examples/agents/` relative to the workspace root (this crate's manifest dir
    /// is `apps/apex-cli`, two levels down from the workspace root).
    fn examples_agents_dir() -> String {
        format!("{}/../../examples/agents", env!("CARGO_MANIFEST_DIR"))
    }

    fn ctx(activity_type: &str, name: Option<&str>, inputs: Value) -> ActivityContext {
        ActivityContext {
            id: "test-activity".to_string(),
            activity_type: activity_type.to_string(),
            name: name.map(str::to_string),
            inputs,
            variables: Default::default(),
            attempt: 1,
        }
    }

    fn executor(agents_dir: &str) -> PlatformExecutor {
        PlatformExecutor {
            registry: ToolRegistry::with_builtins(),
            gateway: Gateway::from_env(),
            agents_dir: agents_dir.to_string(),
        }
    }

    /// A local `agent` activity resolves `<agents_dir>/<name>.yaml` and runs it
    /// through the real `run_agent` loop, mirroring the server's `ServerExecutor`.
    #[tokio::test]
    async fn agent_activity_runs_from_agents_dir() {
        let exec = executor(&examples_agents_dir());
        let out = exec
            .execute(&ctx("agent", Some("hello"), json!({"message": "hi"})))
            .await
            .expect("agent activity should succeed");
        let message = out["message"].as_str().unwrap_or_default();
        assert!(!message.is_empty(), "expected non-empty agent output");
    }

    /// An `agent` activity referencing a name with no matching file under
    /// `agents_dir` fails permanently (no such file to retry into existence),
    /// instead of panicking.
    #[tokio::test]
    async fn agent_activity_fails_for_missing_file() {
        let exec = executor(&examples_agents_dir());
        let err = exec
            .execute(&ctx("agent", Some("does-not-exist"), Value::Null))
            .await
            .expect_err("missing agent file should fail");
        assert!(matches!(err, ActivityError::Permanent(_)));
    }

    /// The full `research-team.yaml` fan-out/join pattern (FUT-001(b)) works through
    /// the local runner too, not just the server: two `agent` activities with no
    /// edge between them run (the engine's existing type-agnostic concurrent-batch
    /// execution), and `synthesize` joins both via `${proResearch.message}`/
    /// `${conResearch.message}` — proving `PlatformExecutor` resolves `${...}`
    /// templates for `agent` activities exactly like `ServerExecutor` does.
    #[tokio::test]
    async fn research_team_runs_locally_and_joins_two_agents() {
        let def = Definition::from_file(&format!(
            "{}/../../examples/workflows/research-team.yaml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("research-team.yaml should parse");

        let executor = Arc::new(executor(&examples_agents_dir()));
        let engine = Engine::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemoryStore::new()),
            executor,
        );

        let (outcome, state) = engine
            .run(
                &def,
                "research-team-local-test",
                json!({"topic": "remote work"}),
            )
            .await
            .expect("workflow should run");
        assert_eq!(outcome, RunOutcome::Completed, "state: {state:?}");

        // Placeholder-free synthesis output (see the crate-level doc comment on
        // `PlatformExecutor::execute`'s `${...}` resolution) is the discriminator: an
        // unresolved reference would still contain the literal `${proResearch.message}`.
        let synth = &state.activities["synthesize"];
        let output = synth
            .output
            .as_ref()
            .expect("synthesize should have output");
        let message = output["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("${"),
            "synthesize output still contains an unresolved placeholder: {message}"
        );
    }
}
