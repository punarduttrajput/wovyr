//! The cross-cutting integration suite for this crate (RM-AIM-P3 SRV-304), moved out
//! of `lib.rs` — which used to be ~86% this module (~2,260 of ~2,620 lines) — into its
//! own file so the production module (`router()`/`serve()`/TLS bootstrap) is
//! navigable. A `mod tests;` file-backed submodule, not a `tests/` external-crate
//! integration test: several tests reach into `AppState`'s `pub(crate)` fields
//! directly (e.g. seeding a workflow engine or tenancy store before driving a
//! request through `router()`), and moving it to `tests/` would have forced those
//! fields to `pub` — a real API-surface change out of scope for a pure code-motion
//! refactor. A same-crate submodule keeps identical privacy without widening
//! anything; `use super::*` below resolves exactly as it did nested inside
//! `lib.rs`'s old `mod tests { ... }`.

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
    // RM-AIM-P2 OBS-201: per-tenant visibility for both the RED aggregate and the
    // LLM cost/token metrics — none of these requests set `X-Apex-Tenant`, so
    // they're all attributed to the "default" tenant.
    assert!(
        text.contains(r#"apex_api_requests_by_tenant_total{status_class="2xx",tenant="default"}"#),
        "metrics:\n{text}"
    );
    assert!(
        text.contains(r#"apex_llm_cost_usd_by_tenant_total{project="none",tenant="default"}"#),
        "metrics:\n{text}"
    );
    assert!(
        text.contains(r#"apex_llm_tokens_by_tenant_total{project="none",tenant="default"}"#),
        "metrics:\n{text}"
    );
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

/// OBS-301: the operability depth gauges are recomputed from their stores at every
/// scrape, so they *move with load* — up when work is queued/in flight, back down
/// when it drains — rather than drifting like inc/dec bookkeeping would.
#[tokio::test]
async fn operability_gauges_move_with_load() {
    let state = Arc::new(AppState::from_env().await);

    async fn scrape(state: &Arc<AppState>) -> String {
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn gauge(text: &str, name: &str) -> f64 {
        text.lines()
            .find(|l| l.starts_with(name) && !l.starts_with('#'))
            .and_then(|l| l.split_whitespace().last())
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("gauge {name} missing:\n{text}"))
    }

    // Baseline: every gauge is present and typed. Values are read as a baseline
    // rather than asserted to be zero — the durable stores are shared with local
    // dev state by design, so this test asserts *movement*, not absolutes.
    let text = scrape(&state).await;
    for name in [
        "apex_async_runs_in_flight",
        "apex_webhook_outbox_pending",
        "apex_webhook_dlq_size",
        "apex_quota_runs_in_flight",
        "apex_workflow_timers_pending",
        "apex_workflow_executions_active",
    ] {
        assert!(text.contains(&format!("# TYPE {name} gauge")), "{text}");
    }
    let base_runs = gauge(&text, "apex_async_runs_in_flight");
    let base_outbox = gauge(&text, "apex_webhook_outbox_pending");
    let base_dlq = gauge(&text, "apex_webhook_dlq_size");
    let base_timers = gauge(&text, "apex_workflow_timers_pending");

    // Load up: a running async run, a pending webhook delivery, and a durable timer.
    state
        .runs
        .insert_running("run_gauge_test".to_string(), "acme".to_string());
    state
        .webhook_outbox
        .enqueue(crate::webhook_outbox::OutboxEntry {
            delivery_id: "evt_g::wh_g".to_string(),
            tenant: "acme".to_string(),
            sub_id: "wh_g".to_string(),
            event: apex_events::Event::new("evt_g", "project.created", "acme", 1, json!({})),
            enqueued_at_ms: 0,
        });
    state
        .timers
        .schedule(apex_workflow::PendingTimer {
            execution_id: "wf-gauge".to_string(),
            timer_id: "t1".to_string(),
            fire_at_ms: u64::MAX - 1,
        })
        .await
        .unwrap();

    let text = scrape(&state).await;
    assert_eq!(
        gauge(&text, "apex_async_runs_in_flight"),
        base_runs + 1.0,
        "{text}"
    );
    assert_eq!(
        gauge(&text, "apex_webhook_outbox_pending"),
        base_outbox + 1.0,
        "{text}"
    );
    assert_eq!(
        gauge(&text, "apex_workflow_timers_pending"),
        base_timers + 1.0,
        "{text}"
    );

    // Drain: the run finishes, the delivery exhausts into the DLQ, the timer fires.
    state.runs.finish(
        "run_gauge_test",
        crate::state::AsyncRunStatus::Failed {
            error: "test drain".to_string(),
        },
    );
    state.webhook_outbox.dead_letter(
        "evt_g::wh_g",
        "http://example.invalid",
        "project.created",
        3,
        1,
    );
    state.timers.cancel("wf-gauge", "t1").await.unwrap();

    let text = scrape(&state).await;
    assert_eq!(
        gauge(&text, "apex_async_runs_in_flight"),
        base_runs,
        "{text}"
    );
    assert_eq!(
        gauge(&text, "apex_webhook_outbox_pending"),
        base_outbox,
        "{text}"
    );
    assert_eq!(
        gauge(&text, "apex_workflow_timers_pending"),
        base_timers,
        "{text}"
    );
    // The exhausted delivery moved to the DLQ — that gauge went *up*.
    assert_eq!(
        gauge(&text, "apex_webhook_dlq_size"),
        base_dlq + 1.0,
        "{text}"
    );
}

#[tokio::test]
async fn invalid_manifest_is_400() {
    let app = test_app().await;
    let body =
        json!({ "manifest": "kind: Workflow\nmetadata:\n  name: x\nspec:\n  instructions: hi\n" })
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

async fn req(state: &Arc<AppState>, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
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
    let (st, list) = tenant_req(&state, "GET", "/api/v1/agents", "beta", "bob", Value::Null).await;
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
    let (st, list) = tenant_req(&state, "GET", "/api/v1/secrets", "beta", "bob", Value::Null).await;
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
    let (st, beta) = tenant_req(&state, "GET", "/api/v1/audit", "beta", "bob", Value::Null).await;
    assert_eq!(st, StatusCode::OK);
    assert!(beta["data"].as_array().unwrap().is_empty());
}

/// SEC-301: `GET /api/v1/audit` supports an inclusive `[after_ms, before_ms]`
/// time range and a seq-based cursor, and its `total_estimate` is always `null`
/// (the one documented exception — an exact count would need the full-log scan
/// this route's bounded paging exists to avoid).
#[tokio::test]
async fn audit_route_time_range_and_cursor_page_through_the_window() {
    use apex_audit::{AuditEvent, AuditLog};
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
    for ts in 1..=10u64 {
        state
            .audit
            .record(AuditEvent::new(
                ts,
                "alice",
                "acme",
                "config.change",
                "project",
                "proj-1",
            ))
            .unwrap();
    }

    // First page of the [3, 8] window: most-recent first, capped at 3.
    let (st, page1) = tenant_req(
        &state,
        "GET",
        "/api/v1/audit?after_ms=3&before_ms=8&limit=3",
        "acme",
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let ts_of = |page: &Value| -> Vec<u64> {
        page["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event"]["timestamp_ms"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(ts_of(&page1), vec![8, 7, 6]);
    assert_eq!(page1["has_more"], true);
    assert!(page1["total_estimate"].is_null());
    let cursor = page1["next_cursor"].as_str().unwrap().to_string();

    // The cursor continues the same window to exhaustion.
    let (st, page2) = tenant_req(
        &state,
        "GET",
        &format!("/api/v1/audit?after_ms=3&before_ms=8&limit=3&cursor={cursor}"),
        "acme",
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ts_of(&page2), vec![5, 4, 3]);
    assert_eq!(page2["has_more"], false);
    assert!(page2["next_cursor"].is_null());
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
    let (st, beta) = tenant_req(&state, "GET", "/api/v1/audit", "beta", "bob", Value::Null).await;
    assert_eq!(st, StatusCode::OK);
    assert!(beta["data"].as_array().unwrap().is_empty());
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
fn insecure_bind_is_refused_only_without_tls_and_without_upstream_termination_on_non_loopback() {
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

/// RM-AIM-P2 SRV-202: the opt-in per-tenant tier bounds all of a tenant's
/// principals with one shared bucket — two different callers under one tenant
/// draw from the same budget, while another tenant's bucket is untouched.
#[tokio::test]
async fn tenant_rate_tier_is_shared_across_principals_and_isolated_by_tenant() {
    let state = Arc::new(
        AppState::from_env()
            .await
            // Generous per-principal buckets: only the tenant ceiling binds.
            .with_rate_limiter_standard(rate_limit::RateLimiter::new(100, 100))
            .with_rate_limiter_tenant(rate_limit::RateLimiter::new(2, 2)),
    );

    // alice and bob (same tenant) together exhaust acme's shared budget…
    for principal in ["alice", "bob"] {
        let resp = raw(
            &state,
            "GET",
            "/api/v1/tools",
            &[("x-apex-principal", principal), ("x-apex-tenant", "acme")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "{principal} admitted");
    }
    let resp = raw(
        &state,
        "GET",
        "/api/v1/tools",
        &[("x-apex-principal", "carol"), ("x-apex-tenant", "acme")],
        Value::Null,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a third principal under the same tenant hits the shared ceiling"
    );
    assert!(resp.headers().get("retry-after").is_some());

    // …while another tenant's bucket is untouched.
    let resp = raw(
        &state,
        "GET",
        "/api/v1/tools",
        &[("x-apex-principal", "dave"), ("x-apex-tenant", "globex")],
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
