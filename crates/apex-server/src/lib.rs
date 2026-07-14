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
mod openapi;
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
    let metrics_state = hardening::MetricsState {
        metrics: state.metrics.clone(),
        tenant_labels: state.tenant_label_cap.clone(),
    };
    let cors = cors_layer(&state.cors_allowed_origins);
    let auth = || axum::middleware::from_fn_with_state(state.clone(), auth::authenticate);
    // Both tiers also apply the optional per-tenant ceiling (SRV-202) when the
    // deployment configured one (`APEX_RATE_LIMIT_TENANT_PER_MIN`).
    let rate_limit_sensitive = || {
        let limiter = state.rate_limiter_sensitive.clone();
        let tenant_limiter = state.rate_limiter_tenant.clone();
        axum::middleware::from_fn(move |headers: HeaderMap, req: Request, next: Next| {
            rate_limit::enforce(limiter.clone(), tenant_limiter.clone(), headers, req, next)
        })
    };
    let rate_limit_standard = || {
        let limiter = state.rate_limiter_standard.clone();
        let tenant_limiter = state.rate_limiter_tenant.clone();
        axum::middleware::from_fn(move |headers: HeaderMap, req: Request, next: Next| {
            rate_limit::enforce(limiter.clone(), tenant_limiter.clone(), headers, req, next)
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
        // Generated OpenAPI spec (RM-AIM-P3 SRV-303) — describes the API's shape,
        // not any tenant's data, so it sits alongside health/metrics unauthenticated.
        .route("/openapi.json", get(openapi::openapi_json_handler))
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
            metrics_state,
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
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "system",
    security(()),
    responses((status = 200, description = "Prometheus (or OpenMetrics, via Accept negotiation) text.")),
)]
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
mod tests;
