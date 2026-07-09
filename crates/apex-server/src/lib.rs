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
//!
//! `state`/`config`/`agents` (RM-GA-P4 HLTH-904) split what used to be one 4,300-line
//! god module: `state` owns `AppState` and its persisted-agent/async-run storage,
//! `config` owns backend-construction factories and cross-cutting HTTP limits, and
//! `agents` owns the agent-run + workflow-visibility HTTP handlers plus the shared
//! `ApiError` envelope. Their items are glob-imported here (`use agents::*;` etc.,
//! matching the crate-root-private visibility every other item in this file already
//! had) so every existing `crate::AppState`/`crate::ApiError`/etc. reference elsewhere
//! in this crate — and the inline test module below, via `use super::*` — keeps
//! resolving unchanged.

mod agents;
mod audit;
mod auth;
mod config;
mod hardening;
mod kms;
pub use auth::{ApiKeyStore, AuthMode, FileApiKeyStore, InMemoryApiKeyStore};
mod marketplace;
mod memory;
mod plugins;
mod rate_limit;
mod secrets;
mod state;
mod tenancy;
mod tools;
mod webhook_outbox;
mod webhooks;
mod workflow_runner;

use agents::*;
use config::*;
use state::*;
// `AppState` is also referenced from `tests/authz_matrix.rs`, a real external
// integration test (a separate crate compiled against this crate's public API) —
// `use state::*;` above is a crate-internal (private) glob-import, so it alone
// doesn't reach that external crate. This explicit re-export does.
pub use state::AppState;

use axum::{
    Router,
    error_handling::HandleErrorLayer,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceBuilder;

/// Build the application router over the given state.
///
/// Every route is gated by [`auth::authenticate`] (SEC-101) except the public
/// `/healthz` and `/metrics` — mounted on a separate, unauthenticated sub-router so a
/// load balancer's health probe never needs a credential. Cross-cutting resource
/// limits (SEC-201) wrap the whole router: a body-size cap, then a concurrency cap
/// with load-shedding (`503` past the limit, rather than an unboundedly growing
/// pending-request queue). The direct agent-run endpoints get a longer, dedicated
/// timeout than the rest of the API. Per-key rate limiting ([`rate_limit`], SEC-203)
/// runs *after* auth (so it keys off the verified principal, not a spoofable raw
/// header) with two tiers: a tighter one for the direct agent-run endpoints plus
/// KMS/secrets, a looser one for everything else.
pub fn router(state: Arc<AppState>) -> Router {
    let limits = state.http_limits;
    let metrics = state.metrics.clone();
    let cors = cors_layer(&state.cors_allowed_origins);
    let auth = || axum::middleware::from_fn_with_state(state.clone(), auth::authenticate);
    let rate_limit_sensitive = || {
        let limiter = state.rate_limiter_sensitive.clone();
        axum::middleware::from_fn(move |headers: HeaderMap, req: Request, next: Next| {
            rate_limit::enforce(limiter.clone(), headers, req, next)
        })
    };
    let rate_limit_standard = || {
        let limiter = state.rate_limiter_standard.clone();
        axum::middleware::from_fn(move |headers: HeaderMap, req: Request, next: Next| {
            rate_limit::enforce(limiter.clone(), headers, req, next)
        })
    };
    // `Idempotency-Key` replay (overview §9, RM-GA-P4 API-703) for every mutating
    // route — innermost of the per-group layers (added first below) so a cache hit
    // still passes through auth/rate-limiting first, and only short-circuits the
    // actual handler.
    let idempotency =
        || axum::middleware::from_fn_with_state(state.clone(), hardening::idempotency_middleware);

    // The direct agent-run endpoints call an LLM provider synchronously and can
    // legitimately run far longer than the rest of the API. Expensive, so a tighter
    // rate-limit tier too (SEC-203), shared with KMS/secrets below.
    let run_routes = Router::new()
        .route("/api/v1/agents:run", post(run_handler))
        .route("/api/v1/agents:stream", post(run_stream_handler))
        .route("/api/v1/agents/{id}/run", post(run_stored_handler))
        // Auth first, so the rate limiter keys off the *verified* principal
        // (auth overwrites `X-Apex-Principal`) — not a spoofable raw header.
        .layer(idempotency())
        .layer(rate_limit_sensitive())
        .layer(auth())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.run_timeout_secs)),
        );

    // KMS + secrets: the default (not the run-routes') timeout, but the same tighter
    // rate-limit tier as the agent-run endpoints (SEC-203) — sensitive, not merely
    // slow.
    let sensitive_routes = Router::new()
        .merge(secrets::routes())
        .merge(kms::routes())
        .layer(idempotency())
        .layer(rate_limit_sensitive())
        .layer(auth())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.timeout_secs)),
        );

    let other_protected = Router::new()
        // Agent persistence: register agents once, then run/inspect them by id.
        .route(
            "/api/v1/agents",
            post(create_agent_handler).get(list_agents_handler),
        )
        .route(
            "/api/v1/agents/{id}",
            get(get_agent_handler).delete(delete_agent_handler),
        )
        // Poll a run submitted via `agents:run` with `Prefer: respond-async`
        // (RM-GA-P2 EXE-604) — a cheap in-memory lookup, not an LLM call, so it
        // belongs in the standard rate-limit tier rather than `run_routes`'.
        .route("/api/v1/agents/runs/{run_id}", get(get_run_handler))
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
        // Audit trail: read the tenant's tamper-evident security records.
        .merge(audit::routes())
        // Tool discovery: list registered tools (built-ins + enabled plugin tools).
        .merge(tools::routes())
        .layer(idempotency())
        .layer(rate_limit_standard())
        .layer(auth())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.timeout_secs)),
        );

    let public = Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.timeout_secs)),
        );

    let app = public
        .merge(run_routes)
        .merge(sensitive_routes)
        .merge(other_protected)
        .with_state(state)
        // Stamp every response (incl. errors) with a request id (API overview §14).
        .layer(axum::middleware::from_fn(hardening::request_id))
        // RED metrics for every route (RM-GA-P4 OBS-801) — same outer position as
        // `request_id`/`deprecation_headers` so it also counts requests a handler
        // never sees (an auth 401, a rate-limit 429, an idempotency replay).
        .layer(axum::middleware::from_fn_with_state(
            metrics,
            hardening::track_metrics,
        ))
        // Deprecation/Sunset headers (deprecation-policy.md §4, RM-GA-P4 API-705) —
        // a no-op today since hardening::DEPRECATIONS is empty; applies broadly since
        // any route, not just a mutating one, could be deprecated.
        .layer(axum::middleware::from_fn_with_state(
            hardening::DEPRECATIONS,
            hardening::deprecation_headers,
        ))
        .layer(DefaultBodyLimit::max(limits.max_body_bytes))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .load_shed()
                .concurrency_limit(limits.max_concurrency),
        );

    // Outermost: a CORS preflight (`OPTIONS`) must never reach auth (which would
    // otherwise reject it for lacking a credential) — tower_http's CorsLayer answers
    // preflight requests directly.
    match cors {
        Some(cors) => app.layer(cors),
        None => app,
    }
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

/// Install rustls' `ring` `CryptoProvider` as the process default, idempotently. Both
/// `ring` (via `reqwest`'s rustls-tls elsewhere in this dependency graph) and
/// `aws-lc-rs` can end up link-able in the same binary; rustls then refuses to guess
/// and panics on the first TLS connection unless one is installed explicitly first.
fn install_default_crypto_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// TLS material paths from `APEX_TLS_CERT`/`APEX_TLS_KEY` (PEM files), if both are
/// configured ([SEC-202](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)).
fn tls_cert_key_paths() -> Option<(String, String)> {
    let cert = std::env::var("APEX_TLS_CERT").ok()?;
    let key = std::env::var("APEX_TLS_KEY").ok()?;
    Some((cert, key))
}

/// Refuse a non-loopback bind that is neither TLS-terminated by this process
/// (`has_tls`) nor explicitly declared as terminated upstream by a reverse proxy
/// (`APEX_TLS_TERMINATED_UPSTREAM`) — cleartext HTTP (carrying credentials post-SEC-101
/// and secret responses) must never be the network-facing default (SEC-202). A
/// loopback bind is always allowed TLS-free, matching local/dev ergonomics elsewhere
/// in this hardening pass (SEC-102).
fn check_insecure_bind(has_tls: bool, addr: SocketAddr) -> apex_common::Result<()> {
    let terminated_upstream = std::env::var_os("APEX_TLS_TERMINATED_UPSTREAM").is_some();
    if !has_tls && !terminated_upstream && !addr.ip().is_loopback() {
        return Err(apex_common::Error::config(format!(
            "refusing to bind {addr} without TLS: set APEX_TLS_CERT and APEX_TLS_KEY \
             (PEM files), or APEX_TLS_TERMINATED_UPSTREAM=1 if a reverse proxy already \
             terminates TLS in front of this process"
        )));
    }
    Ok(())
}

/// Bind to `addr` and serve until the process is stopped. Serves HTTPS via an
/// in-process rustls acceptor when `APEX_TLS_CERT`/`APEX_TLS_KEY` are set (SEC-202);
/// otherwise refuses to bind a non-loopback address unless
/// `APEX_TLS_TERMINATED_UPSTREAM` declares TLS is handled by a reverse proxy.
pub async fn serve(addr: SocketAddr) -> apex_common::Result<()> {
    auth::refuse_anonymous_on_non_loopback(addr)?;
    let tls = tls_cert_key_paths();
    check_insecure_bind(tls.is_some(), addr)?;

    let state = Arc::new(AppState::from_env().await);
    // Crash recovery (RM-GA-P2 EXE-602): a `tokio::spawn`'d `resume` that never got
    // to run (the prior process died mid-drive) would otherwise strand its
    // execution in a non-terminal state forever — nothing else re-scans the store.
    resume_in_flight_executions(&state).await;
    // Re-dispatch webhook deliveries left pending when the previous process died
    // (RM-AIM-P1 SRV-103) — they'd otherwise be lost, since delivery was
    // fire-and-forget with no durable record.
    webhooks::recover_outbox(&state);
    // Background dispatcher loops (RM-GA-P2 EXE-601): without these, G1 durable
    // timers and G2 schedules only ever fired when an operator ran the CLI's
    // `apex workflows tick` on the same host — a `wait: {timer: {after: "30d"}}`
    // workflow submitted over HTTP would simply never resume. Aborted below
    // whenever this function's serving future returns, so they don't outlive the
    // HTTP server itself.
    let dispatch_interval = Duration::from_secs(env_u64("APEX_DISPATCH_INTERVAL_SECS", 5));
    let dispatch_handles = spawn_dispatch_loops(&state, dispatch_interval);
    let app = router(state);
    // Bounded drain deadline: how long graceful shutdown waits for in-flight requests
    // before forcing the listener closed (SRV-101).
    let grace = Duration::from_secs(env_u64("APEX_SHUTDOWN_GRACE_SECS", 30));

    // Graceful shutdown (RM-AIM-P1 SRV-101): on SIGINT/SIGTERM, stop accepting new
    // connections and let in-flight requests finish (within `grace`) rather than
    // dropping them. The serving future then returns and the dispatch loops below are
    // aborted — previously that abort only ever ran on a hard serve error, since
    // nothing signaled a clean stop.
    let result = match tls {
        Some((cert, key)) => serve_tls(addr, &cert, &key, app, grace, shutdown_signal()).await,
        None => match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "apex server listening (http)");
                serve_http(listener, app, shutdown_signal()).await
            }
            Err(e) => Err(apex_common::Error::Io(e)),
        },
    };

    // The serving future returned — a clean drained shutdown or a hard error. Either
    // way stop the background dispatcher loops so they don't outlive the HTTP server.
    for handle in dispatch_handles {
        handle.abort();
    }
    tracing::info!("apex server stopped");
    result
}

/// Serve `app` over plain HTTP on `listener` until `shutdown` resolves, then drain
/// in-flight requests. Extracted from [`serve`] so tests can drive shutdown via a
/// future they control instead of a process-global OS signal.
async fn serve_http(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> apex_common::Result<()> {
    // `with_connect_info` so `rate_limit`'s client-IP fallback (SEC-203) sees the real
    // peer address for callers with no verified principal.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| apex_common::Error::Runtime(format!("server error: {e}")))
}

/// Serve `app` over HTTPS until `shutdown` resolves, then drain in-flight requests
/// within `grace` via `axum_server`'s own graceful-shutdown handle.
async fn serve_tls(
    addr: SocketAddr,
    cert: &str,
    key: &str,
    app: Router,
    grace: Duration,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> apex_common::Result<()> {
    install_default_crypto_provider();
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
        .await
        .map_err(|e| apex_common::Error::config(format!("failed to load TLS cert/key: {e}")))?;
    tracing::info!(%addr, "apex server listening (https)");
    let handle = axum_server::Handle::new();
    let drain = handle.clone();
    tokio::spawn(async move {
        shutdown.await;
        // Stop accepting, then wait up to `grace` for in-flight connections.
        drain.graceful_shutdown(Some(grace));
    });
    axum_server::bind_rustls(addr, config)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| apex_common::Error::Runtime(format!("server error: {e}")))
}

/// Resolve when the process is asked to stop: SIGINT (Ctrl-C) on any platform, or
/// SIGTERM on Unix (what systemd/Kubernetes send). The signal is only *observed* here;
/// the caller decides what draining to do (SRV-101).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // If the handler can't be installed, never resolve via this arm.
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining in-flight requests");
}

#[cfg(test)]
mod tests {
    use super::*;
    // The original, unabridged top-level import block from before the HLTH-904
    // split, moved here rather than kept at the file's top level: none of it is
    // needed by this file's own (now-trimmed) `router()`/`serve()`/`metrics_handler`
    // code, only by this ~2,100-line test module, which resolves these bare names
    // via this `mod`-local `use` exactly as it always resolved them via the
    // module-level one before the split — `super::*` alone doesn't carry a
    // module-private `use` binding here as far up as this immediate scope.
    use apex_tenancy::{InMemoryTenancyStore, TenancyStore};
    use apex_tools::ToolRegistry;
    use apex_workflow::{CheckpointStore, Engine, EventLog, InMemoryStore};
    use serde_json::{Value, json};
    use std::convert::Infallible;
    use std::sync::atomic::Ordering;

    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    async fn test_app() -> Router {
        router(Arc::new(AppState::from_env().await))
    }

    #[tokio::test]
    async fn healthz_ok() {
        let app = test_app().await;
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
        router(Arc::new(AppState::from_env().await.with_workflows(engine)))
    }

    #[tokio::test]
    async fn lists_and_inspects_workflow_executions() {
        ensure_admin_env();
        let app = workflow_app().await;

        // List returns the seeded execution.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows")
                    .header("x-apex-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["data"][0]["execution_id"], "demo-1");
        assert_eq!(v["data"][0]["status"], "completed");
        assert_eq!(v["has_more"], false);

        // Status filter that excludes it yields an empty list.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows?status=running")
                    .header("x-apex-principal", "root")
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
                    .header("x-apex-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["execution"]["activities"]["a"], "completed");
        assert!(v["events"].as_array().unwrap().len() >= 4);

        // Unknown execution → 404.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows/missing")
                    .header("x-apex-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_returns_agent_output() {
        let app = test_app().await;
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

    /// RM-GA-P2 EXE-604 acceptance: `Prefer: respond-async` returns a run id
    /// immediately (before the model/tool loop even starts), and polling `GET
    /// /api/v1/agents/runs/{id}` observes `running` then `succeeded` with the same
    /// output shape the synchronous path returns inline.
    #[tokio::test]
    async fn async_run_returns_immediately_then_polling_reflects_completion() {
        let state = Arc::new(AppState::from_env().await);
        let body = json!({
            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
            "input": { "message": "ping" }
        });

        let resp = raw(
            &state,
            "POST",
            "/api/v1/agents:run",
            &[("prefer", "respond-async")],
            body,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let submitted: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(submitted["status"], "running");
        let run_id = submitted["run_id"].as_str().unwrap().to_string();
        assert!(run_id.starts_with("run_"));

        // Poll until it finishes — the mock provider is near-instant, but the result
        // lands on a background task, not synchronously with the submit response.
        let mut final_body = None;
        for _ in 0..100 {
            let (st, poll_body) = req(
                &state,
                "GET",
                &format!("/api/v1/agents/runs/{run_id}"),
                Value::Null,
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            if poll_body["status"] != "running" {
                final_body = Some(poll_body);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let final_body = final_body.expect("async run did not finish in time");
        assert_eq!(final_body["status"], "succeeded");
        assert_eq!(final_body["run_id"], run_id);
        assert!(
            final_body["output"]["message"]
                .as_str()
                .unwrap()
                .contains("ping")
        );
        assert!(final_body["usage"]["total_tokens"].is_number());
    }

    /// A run rejected by the project quota gate never gets a run id at all — the
    /// async path fails closed up front rather than reporting `failed` later for a
    /// run that never started.
    #[tokio::test]
    async fn async_run_quota_rejection_returns_no_run_id() {
        let state = Arc::new(AppState::from_env().await);
        state
            .tenancy
            .set_quota(
                "prj-async-block",
                apex_tenancy::QuotaLimits {
                    concurrent_agent_runs: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();

        let resp = raw(
            &state,
            "POST",
            "/api/v1/agents:run",
            &[
                ("prefer", "respond-async"),
                ("x-apex-project", "prj-async-block"),
            ],
            json!({
                "manifest": "metadata:\n  name: q\nspec:\n  instructions: Hi.\n",
                "input": { "message": "hi" }
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// Polling an unknown run, or one belonging to another tenant, is `404` — the
    /// same "hidden behind not-found" discipline every other tenant-scoped resource
    /// in this crate uses (never a `403` that would confirm the run exists).
    #[tokio::test]
    async fn async_run_polling_is_tenant_scoped_and_404s_on_unknown() {
        let state = Arc::new(AppState::from_env().await);

        let (st, _) = req(
            &state,
            "GET",
            "/api/v1/agents/runs/run_does_not_exist",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        let resp = raw(
            &state,
            "POST",
            "/api/v1/agents:run",
            &[("prefer", "respond-async")],
            json!({
                "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
                "input": { "message": "ping" }
            }),
        )
        .await;
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let submitted: Value = serde_json::from_slice(&bytes).unwrap();
        let run_id = submitted["run_id"].as_str().unwrap();

        // A different tenant polling the same run id sees the same 404 as unknown.
        let resp = raw(
            &state,
            "GET",
            &format!("/api/v1/agents/runs/{run_id}"),
            &[("x-apex-tenant", "someone-else")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// A caller-supplied `max_steps` reaches [`apex_agent::RunOptions`] rather than
    /// always falling back to the crate default. `max_steps: 0` can't complete even one
    /// model turn, so it must fail instead of silently running with the default budget.
    #[tokio::test]
    async fn max_steps_override_is_honored() {
        let state = Arc::new(AppState::from_env().await);
        let manifest = "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n";

        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" }, "max_steps": 0 }),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("0 steps"),
            "{body}"
        );

        // Omitting the field keeps the existing default-budget behavior.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
    }

    /// An agent's own `spec.max_steps` (settable from the Agent Studio UI) is used as
    /// the run's default budget, but a caller-supplied request-level override still
    /// takes precedence over it — checked via the stored-agent run path too, since that
    /// resolves the definition server-side rather than trusting a request field.
    #[tokio::test]
    async fn agent_level_max_steps_is_a_default_not_a_floor() {
        let state = Arc::new(AppState::from_env().await);
        let manifest =
            "metadata:\n  name: zero-budget\nspec:\n  instructions: Be friendly.\n  max_steps: 0\n";

        // No request-level override → the agent's own (unworkable) budget applies and fails.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" } }),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("0 steps"),
            "{body}"
        );

        // A request-level override wins over the agent's own default.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" }, "max_steps": 5 }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        // Same precedence via the stored-agent path (POST /api/v1/agents/{id}/run), which
        // resolves the definition from the store rather than the inline request body.
        let (_, created) = req(
            &state,
            "POST",
            "/api/v1/agents",
            json!({ "manifest": manifest }),
        )
        .await;
        let id = created["id"].as_str().unwrap();
        let (st, body) = req(
            &state,
            "POST",
            &format!("/api/v1/agents/{id}/run"),
            json!({ "input": { "message": "ping" } }),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    }

    #[tokio::test]
    async fn metrics_endpoint_reflects_a_run() {
        // Share one state across two routers so the run's metrics are visible.
        let state = Arc::new(AppState::from_env().await);
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

        // A second, unrelated route group (RM-GA-P4 OBS-801: every route now emits
        // RED metrics, not just the two `agents:run`/`agents/{id}/run` handlers that
        // used to hand-roll their own recording).
        let tools = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/tools")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tools.status(), StatusCode::OK);

        // And a request the router rejects before any handler runs — an unknown path
        // never reaches a `route_layer`-style handler-adjacent metric, but this
        // whole-app middleware still counts it (under the "unmatched" label).
        let unknown = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/does-not-exist")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

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
        assert!(text.contains(r#"route="tools_list""#), "metrics:\n{text}");
        // Labels render sorted by key (method, route, status).
        assert!(
            text.contains(r#"route="unmatched",status="404""#),
            "metrics:\n{text}"
        );
        assert!(text.contains("apex_api_request_duration_seconds_count"));
        // The mock provider reports a cost, so an LLM cost metric is present.
        assert!(text.contains("apex_llm_cost_usd_total"), "metrics:\n{text}");
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_openmetrics_when_accepted() {
        let state = Arc::new(AppState::from_env().await);
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
        let app = test_app().await;
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
    /// The default identity this helper acts as (RM-GA-P4/GA-003): the
    /// `tenant_authorize` anonymous-default-tenant bypass no longer grants a
    /// credential-less caller anything, so every `req()`-driven test hitting a
    /// tenant-scoped route needs a real principal. `"root"` matches the identical
    /// convention `tenancy.rs`'s own tests already use — setting the same literal
    /// value from multiple test threads is a harmless, idempotent race.
    fn ensure_admin_env() {
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
    }

    async fn req(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        ensure_admin_env();
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("x-apex-principal", "root")
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
        let state = Arc::new(AppState::from_env().await.with_tenancy(tenancy));

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
    ///
    /// Uses an isolated in-memory workflow engine rather than the shared
    /// `~/.apex/workflows` `AppState::from_env()` default: the `GET
    /// /api/v1/workflows` list route scans *every* checkpoint in that directory
    /// (not just this test's own execution), and real accumulated checkpoints
    /// written before API-702's `snake_case` `WorkflowState`/`ActivityState`
    /// change no longer deserialize — the identical incompatibility a real
    /// deployment would hit on upgrade, which a test has no reason to depend on.
    #[tokio::test]
    async fn workflows_are_isolated_per_tenant() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};
        use apex_workflow::ClosureExecutor;

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

        let store = InMemoryStore::new();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let executor = ClosureExecutor::new().on("echo-step", |_| async { Ok(json!("ok")) });
        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_workflows(engine),
        );

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
                .await
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
        let found: Vec<&str> = q["data"]
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
        let found: Vec<&str> = q["data"]
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
                .await
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
        assert_eq!(list["total_estimate"], 0, "beta has no secrets of its own");
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
                .await
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
        let entries = audit["data"].as_array().unwrap();
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
        assert_eq!(beta["total_estimate"], 0);
    }

    /// A fresh in-memory-backed KMS for tests, isolated from the shared `~/.apex/kms`.
    #[cfg(test)]
    fn test_kms() -> Arc<dyn apex_kms::Kms> {
        Arc::new(apex_kms::LocalKms::new(
            apex_kms::generate_key().unwrap(),
            Arc::new(apex_kms::InMemoryKmsStore::new()),
        ))
    }

    #[tokio::test]
    async fn kms_rotate_is_routine_but_destroy_needs_a_higher_tier() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let m = |user: &str, role| Membership {
            user: user.to_string(),
            role,
            scope: MemberScope::Organization(org.id.clone()),
        };
        tenancy.add_membership(m("vic", Role::Viewer)).unwrap();
        tenancy.add_membership(m("edna", Role::Editor)).unwrap();
        tenancy.add_membership(m("alice", Role::OrgAdmin)).unwrap();
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_kms(test_kms()),
        );

        // A viewer can do neither.
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "vic",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer must not rotate");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "vic",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer must not destroy");

        // An editor may rotate (routine, `kms:write`) but not destroy (`kms:admin`).
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "edna",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["version"], 2); // provisions v1 on first use, then rolls to v2
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "edna",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "editor must not destroy");

        // An org admin may destroy — irreversibly: acme's key is crypto-shredded, so
        // even the org admin's own (RBAC-permitted) rotate now fails closed.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "destroyed");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "a destroyed tenant key must fail closed even for an org admin"
        );
    }

    /// RM-GA-P4 OBS-804: create/run/delete on a stored agent are all audited.
    #[tokio::test]
    async fn agent_mutations_are_audited() {
        use apex_audit::AuditLog;
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        tenancy
            .add_membership(Membership {
                user: "alice".to_string(),
                role: Role::OrgAdmin,
                scope: MemberScope::Organization(org.id.clone()),
            })
            .unwrap();
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_audit(AuditLog::in_memory()),
        );

        let manifest = "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n";
        let (st, created) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents",
            "acme",
            "alice",
            json!({ "manifest": manifest }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{created}");
        let id = created["id"].as_str().unwrap().to_string();

        let (st, _) = tenant_req(
            &state,
            "POST",
            &format!("/api/v1/agents/{id}/run"),
            "acme",
            "alice",
            json!({ "input": { "message": "hi" } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, _) = tenant_req(
            &state,
            "DELETE",
            &format!("/api/v1/agents/{id}"),
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT);

        let (st, audit) =
            tenant_req(&state, "GET", "/api/v1/audit", "acme", "alice", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let actions: Vec<&str> = audit["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["event"]["action"].as_str())
            .collect();
        assert!(actions.contains(&"agent.create"), "actions: {actions:?}");
        assert!(actions.contains(&"agent.run"), "actions: {actions:?}");
        assert!(actions.contains(&"agent.delete"), "actions: {actions:?}");
    }

    #[tokio::test]
    async fn kms_tenant_key_mutations_are_audited() {
        use apex_audit::AuditLog;
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
                .await
                .with_tenancy(tenancy)
                .with_kms(test_kms())
                .with_audit(AuditLog::in_memory()),
        );

        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, audit) =
            tenant_req(&state, "GET", "/api/v1/audit", "acme", "alice", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let entries = audit["data"].as_array().unwrap();
        let actions: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["event"]["action"].as_str())
            .collect();
        assert!(
            actions.contains(&"kms.tenant_key.rotate"),
            "actions: {actions:?}"
        );
        assert!(
            actions.contains(&"kms.tenant_key.destroy"),
            "actions: {actions:?}"
        );
        assert_eq!(entries[0]["event"]["actor"]["principal"], "alice");
        assert_eq!(entries[0]["event"]["resource"]["id"], "acme");

        // Tenant-scoped: beta sees none of acme's kms audit records.
        let (st, beta) =
            tenant_req(&state, "GET", "/api/v1/audit", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(beta["total_estimate"], 0);
    }

    /// **RM-GA-P4/GA-003, narrowing SEC-102**: `tenant_authorize` used to skip its
    /// RBAC check entirely for a request with no `X-Apex-Principal` against the
    /// `default` tenant whenever `AppState.anonymous_allowed` — meaning
    /// `APEX_ALLOW_ANONYMOUS=1` alone let an anonymous caller crypto-shred the
    /// default tenant's KMS key material with zero grant (a documented residual
    /// finding, [compliance-mapping.md §7](../../docs/13-security/compliance-mapping.md#7-residual-risk-and-gaps)).
    /// That bypass is now deleted: `anonymous_allowed` governs only whether
    /// [`auth::authenticate`]'s `disabled-loopback` mode lets the request *reach* a
    /// handler at all — RBAC downstream is unconditional. An anonymous caller with
    /// `anonymous_allowed = true` now gets exactly as far as any other principal
    /// with no memberships: past authentication (proven by the `403`, not a `401`),
    /// then denied by RBAC.
    #[tokio::test]
    async fn anonymous_default_tenant_caller_reaches_kms_admin_only_up_to_the_auth_layer_now() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_kms(test_kms())
                .with_anonymous_allowed(true),
        );

        // No `X-Apex-Principal` header, default tenant: authentication passes
        // (`anonymous_allowed`), but RBAC now fail-closes unconditionally — no more
        // "destroyed" on the wire for a credential-less caller, ever.
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "anonymity must no longer grant kms:write, even with APEX_ALLOW_ANONYMOUS=1"
        );

        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "anonymity must no longer grant kms:admin (crypto-shredding), even with APEX_ALLOW_ANONYMOUS=1"
        );
    }

    /// **SEC-102, production default**: with the anonymous escape hatch disabled (no
    /// `APEX_ALLOW_ANONYMOUS`), a credential-less request to a protected route is
    /// rejected at the perimeter by [`auth::authenticate`] (SEC-101) before
    /// `tenant_authorize`'s own (now equally fail-closed) RBAC check ever runs.
    #[tokio::test]
    async fn anonymous_default_tenant_caller_is_denied_when_the_flag_is_off() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_kms(test_kms())
                .with_anonymous_allowed(false),
        );
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn tools_endpoint_lists_builtins_with_descriptions() {
        let state = Arc::new(AppState::from_env().await);
        let (st, body) = req(&state, "GET", "/api/v1/tools", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let tools = body["data"].as_array().unwrap();
        // The safe-by-default built-ins are always registered.
        let ids: Vec<&str> = tools.iter().filter_map(|t| t["id"].as_str()).collect();
        for id in ["echo", "fs_read", "http_get"] {
            assert!(ids.contains(&id), "missing built-in tool `{id}`: {ids:?}");
        }
        // shell is NOT registered by default in a hosted server (SEC-301).
        assert!(!ids.contains(&"shell"), "shell must be opt-in: {ids:?}");
        // Each entry carries a non-empty description (what the UI shows).
        let fs_read = tools.iter().find(|t| t["id"] == "fs_read").unwrap();
        assert!(!fs_read["description"].as_str().unwrap_or("").is_empty());
    }

    /// `APEX_ENABLE_SHELL_TOOL=1` is the explicit operator opt-in (SEC-301) that
    /// re-enables `shell` in the server's own registry. Exercised via `with_registry`
    /// (what `from_env()` would build with the flag set) rather than actually
    /// mutating the process-global env var, which every other test in this crate's
    /// default shell-disabled behavior depends on.
    #[tokio::test]
    async fn shell_tool_opt_in_env_var_re_enables_it() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_registry(ToolRegistry::with_privileged_builtins()),
        );
        let (st, body) = req(&state, "GET", "/api/v1/tools", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["id"].as_str())
            .collect();
        assert!(ids.contains(&"shell"));
    }

    /// RM-GA-P2 DUR-404 acceptance: create an agent as tenant T, then open a *fresh*
    /// `AgentStore` instance against the same directory (the same "simulated restart"
    /// stand-in the crash-recovery tests elsewhere in this workspace use, since a real
    /// process restart isn't practical inside a unit test) — T's agent must still be
    /// visible, and it alone: the anonymous `default` tenant must not see it.
    #[test]
    fn agent_store_survives_a_restart_and_stays_tenant_scoped() {
        let dir =
            std::env::temp_dir().join(format!("apex_server_agent_restart_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("agents.json");

        {
            let store = AgentStore::new(Some(path.clone()));
            store
                .create(
                    "acme",
                    "metadata:\n  name: restart-test\nspec:\n  instructions: hi\n".to_string(),
                )
                .unwrap();
        }

        // A fresh instance — no in-memory state carried over — reopened against the
        // same path, the same shape a server restart takes.
        let reopened = AgentStore::new(Some(path));
        assert_eq!(reopened.list("acme"), vec!["restart-test".to_string()]);
        assert!(
            reopened.list("default").is_empty(),
            "the anonymous default tenant must not see acme's agent after a restart"
        );
        assert!(reopened.manifest("acme", "restart-test").is_some());
        assert!(reopened.manifest("default", "restart-test").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_persistence_lifecycle() {
        // A fresh in-memory agent store (DUR-404 persists the real default to disk,
        // which would accumulate agents from every prior test run and break this
        // test's exact-list assertions below).
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_agents(Arc::new(AgentStore::new(None))),
        );
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
        let state = Arc::new(AppState::from_env().await);
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
        let state = Arc::new(AppState::from_env().await);
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
        let state = Arc::new(AppState::from_env().await);
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
        ensure_admin_env();
        let state = Arc::new(AppState::from_env().await);
        let resp = raw(
            &state,
            "GET",
            "/api/v1/agents/missing",
            &[("x-apex-principal", "root")],
            Value::Null,
        )
        .await;
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
        let state = Arc::new(AppState::from_env().await);
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
        // A fresh in-memory agent store — see agent_persistence_lifecycle's comment.
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_agents(Arc::new(AgentStore::new(None))),
        );
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

    // --- RM-GA-P1 SEC-201: timeout / body-size / concurrency ------------------------

    fn default_limits() -> HttpLimits {
        HttpLimits::from_env()
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_with_413() {
        let state = Arc::new(AppState::from_env().await.with_http_limits(HttpLimits {
            max_body_bytes: 10,
            ..default_limits()
        }));
        let body = json!({
            "manifest": "metadata:\n  name: too-big\nspec:\n  instructions: hi\n",
            "input": { "message": "hi" }
        });
        let resp = raw(&state, "POST", "/api/v1/agents:run", &[], body).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Direct proof of the layer stack `router()` wires for the default/run timeouts
    /// (SEC-201): a request that outruns its timeout gets the standard `408` envelope,
    /// not a raw/unhandled error — built standalone (rather than against a real, fast
    /// mock-provider route) so the test is deterministic regardless of provider speed.
    #[tokio::test]
    async fn timeout_layer_converts_elapsed_into_408() {
        async fn slow() -> &'static str {
            tokio::time::sleep(Duration::from_millis(50)).await;
            "too slow"
        }
        let app: Router = Router::new().route("/slow", get(slow)).layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_millis(5)),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "request_timeout");
    }

    /// SEC-201: "the run permit is released when the client-facing timeout fires."
    /// Proves the general mechanism `RunPermit`'s `Drop` impl relies on — tower's
    /// `Timeout` drops the inner future outright on expiry, so any RAII guard held
    /// across the timed-out `.await` is released — without depending on the real
    /// (near-instant, hard-to-artificially-slow) mock provider actually timing out.
    #[tokio::test]
    async fn timeout_drops_the_inner_future_releasing_raii_guards_held_across_it() {
        use std::sync::atomic::AtomicUsize;

        struct Permit(Arc<AtomicUsize>);
        impl Drop for Permit {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let held = Arc::new(AtomicUsize::new(0));
        let counter = held.clone();
        let app: Router = Router::new()
            .route(
                "/slow",
                get(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let _permit = Permit(counter.clone());
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        "done"
                    }
                }),
            )
            .layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                    .timeout(Duration::from_millis(10)),
            );

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);

        // The dropped future's Drop glue runs synchronously as part of the timeout
        // race resolving; a short grace sleep just avoids asserting mid-poll.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            held.load(Ordering::SeqCst),
            0,
            "the RAII guard held across the timed-out .await must be dropped"
        );
    }

    /// SEC-201: "load past the concurrency cap sheds cleanly" — a request arriving
    /// while the server is already at `APEX_HTTP_MAX_CONCURRENCY` is refused
    /// (`Overloaded`, which `handle_overload_or_timeout` maps to `503`) rather than
    /// queued, and the limit isn't a one-shot: once the in-flight call completes, the
    /// next one is admitted again. Exercises the exact `tower::ServiceBuilder` stack
    /// `router()` wires (`load_shed().concurrency_limit(n)`) directly over a bare
    /// `tower::service_fn`, rather than through a full `axum::Router` — axum's Router
    /// always reports itself ready regardless of inner backpressure (by design, so a
    /// single route's slowness can't stall routing to other routes), which would hide
    /// the very backpressure this test needs to observe.
    #[tokio::test]
    async fn concurrency_limit_sheds_load_past_the_cap_then_recovers() {
        use tower::Service;
        use tower::service_fn;

        let notify = Arc::new(tokio::sync::Notify::new());
        let held = notify.clone();
        let inner = service_fn(move |_req: ()| {
            let held = held.clone();
            async move {
                held.notified().await;
                Ok::<_, Infallible>("done")
            }
        });
        let svc = ServiceBuilder::new()
            .load_shed()
            .concurrency_limit(1)
            .service(inner);

        // The first call occupies the single concurrency slot, blocked on the notify
        // (standing in for a slow real request).
        let mut svc1 = svc.clone();
        let first = tokio::spawn(async move { svc1.ready().await.unwrap().call(()).await });
        // Let the first call actually get admitted and start waiting.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // A second, concurrent call is shed — an error, not queued.
        let mut svc2 = svc.clone();
        let result2 = svc2.ready().await.unwrap().call(()).await;
        assert!(result2.is_err(), "expected the second call to be shed");

        // Releasing the first call frees the slot for the next caller.
        notify.notify_one();
        let result1 = first.await.unwrap();
        assert!(result1.is_ok());
    }

    // --- RM-GA-P1 SEC-202: TLS termination or refuse insecure non-loopback bind -----

    #[test]
    fn insecure_bind_is_refused_only_without_tls_and_without_upstream_termination_on_non_loopback()
    {
        let non_loopback: SocketAddr = "0.0.0.0:8443".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:8443".parse().unwrap();

        // Loopback is always fine, TLS or not — local/dev ergonomics (SEC-102 parity).
        assert!(check_insecure_bind(false, loopback).is_ok());
        assert!(check_insecure_bind(true, loopback).is_ok());

        // Non-loopback with no TLS and no upstream-termination declaration → refused.
        assert!(check_insecure_bind(false, non_loopback).is_err());
        // This process terminating TLS itself → fine.
        assert!(check_insecure_bind(true, non_loopback).is_ok());
    }

    /// Real end-to-end proof the TLS path actually serves HTTPS (not just compiles):
    /// a self-signed cert (minted fresh via `rcgen`, never touching disk) is loaded
    /// through the exact same `RustlsConfig::from_pem_file` + `axum_server::bind_rustls`
    /// call `serve()` makes, and a real TLS client round-trips `/healthz` over it.
    #[tokio::test]
    async fn tls_config_serves_https_end_to_end() {
        // Mint a self-signed cert for `127.0.0.1` and write it to a temp dir — same
        // shape an operator's real PEM files would take (`from_pem_file` reads paths).
        install_default_crypto_provider();
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let dir = std::env::temp_dir().join(format!("apex-tls-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();

        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .unwrap();

        // Bind an OS-assigned port, then hand it to axum-server's TLS acceptor.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(Arc::new(AppState::from_env().await));
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, config)
                .unwrap()
                .serve(app.into_make_service())
                .await
        });

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // self-signed — the point under test
            .build()
            .unwrap();
        let resp = client
            .get(format!("https://{addr}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RM-AIM-P1 SRV-101: a shutdown signal mid-request drains the in-flight request to
    /// completion, then closes the listener so new connections are refused. Drives the
    /// same `serve_http` path `serve()` uses, with a test-controlled shutdown future in
    /// place of a process-global OS signal.
    #[tokio::test]
    async fn graceful_shutdown_drains_in_flight_then_refuses_new_connections() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let started = Arc::new(Notify::new());
        let allow = Arc::new(Notify::new());
        let (s2, a2) = (started.clone(), allow.clone());

        // A slow route: announces it is in-flight, then blocks until the test allows it.
        let app = Router::new().route(
            "/slow",
            axum::routing::get(move || {
                let (started, allow) = (s2.clone(), a2.clone());
                async move {
                    started.notify_one();
                    allow.notified().await;
                    "done"
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_http(listener, app, async move {
            let _ = shutdown_rx.await;
        }));

        // Fire an in-flight request against the slow route.
        let url = format!("http://{addr}/slow");
        let inflight = tokio::spawn({
            let url = url.clone();
            async move {
                reqwest::Client::new()
                    .get(&url)
                    .send()
                    .await
                    .map(|r| r.status())
            }
        });

        // Once the handler is actually running, ask the server to stop.
        started.notified().await;
        shutdown_tx.send(()).unwrap();

        // Let the in-flight request finish; graceful shutdown must drain it, not drop it.
        allow.notify_one();
        let status = inflight.await.unwrap().unwrap();
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "the in-flight request must drain to completion, not be killed"
        );

        // Draining complete → the serving future returns cleanly and the listener closes.
        let result = server.await.unwrap();
        assert!(
            result.is_ok(),
            "graceful shutdown returns Ok, got {result:?}"
        );

        // A new connection is now refused (the socket is closed).
        let after = reqwest::Client::new()
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        assert!(
            after.is_err(),
            "new connections must be refused after shutdown, got {after:?}"
        );
    }

    // --- RM-GA-P1 SEC-203: per-principal rate limiting -------------------------------

    /// A caller past their bucket's budget gets `429` with `Retry-After` — and it's
    /// genuinely per-principal: a different caller has an untouched bucket.
    #[tokio::test]
    async fn standard_tier_rate_limit_returns_429_with_retry_after_then_isolates_by_principal() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_rate_limiter_standard(rate_limit::RateLimiter::new(2, 2)),
        );

        for _ in 0..2 {
            let resp = raw(
                &state,
                "GET",
                "/api/v1/tools",
                &[("x-apex-principal", "alice")],
                Value::Null,
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
        let resp = raw(
            &state,
            "GET",
            "/api/v1/tools",
            &[("x-apex-principal", "alice")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().get("retry-after").is_some());

        // bob has his own bucket — untouched by alice being rate-limited.
        let resp = raw(
            &state,
            "GET",
            "/api/v1/tools",
            &[("x-apex-principal", "bob")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Rate limiting must run *after* auth: the bucket key is the verified principal
    /// (which auth overwrites `X-Apex-Principal` with), not a client-supplied header —
    /// so presenting a fresh, spoofed principal on every request cannot evade the
    /// limiter as long as the underlying credential is the same real caller.
    #[tokio::test]
    async fn rate_limit_keys_off_the_verified_principal_not_a_spoofed_header() {
        use axum::body::Body;

        let keys = InMemoryApiKeyStore::new();
        keys.insert("alice-key", "alice");
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_api_keys(Arc::new(keys))
                .with_auth_mode(AuthMode::ApiKey)
                .with_rate_limiter_standard(rate_limit::RateLimiter::new(1, 1)),
        );

        let call = |spoofed_principal: &'static str| {
            let state = state.clone();
            async move {
                router(state)
                    .oneshot(
                        Request::builder()
                            .method("GET")
                            .uri("/api/v1/tools")
                            .header("authorization", "Bearer alice-key")
                            .header("x-apex-principal", spoofed_principal)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        assert_eq!(call("attacker-1").await.status(), StatusCode::OK);
        // A different spoofed header, same real credential — still rate-limited,
        // since the bucket key came from the verified `alice`, not these headers.
        assert_eq!(
            call("attacker-2").await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    // --- RM-GA-P1 SEC-204: CORS allow-list -------------------------------------------

    /// A configured origin's preflight is answered with that exact origin allowed; an
    /// unlisted origin's preflight doesn't 4xx (tower_http just omits the allow
    /// header), which is what tells the browser not to expose the response to script.
    #[tokio::test]
    async fn configured_origin_passes_preflight_but_unlisted_origin_gets_no_allow_header() {
        use axum::body::Body;

        let state = Arc::new(
            AppState::from_env()
                .await
                .with_cors_allowed_origins(vec!["https://dashboard.example.com".to_string()]),
        );

        let preflight = |origin: &'static str| {
            let state = state.clone();
            async move {
                router(state)
                    .oneshot(
                        Request::builder()
                            .method("OPTIONS")
                            .uri("/api/v1/tools")
                            .header("origin", origin)
                            .header("access-control-request-method", "GET")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        let resp = preflight("https://dashboard.example.com").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("access-control-allow-origin").unwrap(),
            "https://dashboard.example.com"
        );

        let resp = preflight("https://evil.example.com").await;
        assert!(resp.headers().get("access-control-allow-origin").is_none());
    }

    /// Default posture (`APEX_CORS_ALLOWED_ORIGINS` unset/empty): no `CorsLayer` at
    /// all, so no CORS headers appear on any response — same-origin only.
    #[tokio::test]
    async fn no_configured_origins_means_no_cors_headers_at_all() {
        let state = Arc::new(AppState::from_env().await.with_cors_allowed_origins(vec![]));
        let resp = raw(
            &state,
            "GET",
            "/healthz",
            &[("origin", "https://dashboard.example.com")],
            Value::Null,
        )
        .await;
        assert!(resp.headers().get("access-control-allow-origin").is_none());
    }
}
