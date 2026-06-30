//! Single-node HTTP server for the Apex Platform API.
//!
//! v0.1 exposes the minimum needed to satisfy the
//! [roadmap](../../docs/18-roadmap/v0.1.md) exit criterion *"a documented agent
//! runs … against a single-node server"*: a health check and a synchronous agent
//! **run** endpoint, following the [Agents API](../../docs/09-api/agents.md) run
//! response shape and the [error envelope](../../docs/09-api/overview.md#8-error-model).
//!
//! Agents can be **persisted** (`POST/GET /api/v1/agents`, `GET/DELETE
//! /api/v1/agents/{id}`) and run by id (`POST /api/v1/agents/{id}/run`), or run inline
//! via `POST /api/v1/agents:run` (manifest in the body). The store is in-memory for
//! now (durable file/db backing is a later slice). The server also hosts the workflow
//! visibility, multi-tenancy, and webhook routes, and applies the shared `/v1`
//! conventions in [`hardening`]: cursor **pagination** (§6) on list endpoints,
//! **idempotency keys** (§9) on runs, and a **request-id** on every response (§14).

mod audit;
mod hardening;
mod marketplace;
mod memory;
mod plugins;
mod secrets;
mod tenancy;
mod tools;
mod webhooks;
mod workflow_runner;

use apex_agent::{AgentDefinition, NullSink, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_common::Error;
use apex_events::{BackoffPolicy, FileWebhookStore, InMemoryWebhookStore, WebhookStore};
use apex_provider::{CostEvent, CostObserver, Gateway};
use apex_telemetry::Metrics;
use apex_tenancy::{FileTenancyStore, InMemoryTenancyStore, TenancyStore};
use apex_tools::ToolRegistry;
use apex_workflow::{
    CheckpointStore, Engine, EventLog, ExecutionFilter, FileStore, InMemoryStore, WorkflowState,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// In-memory registry of stored agent manifests, keyed by `(tenant, agent id)` so a
/// tenant only ever sees and mutates its own agents (the `metadata.name` is the id,
/// unique *within* a tenant — two tenants may reuse a name without colliding).
/// Manifests are validated on create; durability (file/db) is a later slice.
#[derive(Default)]
struct AgentStore {
    inner: RwLock<BTreeMap<(String, String), String>>,
}

impl AgentStore {
    /// Validate and store a manifest under `tenant`, returning the agent id.
    fn create(&self, tenant: &str, manifest: String) -> Result<String, ApiError> {
        let def = AgentDefinition::from_yaml(&manifest).map_err(|e| {
            ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
        })?;
        let id = def.metadata.name.clone();
        self.inner
            .write()
            .expect("agent store poisoned")
            .insert((tenant.to_string(), id.clone()), manifest);
        Ok(id)
    }

    /// The stored manifest for `id` within `tenant`, if any.
    fn manifest(&self, tenant: &str, id: &str) -> Option<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .get(&(tenant.to_string(), id.to_string()))
            .cloned()
    }

    /// The parsed definition for `id` within `tenant` (manifests are validated on create).
    fn definition(&self, tenant: &str, id: &str) -> Option<AgentDefinition> {
        self.manifest(tenant, id)
            .and_then(|m| AgentDefinition::from_yaml(&m).ok())
    }

    /// All stored agent ids in `tenant`, sorted.
    fn list(&self, tenant: &str) -> Vec<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .iter()
            .filter(|((t, _), _)| t == tenant)
            .map(|((_, id), _)| id.clone())
            .collect()
    }

    /// Remove `id` within `tenant`; returns whether it existed.
    fn delete(&self, tenant: &str, id: &str) -> bool {
        self.inner
            .write()
            .expect("agent store poisoned")
            .remove(&(tenant.to_string(), id.to_string()))
            .is_some()
    }
}

/// Shared server state: the LLM gateway, tool registry, metrics, a run counter, and a
/// read-only workflow engine over the durable store (for visibility endpoints).
pub struct AppState {
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
    metrics: Metrics,
    agents: AgentStore,
    run_counter: AtomicU64,
    /// Read-only engine over the durable workflow store — drives the `GET
    /// /api/v1/workflows*` visibility endpoints (G4). Its executor is never invoked.
    workflows: Engine,
    /// Workflow execution id → owning tenant, stamped at submit so the workflow routes
    /// enforce per-tenant isolation without the (tenant-agnostic) engine. An execution
    /// with no recorded owner belongs to the anonymous `default` space (back-compat).
    /// In-memory for now — durable backing tracks the agent store (a later slice).
    workflow_owners: RwLock<BTreeMap<String, String>>,
    /// Tenancy catalog backing the org/project/membership/quota routes (G: tenancy).
    pub(crate) tenancy: Arc<dyn TenancyStore>,
    /// Per-project run-path quota usage (concurrent runs + daily LLM spend).
    pub(crate) quota: tenancy::QuotaTracker,
    /// Webhook subscriptions; events emitted on mutations are delivered to matches.
    pub(crate) webhooks: Arc<dyn WebhookStore>,
    /// Sends signed webhook payloads (swappable for tests).
    pub(crate) webhook_sender: Arc<dyn webhooks::WebhookSender>,
    /// Retry/backoff policy for webhook delivery.
    pub(crate) webhook_policy: BackoffPolicy,
    /// Monotonic counter for emitted event ids.
    pub(crate) event_counter: AtomicU64,
    /// Caches responses by `Idempotency-Key` so client retries of mutations are safe.
    pub(crate) idempotency: hardening::IdempotencyStore,
    /// Memory engine backing the memory-explorer routes (embeds via the gateway).
    pub(crate) memory: apex_memory::MemoryEngine,
    /// The memory store the engine writes to, kept alongside for namespace/record
    /// enumeration (the engine does not expose its store).
    pub(crate) memory_store: Arc<dyn apex_memory::MemoryStore>,
    /// Secret vault backing the `/api/v1/secrets` routes (tenant-scoped).
    pub(crate) secrets: apex_secrets::Vault,
    /// Tamper-evident audit log; security-sensitive routes append to it.
    pub(crate) audit: apex_audit::AuditLog,
}

impl AppState {
    /// Build state from the environment (provider chosen by `OPENAI_API_KEY`).
    pub fn from_env() -> Self {
        // Export metrics to OTLP when built with `--features otlp` and the endpoint is
        // set; otherwise an in-process-only registry (see `Metrics::with_otlp_export`).
        let metrics = Metrics::with_otlp_export("apex");
        // Cost events from the gateway become LLM token/cost/savings metrics.
        let gateway = Arc::new(Gateway::from_env().with_cost_observer(Arc::new(
            MetricsCostObserver {
                metrics: metrics.clone(),
            },
        )));
        let secrets = default_secrets_vault();
        let mut registry = ToolRegistry::with_builtins();
        // Register enabled plugin tools from the durable catalog into the run registry,
        // routed through a secret-aware runtime (when built with `plugin-wasi`), so agent
        // and workflow runs can invoke them with their tenant-scoped secrets injected.
        // Done before the registry is shared with the workflow engine below.
        plugins::register_enabled_tools(&mut registry, &secrets);
        let (memory, memory_store) = memory::default_engine();
        // Thread gateway + registry into the workflow engine so the ServerExecutor can
        // actually drive function/ai activities when the submit route runs a workflow.
        let workflows = default_workflows_engine(gateway.clone(), registry.clone());
        Self {
            gateway,
            registry,
            metrics,
            agents: AgentStore::default(),
            run_counter: AtomicU64::new(1),
            workflows,
            workflow_owners: RwLock::new(BTreeMap::new()),
            tenancy: default_tenancy_store(),
            quota: tenancy::QuotaTracker::new(),
            webhooks: default_webhook_store(),
            webhook_sender: Arc::new(webhooks::ReqwestSender::default()),
            webhook_policy: BackoffPolicy::default(),
            event_counter: AtomicU64::new(1),
            idempotency: hardening::IdempotencyStore::default(),
            memory,
            memory_store,
            secrets,
            audit: default_audit_log(),
        }
    }

    /// Override the webhook subscription store (tests inject an in-memory store).
    pub fn with_webhooks(mut self, store: Arc<dyn WebhookStore>) -> Self {
        self.webhooks = store;
        self
    }

    /// Override the webhook sender (tests inject a recording sender).
    #[cfg(test)]
    pub(crate) fn with_webhook_sender(mut self, sender: Arc<dyn webhooks::WebhookSender>) -> Self {
        self.webhook_sender = sender;
        self
    }

    /// Override the webhook retry/backoff policy (tests use a zero-delay policy).
    #[cfg(test)]
    pub(crate) fn with_webhook_policy(mut self, policy: BackoffPolicy) -> Self {
        self.webhook_policy = policy;
        self
    }

    /// Override the read-only workflow engine (used by tests to inject a seeded
    /// in-memory store).
    pub fn with_workflows(mut self, engine: Engine) -> Self {
        self.workflows = engine;
        self
    }

    /// Override the tenancy store (used by tests to inject a seeded in-memory store).
    pub fn with_tenancy(mut self, store: Arc<dyn TenancyStore>) -> Self {
        self.tenancy = store;
        self
    }

    /// Override the memory engine + store (tests inject an in-memory store so they don't
    /// touch the shared `~/.apex/memory`).
    #[cfg(test)]
    pub(crate) fn with_memory(
        mut self,
        engine: apex_memory::MemoryEngine,
        store: Arc<dyn apex_memory::MemoryStore>,
    ) -> Self {
        self.memory = engine;
        self.memory_store = store;
        self
    }

    /// Override the secret vault (tests inject an in-memory store).
    #[cfg(test)]
    pub(crate) fn with_secrets(mut self, vault: apex_secrets::Vault) -> Self {
        self.secrets = vault;
        self
    }

    /// Override the audit log (tests inject an in-memory log).
    #[cfg(test)]
    pub(crate) fn with_audit(mut self, audit: apex_audit::AuditLog) -> Self {
        self.audit = audit;
        self
    }

    /// Record the owning tenant of a workflow execution (called at submit).
    fn record_workflow_owner(&self, execution_id: &str, tenant: &str) {
        self.workflow_owners
            .write()
            .expect("workflow owners poisoned")
            .insert(execution_id.to_string(), tenant.to_string());
    }

    /// Whether `tenant` may see/act on workflow execution `execution_id`. An execution
    /// with a recorded owner is visible only to that tenant; one with no recorded owner
    /// belongs to the anonymous `default` space (back-compat for pre-existing or
    /// CLI-created executions).
    fn workflow_visible(&self, execution_id: &str, tenant: &str) -> bool {
        match self
            .workflow_owners
            .read()
            .expect("workflow owners poisoned")
            .get(execution_id)
        {
            Some(owner) => owner == tenant,
            None => tenant == tenancy::DEFAULT_TENANT,
        }
    }
}

/// A durable [`FileTenancyStore`] at `~/.apex/tenancy` (shared with the CLI), falling
/// back to an in-memory store if that directory is unavailable.
fn default_tenancy_store() -> Arc<dyn TenancyStore> {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("tenancy"));
    if let Some(dir) = dir
        && let Ok(store) = FileTenancyStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryTenancyStore::new())
}

/// A secret [`Vault`](apex_secrets::Vault) over a durable [`FileSecretStore`] at
/// `~/.apex/secrets` (shared with the CLI), falling back to in-memory.
fn default_secrets_vault() -> apex_secrets::Vault {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("secrets"));
    let store: Arc<dyn apex_secrets::SecretStore> =
        match dir.and_then(|d| apex_secrets::FileSecretStore::new(d).ok()) {
            Some(s) => Arc::new(s),
            None => Arc::new(apex_secrets::InMemorySecretStore::new()),
        };
    apex_secrets::Vault::new(store)
}

/// A tamper-evident [`AuditLog`](apex_audit::AuditLog) over a durable [`FileAuditSink`]
/// at `~/.apex/audit`, falling back to an in-memory log.
fn default_audit_log() -> apex_audit::AuditLog {
    let sink = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("audit"))
        .and_then(|dir| apex_audit::FileAuditSink::new(dir).ok());
    match sink {
        Some(s) => apex_audit::AuditLog::open(Box::new(s))
            .unwrap_or_else(|_| apex_audit::AuditLog::in_memory()),
        None => apex_audit::AuditLog::in_memory(),
    }
}

/// A durable [`FileWebhookStore`] at `~/.apex/webhooks`, falling back to in-memory.
fn default_webhook_store() -> Arc<dyn WebhookStore> {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| {
            std::path::PathBuf::from(home)
                .join(".apex")
                .join("webhooks")
        });
    if let Some(dir) = dir
        && let Ok(store) = FileWebhookStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryWebhookStore::new())
}

/// An [`Engine`] over the durable workflow store at `~/.apex/workflows` (the same
/// directory the CLI writes to), falling back to an empty in-memory store if that
/// directory is unavailable.  The executor is a [`ServerExecutor`] that handles
/// `function`/`ai`/`human` activities so the write-path submit route can actually
/// drive workflow runs.  The read paths (`list`/`status`/`history`) are unaffected.
fn default_workflows_engine(gateway: Arc<Gateway>, registry: ToolRegistry) -> Engine {
    let executor = Arc::new(workflow_runner::ServerExecutor::new(gateway, registry));
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| {
            std::path::PathBuf::from(home)
                .join(".apex")
                .join("workflows")
        });
    if let Some(dir) = dir
        && let Ok(store) = FileStore::new(dir)
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        return Engine::new(events, checkpoints, executor);
    }
    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    Engine::new(events, checkpoints, executor)
}

/// Translates gateway [`CostEvent`]s into Prometheus metrics
/// ([metrics §6](../../docs/14-observability/metrics.md)).
struct MetricsCostObserver {
    metrics: Metrics,
}

impl CostObserver for MetricsCostObserver {
    fn on_cost(&self, event: CostEvent) {
        self.metrics.counter_add(
            "apex_llm_tokens_total",
            &[("model", &event.model), ("type", "prompt")],
            event.prompt_tokens as f64,
        );
        self.metrics.counter_add(
            "apex_llm_tokens_total",
            &[("model", &event.model), ("type", "completion")],
            event.completion_tokens as f64,
        );
        self.metrics.counter_add(
            "apex_llm_cost_usd_total",
            &[("model", &event.model)],
            event.cost_usd,
        );
        if event.cache.is_some() {
            self.metrics.counter_add(
                "apex_cache_savings_usd_total",
                &[("subsystem", "llm")],
                event.estimated_savings_usd,
            );
        }
    }
}

/// Build the application router over the given state.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .route("/api/v1/agents:run", post(run_handler))
        .route("/api/v1/agents:stream", post(run_stream_handler))
        // Agent persistence: register agents once, then run/inspect them by id.
        .route(
            "/api/v1/agents",
            post(create_agent_handler).get(list_agents_handler),
        )
        .route(
            "/api/v1/agents/{id}",
            get(get_agent_handler).delete(delete_agent_handler),
        )
        .route("/api/v1/agents/{id}/run", post(run_stored_handler))
        // Workflow visibility (G4): list/inspect executions + a minimal read-only UI.
        .route("/api/v1/workflows", get(list_workflows_handler))
        .route("/api/v1/workflows/{id}", get(get_workflow_handler))
        .route("/workflows", get(workflows_ui_handler))
        // Workflow builder write-path: validate, submit, signal, approve, cancel.
        .merge(workflow_runner::routes())
        // Multi-tenancy: organizations, projects, memberships, quotas (RBAC-gated).
        .merge(tenancy::routes())
        // Webhooks: register/list/delete subscriptions (RBAC-gated).
        .merge(webhooks::routes())
        // Memory explorer: namespaces, records, hybrid query, put.
        .merge(memory::routes())
        // Plugins: list the installed catalog, enable/disable.
        .merge(plugins::routes())
        // Marketplace: publish, discover, download, rate, verify, install.
        .merge(marketplace::routes())
        // Secret vault: create/list/get/rotate/delete (tenant-scoped, RBAC-gated).
        .merge(secrets::routes())
        // Audit trail: read the tenant's tamper-evident security records.
        .merge(audit::routes())
        // Tool discovery: list registered tools (built-ins + enabled plugin tools).
        .merge(tools::routes())
        .with_state(state)
        // Stamp every response (incl. errors) with a request id (API overview §14).
        .layer(axum::middleware::from_fn(hardening::request_id))
}

/// Metrics endpoint ([metrics §2](../../docs/14-observability/metrics.md)). Serves
/// **OpenMetrics** (with trace exemplars) when the scraper accepts it, else classic
/// Prometheus text.
async fn metrics_handler(headers: HeaderMap, State(state): State<Arc<AppState>>) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("application/openmetrics-text") {
        (
            [(
                header::CONTENT_TYPE,
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )],
            state.metrics.render_openmetrics(),
        )
            .into_response()
    } else {
        (
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            state.metrics.render_prometheus(),
        )
            .into_response()
    }
}

/// Bind to `addr` and serve until the process is stopped.
pub async fn serve(addr: SocketAddr) -> apex_common::Result<()> {
    let state = Arc::new(AppState::from_env());
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "apex server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| Error::Runtime(format!("server error: {e}")))?;
    Ok(())
}

/// Liveness probe.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Body for `POST /api/v1/agents:run`.
#[derive(Debug, Deserialize)]
struct RunRequest {
    /// The agent manifest (YAML), supplied inline in v0.1.
    manifest: String,
    /// Run input (e.g. `{"message": "..."}`).
    #[serde(default)]
    input: Value,
}

/// Run an agent, recording RED golden-signal metrics for the route. Instrumented so
/// the request runs under a trace whose id becomes the latency exemplar.
#[tracing::instrument(name = "api.agents_run", skip_all)]
async fn run_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Json<Value>, ApiError> {
    let start = Instant::now();
    let tenant = tenancy::run_tenant(&headers);

    // Idempotency (overview §9): replay the original response for a repeated key.
    let idem_key = hardening::idempotency_key(&headers);
    if let Some(key) = &idem_key
        && let Some(cached) = state.idempotency.get(&tenant, key)
    {
        return Ok(Json(cached));
    }

    let result = run_inner(&state, tenant.clone(), tenancy::run_project(&headers), req).await;

    // Cache successful responses so a client retry with the same key is safe.
    if let (Some(key), Ok(Json(body))) = (&idem_key, &result) {
        state.idempotency.put(&tenant, key, body.clone());
    }

    let status = match &result {
        Ok(_) => 200u16,
        Err(e) => e.status.as_u16(),
    };
    state.metrics.counter_inc(
        "apex_api_requests_total",
        &[("route", "agents_run"), ("status", &status.to_string())],
    );
    state.metrics.histogram_observe(
        "apex_api_request_duration_seconds",
        &[("route", "agents_run")],
        start.elapsed().as_secs_f64(),
    );
    result
}

/// Parse the inline manifest then run it ([Agents API §5](../../docs/09-api/agents.md)).
async fn run_inner(
    state: &Arc<AppState>,
    tenant: String,
    project: Option<String>,
    req: RunRequest,
) -> Result<Json<Value>, ApiError> {
    let def = AgentDefinition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    run_definition(state, def, req.input, &tenant, project.as_deref()).await
}

/// A [`RunEventSink`] that forwards each run event to an SSE channel as a JSON frame.
struct ChannelSink {
    tx: futures::channel::mpsc::UnboundedSender<Event>,
}

impl RunEventSink for ChannelSink {
    fn emit(&mut self, event: RunEvent<'_>) {
        let data = match event {
            RunEvent::Start { model, provider } => {
                json!({ "type": "start", "model": model, "provider": provider })
            }
            RunEvent::MemoryRetrieved { source, score } => {
                json!({ "type": "memory", "source": source, "score": score })
            }
            RunEvent::Delta { text } => json!({ "type": "delta", "text": text }),
            RunEvent::ToolCall { name, arguments } => {
                json!({ "type": "tool_call", "name": name, "arguments": arguments })
            }
            RunEvent::ToolResult { name, ok } => {
                json!({ "type": "tool_result", "name": name, "ok": ok })
            }
            RunEvent::Done { usage } => json!({
                "type": "done",
                "usage": { "total_tokens": usage.total_tokens, "cost_usd": usage.cost_usd }
            }),
        };
        let _ = self
            .tx
            .unbounded_send(Event::default().data(data.to_string()));
    }
}

/// `POST /api/v1/agents:stream` — run an inline-manifest agent, streaming its events
/// (start / delta / tool_call / tool_result / done, then a final `result`) as SSE.
async fn run_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Response {
    let def = match AgentDefinition::from_yaml(&req.manifest) {
        Ok(d) => d,
        Err(e) => {
            return ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
                .into_response();
        }
    };
    let input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };

    // Quota gate before streaming begins (429 if exceeded); the permit rides along in
    // the run task and releases when it ends.
    let project = tenancy::run_project(&headers);
    let tenant = tenancy::run_tenant(&headers);
    let permit = match tenancy::admit_run(&state, project.as_deref()) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let (tx, rx) = futures::channel::mpsc::unbounded::<Event>();
    tokio::spawn(async move {
        let _permit = permit;
        let mut sink = ChannelSink { tx: tx.clone() };
        let frame = match run_agent(
            &def,
            &state.gateway,
            &state.registry,
            RunOptions::new(input).with_tenant(tenant),
            &mut sink,
        )
        .await
        {
            Ok(out) => {
                tenancy::record_run_cost(&state, project.as_deref(), out.usage.cost_usd);
                Event::default().event("result").data(
                    json!({ "status": "succeeded", "output": { "message": out.text }, "steps": out.steps })
                        .to_string(),
                )
            }
            Err(e) => Event::default().event("error").data(e.to_string()),
        };
        let _ = tx.unbounded_send(frame);
        // `tx` (and the sink's clone) drop here, closing the SSE stream.
    });

    Sse::new(rx.map(Ok::<Event, Infallible>)).into_response()
}

/// Run a (parsed) agent definition with `input`, returning the run-response shape.
/// Shared by the inline-manifest and stored-agent run endpoints. Enforces the in-scope
/// `project`'s quota (concurrent runs + daily LLM spend) when one is set.
async fn run_definition(
    state: &Arc<AppState>,
    def: AgentDefinition,
    input: Value,
    tenant: &str,
    project: Option<&str>,
) -> Result<Json<Value>, ApiError> {
    let input = if input.is_null() { json!({}) } else { input };

    // Quota gate: hold a concurrency slot for the duration of the run (released on
    // drop), then record the run's cost against the project's daily budget.
    let _permit = tenancy::admit_run(state, project)?;
    let out = match run_agent(
        &def,
        &state.gateway,
        &state.registry,
        RunOptions::new(input).with_tenant(tenant),
        &mut NullSink,
    )
    .await
    {
        Ok(out) => out,
        Err(e) => {
            webhooks::emit(
                state,
                "agent.run.failed",
                tenant,
                json!({ "error": e.to_string() }),
            );
            return Err(ApiError::from(e));
        }
    };
    tenancy::record_run_cost(state, project, out.usage.cost_usd);

    let run_id = format!("run_{}", state.run_counter.fetch_add(1, Ordering::SeqCst));
    webhooks::emit(
        state,
        "agent.run.completed",
        tenant,
        json!({ "run_id": run_id, "total_tokens": out.usage.total_tokens }),
    );
    Ok(Json(json!({
        "run_id": run_id,
        "status": "succeeded",
        "output": { "message": out.text },
        "steps": out.steps,
        "usage": {
            "total_tokens": out.usage.total_tokens,
            "cost_usd": out.usage.cost_usd,
        }
    })))
}

/// Body for `POST /api/v1/agents` — register an agent from its YAML manifest.
#[derive(Debug, Deserialize)]
struct CreateAgentRequest {
    /// The agent manifest (YAML).
    manifest: String,
}

/// Body for `POST /api/v1/agents/{id}/run` — run a stored agent.
#[derive(Debug, Default, Deserialize)]
struct RunStoredRequest {
    /// Run input (e.g. `{"message": "..."}`).
    #[serde(default)]
    input: Value,
}

/// `POST /api/v1/agents` — register an agent; returns its id.
async fn create_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:write")?;
    let id = state.agents.create(&tenant, req.manifest)?;
    Ok(Json(json!({ "id": id, "status": "created" })))
}

/// `GET /api/v1/agents` — list the caller's tenant's agent ids (cursor-paginated,
/// overview §6).
async fn list_agents_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<hardening::PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:read")?;
    let items: Vec<Value> = state
        .agents
        .list(&tenant)
        .into_iter()
        .map(Value::String)
        .collect();
    Ok(Json(hardening::paginate(items, &page.page())))
}

/// `GET /api/v1/agents/{id}` — fetch a stored agent's manifest (within the caller's tenant).
async fn get_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:read")?;
    match state.agents.manifest(&tenant, &id) {
        Some(manifest) => Ok(Json(json!({ "id": id, "manifest": manifest }))),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        )),
    }
}

/// `DELETE /api/v1/agents/{id}` — remove a stored agent (within the caller's tenant).
async fn delete_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:write")?;
    if state.agents.delete(&tenant, &id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        ))
    }
}

/// `POST /api/v1/agents/{id}/run` — run a stored agent by id.
#[tracing::instrument(name = "api.agents_run_stored", skip_all)]
async fn run_stored_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<RunStoredRequest>,
) -> Result<Json<Value>, ApiError> {
    let start = Instant::now();
    let project = tenancy::run_project(&headers);
    // Authorize the run in the caller's tenant, then resolve the agent *within* that
    // tenant — a caller can only run its own tenant's stored agents.
    let result = match tenancy::tenant_authorize(&state, &headers, "agents:run") {
        Ok(tenant) => match state.agents.definition(&tenant, &id) {
            Some(def) => run_definition(&state, def, req.input, &tenant, project.as_deref()).await,
            None => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("agent `{id}` not found"),
            )),
        },
        Err(e) => Err(e),
    };

    let status = match &result {
        Ok(_) => 200u16,
        Err(e) => e.status.as_u16(),
    };
    state.metrics.counter_inc(
        "apex_api_requests_total",
        &[
            ("route", "agents_run_stored"),
            ("status", &status.to_string()),
        ],
    );
    state.metrics.histogram_observe(
        "apex_api_request_duration_seconds",
        &[("route", "agents_run_stored")],
        start.elapsed().as_secs_f64(),
    );
    result
}

/// Query params for `GET /api/v1/workflows`: filters plus cursor pagination.
#[derive(Debug, Deserialize)]
struct WorkflowListQuery {
    /// Filter to a workflow name.
    workflow: Option<String>,
    /// Filter to a status (e.g. `running`, `completed`, `failed`).
    status: Option<String>,
    /// `limit` + `cursor` (overview §6).
    #[serde(flatten)]
    page: hardening::PageQuery,
}

/// `GET /api/v1/workflows` — list executions, optionally filtered (G4 visibility),
/// cursor-paginated (overview §6).
async fn list_workflows_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkflowListQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    let status = match query
        .status
        .as_deref()
        .map(parse_workflow_status)
        .transpose()
    {
        Ok(status) => status,
        Err(msg) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation_failed",
                msg,
            ));
        }
    };
    // Fetch the full filtered set; pagination slices it (the cursor is the offset).
    let filter = ExecutionFilter {
        workflow_name: query.workflow,
        status,
        limit: None,
    };
    let executions = state.workflows.list(&filter).await?;
    let items: Vec<Value> = executions
        .into_iter()
        // Tenant isolation: only show executions this tenant owns.
        .filter(|e| state.workflow_visible(&e.execution_id, &tenant))
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Ok(Json(hardening::paginate(items, &query.page.page())))
}

/// `GET /api/v1/workflows/{id}` — an execution's status plus its event timeline (G4).
async fn get_workflow_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    // Hide cross-tenant executions behind the same 404 as a missing one.
    let missing = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("execution `{id}` not found"),
        )
    };
    if !state.workflow_visible(&id, &tenant) {
        return Err(missing());
    }
    let summary = state.workflows.status(&id).await?.ok_or_else(missing)?;
    let events = state.workflows.history(&id).await?;
    Ok(Json(json!({ "execution": summary, "events": events })))
}

/// Reject access to a workflow execution the caller's tenant does not own, hiding its
/// existence behind the same `404` a missing execution returns (used by the write-path
/// signal/approve/cancel routes).
pub(crate) fn require_workflow_visible(
    state: &AppState,
    id: &str,
    tenant: &str,
) -> Result<(), ApiError> {
    if state.workflow_visible(id, tenant) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("execution `{id}` not found"),
        ))
    }
}

/// `GET /workflows` — a minimal, read-only HTML UI over the visibility API (G4).
async fn workflows_ui_handler() -> Html<&'static str> {
    Html(WORKFLOWS_UI)
}

/// Parse a workflow status name (case-insensitive) for the list filter.
fn parse_workflow_status(s: &str) -> Result<WorkflowState, String> {
    match s.to_ascii_lowercase().as_str() {
        "created" => Ok(WorkflowState::Created),
        "validated" => Ok(WorkflowState::Validated),
        "scheduled" => Ok(WorkflowState::Scheduled),
        "running" => Ok(WorkflowState::Running),
        "waiting" => Ok(WorkflowState::Waiting),
        "resumed" => Ok(WorkflowState::Resumed),
        "compensating" => Ok(WorkflowState::Compensating),
        "completed" => Ok(WorkflowState::Completed),
        "failed" => Ok(WorkflowState::Failed),
        "cancelled" | "canceled" => Ok(WorkflowState::Cancelled),
        other => Err(format!("unknown status `{other}`")),
    }
}

/// A self-contained read-only UI that calls the visibility JSON API.
const WORKFLOWS_UI: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Apex Workflows</title>
<style>
  body { font: 14px system-ui, sans-serif; margin: 0; padding: 1.5rem; color: #1a1a1a; }
  h1 { font-size: 1.2rem; } h2 { font-size: 1rem; margin-top: 1.2rem; }
  .row { display: flex; gap: 1.5rem; align-items: flex-start; }
  .col { flex: 1; min-width: 0; }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: left; padding: .4rem .6rem; border-bottom: 1px solid #e3e3e3; }
  tbody tr { cursor: pointer; } tbody tr:hover { background: #f3f6ff; }
  .pill { padding: .1rem .5rem; border-radius: 1rem; font-size: .8rem; }
  .Running, .Waiting { background: #fff4d6; } .Completed { background: #d8f5dd; }
  .Failed, .Cancelled { background: #fbdcdc; }
  pre { background: #f6f8fa; padding: .8rem; border-radius: 6px; overflow-x: auto; }
  .muted { color: #888; } button { font: inherit; }
</style>
</head>
<body>
  <h1>Apex Workflows</h1>
  <div class="muted">Read-only visibility over <code>/api/v1/workflows</code>.
    <button onclick="load()">Refresh</button></div>
  <div class="row" style="margin-top:1rem">
    <div class="col">
      <h2>Executions</h2>
      <table><thead><tr><th>id</th><th>workflow</th><th>status</th></tr></thead>
      <tbody id="list"><tr><td colspan="3" class="muted">loading…</td></tr></tbody></table>
    </div>
    <div class="col"><h2>Detail</h2><div id="detail" class="muted">Select an execution.</div></div>
  </div>
<script>
async function load() {
  const tbody = document.getElementById('list');
  try {
    const res = await fetch('/api/v1/workflows');
    const data = await res.json();
    const rows = (data.data || []);
    if (!rows.length) { tbody.innerHTML = '<tr><td colspan="3" class="muted">no executions</td></tr>'; return; }
    tbody.innerHTML = '';
    for (const e of rows) {
      const tr = document.createElement('tr');
      tr.onclick = () => detail(e.execution_id);
      tr.innerHTML = `<td>${e.execution_id}</td><td>${e.workflow_name}</td>`
        + `<td><span class="pill ${e.status}">${e.status}</span></td>`;
      tbody.appendChild(tr);
    }
  } catch (err) { tbody.innerHTML = `<tr><td colspan="3">error: ${err}</td></tr>`; }
}
async function detail(id) {
  const el = document.getElementById('detail');
  el.textContent = 'loading…';
  try {
    const res = await fetch('/api/v1/workflows/' + encodeURIComponent(id));
    if (!res.ok) { el.textContent = 'not found'; return; }
    const d = await res.json();
    const acts = Object.entries(d.execution.activities || {})
      .map(([k, v]) => `<li>${k}: <span class="pill ${v}">${v}</span></li>`).join('');
    const events = (d.events || []).map((ev, i) => `${String(i + 1).padStart(3)}. ${JSON.stringify(ev)}`).join('\n');
    el.innerHTML = `<div><b>${d.execution.execution_id}</b> — ${d.execution.workflow_name} `
      + `v${d.execution.workflow_version} — <span class="pill ${d.execution.status}">${d.execution.status}</span></div>`
      + `<ul>${acts}</ul><h2>Timeline</h2><pre>${events.replace(/</g, '&lt;')}</pre>`;
  } catch (err) { el.textContent = 'error: ' + err; }
}
load();
</script>
</body>
</html>"#;

/// An API error rendered as the standard envelope.
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Config(m) | Error::Invalid(m) => {
                ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", m)
            }
            Error::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, "not_found", m),
            Error::Forbidden(m) => ApiError::new(StatusCode::FORBIDDEN, "forbidden", m),
            Error::Conflict(m) => ApiError::new(StatusCode::CONFLICT, "conflict", m),
            Error::QuotaExceeded(m) => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, "quota_exceeded", m)
            }
            Error::Provider(m) => ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", m),
            other => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                other.to_string(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let kind = match self.status.as_u16() {
            400..=499 => "client_error",
            _ => "server_error",
        };
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "type": kind,
                "status": self.status.as_u16(),
            }
        });
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    fn test_app() -> Router {
        router(Arc::new(AppState::from_env()))
    }

    #[tokio::test]
    async fn healthz_ok() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Build a router whose workflow engine is seeded with one completed execution.
    async fn workflow_app() -> Router {
        use apex_workflow::{ClosureExecutor, Definition};
        let store = InMemoryStore::new();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!("ok")) });
        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let def = Definition::from_yaml(
            "metadata:\n  name: demo\nspec:\n  activities:\n    - {id: a, type: function}\n",
        )
        .unwrap();
        engine.run(&def, "demo-1", json!({})).await.unwrap();
        router(Arc::new(AppState::from_env().with_workflows(engine)))
    }

    #[tokio::test]
    async fn lists_and_inspects_workflow_executions() {
        let app = workflow_app().await;

        // List returns the seeded execution.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["data"][0]["execution_id"], "demo-1");
        assert_eq!(v["data"][0]["status"], "Completed");
        assert_eq!(v["has_more"], false);

        // Status filter that excludes it yields an empty list.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows?status=running")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["data"].as_array().unwrap().len(), 0);

        // Detail returns the summary plus a non-empty event timeline.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows/demo-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["execution"]["activities"]["a"], "Completed");
        assert!(v["events"].as_array().unwrap().len() >= 4);

        // Unknown execution → 404.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows/missing")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_returns_agent_output() {
        let app = test_app();
        let body = json!({
            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
            "input": { "message": "ping" }
        })
        .to_string();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert!(v["run_id"].as_str().unwrap().starts_with("run_"));
        assert!(v["output"]["message"].as_str().unwrap().contains("ping"));
    }

    #[tokio::test]
    async fn metrics_endpoint_reflects_a_run() {
        // Share one state across two routers so the run's metrics are visible.
        let state = Arc::new(AppState::from_env());
        let body = json!({
            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
            "input": { "message": "ping" }
        })
        .to_string();

        let run = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(run.status(), StatusCode::OK);

        let metrics = router(state)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metrics.status(), StatusCode::OK);
        let bytes = to_bytes(metrics.into_body(), 64 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(text.contains("apex_api_requests_total"), "metrics:\n{text}");
        assert!(text.contains(r#"route="agents_run""#));
        assert!(text.contains("apex_api_request_duration_seconds_count"));
        // The mock provider reports a cost, so an LLM cost metric is present.
        assert!(text.contains("apex_llm_cost_usd_total"), "metrics:\n{text}");
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_openmetrics_when_accepted() {
        let state = Arc::new(AppState::from_env());
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("accept", "application/openmetrics-text")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            ct.contains("application/openmetrics-text"),
            "content-type was {ct}"
        );
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            text.trim_end().ends_with("# EOF"),
            "OpenMetrics body:\n{text}"
        );
    }

    #[tokio::test]
    async fn invalid_manifest_is_400() {
        let app = test_app();
        let body = json!({ "manifest": "kind: Workflow\nmetadata:\n  name: x\nspec:\n  instructions: hi\n" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "client_error");
    }

    /// POST/GET/DELETE a JSON request against a shared state, returning (status, body).
    async fn req(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
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

    /// Issue an agent request as `principal` acting in `tenant` (with project header),
    /// returning (status, body).
    async fn tenant_req(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        tenant: &str,
        principal: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let resp = raw(
            state,
            method,
            uri,
            &[("x-apex-tenant", tenant), ("x-apex-principal", principal)],
            body,
        )
        .await;
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Cross-tenant isolation: a stored agent created in one tenant is invisible and
    /// inaccessible to a principal of another tenant, and the `X-Apex-Tenant` header
    /// cannot be spoofed by a principal lacking a membership in the claimed tenant.
    /// (v0.3 exit criterion: zero cross-tenant leakage — agents surface.)
    #[tokio::test]
    async fn agents_are_isolated_per_tenant() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        // Two tenants, each with an org-admin and (in acme) a read-only viewer.
        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let member = |user: &str, role, org: &str| Membership {
            user: user.to_string(),
            role,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy
            .add_membership(member("alice", Role::OrgAdmin, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(member("carol", Role::Viewer, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(member("bob", Role::OrgAdmin, &org_b.id))
            .unwrap();
        let state = Arc::new(AppState::from_env().with_tenancy(tenancy));

        let manifest = "metadata:\n  name: secret-agent\nspec:\n  instructions: Be terse.\n";

        // Alice creates an agent in tenant `acme`.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents",
            "acme",
            "alice",
            json!({ "manifest": manifest }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["id"], "secret-agent");

        // Bob (tenant `beta`) sees an empty list and cannot read/run it by id (404).
        let (st, list) =
            tenant_req(&state, "GET", "/api/v1/agents", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["data"], json!([]), "beta must not see acme's agents");
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents/secret-agent",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents/secret-agent/run",
            "beta",
            "bob",
            json!({ "input": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // Header spoofing: Bob claims tenant `acme` but holds no membership there → 403,
        // for both read and the more dangerous delete.
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents/secret-agent",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "spoofed tenant must be denied");
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            "/api/v1/agents/secret-agent",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);

        // A viewer in acme may read but not create or run (scope granularity).
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents/secret-agent",
            "acme",
            "carol",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "viewer may read");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents/secret-agent/run",
            "acme",
            "carol",
            json!({ "input": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer may not run");

        // Alice (real member of acme) retains full access; the agent survived every
        // cross-tenant attempt above.
        let (st, list) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["data"], json!(["secret-agent"]));
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            "/api/v1/agents/secret-agent",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
    }

    /// Cross-tenant isolation for the workflow surface: an execution submitted in one
    /// tenant is invisible and inaccessible (list/get/signal/cancel) to another tenant,
    /// and a spoofed `X-Apex-Tenant` is rejected. (v0.3 exit criterion: zero
    /// cross-tenant leakage — workflows surface.)
    #[tokio::test]
    async fn workflows_are_isolated_per_tenant() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let member = |user: &str, org: &str| Membership {
            user: user.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(member("alice", &org_a.id)).unwrap();
        tenancy.add_membership(member("bob", &org_b.id)).unwrap();
        let state = Arc::new(AppState::from_env().with_tenancy(tenancy));

        let manifest = "metadata:\n  name: iso-wf\nspec:\n  activities:\n    - id: echo-step\n      type: function\n      name: echo\n      inputs:\n        message: hi\n";
        let exec_id = "wf-iso-acme-1";

        // Alice submits a workflow in tenant `acme`.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/workflows",
            "acme",
            "alice",
            json!({ "manifest": manifest, "execution_id": exec_id }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["execution_id"], exec_id);

        // Bob (tenant `beta`) cannot see it in his list, nor read/signal/cancel it (404).
        let (st, list) = tenant_req(
            &state,
            "GET",
            "/api/v1/workflows",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let beta_ids: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["execution_id"].as_str())
            .collect();
        assert!(
            !beta_ids.contains(&exec_id),
            "beta must not see acme's execution: {beta_ids:?}"
        );
        let (st, _) = tenant_req(
            &state,
            "GET",
            &format!("/api/v1/workflows/{exec_id}"),
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = tenant_req(
            &state,
            "POST",
            &format!("/api/v1/workflows/{exec_id}/signal"),
            "beta",
            "bob",
            json!({ "manifest": manifest, "event": "go" }),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "signal must not cross tenants");
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            &format!("/api/v1/workflows/{exec_id}"),
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "cancel must not cross tenants");

        // Header spoofing: Bob claims `acme` but holds no membership there → 403.
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/workflows",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "spoofed tenant must be denied");

        // Alice (real member of acme) sees and can act on her execution.
        let (st, alice_list) = tenant_req(
            &state,
            "GET",
            "/api/v1/workflows",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let acme_ids: Vec<&str> = alice_list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["execution_id"].as_str())
            .collect();
        assert!(
            acme_ids.contains(&exec_id),
            "acme must see its own execution: {acme_ids:?}"
        );
        let (st, _) = tenant_req(
            &state,
            "GET",
            &format!("/api/v1/workflows/{exec_id}"),
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "owner may read its execution");
    }

    /// Cross-tenant isolation for the memory surface: records stored by one tenant are
    /// invisible to another via namespaces, record listing, and hybrid query — even when
    /// both tenants use the same logical namespace — and `X-Apex-Tenant` cannot be
    /// spoofed. (v0.3 exit criterion: zero cross-tenant leakage — memory surface.)
    #[tokio::test]
    async fn memory_is_isolated_per_tenant() {
        use apex_memory::{InMemoryStore, MemoryEngine};
        use apex_provider::Gateway;
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let member = |user: &str, org: &str| Membership {
            user: user.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(member("alice", &org_a.id)).unwrap();
        tenancy.add_membership(member("bob", &org_b.id)).unwrap();

        let store: Arc<dyn apex_memory::MemoryStore> = Arc::new(InMemoryStore::new());
        let engine = MemoryEngine::new(Gateway::from_env(), store.clone());
        let state = Arc::new(
            AppState::from_env()
                .with_tenancy(tenancy)
                .with_memory(engine, store),
        );

        // Both tenants write into the *same* logical namespace "notes".
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory/records",
            "acme",
            "alice",
            json!({ "namespace": "notes", "content": "acme launch codes" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory/records",
            "beta",
            "bob",
            json!({ "namespace": "notes", "content": "beta picnic plan" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // Bob lists records in "notes" — sees only his own, never acme's.
        let (st, list) = tenant_req(
            &state,
            "GET",
            "/api/v1/memory/records?namespace=notes",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let contents: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect();
        assert_eq!(
            contents,
            vec!["beta picnic plan"],
            "beta sees only its own record"
        );

        // Bob's hybrid query cannot surface acme's record.
        let (st, q) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory:query",
            "beta",
            "bob",
            json!({ "text": "launch codes" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let found: Vec<&str> = q["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect();
        assert!(
            !found.iter().any(|c| c.contains("acme")),
            "beta query must not surface acme's record: {found:?}"
        );

        // Header spoofing: bob claims acme but isn't a member → 403.
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/memory/namespaces",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);

        // Alice sees her own record and namespace, and not beta's.
        let (st, ns) = tenant_req(
            &state,
            "GET",
            "/api/v1/memory/namespaces",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(ns["total"], 1, "acme sees only its own record count");
        let (st, q) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory:query",
            "acme",
            "alice",
            json!({ "text": "launch codes" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let found: Vec<&str> = q["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect();
        assert!(
            found.iter().any(|c| c.contains("acme")),
            "acme owner must find its own record: {found:?}"
        );
    }

    /// The secret vault is tenant-scoped, RBAC-gated, and never returns a value over the
    /// API (values leave the vault only via the resolution/injection path).
    #[tokio::test]
    async fn secrets_are_isolated_masked_and_rbac_gated() {
        use apex_secrets::{InMemorySecretStore, Vault};
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let m = |user: &str, role, org: &str| Membership {
            user: user.to_string(),
            role,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy
            .add_membership(m("alice", Role::OrgAdmin, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(m("carol", Role::Viewer, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(m("bob", Role::OrgAdmin, &org_b.id))
            .unwrap();
        let state = Arc::new(
            AppState::from_env()
                .with_tenancy(tenancy)
                .with_secrets(Vault::new(Arc::new(InMemorySecretStore::new()))),
        );

        // Alice creates a secret in acme — the response carries metadata, never the value.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets",
            "acme",
            "alice",
            json!({ "name": "api-key", "value": "s3cr3t-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["reference"], "secret://acme/api-key");
        assert_eq!(body["version"], 1);
        assert!(
            !body.to_string().contains("s3cr3t"),
            "the value must never appear in a response: {body}"
        );

        // A viewer in acme may read but not write (RBAC).
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/secrets",
            "acme",
            "carol",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "viewer may list");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets",
            "acme",
            "carol",
            json!({ "name": "x", "value": "y" }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer may not create");

        // Bob (beta) cannot see or read acme's secret, and cannot spoof the tenant.
        let (st, list) =
            tenant_req(&state, "GET", "/api/v1/secrets", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["total"], 0, "beta has no secrets of its own");
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/secrets/api-key",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/secrets/api-key",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "spoofed tenant denied");

        // Owner can rotate (version bumps) and the value is still never returned.
        let (st, rotated) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets/api-key/rotate",
            "acme",
            "alice",
            json!({ "value": "rotated-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rotated["version"], 2);
        assert!(!rotated.to_string().contains("rotated-value"));

        // Owner can delete.
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            "/api/v1/secrets/api-key",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
    }

    /// Secret-management actions are written to the tamper-evident audit log (by
    /// reference, never value) and readable via the tenant-scoped audit route.
    #[tokio::test]
    async fn secret_mutations_are_audited() {
        use apex_audit::AuditLog;
        use apex_secrets::{InMemorySecretStore, Vault};
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let m = |u: &str, org: &str| Membership {
            user: u.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(m("alice", &org_a.id)).unwrap();
        tenancy.add_membership(m("bob", &org_b.id)).unwrap();
        let state = Arc::new(
            AppState::from_env()
                .with_tenancy(tenancy)
                .with_secrets(Vault::new(Arc::new(InMemorySecretStore::new())))
                .with_audit(AuditLog::in_memory()),
        );

        // Alice creates then rotates a secret.
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets",
            "acme",
            "alice",
            json!({ "name": "api-key", "value": "s3cr3t-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets/api-key/rotate",
            "acme",
            "alice",
            json!({ "value": "rotated-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // The audit trail records both actions, by reference and without the value.
        let (st, audit) =
            tenant_req(&state, "GET", "/api/v1/audit", "acme", "alice", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let entries = audit["entries"].as_array().unwrap();
        let actions: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["event"]["action"].as_str())
            .collect();
        assert!(actions.contains(&"secret.create"), "actions: {actions:?}");
        assert!(actions.contains(&"secret.rotate"), "actions: {actions:?}");
        assert!(
            !audit.to_string().contains("s3cr3t") && !audit.to_string().contains("rotated-value"),
            "audit must not leak secret values: {audit}"
        );
        // Actor + resource are recorded (by reference).
        assert_eq!(entries[0]["event"]["actor"]["principal"], "alice");
        assert_eq!(
            entries[0]["event"]["resource"]["id"],
            "secret://acme/api-key"
        );

        // Tenant-scoped: a beta principal sees none of acme's audit records.
        let (st, beta) =
            tenant_req(&state, "GET", "/api/v1/audit", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(beta["total"], 0);
    }

    #[tokio::test]
    async fn tools_endpoint_lists_builtins_with_descriptions() {
        let state = Arc::new(AppState::from_env());
        let (st, body) = req(&state, "GET", "/api/v1/tools", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let tools = body["tools"].as_array().unwrap();
        // The four built-ins are always registered.
        let ids: Vec<&str> = tools.iter().filter_map(|t| t["id"].as_str()).collect();
        for id in ["echo", "fs_read", "http_get", "shell"] {
            assert!(ids.contains(&id), "missing built-in tool `{id}`: {ids:?}");
        }
        // Each entry carries a non-empty description (what the UI shows).
        let fs_read = tools.iter().find(|t| t["id"] == "fs_read").unwrap();
        assert!(!fs_read["description"].as_str().unwrap_or("").is_empty());
    }

    #[tokio::test]
    async fn agent_persistence_lifecycle() {
        let state = Arc::new(AppState::from_env());
        let manifest = "metadata:\n  name: persisted\nspec:\n  instructions: Be friendly.\n";

        // Create → returns the agent id.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents",
            json!({ "manifest": manifest }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["id"], "persisted");

        // List includes it (paginated envelope); Get returns its manifest.
        let (_, list) = req(&state, "GET", "/api/v1/agents", Value::Null).await;
        assert_eq!(list["data"], json!(["persisted"]));
        assert_eq!(list["total_estimate"], 1);
        let (st, got) = req(&state, "GET", "/api/v1/agents/persisted", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert!(got["manifest"].as_str().unwrap().contains("persisted"));

        // Run by id → succeeds and reflects the mock output.
        let (st, run) = req(
            &state,
            "POST",
            "/api/v1/agents/persisted/run",
            json!({ "input": { "message": "hi there" } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(run["status"], "succeeded");
        assert!(
            run["output"]["message"]
                .as_str()
                .unwrap()
                .contains("hi there")
        );

        // Delete → 204; subsequent get + run-by-id are 404.
        let (st, _) = req(&state, "DELETE", "/api/v1/agents/persisted", Value::Null).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = req(&state, "GET", "/api/v1/agents/persisted", Value::Null).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = req(
            &state,
            "POST",
            "/api/v1/agents/persisted/run",
            json!({ "input": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn streaming_endpoint_emits_sse_event_frames() {
        let state = Arc::new(AppState::from_env());
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:stream")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
                            "input": { "message": "stream me" }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type was {ct}");

        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        // The run streamed start, multiple delta frames, a done, and a final result.
        assert!(body.contains(r#""type":"start""#), "body:\n{body}");
        assert!(
            body.matches(r#""type":"delta""#).count() > 1,
            "expected multiple delta frames:\n{body}"
        );
        assert!(body.contains(r#""type":"done""#), "body:\n{body}");
        assert!(body.contains("event: result"), "body:\n{body}");
        assert!(
            body.contains("stream me"),
            "delta text should reach the client:\n{body}"
        );
    }

    #[tokio::test]
    async fn create_agent_rejects_invalid_manifest() {
        let state = Arc::new(AppState::from_env());
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents",
            json!({ "manifest": "kind: Workflow\nbroken: true\n" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "client_error");
    }

    // --- /v1 hardening: request id, idempotency, pagination ----------------------

    /// Issue a request with arbitrary headers, returning the response (for header asserts).
    async fn raw(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Value,
    ) -> axum::http::Response<axum::body::Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        builder = builder.header("content-type", "application/json");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        router(state.clone())
            .oneshot(
                builder
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn request_id_is_generated_and_honored() {
        let state = Arc::new(AppState::from_env());
        // Generated when absent.
        let resp = raw(&state, "GET", "/healthz", &[], Value::Null).await;
        assert!(resp.headers().get("x-request-id").is_some());
        // Honored when supplied (client correlation).
        let resp = raw(
            &state,
            "GET",
            "/healthz",
            &[("x-request-id", "req-abc")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.headers()["x-request-id"], "req-abc");
    }

    #[tokio::test]
    async fn error_envelope_carries_request_id() {
        let state = Arc::new(AppState::from_env());
        let resp = raw(&state, "GET", "/api/v1/agents/missing", &[], Value::Null).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            v["error"]["request_id"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "error body should carry a request_id: {v}"
        );
    }

    #[tokio::test]
    async fn run_is_idempotent_per_key() {
        let state = Arc::new(AppState::from_env());
        let body = json!({
            "manifest": "metadata:\n  name: idem\nspec:\n  instructions: Hi.\n",
            "input": { "message": "hi" }
        });
        let run = |key: &'static str| {
            let state = state.clone();
            let body = body.clone();
            async move {
                let resp = raw(
                    &state,
                    "POST",
                    "/api/v1/agents:run",
                    &[("idempotency-key", key)],
                    body,
                )
                .await;
                let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                v["run_id"].as_str().unwrap().to_string()
            }
        };
        // Same key → same run_id (replayed); a different key → a fresh run.
        let first = run("k1").await;
        assert_eq!(run("k1").await, first);
        assert_ne!(run("k2").await, first);
    }

    #[tokio::test]
    async fn agent_list_is_cursor_paginated() {
        let state = Arc::new(AppState::from_env());
        for name in ["alpha", "bravo", "charlie"] {
            let m = format!("metadata:\n  name: {name}\nspec:\n  instructions: hi\n");
            req(&state, "POST", "/api/v1/agents", json!({ "manifest": m })).await;
        }
        // First page of 2 → has_more + a cursor.
        let (_, p1) = req(&state, "GET", "/api/v1/agents?limit=2", Value::Null).await;
        assert_eq!(p1["data"].as_array().unwrap().len(), 2);
        assert_eq!(p1["has_more"], true);
        assert_eq!(p1["total_estimate"], 3);
        let cursor = p1["next_cursor"].as_str().unwrap();
        // Following the cursor returns the remainder.
        let (_, p2) = req(
            &state,
            "GET",
            &format!("/api/v1/agents?limit=2&cursor={cursor}"),
            Value::Null,
        )
        .await;
        assert_eq!(p2["data"].as_array().unwrap().len(), 1);
        assert_eq!(p2["has_more"], false);
        assert_eq!(p2["next_cursor"], Value::Null);
    }
}
