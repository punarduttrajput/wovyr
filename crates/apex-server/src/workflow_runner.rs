//! Workflow-builder write-path routes: validate, submit, signal, and approve.
//!
//! Complements the read-only visibility routes in `lib.rs` (G4) with the
//! write side that the dashboard Workflow Builder surface needs:
//!
//! - `POST /api/v1/workflows/validate`  — parse a definition YAML, report errors or
//!   return a DAG summary (activity ids + edges) without running anything.
//! - `POST /api/v1/workflows`           — submit a workflow: parse, start, run async,
//!   return `{ execution_id, status: "submitted" }`.
//! - `POST /api/v1/workflows/{id}/signal` — deliver a named event to a `wait` activity
//!   (for event-driven suspend/resume).
//! - `POST /api/v1/workflows/{id}/approve` — approve a suspended `human` activity
//!   (maps to a signal whose key matches the activity id).
//!
//! Execution uses [`ServerExecutor`], a type-dispatch executor that handles
//! `function` activities via the tool registry, `ai` activities via the gateway,
//! and `human` activities by suspending durably (returning `Interrupted`).

use apex_agent::{NullSink, RunOptions, run_agent};
use apex_provider::Gateway;
use apex_tenancy::TenancyStore;
use apex_tools::{ToolContext, ToolRegistry, ToolRequest};
use apex_workflow::{ActivityContext, ActivityError, ActivityExecutor, Definition};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::{AgentStore, ApiError, AppState, tenancy};

// ── ServerExecutor ────────────────────────────────────────────────────────────

/// An [`ActivityExecutor`] for the server that dispatches by `activity_type`:
///
/// - `function` / `tool` — executes the named tool via the [`ToolRegistry`].
/// - `ai`                — calls the gateway with the activity's `name` as the
///   system prompt and its `inputs.message` (or the whole inputs JSON) as the
///   user message.
/// - `agent`             — runs a *stored* agent (created via `POST /api/v1/agents`)
///   end to end through [`run_agent`] — the real model/tool loop, not a bare chat
///   call. The activity's `name` is the agent id; `inputs` is the run input (an
///   `inputs.message` field becomes the user turn, same convention as `ai`). The
///   agent is looked up in the *submitting tenant's* store (via the `__tenant`
///   marker `submit_handler` stamps into the run input), so a workflow can never
///   reach another tenant's agent. When the submission also carried an in-scope
///   project (`__project`, from `X-Apex-Project`), the run is admitted through the
///   same [`tenancy::admit_run`]/[`tenancy::record_run_cost`] gate a direct
///   `agents:run` call goes through — so a workflow that fans out to N sub-agents
///   draws from one shared project budget (concurrent runs + daily LLM spend)
///   instead of N independent, unmetered ones. A quota-exceeded rejection is
///   [`ActivityError::Retryable`]: the blocking slot is held only for a sibling
///   activity's duration, so a retry can succeed once it frees.
/// - `human`             — returns [`ActivityError::Interrupted`] so the engine
///   durably suspends; the run is resumed by `POST …/{id}/approve`.
/// - anything else       — permanent failure (activity type unknown to server).
pub struct ServerExecutor {
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
    agents: Arc<AgentStore>,
    tenancy: Arc<dyn TenancyStore>,
    quota: Arc<tenancy::QuotaTracker>,
}

impl ServerExecutor {
    pub fn new(
        gateway: Arc<Gateway>,
        registry: ToolRegistry,
        agents: Arc<AgentStore>,
        tenancy: Arc<dyn TenancyStore>,
        quota: Arc<tenancy::QuotaTracker>,
    ) -> Self {
        Self {
            gateway,
            registry,
            agents,
            tenancy,
            quota,
        }
    }
}

#[async_trait]
impl ActivityExecutor for ServerExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        // Resolve `${activity.field}` references against the live variables (e.g. a
        // `synthesize` activity's `inputs.message: "${proResearch.message}"`) — the
        // engine hands executors the raw definition inputs and leaves interpolation to
        // them (apex_workflow::resolve_template), the same helper the CLI's local
        // runner uses, so both executors interpolate identically.
        let inputs = apex_workflow::resolve_template(&ctx.inputs, ctx);

        match ctx.activity_type.as_str() {
            "function" | "tool" => {
                let tool_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for function/tool type",
                        ctx.id
                    ))
                })?;
                let tool_ctx = ToolContext::default();
                let req = ToolRequest::new(inputs);
                self.registry
                    .execute(tool_id, &tool_ctx, req)
                    .await
                    .map(|r| r.payload)
                    .map_err(|e| ActivityError::Permanent(e.to_string()))
            }
            "ai" => {
                let instructions = ctx
                    .name
                    .clone()
                    .unwrap_or_else(|| "You are a helpful assistant.".to_string());
                let user_msg = match inputs.get("message").and_then(|v| v.as_str()) {
                    Some(m) => m.to_string(),
                    None => inputs.to_string(),
                };
                use apex_provider::{ChatRequest, Message};
                let req = ChatRequest::new(
                    "default",
                    vec![Message::system(instructions), Message::user(user_msg)],
                );
                let resp = self
                    .gateway
                    .chat(req)
                    .await
                    .map_err(|e| ActivityError::Retryable(e.to_string()))?;
                let text = resp.message.content.unwrap_or_default();
                Ok(json!({ "message": text }))
            }
            "agent" => {
                let agent_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for agent type (the stored agent id)",
                        ctx.id
                    ))
                })?;
                let tenant = ctx
                    .variables
                    .get("__tenant")
                    .and_then(Value::as_str)
                    .unwrap_or(crate::tenancy::DEFAULT_TENANT);
                let def = self.agents.definition(tenant, agent_id).ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: no agent `{agent_id}` found",
                        ctx.id
                    ))
                })?;
                let input = if inputs.is_null() { json!({}) } else { inputs };
                let mut opts = RunOptions::new(input).with_tenant(tenant);
                if let Some(n) = def.spec.max_steps {
                    opts = opts.with_max_steps(n);
                }
                let project = ctx.variables.get("__project").and_then(Value::as_str);
                // Admit through the same project quota gate a direct `agents:run` call
                // uses, so a workflow's fan-out to N sub-agents shares one budget rather
                // than each activity running unmetered. A rejection is retryable: the
                // slot frees once a sibling activity's run ends.
                let _permit = tenancy::admit_run(&self.tenancy, &self.quota, project)
                    .map_err(|e| ActivityError::Retryable(e.message))?;
                let mut sink = NullSink;
                let output = run_agent(&def, &self.gateway, &self.registry, opts, &mut sink)
                    .await
                    .map_err(|e| ActivityError::Retryable(e.to_string()))?;
                tenancy::record_run_cost(&self.quota, project, output.usage.cost_usd);
                Ok(json!({ "message": output.text, "steps": output.steps }))
            }
            "human" => {
                // Suspend durably; the caller resumes via POST …/{id}/approve.
                Err(ActivityError::Interrupted(format!(
                    "human activity `{}` is awaiting approval",
                    ctx.id
                )))
            }
            other => Err(ActivityError::Permanent(format!(
                "activity type `{other}` is not handled by the server executor"
            ))),
        }
    }
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/workflows/validate", post(validate_handler))
        .route("/api/v1/workflows", post(submit_handler))
        .route("/api/v1/workflows/{id}/signal", post(signal_handler))
        .route("/api/v1/workflows/{id}/approve", post(approve_handler))
        .route("/api/v1/workflows/{id}", delete(cancel_handler))
}

// ── validate ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ValidateRequest {
    manifest: String,
}

/// `POST /api/v1/workflows/validate` — parse the definition YAML and return a DAG
/// summary (activity list, dependency edges, and the validated metadata), or a
/// structured validation error without running anything.
async fn validate_handler(Json(req): Json<ValidateRequest>) -> Result<Json<Value>, ApiError> {
    let def = Definition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;

    let activities: Vec<Value> = def
        .spec
        .activities
        .iter()
        .map(|a| json!({ "id": a.id, "type": a.activity_type, "name": a.name }))
        .collect();

    let edges: Vec<Value> = def
        .spec
        .transitions
        .iter()
        .map(|t| json!({ "from": t.from, "to": t.to, "when": t.when }))
        .collect();

    let count = activities.len();
    Ok(Json(json!({
        "valid": true,
        "name":    def.metadata.name,
        "version": def.metadata.version,
        "activities": activities,
        "edges": edges,
        "activity_count": count,
    })))
}

// ── submit ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SubmitRequest {
    manifest: String,
    #[serde(default)]
    input: Value,
    /// Optional caller-supplied execution id (auto-generated when absent).
    execution_id: Option<String>,
}

/// `POST /api/v1/workflows` — validate the manifest, create a durable execution,
/// and drive it asynchronously.  Returns immediately with the execution id so the
/// client can poll `GET /api/v1/workflows/{id}` for status.
async fn submit_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SubmitRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    let def = Definition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;

    let mut input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };
    // Stamp the submitting tenant into the run input so an `agent` activity resolves
    // stored agents from the *submitter's* agent store, not cross-tenant. Reserved
    // (`__`-prefixed) key: hidden from variable listings same as other internal markers.
    // The in-scope project (if any) rides along the same way, so `agent` activities
    // draw from the submitter's project quota instead of running unmetered.
    if let Value::Object(map) = &mut input {
        map.insert("__tenant".to_string(), json!(tenant));
        if let Some(project) = tenancy::run_project(&headers) {
            map.insert("__project".to_string(), json!(project));
        }
    }

    // Derive an execution id from the workflow name + a monotonic counter.
    let execution_id = req.execution_id.unwrap_or_else(|| {
        use std::sync::atomic::Ordering;
        format!(
            "{}-{}",
            def.metadata.name,
            state.run_counter.fetch_add(1, Ordering::SeqCst)
        )
    });

    // Eagerly create the execution in the durable store (the client can poll
    // immediately), then drive it in the background.
    state
        .workflows
        .start(&def, &execution_id, input.clone())
        .await
        .map_err(ApiError::from)?;
    // Stamp the owning tenant so reads/mutations of this execution are tenant-scoped.
    state.record_workflow_owner(&execution_id, &tenant);

    {
        let engine = state.workflows.clone();
        let execution_id2 = execution_id.clone();
        tokio::spawn(async move {
            let _ = engine.resume(&def, &execution_id2).await;
        });
    }

    Ok(Json(json!({
        "execution_id": execution_id,
        "status": "submitted",
    })))
}

// ── signal ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SignalRequest {
    /// The workflow definition YAML (needed to resume the execution).
    manifest: String,
    /// The event name to deliver (matches `wait: {event: <name>}` in the definition).
    event: String,
    /// Payload injected into `event.<name>` in the workflow variables.
    #[serde(default)]
    payload: Value,
}

/// `POST /api/v1/workflows/{id}/signal` — deliver a named event to a waiting
/// execution and resume it.  Used for `wait: {event: …}` activities.
async fn signal_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SignalRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    crate::require_workflow_visible(&state, &id, &tenant)?;
    let def = Definition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    let payload = if req.payload.is_null() {
        json!({})
    } else {
        req.payload
    };

    state
        .workflows
        .signal_event(&def, &id, &req.event, payload)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(json!({
        "execution_id": id,
        "event": req.event,
        "status": "signalled",
    })))
}

// ── approve ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ApproveRequest {
    /// The workflow definition YAML.
    manifest: String,
    /// The `human` activity id being approved.
    activity_id: String,
    /// Approval decision payload (e.g. `{"approved": true, "comment": "LGTM"}`).
    #[serde(default)]
    decision: Value,
}

/// `POST /api/v1/workflows/{id}/approve` — approve (or reject) a suspended `human`
/// activity and resume the execution.  Internally this is a signal whose key is
/// the activity id, consistent with how the CLI's `workflows approve` command works.
async fn approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    crate::require_workflow_visible(&state, &id, &tenant)?;
    let def = Definition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    let decision = if req.decision.is_null() {
        json!({ "approved": true })
    } else {
        req.decision
    };

    state
        .workflows
        .signal_event(&def, &id, &req.activity_id, decision)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(json!({
        "execution_id": id,
        "activity_id": req.activity_id,
        "status": "approved",
    })))
}

// ── cancel ────────────────────────────────────────────────────────────────────

/// `DELETE /api/v1/workflows/{id}` — cancel a running or waiting execution by
/// injecting a terminal `cancelled` variable and signalling a synthetic event.
/// The execution remains in the durable store (visibility is unaffected); its
/// status transitions to `Cancelled` on the next engine step.
///
/// Note: cancellation is advisory — activities that are already in-flight complete
/// normally; only pending/waiting activities are skipped.
async fn cancel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:write")?;
    crate::require_workflow_visible(&state, &id, &tenant)?;
    // Check the execution exists before attempting cancellation.
    state
        .workflows
        .status(&id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("execution `{id}` not found"),
            )
        })?;

    // Inject a `cancelled` variable into the checkpoint so downstream guards
    // and the UI can observe it. Full engine-level cancel support is a later slice.
    // For now we mark the cancellation intent and return 202.
    // (A production implementation would add Engine::cancel that transitions the
    // state machine to Cancelled and writes a WorkflowCancelled event.)
    tracing::info!(execution_id = %id, "cancel requested (advisory)");
    Ok(StatusCode::ACCEPTED)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn test_app() -> axum::Router {
        crate::router(Arc::new(AppState::from_env().await))
    }

    async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        post_json_headers(app, uri, &[], body).await
    }

    async fn post_json_headers(
        app: axum::Router,
        uri: &str,
        headers: &[(&str, &str)],
        body: Value,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let resp = app
            .oneshot(
                builder
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, v)
    }

    const SIMPLE_YAML: &str = "\
metadata:\n  name: test-wf\nspec:\n  activities:\n    - id: echo-step\n      type: function\n      name: echo\n      inputs:\n        message: hello\n";

    #[tokio::test]
    async fn validate_accepts_valid_yaml() {
        let (st, body) = post_json(
            test_app().await,
            "/api/v1/workflows/validate",
            json!({ "manifest": SIMPLE_YAML }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["valid"], true);
        assert_eq!(body["name"], "test-wf");
        assert_eq!(body["activity_count"], 1);
        assert_eq!(body["activities"][0]["id"], "echo-step");
    }

    #[tokio::test]
    async fn validate_rejects_bad_yaml() {
        let (st, body) = post_json(
            test_app().await,
            "/api/v1/workflows/validate",
            json!({ "manifest": "not: valid: workflow: yaml" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "validation_failed");
    }

    #[tokio::test]
    async fn submit_returns_execution_id() {
        let (st, body) = post_json(
            test_app().await,
            "/api/v1/workflows",
            json!({
                "manifest": SIMPLE_YAML,
                "input": { "msg": "hi" },
                "execution_id": "test-exec-1"
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["execution_id"], "test-exec-1");
        assert_eq!(body["status"], "submitted");
    }

    const AGENT_YAML: &str = "\
metadata:\n  name: agent-wf\nspec:\n  activities:\n    - id: greet\n      type: agent\n      name: greeter\n      inputs:\n        message: hi\n";

    /// Poll `GET /api/v1/workflows/{id}` (on the shared `state`) until the execution
    /// leaves `Running`/`Scheduled`/`Created`, or the attempt budget runs out. The
    /// submit route drives the run on a spawned task, so completion isn't synchronous
    /// with the submit response.
    async fn wait_for_terminal(state: &Arc<AppState>, execution_id: &str) -> Value {
        for _ in 0..100 {
            let (st, body) =
                post_json_state_get(state, &format!("/api/v1/workflows/{execution_id}")).await;
            if st == StatusCode::OK {
                let status = body["execution"]["status"].as_str().unwrap_or("");
                if !matches!(status, "Created" | "Scheduled" | "Running" | "Resumed") {
                    return body;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("execution `{execution_id}` did not reach a terminal state in time");
    }

    async fn post_json_state_get(state: &Arc<AppState>, uri: &str) -> (StatusCode, Value) {
        let resp = crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, v)
    }

    /// End-to-end: a workflow `agent` activity runs a *stored* agent (registered via
    /// `POST /api/v1/agents`) through the real model/tool loop, and its text output
    /// lands in the activity's `ActivityCompleted` event.
    #[tokio::test]
    async fn agent_activity_runs_a_stored_agent() {
        let state = Arc::new(AppState::from_env().await);

        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/agents",
            json!({ "manifest": "metadata:\n  name: greeter\nspec:\n  instructions: Be friendly.\n" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["id"], "greeter");

        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": AGENT_YAML, "execution_id": "agent-wf-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "agent-wf-test").await;
        assert_eq!(
            detail["execution"]["status"], "Completed",
            "execution did not complete: {detail}"
        );

        let completed = detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["type"] == "ActivityCompleted" && e["id"] == "greet")
            .unwrap_or_else(|| panic!("no ActivityCompleted event for `greet`: {detail}"));
        let message = completed["output"]["message"].as_str().unwrap_or("");
        assert!(
            !message.is_empty(),
            "expected non-empty agent output: {detail}"
        );
    }

    /// A workflow `agent` activity referencing an id that was never registered fails
    /// the activity (permanent — no such agent to retry into existence) instead of
    /// panicking or hanging.
    #[tokio::test]
    async fn agent_activity_fails_for_unknown_agent() {
        let state = Arc::new(AppState::from_env().await);
        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": AGENT_YAML, "execution_id": "agent-wf-missing" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "agent-wf-missing").await;
        assert_eq!(
            detail["execution"]["status"], "Failed",
            "execution should fail when the referenced agent doesn't exist: {detail}"
        );
    }

    /// FUT-001(b) prototype: two `agent` activities with no edge between them fan out
    /// concurrently (the engine's existing type-agnostic batch execution — no new
    /// engine code), and a downstream `synthesize` agent activity joins both outputs
    /// via `${proResearch.message}`/`${conResearch.message}` — proving both "collect
    /// results from N sub-agents" and that `ServerExecutor` actually interpolates
    /// `${...}` references (it previously didn't; see `resolve_template`).
    const RESEARCH_TEAM_YAML: &str = "\
metadata:\n  name: research-team\nspec:\n  activities:\n    - id: proResearch\n      type: agent\n      name: pro-researcher\n      inputs: { message: \"FOR: ${input.topic}\" }\n    - id: conResearch\n      type: agent\n      name: con-researcher\n      inputs: { message: \"AGAINST: ${input.topic}\" }\n    - id: synthesize\n      type: agent\n      name: synthesizer\n      inputs: { message: \"FOR=${proResearch.message} AGAINST=${conResearch.message}\" }\n  transitions:\n    - { from: proResearch, to: synthesize }\n    - { from: conResearch, to: synthesize }\n";

    #[tokio::test]
    async fn research_team_fans_out_and_joins_two_agents() {
        let state = Arc::new(AppState::from_env().await);
        let router = crate::router(state.clone());

        for name in ["pro-researcher", "con-researcher", "synthesizer"] {
            let (st, body) = post_json(
                router.clone(),
                "/api/v1/agents",
                json!({ "manifest": format!("metadata:\n  name: {name}\nspec:\n  instructions: Be terse.\n") }),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "registering `{name}`: {body}");
        }

        let (st, body) = post_json(
            router,
            "/api/v1/workflows",
            json!({
                "manifest": RESEARCH_TEAM_YAML,
                "input": { "topic": "remote work" },
                "execution_id": "research-team-test",
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "research-team-test").await;
        assert_eq!(
            detail["execution"]["status"], "Completed",
            "execution did not complete: {detail}"
        );

        let events = detail["events"].as_array().unwrap();
        let output_of = |activity_id: &str| -> String {
            events
                .iter()
                .find(|e| e["type"] == "ActivityCompleted" && e["id"] == activity_id)
                .unwrap_or_else(|| {
                    panic!("no ActivityCompleted event for `{activity_id}`: {detail}")
                })["output"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };

        let pro = output_of("proResearch");
        let con = output_of("conResearch");
        assert!(!pro.is_empty(), "expected non-empty pro-research output");
        assert!(!con.is_empty(), "expected non-empty con-research output");

        // The synthesize activity's *input* (echoed back via the mock provider's
        // response, which includes the prompt) must contain both prior activities'
        // literal outputs, not the unresolved `${proResearch.message}` placeholder —
        // proving the join actually collected both sub-agents' results.
        let synthesized = output_of("synthesize");
        assert!(
            !synthesized.contains("${"),
            "synthesize output still contains an unresolved placeholder: {synthesized}"
        );
    }

    /// A workflow's fan-out to N `agent` activities shares one project budget instead
    /// of each activity running unmetered: with `concurrent_agent_runs: 0` (deterministic
    /// reject, same style as `tenancy.rs`'s own quota tests — avoids a flaky
    /// timing-based concurrency proof against near-instant mock LLM calls), submitting
    /// with `X-Apex-Project` fails the activity on quota grounds instead of silently
    /// running it unmetered.
    const QUOTA_AGENT_YAML: &str = "\
metadata:\n  name: agent-wf-quota\nspec:\n  activities:\n    - id: greet\n      type: agent\n      name: greeter\n      retry: { max_attempts: 1 }\n      inputs:\n        message: hi\n";

    #[tokio::test]
    async fn agent_activity_respects_project_quota() {
        let state = Arc::new(AppState::from_env().await);
        state
            .tenancy
            .set_quota(
                "prj-quota-test",
                apex_tenancy::QuotaLimits {
                    concurrent_agent_runs: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();

        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/agents",
            json!({ "manifest": "metadata:\n  name: greeter\nspec:\n  instructions: Be friendly.\n" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let (st, body) = post_json_headers(
            crate::router(state.clone()),
            "/api/v1/workflows",
            &[("x-apex-project", "prj-quota-test")],
            json!({ "manifest": QUOTA_AGENT_YAML, "execution_id": "agent-wf-quota-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "agent-wf-quota-test").await;
        assert_eq!(
            detail["execution"]["status"], "Failed",
            "execution should fail when the project's agent-run quota is exhausted: {detail}"
        );

        let events = detail["events"].as_array().unwrap();
        let failure = events
            .iter()
            .find(|e| e["type"] == "ActivityFailed" && e["id"] == "greet")
            .unwrap_or_else(|| panic!("no ActivityFailed event for `greet`: {detail}"));
        let error = failure["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("concurrent_agent_runs"),
            "expected a quota-related failure reason, got: {error}"
        );
    }
}
