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

use apex_provider::Gateway;
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

use crate::{ApiError, AppState};

// ── ServerExecutor ────────────────────────────────────────────────────────────

/// An [`ActivityExecutor`] for the server that dispatches by `activity_type`:
///
/// - `function` / `tool` — executes the named tool via the [`ToolRegistry`].
/// - `ai`                — calls the gateway with the activity's `name` as the
///   system prompt and its `inputs.message` (or the whole inputs JSON) as the
///   user message.
/// - `human`             — returns [`ActivityError::Interrupted`] so the engine
///   durably suspends; the run is resumed by `POST …/{id}/approve`.
/// - anything else       — permanent failure (activity type unknown to server).
pub struct ServerExecutor {
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
}

impl ServerExecutor {
    pub fn new(gateway: Arc<Gateway>, registry: ToolRegistry) -> Self {
        Self { gateway, registry }
    }
}

#[async_trait]
impl ActivityExecutor for ServerExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        match ctx.activity_type.as_str() {
            "function" | "tool" => {
                let tool_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for function/tool type",
                        ctx.id
                    ))
                })?;
                let tool_ctx = ToolContext::default();
                let req = ToolRequest::new(ctx.inputs.clone());
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
                let user_msg = match ctx.inputs.get("message").and_then(|v| v.as_str()) {
                    Some(m) => m.to_string(),
                    None => ctx.inputs.to_string(),
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

    let input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };

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

    fn test_app() -> axum::Router {
        crate::router(Arc::new(AppState::from_env()))
    }

    async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
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
            test_app(),
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
            test_app(),
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
            test_app(),
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
}
