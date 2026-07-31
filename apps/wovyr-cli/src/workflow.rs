//! `wovyr workflows` commands: validate, run, and approve workflows locally.
//!
//! Bridges the [workflow engine](wovyr_workflow) to the platform via the shared
//! [`wovyr_runtime::PlatformActivityExecutor`] (RM-GA-P4 HLTH-901) — the same
//! dispatch body the server uses — parameterized here by [`FileAgentResolver`],
//! which resolves `agent`-typed activities from `<agents_dir>/<name>.yaml` (the
//! CLI's file-based stand-in for the server's stored-agent-by-id lookup, since
//! there's no local agent store). Executions persist to a
//! [`FileStore`](wovyr_workflow::FileStore) under `~/.wovyr/workflows`, so a
//! `human` task can suspend durably and be resumed by `approve` — the
//! customer-support example ([docs](../../docs/16-examples/customer-support.md)).

use crate::config;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use wovyr_agent::AgentDefinition;
use wovyr_provider::Gateway;
use wovyr_runtime::{AgentResolver, PlatformActivityExecutor};
use wovyr_tools::ToolRegistry;
use wovyr_workflow::{
    ActivityContext, ActivityState, CheckpointStore, Clock, Definition, DefinitionResolver, Engine,
    EventLog, ExecutionFilter, FileScheduleStore, FileStore, FileTimerStore, RunOutcome, Schedule,
    ScheduleDispatcher, ScheduleStore, SystemClock, TimerDispatcher, TimerStore, WorkflowState,
};

/// Resolves `agent`-typed activities from `<agents_dir>/<name>.yaml` — no tenant,
/// unhosted, no admission gate, matching `agents run --local`'s trust level
/// (the [`AgentResolver`] trait's default methods already model exactly this).
/// `resolve_mcp_tools` (RM-MCX-P2-204) is the one method this impl overrides: a
/// local workflow's `agent` activity gets the same `spec.mcp_servers`
/// resolution `agents run --local` does, against the tenant-less "" namespace
/// of `mcp_store` (the real `~/.wovyr/mcp` store for [`engine`]'s real
/// callers; an injectable field — rather than always opening
/// [`crate::mcp::store`] internally — so tests can point this at a scratch
/// directory instead of the operator's real connection catalog).
struct FileAgentResolver {
    agents_dir: String,
    mcp_store: wovyr_tools::McpConnectionStore,
    mcp_cache: wovyr_tools::McpClientCache,
}

#[async_trait]
impl AgentResolver for FileAgentResolver {
    async fn resolve(
        &self,
        ctx: &ActivityContext,
        agent_id: &str,
    ) -> Result<AgentDefinition, String> {
        let path = std::path::Path::new(&self.agents_dir).join(format!("{agent_id}.yaml"));
        AgentDefinition::from_file(&path.to_string_lossy()).map_err(|e| {
            format!(
                "activity `{}`: could not load agent `{agent_id}` from `{}`: {e}",
                ctx.id,
                path.display()
            )
        })
    }

    async fn resolve_mcp_tools(
        &self,
        _ctx: &ActivityContext,
        connection_names: &[String],
        registry: &mut ToolRegistry,
    ) -> Result<Vec<String>, String> {
        let vault = crate::mcp::secrets_vault();
        self.mcp_cache
            .resolve_agent_mcp_tools(
                &self.mcp_store,
                Some(&vault),
                "",
                connection_names,
                registry,
            )
            .await
            .map_err(|e| e.to_string())
    }
}

/// The durable workflow directory under `~/.wovyr/workflows`.
fn workflows_dir() -> wovyr_common::Result<std::path::PathBuf> {
    Ok(config::config_dir()?.join("workflows"))
}

/// A durable engine over `~/.wovyr/workflows`, plus the platform executor. A durable
/// [`FileTimerStore`] is attached so wall-clock `wait` timers fire across `tick`s.
/// `agents_dir` is where `agent`-typed activities resolve `name` against; commands
/// without an `--agents-dir` flag of their own pass `"."`.
fn engine(agents_dir: &str, allow_privileged: bool) -> wovyr_common::Result<Engine> {
    let dir = workflows_dir()?;
    let store = FileStore::new(dir.clone())?;
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let timers: Arc<dyn TimerStore> = Arc::new(FileTimerStore::new(dir)?);
    // One gateway for both the registry (so `image_generate` routes through the same
    // retry/failover/breaker pipeline as every other call) and the executor.
    let gateway = Arc::new(Gateway::from_env());
    let executor = Arc::new(PlatformActivityExecutor::new(
        // SBX-305: `--local` is SEC-301's documented trusted-first-party escape hatch,
        // but the privileged builtins now require an explicit per-run opt-in rather
        // than being inferred from `--local` alone. See
        // `crate::privileged_tools_enabled`.
        crate::local_registry(allow_privileged, &gateway),
        gateway,
        Arc::new(FileAgentResolver {
            agents_dir: agents_dir.to_string(),
            mcp_store: crate::mcp::store()?,
            mcp_cache: wovyr_tools::McpClientCache::default(),
        }),
    ));
    Ok(Engine::new(events, checkpoints, executor).with_timer_store(timers))
}

/// Durable checkpoint store at `~/.wovyr/workflows` (for decision injection).
fn checkpoint_store() -> wovyr_common::Result<FileStore> {
    FileStore::new(workflows_dir()?)
}

/// Durable timer store at `~/.wovyr/workflows` (shared with the engine).
fn timer_store() -> wovyr_common::Result<Arc<dyn TimerStore>> {
    Ok(Arc::new(FileTimerStore::new(workflows_dir()?)?))
}

/// Durable schedule store at `~/.wovyr/workflows`.
fn schedule_store() -> wovyr_common::Result<Arc<dyn ScheduleStore>> {
    Ok(Arc::new(FileScheduleStore::new(workflows_dir()?)?))
}

/// A resolver that returns `def` for its own workflow name — the CLI drives one
/// definition file at a time, so `tick` resolves only that workflow.
fn resolver_for(def: &Definition) -> DefinitionResolver {
    let name = def.metadata.name.clone();
    let def = def.clone();
    Arc::new(move |want: &str| (want == name).then(|| def.clone()))
}

/// `wovyr workflows validate -f <file>` — compile and validate a definition.
pub fn validate_cmd(file: &str) -> wovyr_common::Result<()> {
    let def = Definition::from_file(file)?;
    println!(
        "ok: workflow '{}' v{} ({} activities)",
        def.metadata.name,
        def.metadata.version,
        def.spec.activities.len()
    );
    Ok(())
}

/// Every tool id a definition's activities name, including the bodies of
/// `for_each`/`map` fan-outs (whose per-item activity template lives inside the
/// parent's raw `inputs.activity`, so a `shell` hidden there would otherwise slip
/// past SBX-305's gate).
///
/// `function` is included alongside `tool` deliberately: `PlatformActivityExecutor`
/// dispatches both through `ToolRegistry::execute` (RM-GA-P4 HLTH-901), so a
/// `function` activity named `shell` is a shell invocation.
fn declared_tool_ids(def: &Definition) -> Vec<String> {
    let mut ids = Vec::new();
    let mut push = |activity_type: &str, name: Option<&str>| {
        if matches!(activity_type, "tool" | "function")
            && let Some(name) = name
        {
            ids.push(name.to_string());
        }
    };
    for activity in &def.spec.activities {
        push(&activity.activity_type, activity.name.as_deref());
        if wovyr_workflow::is_for_each(&activity.activity_type) {
            let body = activity.inputs.get("activity");
            let body_type = body.and_then(|b| b.get("type")).and_then(Value::as_str);
            let body_name = body.and_then(|b| b.get("name")).and_then(Value::as_str);
            if let Some(body_type) = body_type {
                push(body_type, body_name);
            }
        }
    }
    ids
}

/// `wovyr workflows run --local -f <file> --input <json> [--id <id>] [--agents-dir <dir>]`.
pub async fn run_cmd(
    file: &str,
    input: &str,
    local: bool,
    id: Option<String>,
    agents_dir: &str,
    allow_privileged_tools: bool,
) -> wovyr_common::Result<()> {
    if !local {
        return Err(wovyr_common::Error::config(
            "the workflow runner supports local execution only; pass --local",
        ));
    }

    let def = Definition::from_file(file)?;
    // SBX-305: fail closed before anything runs when the definition names a privileged
    // tool this invocation didn't opt into.
    let allow_privileged = crate::privileged_tools_enabled(allow_privileged_tools);
    let declared = declared_tool_ids(&def);
    crate::reject_privileged_tools(declared.iter().map(String::as_str), allow_privileged)?;
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));
    let exec_id = id.unwrap_or_else(|| format!("wf-{}", def.metadata.name));

    let (outcome, state) = engine(agents_dir, allow_privileged)?
        .run(&def, &exec_id, input_value)
        .await?;
    report(&def, &exec_id, &outcome, &state);

    if let RunOutcome::Failed(_) = outcome {
        return Err(wovyr_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}

/// `wovyr workflows approve -f <file> --id <id> --task <activity> [--decision approved]`
/// — inject a human decision into the durable checkpoint and resume.
pub async fn approve_cmd(
    file: &str,
    id: &str,
    task: &str,
    decision: &str,
) -> wovyr_common::Result<()> {
    let def = Definition::from_file(file)?;

    let store = checkpoint_store()?;
    let mut snapshot = store.latest(id).await?.ok_or_else(|| {
        wovyr_common::Error::NotFound(format!("no execution `{id}`; run the workflow first"))
    })?;

    // Record the decision under the human activity's id; the executor reads it on
    // resume and a downstream guard (e.g. `${task}.decision == 'approved'`) routes.
    snapshot.variables.insert(
        task.to_string(),
        json!({ "decision": decision, "approved": decision == "approved" }),
    );
    store.save(&snapshot).await?;
    println!("recorded decision '{decision}' for task '{task}' on execution '{id}'");

    let (outcome, state) = engine(".", crate::privileged_tools_enabled(false))?
        .resume(&def, id)
        .await?;
    report(&def, id, &outcome, &state);

    if let RunOutcome::Failed(_) = outcome {
        return Err(wovyr_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}

/// `wovyr workflows signal -f <file> --id <id> (--event <name> [--payload json] | --timer <id>)`
/// — deliver a waiting-state signal to a suspended execution and resume it.
pub async fn signal_cmd(
    file: &str,
    id: &str,
    event: Option<String>,
    timer: Option<String>,
    payload: &str,
) -> wovyr_common::Result<()> {
    let def = Definition::from_file(file)?;
    let engine = engine(".", crate::privileged_tools_enabled(false))?;

    let (outcome, state) = match (event, timer) {
        (Some(name), None) => {
            let payload = serde_json::from_str(payload).map_err(|e| {
                wovyr_common::Error::Invalid(format!("invalid --payload JSON: {e}"))
            })?;
            engine.signal_event(&def, id, &name, payload).await?
        }
        (None, Some(timer)) => engine.fire_timer(&def, id, &timer).await?,
        _ => {
            return Err(wovyr_common::Error::Invalid(
                "provide exactly one of --event or --timer".into(),
            ));
        }
    };

    report(&def, id, &outcome, &state);
    if let RunOutcome::Failed(_) = outcome {
        return Err(wovyr_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}

/// `wovyr workflows status --id <id>` — read an execution's live state without
/// resuming it (a side-effect-free query, G3).
pub async fn status_cmd(id: &str) -> wovyr_common::Result<()> {
    let summary = engine(".", crate::privileged_tools_enabled(false))?
        .status(id)
        .await?
        .ok_or_else(|| {
            wovyr_common::Error::NotFound(format!("no execution `{id}`; run the workflow first"))
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
fn parse_status(s: &str) -> wovyr_common::Result<WorkflowState> {
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
            return Err(wovyr_common::Error::Invalid(format!(
                "unknown status `{other}`"
            )));
        }
    };
    Ok(status)
}

/// `wovyr workflows list [--workflow <name>] [--status <status>] [--limit <n>]` —
/// list executions, optionally filtered (G4 visibility).
pub async fn list_cmd(
    workflow: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
) -> wovyr_common::Result<()> {
    let filter = ExecutionFilter {
        workflow_name: workflow,
        status: status.as_deref().map(parse_status).transpose()?,
        limit,
    };
    let executions = engine(".", crate::privileged_tools_enabled(false))?
        .list(&filter)
        .await?;
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

/// `wovyr workflows show --id <id>` — show an execution's status plus its full event
/// timeline (G4 visibility).
pub async fn show_cmd(id: &str) -> wovyr_common::Result<()> {
    let engine = engine(".", crate::privileged_tools_enabled(false))?;
    let summary = engine.status(id).await?.ok_or_else(|| {
        wovyr_common::Error::NotFound(format!("no execution `{id}`; run the workflow first"))
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

/// `wovyr workflows tick -f <file>` — fire any due wall-clock timers (G1) and start
/// any due schedules (G2) for the given workflow, then report what happened.
/// Caller-driven: run it on a cron/interval to advance time-based work.
pub async fn tick_cmd(file: &str) -> wovyr_common::Result<()> {
    let def = Definition::from_file(file)?;
    let engine = engine(".", crate::privileged_tools_enabled(false))?;
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

/// `wovyr workflows schedule create -f <file> --id <id> (--every <ms> | --cron <expr>)
/// [--input json]` — register a recurring schedule that starts the workflow on an
/// interval or a cron expression (UTC) (G2).
pub async fn schedule_create_cmd(
    file: &str,
    id: &str,
    every_ms: Option<u64>,
    cron: Option<String>,
    input: &str,
) -> wovyr_common::Result<()> {
    let def = Definition::from_file(file)?;
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));
    let now = SystemClock.now_millis();

    let schedule = match (every_ms, cron) {
        (Some(_), Some(_)) => {
            return Err(wovyr_common::Error::Invalid(
                "provide exactly one of --every or --cron".into(),
            ));
        }
        (Some(every_ms), None) => {
            if every_ms == 0 {
                return Err(wovyr_common::Error::Invalid(
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
            return Err(wovyr_common::Error::Invalid(
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
    println!("advance it with: wovyr workflows tick -f {file}");
    Ok(())
}

/// `wovyr workflows schedule list` — list registered schedules.
pub async fn schedule_list_cmd() -> wovyr_common::Result<()> {
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
    state: &wovyr_workflow::ExecutionState,
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
                    "\nto resume: wovyr workflows approve -f <file> --id {exec_id} --task {} --decision approved",
                    activity.id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wovyr_workflow::{ActivityExecutor, InMemoryStore};

    /// `examples/agents/` relative to the workspace root (this crate's manifest dir
    /// is `apps/wovyr-cli`, two levels down from the workspace root).
    fn examples_agents_dir() -> String {
        format!("{}/../../examples/agents", env!("CARGO_MANIFEST_DIR"))
    }

    /// `agents run --local` and `workflows run --local` must advertise the *same*
    /// tools. They drifted once already: `image_generate` was registered inline in
    /// the agent command's body, so a workflow `tool` activity naming it failed with
    /// a bare "unknown tool" even with a key configured — while the same activity
    /// worked against the server, whose one shared registry has it. Both paths now
    /// build through `crate::local_registry`, and this pins that.
    ///
    /// Asserted against the shared constructor rather than by reaching into the
    /// commands, since `engine()` builds a durable `~/.wovyr/workflows` store that a
    /// unit test has no business creating.
    #[test]
    fn both_local_run_paths_build_the_same_tool_set() {
        let gateway = Arc::new(Gateway::from_env());
        for allow_privileged in [false, true] {
            let agent_side = crate::local_registry(allow_privileged, &gateway);
            let workflow_side = crate::local_registry(allow_privileged, &gateway);
            let mut a = agent_side.ids();
            let mut w = workflow_side.ids();
            a.sort();
            w.sort();
            assert_eq!(a, w, "privileged={allow_privileged}");
            // The safe builtins are present either way; the privileged ones only on
            // the opt-in (SBX-305).
            assert!(a.contains(&"echo".to_string()));
            assert_eq!(a.contains(&"shell".to_string()), allow_privileged);
        }
    }

    /// `image_generate` is keyed off a configured provider key, not off which
    /// command is running — so whatever the environment says, both paths agree.
    #[test]
    fn image_generate_presence_follows_the_key_not_the_command() {
        let gateway = Arc::new(Gateway::from_env());
        let registry = crate::local_registry(false, &gateway);
        let has_key = std::env::var_os("OPENAI_API_KEY").is_some();
        assert_eq!(
            registry.ids().contains(&"image_generate".to_string()),
            has_key,
            "image_generate must be registered exactly when a provider key is set"
        );
    }

    fn ctx(activity_type: &str, name: Option<&str>, inputs: Value) -> ActivityContext {
        ActivityContext {
            id: "test-activity".to_string(),
            activity_type: activity_type.to_string(),
            name: name.map(str::to_string),
            inputs,
            variables: Default::default(),
            attempt: 1,
            progress: None,
        }
    }

    /// A scratch-directory-backed MCP connection store — never the operator's
    /// real `~/.wovyr/mcp` catalog, since these are unit tests.
    fn scratch_mcp_store(label: &str) -> wovyr_tools::McpConnectionStore {
        let dir = std::env::temp_dir().join(format!(
            "wovyr_cli_workflow_mcp_test_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        wovyr_tools::McpConnectionStore::new(dir).unwrap()
    }

    fn executor(agents_dir: &str) -> PlatformActivityExecutor {
        executor_with_mcp(agents_dir, scratch_mcp_store("default"))
    }

    fn executor_with_mcp(
        agents_dir: &str,
        mcp_store: wovyr_tools::McpConnectionStore,
    ) -> PlatformActivityExecutor {
        PlatformActivityExecutor::new(
            ToolRegistry::with_builtins(),
            Arc::new(Gateway::from_env()),
            Arc::new(FileAgentResolver {
                agents_dir: agents_dir.to_string(),
                mcp_store,
                mcp_cache: wovyr_tools::McpClientCache::default(),
            }),
        )
    }

    /// A local `agent` activity resolves `<agents_dir>/<name>.yaml` and runs it
    /// through the real `run_agent` loop, mirroring the server's
    /// `StoredAgentResolver` (both now go through the shared
    /// `wovyr_runtime::PlatformActivityExecutor` — only agent *resolution*
    /// differs, via `FileAgentResolver`).
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
        assert!(matches!(err, wovyr_workflow::ActivityError::Permanent(_)));
    }

    /// RM-MCX-P2-204: a local workflow's `agent` activity resolves its
    /// declared `spec.mcp_servers` (against a scratch-directory-backed store,
    /// never the operator's real `~/.wovyr/mcp`) and can run with the
    /// resulting tool — proving `FileAgentResolver::resolve_mcp_tools` is
    /// actually wired up, not just present.
    #[tokio::test]
    async fn agent_activity_resolves_declared_mcp_servers() {
        let agents_dir = std::env::temp_dir().join(format!(
            "wovyr_cli_workflow_mcp_agents_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&agents_dir);
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("docs-agent.yaml"),
            "metadata:\n  name: docs-agent\nspec:\n  instructions: hi\n  mcp_servers: [docs]\n",
        )
        .unwrap();

        let store = scratch_mcp_store("agent_uses_mcp");
        store
            .put(
                "",
                wovyr_tools::McpConnection {
                    name: "docs".to_string(),
                    transport: wovyr_tools::McpTransportConfig::Stdio {
                        command: "node".to_string(),
                        args: vec![
                            "-e".to_string(),
                            r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { serverInfo: { name: 'x', version: '1' } } }) + '\n');
  } else if (msg.method === 'tools/list') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { tools: [{ name: 'search_docs', description: 'search' }] } }) + '\n');
  }
});
"#
                            .to_string(),
                        ],
                    },
                    secret_ref: None,
                    secret_env_var: None,
                    tool_permissions: None,
                    created_ms: 1,
                    updated_ms: 1,
                },
            )
            .unwrap();

        let exec = executor_with_mcp(&agents_dir.to_string_lossy(), store);
        let out = exec
            .execute(&ctx("agent", Some("docs-agent"), json!({"message": "hi"})))
            .await
            .expect("agent naming a configured MCP connection should run");
        assert!(out.get("message").is_some());

        let _ = std::fs::remove_dir_all(&agents_dir);
    }

    /// The full `research-team.yaml` fan-out/join pattern (FUT-001(b)) works through
    /// the local runner too, not just the server: two `agent` activities with no
    /// edge between them run (the engine's existing type-agnostic concurrent-batch
    /// execution), and `synthesize` joins both via `${proResearch.message}`/
    /// `${conResearch.message}` — proving the shared executor resolves `${...}`
    /// templates for `agent` activities identically for the CLI's `FileAgentResolver`
    /// and the server's stored-agent resolver.
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

        // Placeholder-free synthesis output is the discriminator: an unresolved
        // reference would still contain the literal `${proResearch.message}`.
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
