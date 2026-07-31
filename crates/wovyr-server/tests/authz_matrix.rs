//! RM-GA-P1 SEC-105 — negative-authorization CI gate.
//!
//! A table-driven proof that every mutating/secret/KMS/plugin/marketplace/audit route
//! fails closed: (a) with **no credential** at all, every protected route is `401`
//! (SEC-101's auth middleware runs before any handler); (b) with a **valid but
//! under-scoped** credential (a real, authenticated principal holding zero tenancy
//! memberships), every route that requires a specific RBAC scope is `403`.
//!
//! Compiled as an integration test — a normally-built copy of `wovyr-server` with no
//! `cfg(test)` — so [`auth::resolve_anonymous_allowed`]'s test-only default does not
//! apply here: this suite exercises the real, secure-by-default behavior a production
//! deployment gets, not the existing unit suite's dev-ergonomic default.
//!
//! Constructed entirely from `wovyr_server`'s public `AppState` builders
//! (`with_tenancy`/`with_api_keys`/`with_auth_mode`/`with_anonymous_allowed`), so it
//! never touches a real `~/.wovyr/*` directory or races the crate's own unit tests'
//! process-global env vars.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use wovyr_server::{AppState, AuthMode, InMemoryApiKeyStore};
use wovyr_tenancy::{InMemoryTenancyStore, TenancyStore};

/// `AppState::from_env`, but against a scratch state root instead of the
/// developer's real `~/.wovyr`.
///
/// This crate's own unit tests get the redirect via the crate-private
/// `AppState::for_test`, which an integration test can't reach — so it calls the
/// same `wovyr-config` hook directly. Without it, running this file wrote the
/// audit chain, tenancy catalog, KMS root key and workflow store into live local
/// state (the tests below override `tenancy`/`api_keys` per case, but everything
/// `from_env` builds underneath them still resolved through `HOME`).
async fn state_from_env() -> AppState {
    wovyr_config::root::redirect_to_scratch("server-authz");
    AppState::from_env().await
}

/// One route under test: `scope` is the exact `tenant_authorize`/`ctx.authorize` scope
/// the handler requires, or `None` for a route that is open to any authenticated
/// principal (no RBAC check beyond SEC-101's identity verification) — e.g. the
/// unmetered inline agent run, or marketplace discovery/reporting endpoints.
struct RouteCase {
    method: &'static str,
    uri: &'static str,
    body: Value,
    scope: Option<&'static str>,
}

/// The full mutating/sensitive-read route table. **Keep this in sync with
/// `crates/wovyr-server/src/lib.rs`'s `router()` and the sub-route modules it merges**
/// — a new route with no entry here isn't covered by this gate (see
/// `table_covers_every_known_route` below for the hand-maintained coverage tripwire).
fn routes() -> Vec<RouteCase> {
    let wf_manifest =
        "metadata:\n  name: wf\nspec:\n  activities:\n    - {id: a, type: function}\n";
    let agent_manifest = "metadata:\n  name: t\nspec:\n  instructions: hi\n";
    vec![
        // --- agents ---------------------------------------------------------------
        RouteCase {
            method: "POST",
            uri: "/api/v1/agents:run",
            body: json!({"manifest": agent_manifest}),
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/agents:stream",
            body: json!({"manifest": agent_manifest}),
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/agents",
            body: json!({"manifest": agent_manifest}),
            scope: Some("agents:write"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/agents",
            body: Value::Null,
            scope: Some("agents:read"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/agents/x",
            body: Value::Null,
            scope: Some("agents:read"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/agents/x",
            body: Value::Null,
            scope: Some("agents:write"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/agents/x/run",
            body: json!({}),
            scope: Some("agents:run"),
        },
        // --- workflows --------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/workflows",
            body: Value::Null,
            scope: Some("workflows:read"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/workflows/x",
            body: Value::Null,
            scope: Some("workflows:read"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/workflows/validate",
            body: json!({"manifest": wf_manifest}),
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/workflows",
            body: json!({"manifest": wf_manifest}),
            scope: Some("workflows:run"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/workflows/x/signal",
            body: json!({"manifest": wf_manifest, "event": "e"}),
            scope: Some("workflows:run"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/workflows/x/approve",
            body: json!({"manifest": wf_manifest, "activity_id": "a"}),
            scope: Some("workflows:run"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/workflows/x",
            body: Value::Null,
            scope: Some("workflows:write"),
        },
        // --- tenancy: organizations/projects/members/quota --------------------------
        RouteCase {
            method: "POST",
            uri: "/api/v1/organizations",
            body: json!({"name": "n"}),
            scope: Some("org.admin"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/organizations",
            body: Value::Null,
            scope: Some("projects:read"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/projects",
            body: json!({"name": "p", "organization": "o1"}),
            scope: Some("projects:admin"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/projects",
            body: Value::Null,
            scope: Some("projects:read"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/projects/x",
            body: Value::Null,
            scope: Some("projects:read"),
        },
        RouteCase {
            method: "PATCH",
            uri: "/api/v1/projects/x",
            body: json!({}),
            scope: Some("projects:admin"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/projects/x",
            body: Value::Null,
            scope: Some("projects:admin"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/projects/x/members",
            body: Value::Null,
            scope: Some("projects:read"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/projects/x/members",
            body: json!({"user": "u", "role": "viewer"}),
            scope: Some("projects:admin"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/projects/x/members/u1",
            body: Value::Null,
            scope: Some("projects:admin"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/projects/x/quota",
            body: Value::Null,
            scope: Some("projects:read"),
        },
        RouteCase {
            method: "PATCH",
            uri: "/api/v1/projects/x/quota",
            body: json!({}),
            scope: Some("org.admin"),
        },
        // --- webhooks -----------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/webhooks",
            body: Value::Null,
            scope: Some("projects:read"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/webhooks",
            body: json!({"url": "https://example.com", "events": ["*"], "secret": "s"}),
            scope: Some("org.admin"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/webhooks/x",
            body: Value::Null,
            scope: Some("org.admin"),
        },
        // --- memory ---------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/memory/namespaces",
            body: Value::Null,
            scope: Some("memory:read"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/memory/records",
            body: Value::Null,
            scope: Some("memory:read"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/memory/records",
            body: json!({"namespace": "n", "content": "c"}),
            scope: Some("memory:write"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/memory:query",
            body: json!({"text": "q"}),
            scope: Some("memory:read"),
        },
        // --- plugins ----------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/plugins",
            body: Value::Null,
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/plugins:install",
            body: json!({"wovyrpkg": "x"}),
            scope: Some("plugins:admin"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/plugins:enable",
            body: json!({"id": "a/b"}),
            scope: Some("plugins:admin"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/plugins:disable",
            body: json!({"id": "a/b"}),
            scope: Some("plugins:admin"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/plugins:upgrade",
            body: json!({"wovyrpkg": "x"}),
            scope: Some("plugins:admin"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/plugins:rollback",
            body: json!({"id": "a/b"}),
            scope: Some("plugins:admin"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/plugins:trust",
            body: json!({"publisher": "p", "public_key_hex": "00"}),
            scope: Some("plugins:admin"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/plugins/a%2Fb",
            body: Value::Null,
            scope: Some("plugins:admin"),
        },
        // --- marketplace --------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/marketplace/listings",
            body: Value::Null,
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace:publish",
            body: json!({"wovyrpkg": "x"}),
            scope: None,
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/marketplace/listings/x",
            body: Value::Null,
            scope: None,
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/marketplace/listings/x/download",
            body: Value::Null,
            scope: None,
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/marketplace/listings/x/attestation",
            body: Value::Null,
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/reviews",
            body: json!({"author": "a", "rating": 5}),
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/verify",
            body: json!({}),
            scope: Some("marketplace:moderate"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/request-review",
            body: json!({}),
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/approve",
            body: json!({}),
            scope: Some("marketplace:moderate"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/reject",
            body: json!({"reason": "r"}),
            scope: Some("marketplace:moderate"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/install",
            body: json!({}),
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/report",
            body: json!({"reason": "r"}),
            scope: None,
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/marketplace/listings/x/reports",
            body: Value::Null,
            scope: None,
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/reports/1/resolve",
            body: json!({}),
            scope: Some("marketplace:moderate"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/marketplace/listings/x/reports/1/dismiss",
            body: json!({"reason": "r"}),
            scope: Some("marketplace:moderate"),
        },
        // --- secrets ------------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/secrets",
            body: Value::Null,
            scope: Some("secrets:read"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/secrets",
            body: json!({"name": "n", "value": "v"}),
            scope: Some("secrets:write"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/secrets/n",
            body: Value::Null,
            scope: Some("secrets:read"),
        },
        RouteCase {
            method: "DELETE",
            uri: "/api/v1/secrets/n",
            body: Value::Null,
            scope: Some("secrets:write"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/secrets/n/rotate",
            body: json!({"value": "v2"}),
            scope: Some("secrets:write"),
        },
        // --- kms ------------------------------------------------------------------------
        RouteCase {
            method: "POST",
            uri: "/api/v1/kms/tenant-key/rotate",
            body: Value::Null,
            scope: Some("kms:write"),
        },
        RouteCase {
            method: "POST",
            uri: "/api/v1/kms/tenant-key/destroy",
            body: Value::Null,
            scope: Some("kms:admin"),
        },
        // --- audit + tools --------------------------------------------------------------
        RouteCase {
            method: "GET",
            uri: "/api/v1/audit",
            body: Value::Null,
            scope: Some("audit:read"),
        },
        RouteCase {
            method: "GET",
            uri: "/api/v1/tools",
            body: Value::Null,
            scope: None,
        },
    ]
}

async fn send(state: &Arc<AppState>, case: &RouteCase, auth_header: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .method(case.method)
        .uri(case.uri)
        .header("content-type", "application/json");
    if let Some(h) = auth_header {
        builder = builder.header("authorization", h);
    }
    let body = if case.body.is_null() {
        Body::empty()
    } else {
        Body::from(case.body.to_string())
    };
    let resp = wovyr_server::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    resp.status()
}

/// SEC-101 acceptance criterion: **every** protected route (mutating or not) rejects
/// a request with no credential — `401`, before any handler runs.
#[tokio::test]
async fn every_route_rejects_a_missing_credential() {
    let state = Arc::new(
        state_from_env()
            .await
            .with_tenancy(Arc::new(InMemoryTenancyStore::new()))
            .with_api_keys(Arc::new(InMemoryApiKeyStore::new()))
            .with_auth_mode(AuthMode::ApiKey)
            .with_anonymous_allowed(false),
    );

    for case in routes() {
        let status = send(&state, &case, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} should 401 with no credential, got {status}",
            case.method,
            case.uri
        );
        // An invalid/unknown credential must be refused identically.
        let status = send(&state, &case, Some("Bearer not-a-real-key")).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} should 401 with an unknown key, got {status}",
            case.method,
            case.uri
        );
    }
}

/// SEC-105 acceptance criterion: a **valid but under-scoped** credential (a real,
/// authenticated principal holding zero tenancy memberships) is `403` on every
/// scope-gated route, and is *not* blocked by the auth layer itself (401) on any
/// route — proving RBAC, not authentication, is what's denying it.
#[tokio::test]
async fn under_scoped_credential_is_denied_by_rbac_not_authentication() {
    let keys = InMemoryApiKeyStore::new();
    keys.insert("poweruser-key", "poweruser");
    let state = Arc::new(
        state_from_env()
            .await
            .with_tenancy(Arc::new(InMemoryTenancyStore::new()))
            .with_api_keys(Arc::new(keys))
            .with_auth_mode(AuthMode::ApiKey),
    );

    for case in routes() {
        let status = send(&state, &case, Some("Bearer poweruser-key")).await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} must authenticate the valid key (not 401), got {status}",
            case.method,
            case.uri
        );
        if let Some(scope) = case.scope {
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{} {} requires `{scope}` — a member-less principal should get 403, got {status}",
                case.method,
                case.uri
            );
        } else {
            assert_ne!(
                status,
                StatusCode::FORBIDDEN,
                "{} {} is documented as open to any authenticated principal, got {status}",
                case.method,
                case.uri
            );
        }
    }
}

/// The number of `(method, uri)` endpoints registered across the whole server as of
/// this writing — every one of them *except* `/healthz`, `/metrics` (deliberately
/// public) and `/workflows` (the read-only HTML UI shell, not a JSON API route) has
/// an entry in `routes()` above. There's no reliable AST-free way to derive this count
/// automatically (a text scan for `.route(`/`get(`/`post(` false-positives on
/// unrelated calls like `HashMap::get`), so it's a hand-maintained tripwire instead:
/// **adding, removing, or renaming any route must update both `routes()` and this
/// constant in the same change**, which is exactly the discipline SEC-105 exists to
/// enforce.
const TOTAL_ENDPOINTS_OUTSIDE_TABLE: usize = 3;

#[test]
fn table_covers_every_known_route() {
    assert_eq!(
        routes().len() + TOTAL_ENDPOINTS_OUTSIDE_TABLE,
        68,
        "the route table size changed — update `routes()` above (and this constant) \
         together, per RM-GA-P1 SEC-105's 100%-mutating-route-coverage requirement"
    );
}

/// Send a request carrying both an API-key credential and a (client-asserted,
/// unverified) `X-Wovyr-Tenant` header.
async fn send_as(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    tenant: &str,
    key: &str,
    body: Value,
) -> StatusCode {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .header("x-wovyr-tenant", tenant)
        .body(body)
        .unwrap();
    wovyr_server::router(state.clone())
        .oneshot(req)
        .await
        .unwrap()
        .status()
}

/// RM-AR-P1 SEC-402: an org admin in one tenant cannot reach org-level operations
/// in another by spoofing `X-Wovyr-Tenant`. The pre-fix `context()` granted an
/// org-scoped role on *any* project-less request, so an `OrgAdmin` in tenant A
/// passed `authorize("org.admin")` for tenant B's `create_org`/`list_orgs`. The
/// fix requires the org-scoped role's org to belong to the request's tenant.
#[tokio::test]
async fn org_admin_cannot_cross_tenants_on_org_level_routes() {
    use wovyr_tenancy::{MemberScope, Membership, Organization, Role};

    let tenancy = InMemoryTenancyStore::new();
    // Alice is an OrgAdmin of an org that lives in tenant A — and nowhere else.
    let org_a = tenancy
        .create_org(Organization::new("tenant-a", "Acme"))
        .unwrap();
    tenancy
        .add_membership(Membership {
            user: "alice".to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org_a.id.clone()),
        })
        .unwrap();

    let keys = InMemoryApiKeyStore::new();
    keys.insert("alice-key", "alice");
    let state = Arc::new(
        state_from_env()
            .await
            .with_tenancy(Arc::new(tenancy))
            .with_api_keys(Arc::new(keys))
            .with_auth_mode(AuthMode::ApiKey),
    );

    // Spoofing tenant B: Alice holds no membership there, so org-level authz fails.
    for (method, uri, body) in [
        ("GET", "/api/v1/organizations", Value::Null),
        ("POST", "/api/v1/organizations", json!({"name": "evil-org"})),
        ("GET", "/api/v1/projects", Value::Null),
    ] {
        let status = send_as(&state, method, uri, "tenant-b", "alice-key", body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} with spoofed X-Wovyr-Tenant: tenant-b must be 403, got {status}"
        );
    }

    // No regression: in her *own* tenant Alice's org.admin still works.
    let status = send_as(
        &state,
        "POST",
        "/api/v1/organizations",
        "tenant-a",
        "alice-key",
        json!({"name": "Another"}),
    )
    .await;
    assert!(
        status.is_success(),
        "legitimate same-tenant org.admin must still succeed, got {status}"
    );
    let status = send_as(
        &state,
        "GET",
        "/api/v1/organizations",
        "tenant-a",
        "alice-key",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "same-tenant list must succeed");
}
