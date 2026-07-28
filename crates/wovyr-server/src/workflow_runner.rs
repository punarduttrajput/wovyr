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
//! `signal`/`approve` no longer require the client to re-upload the definition YAML
//! on every call (RM-GA-P2 DUR-405): the server resolves the execution's own
//! pinned workflow definition when `manifest` is omitted — see
//! [`resolve_definition`].
//!
//! Execution uses the shared [`wovyr_runtime::PlatformActivityExecutor`]
//! (RM-GA-P4 HLTH-901 — the same dispatch body the CLI's local runner and
//! `wovyr-eval`'s comparison harness use), parameterized here by
//! [`StoredAgentResolver`] for `agent`-typed activities.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

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
use utoipa::ToSchema;
use wovyr_runtime::{AdmissionGuard, AgentResolver, PlatformActivityExecutor};
use wovyr_tenancy::TenancyStore;
use wovyr_tools::ToolRegistry;
use wovyr_workflow::{ActivityContext, Definition};

use crate::{AgentStore, ApiError, AppState, tenancy};

// ── StoredAgentResolver ───────────────────────────────────────────────────────

/// Resolves `agent`-typed activities against a *stored* agent (created via
/// `POST /api/v1/agents`), and supplies the server's platform context around
/// that run:
///
/// - **Tenant scoping** — the agent is looked up in the *submitting tenant's*
///   store (via the `__tenant` marker `submit_handler` stamps into the run
///   input), so a workflow can never reach another tenant's agent. The run
///   itself is `with_tenant(..).with_hosted(true)` (SEC-303: a manifest with no
///   `permissions:` block gets no tool grants, not an unrestricted one — the
///   network-facing default).
/// - **Quota admission** — when the submission also carried an in-scope project
///   (`__project`, from `X-Wovyr-Project`), the run is admitted through the same
///   [`tenancy::admit_run`]/[`tenancy::record_run_usage`] gate a direct
///   `agents:run` call goes through, so a workflow that fans out to N
///   sub-agents draws from one shared project budget (concurrent runs + daily
///   LLM spend) instead of N independent, unmetered ones. The returned
///   [`RunPermit`](tenancy::RunPermit) is boxed as an [`AdmissionGuard`] and
///   held by the shared executor for the run's duration — releasing it only
///   then is what makes the concurrency slot mean anything.
pub struct StoredAgentResolver {
    agents: Arc<AgentStore>,
    tenancy: Arc<dyn TenancyStore>,
    quota: Arc<tenancy::QuotaTracker>,
    /// Per-tenant/per-project LLM cost+token visibility (RM-AIM-P2 OBS-201) — a
    /// sub-agent run's usage is metered here the same way a direct `agents:run` call
    /// is metered in `agents.rs`.
    metrics: wovyr_telemetry::Metrics,
    tenant_label_cap: crate::hardening::TenantLabelCap,
    /// The MCP connection-management runtime (PRD-006, RM-MCX-P2-204): a
    /// workflow's `agent` activity gets the same `spec.mcp_servers`
    /// resolution a direct `agents:run` call does — see
    /// [`AgentResolver::resolve_mcp_tools`].
    mcp: Arc<crate::mcp::McpRuntime>,
    /// The tenant-scoped secret vault a connection's `secret_ref` resolves
    /// against — the same vault `agents.rs`'s `resolve_run_registry` uses.
    secrets: wovyr_secrets::Vault,
}

impl StoredAgentResolver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agents: Arc<AgentStore>,
        tenancy: Arc<dyn TenancyStore>,
        quota: Arc<tenancy::QuotaTracker>,
        metrics: wovyr_telemetry::Metrics,
        tenant_label_cap: crate::hardening::TenantLabelCap,
        mcp: Arc<crate::mcp::McpRuntime>,
        secrets: wovyr_secrets::Vault,
    ) -> Self {
        Self {
            agents,
            tenancy,
            quota,
            metrics,
            tenant_label_cap,
            mcp,
            secrets,
        }
    }

    fn tenant(ctx: &ActivityContext) -> &str {
        ctx.variables
            .get("__tenant")
            .and_then(Value::as_str)
            .unwrap_or(crate::tenancy::DEFAULT_TENANT)
    }

    fn project(ctx: &ActivityContext) -> Option<&str> {
        ctx.variables.get("__project").and_then(Value::as_str)
    }
}

#[async_trait]
impl AgentResolver for StoredAgentResolver {
    async fn resolve(
        &self,
        _ctx: &ActivityContext,
        agent_id: &str,
    ) -> Result<wovyr_agent::AgentDefinition, String> {
        let tenant = Self::tenant(_ctx);
        self.agents
            .definition(tenant, agent_id)
            .ok_or_else(|| format!("no agent `{agent_id}` found"))
    }

    fn customize_options(
        &self,
        ctx: &ActivityContext,
        opts: wovyr_agent::RunOptions,
    ) -> wovyr_agent::RunOptions {
        opts.with_tenant(Self::tenant(ctx)).with_hosted(true)
    }

    async fn admit(&self, ctx: &ActivityContext) -> Result<Box<dyn AdmissionGuard>, String> {
        tenancy::admit_run(&self.tenancy, &self.quota, Self::project(ctx))
            .await
            .map(|permit| Box::new(permit) as Box<dyn AdmissionGuard>)
            .map_err(|e| e.message)
    }

    fn record(&self, ctx: &ActivityContext, usage: &wovyr_common::Usage) {
        tenancy::record_run_usage(
            &self.tenancy,
            &self.quota,
            Self::project(ctx),
            usage.cost_usd,
            u64::from(usage.total_tokens),
        );
        crate::hardening::record_llm_usage_metrics(
            &self.metrics,
            &self.tenant_label_cap,
            Self::tenant(ctx),
            Self::project(ctx),
            usage.cost_usd,
            u64::from(usage.total_tokens),
        );
    }

    async fn resolve_mcp_tools(
        &self,
        ctx: &ActivityContext,
        connection_names: &[String],
        registry: &mut ToolRegistry,
    ) -> Result<Vec<String>, String> {
        self.mcp
            .cache()
            .resolve_agent_mcp_tools(
                self.mcp.store(),
                Some(&self.secrets),
                Self::tenant(ctx),
                connection_names,
                registry,
            )
            .await
            .map_err(|e| e.to_string())
    }
}

/// Build the shared executor for the server: `StoredAgentResolver` for `agent`
/// activities; `tool`/`function`/`ai`/`human` dispatch is identical to the CLI's
/// and eval's — see [`wovyr_runtime::PlatformActivityExecutor`]. Wrapped in the
/// [`crate::ui::UiActivityExecutor`] decorator (RM-GUI-P1 HIL-301), which adds
/// the server-only `ui` activity type: present a policy-checked frame, suspend
/// durably, resume on a validated decision.
#[allow(clippy::too_many_arguments)]
pub(crate) fn server_executor(
    gateway: Arc<wovyr_provider::Gateway>,
    registry: ToolRegistry,
    agents: Arc<AgentStore>,
    tenancy: Arc<dyn TenancyStore>,
    quota: Arc<tenancy::QuotaTracker>,
    metrics: wovyr_telemetry::Metrics,
    tenant_label_cap: crate::hardening::TenantLabelCap,
    ui: Arc<crate::ui::UiRuntime>,
    mcp: Arc<crate::mcp::McpRuntime>,
    secrets: wovyr_secrets::Vault,
) -> crate::ui::UiActivityExecutor<PlatformActivityExecutor> {
    crate::ui::UiActivityExecutor::new(
        PlatformActivityExecutor::new(
            registry,
            gateway,
            Arc::new(StoredAgentResolver::new(
                agents,
                tenancy,
                quota,
                metrics,
                tenant_label_cap,
                mcp,
                secrets,
            )),
        ),
        ui,
    )
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

#[derive(Deserialize, ToSchema)]
pub(crate) struct ValidateRequest {
    manifest: String,
}

/// `POST /api/v1/workflows/validate` — parse the definition YAML and return a DAG
/// summary (activity list, dependency edges, and the validated metadata), or a
/// structured validation error without running anything.
#[utoipa::path(
    post,
    path = "/api/v1/workflows/validate",
    tag = "workflows",
    request_body = ValidateRequest,
    responses(
        (status = 200, description = "A DAG summary: activities, edges, validated metadata."),
        (status = 400, description = "Invalid definition.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn validate_handler(
    Json(req): Json<ValidateRequest>,
) -> Result<Json<Value>, ApiError> {
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

#[derive(Deserialize, ToSchema)]
pub(crate) struct SubmitRequest {
    manifest: String,
    #[serde(default)]
    input: Value,
    /// Optional caller-supplied execution id (auto-generated when absent).
    execution_id: Option<String>,
}

/// `POST /api/v1/workflows` — validate the manifest, create a durable execution,
/// and drive it asynchronously.  Returns immediately with the execution id so the
/// client can poll `GET /api/v1/workflows/{id}` for status.
#[utoipa::path(
    post,
    path = "/api/v1/workflows",
    tag = "workflows",
    request_body = SubmitRequest,
    responses(
        (status = 200, description = "Execution created (status: submitted); poll GET /api/v1/workflows/{id}."),
        (status = 400, description = "Invalid definition.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn submit_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SubmitRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    let def = Definition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    // Persist the manifest by workflow name (RM-GA-P2 EXE-601) so the background
    // timer/schedule dispatchers can resolve a Definition for this execution long
    // after this request's connection is gone — a durable timer can fire days later,
    // with no caller left to re-supply the YAML.
    crate::save_definition(&def.metadata.name, &req.manifest);

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
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "workflow.execution.submit",
        "workflow_execution",
        &execution_id,
    );

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

// ── definition resolution (RM-GA-P2 DUR-405) ────────────────────────────────────

/// Resolve the `Definition` a signal/approve call needs to resume `execution_id`.
///
/// Prefers an explicitly supplied `manifest` (kept for back-compat and for a
/// caller resuming an execution whose workflow was never `POST /api/v1/workflows`-
/// submitted, e.g. one started only through the CLI's local runner) — a
/// mismatched one is still rejected fail-closed by G7's pin check inside
/// `Engine::resume`, so re-uploading the *wrong* definition can't silently replay
/// a different DAG. Otherwise looks the execution's workflow name up via
/// `Engine::query` and resolves it through the same persisted-by-name
/// [`crate::definition_resolver`] EXE-601's background dispatchers already use —
/// `submit_handler` persists every submitted manifest there, so the common case
/// (signal/approve an execution this server itself started) needs only the
/// execution id and event/decision payload, no manifest re-upload.
pub(crate) async fn resolve_definition(
    state: &AppState,
    execution_id: &str,
    manifest: Option<&str>,
) -> Result<Definition, ApiError> {
    if let Some(manifest) = manifest {
        return Definition::from_yaml(manifest).map_err(|e| {
            ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
        });
    }
    let execution = state
        .workflows
        .query(execution_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("execution `{execution_id}` not found"),
            )
        })?;
    crate::definition_resolver()(&execution.workflow_name).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "definition_not_found",
            format!(
                "no persisted definition found for workflow `{}`; supply `manifest` explicitly",
                execution.workflow_name
            ),
        )
    })
}

// ── signal ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub(crate) struct SignalRequest {
    /// The workflow definition YAML. Optional (RM-GA-P2 DUR-405): when omitted,
    /// the server resolves the execution's own workflow definition instead of
    /// requiring the client to re-upload it on every call — see
    /// [`resolve_definition`].
    #[serde(default)]
    manifest: Option<String>,
    /// The event name to deliver (matches `wait: {event: <name>}` in the definition).
    event: String,
    /// Payload injected into `event.<name>` in the workflow variables.
    #[serde(default)]
    payload: Value,
}

/// `POST /api/v1/workflows/{id}/signal` — deliver a named event to a waiting
/// execution and resume it.  Used for `wait: {event: …}` activities.
#[utoipa::path(
    post,
    path = "/api/v1/workflows/{id}/signal",
    tag = "workflows",
    params(("id" = String, Path, description = "The execution id.")),
    request_body = SignalRequest,
    responses(
        (status = 200, description = "Event delivered; execution resumed."),
        (status = 404, description = "Unknown execution.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn signal_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SignalRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    crate::require_workflow_visible(&state, &id, &tenant)?;
    let def = resolve_definition(&state, &id, req.manifest.as_deref()).await?;
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
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "workflow.execution.signal",
        "workflow_execution",
        &id,
    );

    Ok(Json(json!({
        "execution_id": id,
        "event": req.event,
        "status": "signalled",
    })))
}

// ── approve ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub(crate) struct ApproveRequest {
    /// The workflow definition YAML. Optional (RM-GA-P2 DUR-405) — see
    /// [`resolve_definition`].
    #[serde(default)]
    manifest: Option<String>,
    /// The `human` activity id being approved.
    activity_id: String,
    /// Approval decision payload (e.g. `{"approved": true, "comment": "LGTM"}`).
    #[serde(default)]
    decision: Value,
}

/// `POST /api/v1/workflows/{id}/approve` — approve (or reject) a suspended `human`
/// activity and resume the execution.  Internally this is a signal whose key is
/// the activity id, consistent with how the CLI's `workflows approve` command works.
#[utoipa::path(
    post,
    path = "/api/v1/workflows/{id}/approve",
    tag = "workflows",
    params(("id" = String, Path, description = "The execution id.")),
    request_body = ApproveRequest,
    responses(
        (status = 200, description = "Decision delivered; execution resumed."),
        (status = 404, description = "Unknown execution.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    crate::require_workflow_visible(&state, &id, &tenant)?;
    let def = resolve_definition(&state, &id, req.manifest.as_deref()).await?;
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
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "workflow.execution.approve",
        "workflow_execution",
        &id,
    );

    Ok(Json(json!({
        "execution_id": id,
        "activity_id": req.activity_id,
        "status": "approved",
    })))
}

// ── cancel ────────────────────────────────────────────────────────────────────

/// `DELETE /api/v1/workflows/{id}` — cancel a running or waiting execution
/// (RM-GA-P2 EXE-603): [`Engine::cancel`] transitions it to the terminal
/// `Cancelled` state, writes a `WorkflowCancelled` event, and marks every
/// pending/waiting activity `Skipped`. Returns `200` only on a real state
/// transition — an unknown or already-terminal execution is `404`/`409`, never a
/// fake success.
///
/// Note: cancellation is advisory for activities already **in flight** — this only
/// mutates the durable checkpoint, so a step a concurrently-running driver commits
/// immediately afterward is not retroactively undone.
#[utoipa::path(
    delete,
    path = "/api/v1/workflows/{id}",
    tag = "workflows",
    params(("id" = String, Path, description = "The execution id.")),
    responses(
        (status = 200, description = "Execution cancelled (a real state transition, not advisory)."),
        (status = 404, description = "Unknown execution.", body = crate::openapi::ApiErrorBody),
        (status = 409, description = "Execution already terminal.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn cancel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:write")?;
    crate::require_workflow_visible(&state, &id, &tenant)?;
    let cancelled = state.workflows.cancel(&id).await.map_err(ApiError::from)?;
    tracing::info!(execution_id = %id, "execution cancelled");
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "workflow.execution.cancel",
        "workflow_execution",
        &id,
    );
    Ok(Json(json!({
        "execution_id": cancelled.execution_id,
        "status": "cancelled",
    })))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use axum::body::to_bytes;
    use axum::http::Request;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;
    use wovyr_workflow::{
        ActivityError, CheckpointStore, ClosureExecutor, Engine, EventLog, FileStore,
        InMemoryStore, RunOutcome,
    };

    /// State with a fresh, isolated agent store, timer store, *and* workflow engine
    /// (all in-memory), so a test doesn't observe agents/executions/timers
    /// persisted by a prior test or process run against the real
    /// `~/.wovyr/workflows` (DUR-404/EXE-601 made all three durable there) — every
    /// other piece of state (gateway, registry, tenancy, quota) is the real
    /// default. Needed by any test whose assertions depend on a stored agent,
    /// execution, or timer *not* already existing. The timer store is attached to
    /// both the engine (so a `wait` activity actually schedules into it) and
    /// `AppState.timers` (so `spawn_dispatch_loops` polls that same store).
    async fn isolated_state() -> Arc<AppState> {
        let state = AppState::for_test().await;
        let agents = Arc::new(AgentStore::new(None));
        // An isolated, in-memory UI runtime sharing the state's audit log —
        // handed to both the engine's executor and (via `with_ui`) the routes,
        // so a `ui` activity's pending frame is visible to `/api/v1/ui/*`.
        let ui = Arc::new(crate::ui::UiRuntime::in_memory(state.audit.clone()));
        let executor = Arc::new(server_executor(
            state.gateway.clone(),
            state.registry.clone(),
            agents.clone(),
            state.tenancy.clone(),
            state.quota.clone(),
            state.metrics.clone(),
            state.tenant_label_cap.clone(),
            ui.clone(),
            state.mcp.clone(),
            state.secrets.clone(),
        ));
        let store = InMemoryStore::new();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let timers: Arc<dyn wovyr_workflow::TimerStore> =
            Arc::new(wovyr_workflow::InMemoryTimerStore::new());
        let engine = Engine::new(events, checkpoints, executor).with_timer_store(timers.clone());
        Arc::new(
            state
                .with_agents(agents)
                .with_workflows(engine)
                .with_timers(timers)
                .with_ui(ui),
        )
    }

    async fn test_app() -> axum::Router {
        crate::router(Arc::new(AppState::for_test().await))
    }

    /// The default identity these test helpers act as (RM-GA-P4/GA-003): since the
    /// `tenant_authorize` anonymous-default-tenant bypass no longer grants a
    /// credential-less caller anything, every test driving a `workflows:*`/
    /// `agents:*`-gated route through `post_json`/`post_json_state_get` needs a real
    /// principal. `"root"` matches the identical convention `tenancy.rs`'s own tests
    /// already use for the same purpose — setting the same literal value from
    /// multiple test threads is a harmless, idempotent race, not a real one.
    fn ensure_admin_env() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
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
        ensure_admin_env();
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("x-wovyr-principal"))
        {
            builder = builder.header("x-wovyr-principal", "root");
        }
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

    const WAIT_YAML: &str = "\
metadata:\n  name: suspends-forever\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n";

    /// Poll `GET /api/v1/workflows/{id}` until the named activity suspends
    /// (`Waiting`) — unlike `wait_for_terminal`, a suspended execution's top-level
    /// status stays `Running` forever (only the activity itself transitions), so
    /// waiting for a terminal *workflow* status would hang.
    async fn wait_for_activity_waiting(
        state: &Arc<AppState>,
        execution_id: &str,
        activity_id: &str,
    ) -> Value {
        for _ in 0..100 {
            let (st, body) =
                post_json_state_get(state, &format!("/api/v1/workflows/{execution_id}")).await;
            if st == StatusCode::OK && body["execution"]["activities"][activity_id] == "waiting" {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("activity `{activity_id}` on `{execution_id}` did not suspend in time");
    }

    /// RM-GA-P2 EXE-603: `DELETE /api/v1/workflows/{id}` really cancels a suspended
    /// execution — no more the old handler's unconditional `202` with no state
    /// change. Success reports `Cancelled` with a trailing `WorkflowCancelled`
    /// event and the pending activity `Skipped`; a second cancel is `409`, and an
    /// unknown execution is `404`.
    ///
    /// Uses `isolated_state()` rather than the shared `~/.wovyr/workflows`
    /// `AppState::for_test()`: this fixed execution id has been reused by this
    /// test across many sessions, and its real on-disk event log now mixes
    /// pre-API-702 (PascalCase) and post-API-702 (snake_case) `WorkflowEvent`
    /// JSON — the event log is append-only and is never rewritten by `start()`,
    /// only the checkpoint is. A real deployment upgrading past this change hits
    /// the identical incompatibility; there is no migration for it (see the
    /// `WorkflowEvent` doc comment) — but a test has no excuse to depend on
    /// pre-existing disk state at all.
    #[tokio::test]
    async fn cancel_route_really_cancels_a_suspended_execution() {
        let state = isolated_state().await;
        let router = crate::router(state.clone());

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows",
            json!({ "manifest": WAIT_YAML, "execution_id": "cancel-route-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_activity_waiting(&state, "cancel-route-test", "hold").await;
        assert_eq!(detail["execution"]["status"], "running");
        assert_eq!(detail["execution"]["waiting_on"], json!(["hold"]));

        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/workflows/cancel-route-test")
                    .header("x-wovyr-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], "cancelled");

        let (st, detail) = post_json_state_get(&state, "/api/v1/workflows/cancel-route-test").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(detail["execution"]["status"], "cancelled");
        assert_eq!(detail["execution"]["activities"]["hold"], "skipped");
        assert!(
            detail["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "workflow_cancelled"),
            "expected a WorkflowCancelled event: {detail}"
        );

        // A second cancel of an already-terminal execution is a real error, not a
        // repeated fake success.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/workflows/cancel-route-test")
                    .header("x-wovyr-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // An unknown execution is 404, not a fake success either.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/workflows/does-not-exist")
                    .header("x-wovyr-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// RM-GA-P4 OBS-804: submitting and cancelling a workflow execution are both
    /// audited. Mirrors `isolated_state()`'s construction (fresh in-memory engine +
    /// timer store, not the shared `~/.wovyr/workflows`) plus an in-memory audit log.
    #[tokio::test]
    async fn submit_and_cancel_are_audited() {
        use wovyr_audit::{AuditFilter, AuditLog};
        use wovyr_workflow::{CheckpointStore, EventLog, InMemoryStore};

        let base = AppState::for_test().await;
        let agents = Arc::new(AgentStore::new(None));
        let ui = Arc::new(crate::ui::UiRuntime::in_memory(base.audit.clone()));
        let executor = Arc::new(server_executor(
            base.gateway.clone(),
            base.registry.clone(),
            agents.clone(),
            base.tenancy.clone(),
            base.quota.clone(),
            base.metrics.clone(),
            base.tenant_label_cap.clone(),
            ui,
            base.mcp.clone(),
            base.secrets.clone(),
        ));
        let store = InMemoryStore::new();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let timers: Arc<dyn wovyr_workflow::TimerStore> =
            Arc::new(wovyr_workflow::InMemoryTimerStore::new());
        let engine = Engine::new(events, checkpoints, executor).with_timer_store(timers.clone());
        let state = Arc::new(
            base.with_agents(agents)
                .with_workflows(engine)
                .with_timers(timers)
                .with_audit(AuditLog::in_memory()),
        );
        let router = crate::router(state.clone());

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows",
            json!({ "manifest": WAIT_YAML, "execution_id": "audit-cancel-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        wait_for_activity_waiting(&state, "audit-cancel-test", "hold").await;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/workflows/audit-cancel-test")
                    .header("x-wovyr-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let entries = state
            .audit
            .query(&AuditFilter {
                tenant: Some("default".to_string()),
                ..Default::default()
            })
            .unwrap();
        let actions: Vec<&str> = entries.iter().map(|e| e.event.action.as_str()).collect();
        assert!(
            actions.contains(&"workflow.execution.submit"),
            "actions: {actions:?}"
        );
        assert!(
            actions.contains(&"workflow.execution.cancel"),
            "actions: {actions:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.event.resource.id == "audit-cancel-test"),
            "audit entries should reference the execution id: {entries:?}"
        );
    }

    const TIMER_YAML: &str = "\
metadata:\n  name: exe601-timer\nspec:\n  activities:\n    - {id: wait_a, type: wait, inputs: {timer: {after: \"1s\"}}}\n    - {id: after, type: function, name: echo, inputs: {message: done}}\n  transitions:\n    - {from: wait_a, to: after}\n";

    /// RM-GA-P2 EXE-601 acceptance: a workflow submitted over HTTP with a durable
    /// wall-clock timer resumes and completes with the background dispatcher loop
    /// alone — no `wovyr workflows tick` CLI invocation. Before this, the server's
    /// engine had no timer store at all (a `wait` with a wall-clock deadline would
    /// error immediately), and even with one attached, nothing ever polled it.
    #[tokio::test]
    async fn durable_timer_fires_via_the_background_dispatcher_with_no_cli_invocation() {
        let state = isolated_state().await;
        // Persist the definition by name (normally done by submit_handler itself,
        // done here explicitly since isolated_state's engine bypasses AppState's
        // own construction path) so the dispatcher's resolver can find it.
        crate::save_definition("exe601-timer", TIMER_YAML);
        let handles = crate::spawn_dispatch_loops(&state, std::time::Duration::from_millis(50));

        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": TIMER_YAML, "execution_id": "exe601-timer-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        // The timer fires ~1s from now; the dispatcher polls every 50ms, so this
        // must land comfortably within the retry budget below (10s).
        let detail = wait_for_terminal(&state, "exe601-timer-test").await;
        assert_eq!(
            detail["execution"]["status"], "completed",
            "the durable timer must fire and let the workflow complete on its own: {detail}"
        );

        for h in handles {
            h.abort();
        }
    }

    /// RM-GA-P2 EXE-602 acceptance: a server restart no longer strands an
    /// in-flight execution forever. `submit_handler` drives a run on a
    /// fire-and-forget `tokio::spawn`, so a process killed mid-drive leaves the
    /// execution wherever its last checkpoint landed; nothing used to re-scan the
    /// store on the next startup. `resume_in_flight_executions` is that scan.
    #[tokio::test]
    async fn startup_resume_drives_an_interrupted_execution_to_completion_with_no_duplicate_effects()
     {
        let dir = std::env::temp_dir().join(format!("wovyr_server_exe602_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let def = wovyr_workflow::Definition::from_yaml(
            "metadata:\n  name: exe602-restart\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n  transitions:\n    - {from: a, to: b}\n",
        )
        .unwrap();
        // The dispatcher's resolver reads by name from the real, shared
        // definitions directory (unlike the checkpoint store below, this isn't
        // test-injectable) — persist it there so resume_in_flight_executions can
        // find it after the simulated restart.
        crate::save_definition(
            "exe602-restart",
            "metadata:\n  name: exe602-restart\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n  transitions:\n    - {from: a, to: b}\n",
        );

        let a_runs = Arc::new(AtomicUsize::new(0));

        // --- "Instance 1": completes `a`, then interrupts on `b` (simulated crash
        // mid-drive — the same worker-yield `ActivityError::Interrupted` a real
        // crash would leave behind at the last checkpoint). ---
        {
            let store = FileStore::new(&dir).unwrap();
            let events: Arc<dyn EventLog> = Arc::new(store.clone());
            let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
            let executor = ClosureExecutor::new()
                .on("a", {
                    let a_runs = a_runs.clone();
                    move |_| {
                        let a_runs = a_runs.clone();
                        async move {
                            a_runs.fetch_add(1, Ordering::SeqCst);
                            Ok(json!({"a": true}))
                        }
                    }
                })
                .on("b", |_| async {
                    Err(ActivityError::Interrupted("worker crash".into()))
                });
            let engine = Engine::new(events, checkpoints, Arc::new(executor));
            let (outcome, _) = engine
                .run(&def, "exe602-restart-1", json!({}))
                .await
                .unwrap();
            assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        }

        // --- "Instance 2" (a fresh AppState/engine, the shape a restarted process
        // takes): `b` now succeeds — the transient condition that interrupted it
        // has cleared, the same as a real worker restarting cleanly. ---
        let state2 = {
            let base = AppState::for_test().await;
            let store = FileStore::new(&dir).unwrap();
            let events: Arc<dyn EventLog> = Arc::new(store.clone());
            let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
            let executor = ClosureExecutor::new()
                .on("a", |_| async {
                    panic!("completed activity `a` must not be re-executed")
                })
                .on("b", |_| async { Ok(json!({"b": true})) });
            let engine = Engine::new(events, checkpoints, Arc::new(executor));
            Arc::new(base.with_workflows(engine))
        };

        crate::resume_in_flight_executions(&state2).await;

        let status = state2
            .workflows
            .status("exe602-restart-1")
            .await
            .unwrap()
            .expect("execution still exists after the simulated restart");
        assert_eq!(
            status.status,
            wovyr_workflow::WorkflowState::Completed,
            "startup resume must drive the interrupted execution to completion"
        );
        assert_eq!(
            a_runs.load(Ordering::SeqCst),
            1,
            "`a` ran exactly once across the crash + startup resume"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    const DUR405_SIGNAL_YAML: &str = "\
metadata:\n  name: dur405-signal-wait\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n";

    /// RM-GA-P2 DUR-405 acceptance: `POST …/signal` resolves the execution's own
    /// pinned definition (persisted by `submit_handler`, EXE-601) instead of
    /// requiring the client to re-upload the workflow YAML on every call — the
    /// request body carries only the event name.
    ///
    /// Uses `isolated_state()` for the same reason
    /// `cancel_route_really_cancels_a_suspended_execution` does — a fixed
    /// execution id's real on-disk event log can predate API-702's `snake_case`
    /// `WorkflowEvent` change. `save_definition`/`definition_resolver` (what this
    /// test actually exercises) read/write the real `~/.wovyr/workflows/
    /// definitions` directory regardless of which checkpoint/event store the
    /// engine uses, so `isolated_state()` doesn't weaken this test's coverage.
    #[tokio::test]
    async fn signal_succeeds_with_only_execution_id_and_event_when_manifest_is_omitted() {
        let state = isolated_state().await;
        let router = crate::router(state.clone());

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows",
            json!({ "manifest": DUR405_SIGNAL_YAML, "execution_id": "dur405-signal-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        wait_for_activity_waiting(&state, "dur405-signal-test", "hold").await;

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows/dur405-signal-test/signal",
            json!({ "event": "go" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "signalled");

        let detail = wait_for_terminal(&state, "dur405-signal-test").await;
        assert_eq!(detail["execution"]["status"], "completed", "{detail}");
    }

    const DUR405_APPROVE_YAML: &str = "\
metadata:\n  name: dur405-approve-wait\nspec:\n  activities:\n    - {id: review, type: wait, inputs: {event: review}}\n";

    /// Same acceptance for `/approve`: internally a signal keyed by activity id
    /// ([`approve_handler`]), so it resolves the definition the same way. Uses
    /// `isolated_state()` — see the comment on the `signal` version above.
    #[tokio::test]
    async fn approve_succeeds_with_only_execution_id_when_manifest_is_omitted() {
        let state = isolated_state().await;
        let router = crate::router(state.clone());

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows",
            json!({ "manifest": DUR405_APPROVE_YAML, "execution_id": "dur405-approve-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        wait_for_activity_waiting(&state, "dur405-approve-test", "review").await;

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows/dur405-approve-test/approve",
            json!({ "activity_id": "review" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "approved");

        let detail = wait_for_terminal(&state, "dur405-approve-test").await;
        assert_eq!(detail["execution"]["status"], "completed", "{detail}");
    }

    /// A re-uploaded manifest that has drifted from the pinned definition is still
    /// rejected fail-closed (G7). DUR-405 makes `manifest` optional; it doesn't
    /// weaken the drift guard for a caller that still supplies one. Uses
    /// `isolated_state()` — see the comment on the `signal` success test above.
    #[tokio::test]
    async fn signal_rejects_a_drifted_re_uploaded_manifest() {
        let state = isolated_state().await;
        let router = crate::router(state.clone());

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows",
            json!({ "manifest": DUR405_SIGNAL_YAML, "execution_id": "dur405-drift-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        wait_for_activity_waiting(&state, "dur405-drift-test", "hold").await;

        let drifted_yaml = "\
metadata:\n  name: dur405-signal-wait\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n    - {id: extra, type: function, name: echo}\n  transitions:\n    - {from: hold, to: extra}\n";
        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows/dur405-drift-test/signal",
            json!({ "manifest": drifted_yaml, "event": "go" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    }

    /// When no manifest is supplied and the server has no persisted definition for
    /// the execution's workflow name (an execution started without ever going
    /// through `submit_handler`'s `save_definition` step), the fallback is a clear
    /// error asking for the manifest — not a panic or a silent resume-with-nothing.
    #[tokio::test]
    async fn signal_without_manifest_and_without_a_persisted_definition_is_a_clear_error() {
        let state = isolated_state().await;
        let def = wovyr_workflow::Definition::from_yaml(
            "metadata:\n  name: dur405-unpersisted\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n",
        )
        .unwrap();
        state
            .workflows
            .run(&def, "dur405-unpersisted-test", json!({}))
            .await
            .unwrap();
        state.record_workflow_owner("dur405-unpersisted-test", "default");

        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows/dur405-unpersisted-test/signal",
            json!({ "event": "go" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "definition_not_found", "{body}");
    }

    const HUMAN_YAML: &str = "\
metadata:\n  name: human-approval-wf\nspec:\n  activities:\n    - {id: review, type: human}\n";

    /// Poll `GET /api/v1/workflows/{id}` until a `WorkflowInterrupted` event for
    /// `activity_id` appears. Unlike the engine-native `wait` activity type, a
    /// `human` activity never transitions to `ActivityState::Waiting` — an
    /// `ActivityError::Interrupted` resets it to `Ready` instead — so
    /// `wait_for_activity_waiting` doesn't apply here; the interrupted event is
    /// the only durable signal that the submit's background drive actually
    /// reached and attempted the activity before the test approves it.
    async fn wait_for_interrupted_event(
        state: &Arc<AppState>,
        execution_id: &str,
        activity_id: &str,
    ) -> Value {
        for _ in 0..100 {
            let (st, body) =
                post_json_state_get(state, &format!("/api/v1/workflows/{execution_id}")).await;
            if st == StatusCode::OK
                && body["events"].as_array().is_some_and(|events| {
                    events.iter().any(|e| {
                        e["type"] == "workflow_interrupted" && e["activity"] == activity_id
                    })
                })
            {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("activity `{activity_id}` on `{execution_id}` did not interrupt in time");
    }

    /// Regression: `approve_handler`'s decision must actually resolve the
    /// suspended `human` activity, not be silently discarded on every resume.
    /// Before this fix, `ServerExecutor`'s `"human"` branch unconditionally
    /// returned `Interrupted` even after `signal_event` had injected the
    /// decision, so the execution could never leave `Running` no matter how many
    /// times `/approve` was called — the HTTP route reported `200 approved` but
    /// nothing was actually consumed.
    ///
    /// Uses `isolated_state()` (an in-memory event log/checkpoint store), not the
    /// shared `~/.wovyr/workflows` `AppState::for_test()` most of this file's other
    /// tests use for a fixed execution id: `wait_for_interrupted_event` polls the
    /// *accumulated* event history, and a prior run's `WorkflowInterrupted` event
    /// for the same id/activity would satisfy that poll immediately on a repeat
    /// `cargo test` invocation, racing the fresh submission's own background
    /// drive instead of actually waiting for it.
    #[tokio::test]
    async fn approve_decision_is_consumed_and_the_execution_completes() {
        let state = isolated_state().await;
        let router = crate::router(state.clone());

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows",
            json!({ "manifest": HUMAN_YAML, "execution_id": "human-approve-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        wait_for_interrupted_event(&state, "human-approve-test", "review").await;

        let (st, body) = post_json(
            router.clone(),
            "/api/v1/workflows/human-approve-test/approve",
            json!({ "activity_id": "review", "decision": { "approved": true } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "approved");

        let detail = wait_for_terminal(&state, "human-approve-test").await;
        assert_eq!(
            detail["execution"]["status"], "completed",
            "the approval decision must be consumed, not discarded: {detail}"
        );
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
                if !matches!(status, "created" | "scheduled" | "running" | "resumed") {
                    return body;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("execution `{execution_id}` did not reach a terminal state in time");
    }

    async fn post_json_state_get(state: &Arc<AppState>, uri: &str) -> (StatusCode, Value) {
        ensure_admin_env();
        let resp = crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-wovyr-principal", "root")
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
        let state = isolated_state().await;

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
            detail["execution"]["status"], "completed",
            "execution did not complete: {detail}"
        );

        let completed = detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["type"] == "activity_completed" && e["id"] == "greet")
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
        let state = isolated_state().await;
        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": AGENT_YAML, "execution_id": "agent-wf-missing" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "agent-wf-missing").await;
        assert_eq!(
            detail["execution"]["status"], "failed",
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
        let state = isolated_state().await;
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
            detail["execution"]["status"], "completed",
            "execution did not complete: {detail}"
        );

        let events = detail["events"].as_array().unwrap();
        let output_of = |activity_id: &str| -> String {
            events
                .iter()
                .find(|e| e["type"] == "activity_completed" && e["id"] == activity_id)
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
    /// with `X-Wovyr-Project` fails the activity on quota grounds instead of silently
    /// running it unmetered.
    const QUOTA_AGENT_YAML: &str = "\
metadata:\n  name: agent-wf-quota\nspec:\n  activities:\n    - id: greet\n      type: agent\n      name: greeter\n      retry: { max_attempts: 1 }\n      inputs:\n        message: hi\n";

    #[tokio::test]
    async fn agent_activity_respects_project_quota() {
        let state = isolated_state().await;
        state
            .tenancy
            .set_quota(
                "prj-quota-test",
                wovyr_tenancy::QuotaLimits {
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
            &[("x-wovyr-project", "prj-quota-test")],
            json!({ "manifest": QUOTA_AGENT_YAML, "execution_id": "agent-wf-quota-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "agent-wf-quota-test").await;
        assert_eq!(
            detail["execution"]["status"], "failed",
            "execution should fail when the project's agent-run quota is exhausted: {detail}"
        );

        let events = detail["events"].as_array().unwrap();
        let failure = events
            .iter()
            .find(|e| e["type"] == "activity_failed" && e["id"] == "greet")
            .unwrap_or_else(|| panic!("no ActivityFailed event for `greet`: {detail}"));
        let error = failure["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("concurrent_agent_runs"),
            "expected a quota-related failure reason, got: {error}"
        );
    }

    /// RUN-202 acceptance: a sub-agent activity's (non-zero) run cost is charged
    /// to the submitting project's daily accumulator — the same accounting a
    /// direct `agents:run` call feeds — not silently dropped.
    #[tokio::test]
    async fn agent_activity_cost_is_charged_to_the_project_accumulator() {
        let state = isolated_state().await;

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
            &[("x-wovyr-project", "prj-run202-cost")],
            json!({ "manifest": QUOTA_AGENT_YAML, "execution_id": "agent-wf-cost-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let detail = wait_for_terminal(&state, "agent-wf-cost-test").await;
        assert_eq!(
            detail["execution"]["status"], "completed",
            "the agent workflow should complete: {detail}"
        );

        let (spent, tokens) = state
            .quota
            .used_today("prj-run202-cost")
            .expect("the project must have a usage entry for today");
        assert!(
            spent > 0.0,
            "the sub-agent run's cost must land in the project's daily accumulator, got {spent}"
        );
        assert!(
            tokens > 0,
            "the sub-agent run's token usage must land there too (SRV-202), got {tokens}"
        );

        // RM-AIM-P2 OBS-201: the same sub-agent run also lands in the per-tenant LLM
        // usage metric — proving the workflow `agent`-activity path (StoredAgentResolver)
        // records it, not just the direct `agents:run` call sites.
        let out = state.metrics.render_prometheus();
        assert!(
            out.contains(
                r#"wovyr_llm_cost_usd_by_tenant_total{project="prj-run202-cost",tenant="default"}"#
            ),
            "got:\n{out}"
        );
        assert!(
            out.contains(
                r#"wovyr_llm_tokens_by_tenant_total{project="prj-run202-cost",tenant="default"}"#
            ),
            "got:\n{out}"
        );
    }
}
