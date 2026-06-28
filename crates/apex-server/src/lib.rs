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
//! now (durable file/db backing is a later slice). Streaming (SSE), auth, and
//! idempotency arrive in later milestones.

use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_common::Error;
use apex_provider::{CostEvent, CostObserver, Gateway};
use apex_telemetry::Metrics;
use apex_tools::ToolRegistry;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// In-memory registry of stored agent manifests, keyed by agent id (`metadata.name`).
/// Manifests are validated on create; durability (file/db) is a later slice.
#[derive(Default)]
struct AgentStore {
    inner: RwLock<BTreeMap<String, String>>,
}

impl AgentStore {
    /// Validate and store a manifest, returning the agent id.
    fn create(&self, manifest: String) -> Result<String, ApiError> {
        let def = AgentDefinition::from_yaml(&manifest).map_err(|e| {
            ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
        })?;
        let id = def.metadata.name.clone();
        self.inner
            .write()
            .expect("agent store poisoned")
            .insert(id.clone(), manifest);
        Ok(id)
    }

    /// The stored manifest for `id`, if any.
    fn manifest(&self, id: &str) -> Option<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .get(id)
            .cloned()
    }

    /// The parsed definition for `id` (manifests are validated on create).
    fn definition(&self, id: &str) -> Option<AgentDefinition> {
        self.manifest(id)
            .and_then(|m| AgentDefinition::from_yaml(&m).ok())
    }

    /// All stored agent ids, sorted.
    fn list(&self) -> Vec<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// Remove `id`; returns whether it existed.
    fn delete(&self, id: &str) -> bool {
        self.inner
            .write()
            .expect("agent store poisoned")
            .remove(id)
            .is_some()
    }
}

/// Shared server state: the LLM gateway, tool registry, metrics, and a run counter.
pub struct AppState {
    gateway: Gateway,
    registry: ToolRegistry,
    metrics: Metrics,
    agents: AgentStore,
    run_counter: AtomicU64,
}

impl AppState {
    /// Build state from the environment (provider chosen by `OPENAI_API_KEY`).
    pub fn from_env() -> Self {
        let metrics = Metrics::new();
        // Cost events from the gateway become LLM token/cost/savings metrics.
        let gateway = Gateway::from_env().with_cost_observer(Arc::new(MetricsCostObserver {
            metrics: metrics.clone(),
        }));
        Self {
            gateway,
            registry: ToolRegistry::with_builtins(),
            metrics,
            agents: AgentStore::default(),
            run_counter: AtomicU64::new(1),
        }
    }
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
        .with_state(state)
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
    Json(req): Json<RunRequest>,
) -> Result<Json<Value>, ApiError> {
    let start = Instant::now();
    let result = run_inner(&state, req).await;

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
async fn run_inner(state: &Arc<AppState>, req: RunRequest) -> Result<Json<Value>, ApiError> {
    let def = AgentDefinition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    run_definition(state, def, req.input).await
}

/// Run a (parsed) agent definition with `input`, returning the run-response shape.
/// Shared by the inline-manifest and stored-agent run endpoints.
async fn run_definition(
    state: &Arc<AppState>,
    def: AgentDefinition,
    input: Value,
) -> Result<Json<Value>, ApiError> {
    let input = if input.is_null() { json!({}) } else { input };

    let out = run_agent(
        &def,
        &state.gateway,
        &state.registry,
        RunOptions::new(input),
        &mut NullSink,
    )
    .await
    .map_err(ApiError::from)?;

    let run_id = format!("run_{}", state.run_counter.fetch_add(1, Ordering::SeqCst));
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
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiError> {
    let id = state.agents.create(req.manifest)?;
    Ok(Json(json!({ "id": id, "status": "created" })))
}

/// `GET /api/v1/agents` — list stored agent ids.
async fn list_agents_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "agents": state.agents.list() }))
}

/// `GET /api/v1/agents/{id}` — fetch a stored agent's manifest.
async fn get_agent_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match state.agents.manifest(&id) {
        Some(manifest) => Ok(Json(json!({ "id": id, "manifest": manifest }))),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        )),
    }
}

/// `DELETE /api/v1/agents/{id}` — remove a stored agent.
async fn delete_agent_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.agents.delete(&id) {
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
    Path(id): Path<String>,
    Json(req): Json<RunStoredRequest>,
) -> Result<Json<Value>, ApiError> {
    let start = Instant::now();
    let def = state.agents.definition(&id).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        )
    });
    let result = match def {
        Ok(def) => run_definition(&state, def, req.input).await,
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

/// An API error rendered as the standard envelope.
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
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

        // List includes it; Get returns its manifest.
        let (_, list) = req(&state, "GET", "/api/v1/agents", Value::Null).await;
        assert_eq!(list["agents"], json!(["persisted"]));
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
}
