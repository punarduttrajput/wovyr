//! Single-node HTTP server for the Apex Platform API.
//!
//! v0.1 exposes the minimum needed to satisfy the
//! [roadmap](../../docs/18-roadmap/v0.1.md) exit criterion *"a documented agent
//! runs … against a single-node server"*: a health check and a synchronous agent
//! **run** endpoint, following the [Agents API](../../docs/09-api/agents.md) run
//! response shape and the [error envelope](../../docs/09-api/overview.md#8-error-model).
//!
//! Deviation (documented): there is no agent store yet, so the run endpoint is
//! `POST /api/v1/agents:run` and accepts the agent manifest inline rather than
//! `POST /api/v1/agents/{id}:run` against a stored agent. Persistence, streaming
//! (SSE), auth, and idempotency arrive in later milestones.

use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_common::Error;
use apex_provider::{CostEvent, CostObserver, Gateway};
use apex_telemetry::Metrics;
use apex_tools::ToolRegistry;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Shared server state: the LLM gateway, tool registry, metrics, and a run counter.
pub struct AppState {
    gateway: Gateway,
    registry: ToolRegistry,
    metrics: Metrics,
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
        .with_state(state)
}

/// Prometheus metrics endpoint ([metrics §2](../../docs/14-observability/metrics.md)).
async fn metrics_handler(State(state): State<Arc<AppState>>) -> String {
    state.metrics.render_prometheus()
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

/// Run an agent, recording RED golden-signal metrics for the route.
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

/// The actual agent-run logic; returns the [Agents API §5](../../docs/09-api/agents.md) shape.
async fn run_inner(state: &Arc<AppState>, req: RunRequest) -> Result<Json<Value>, ApiError> {
    let def = AgentDefinition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;

    let input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };

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
}
