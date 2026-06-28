//! `apex workflows` commands: validate and locally run a workflow.
//!
//! Bridges the [workflow engine](apex_workflow) to the platform's
//! [tool runtime](apex_tools) via a [`PlatformExecutor`] that maps workflow
//! activities onto tool invocations. v0.2 supports `tool` and `function`
//! activities locally; `ai`/`http`/`human`/`event`/`timer` and remote/durable
//! execution arrive in later slices.

use apex_tools::{ToolContext, ToolError, ToolRegistry, ToolRequest};
use apex_workflow::{
    ActivityContext, ActivityError, ActivityExecutor, CheckpointStore, Definition, Engine,
    EventLog, InMemoryStore, RunOutcome,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Executes workflow activities against the built-in tool registry.
struct PlatformExecutor {
    registry: ToolRegistry,
}

#[async_trait]
impl ActivityExecutor for PlatformExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        match ctx.activity_type.as_str() {
            // `function` activities are pass-throughs in v0.2: echo their inputs.
            "function" => Ok(ctx.inputs.clone()),

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
                };
                let params = if ctx.inputs.is_null() {
                    Value::Object(Default::default())
                } else {
                    ctx.inputs.clone()
                };

                match tool.execute(&tool_ctx, ToolRequest::new(params)).await {
                    Ok(resp) => Ok(resp.payload),
                    // Classify tool errors for the retry engine.
                    Err(ToolError::Validation(m)) | Err(ToolError::PermissionDenied(m)) => {
                        Err(ActivityError::Permanent(m))
                    }
                    Err(ToolError::Network(m)) | Err(ToolError::Internal(m)) => {
                        Err(ActivityError::Retryable(m))
                    }
                }
            }

            other => Err(ActivityError::Permanent(format!(
                "unsupported activity type `{other}` in v0.2 local runner"
            ))),
        }
    }
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

/// `apex workflows run --local -f <file> --input <json>` — run a workflow with the
/// embedded engine and the tool-backed executor.
pub async fn run_cmd(file: &str, input: &str, local: bool) -> apex_common::Result<()> {
    if !local {
        return Err(apex_common::Error::config(
            "v0.2 supports local workflow runs only; pass --local",
        ));
    }

    let def = Definition::from_file(file)?;
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let executor = Arc::new(PlatformExecutor {
        registry: ToolRegistry::with_builtins(),
    });
    let engine = Engine::new(events, checkpoints, executor);

    let exec_id = format!("wf-{}-local", def.metadata.name);
    let (outcome, state) = engine.run(&def, &exec_id, input_value).await?;

    match &outcome {
        RunOutcome::Completed => println!("workflow '{}' completed", def.metadata.name),
        RunOutcome::Compensated(msg) => {
            println!(
                "workflow '{}' rolled back after failure: {msg}",
                def.metadata.name
            )
        }
        RunOutcome::Failed(msg) => println!("workflow '{}' failed: {msg}", def.metadata.name),
        RunOutcome::Interrupted(msg) => {
            println!("workflow '{}' interrupted: {msg}", def.metadata.name)
        }
    }
    for activity in &def.spec.activities {
        let record = &state.activities[&activity.id];
        println!("  {:<16} {:?}", activity.id, record.state);
    }

    if let RunOutcome::Failed(_) = outcome {
        return Err(apex_common::Error::Runtime("workflow failed".into()));
    }
    Ok(())
}
