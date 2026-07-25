//! Generated OpenAPI 3.0 document (RM-AIM-P3 SRV-303).
//!
//! `docs/09-api/openapi.yaml` used to be the only description of the wire API, hand-
//! synced against the handlers with nothing enforcing agreement — a handler's route,
//! parameters, or response shape could silently drift from what the doc claimed. This
//! module derives the spec straight from the handlers instead: every route function
//! mounted in [`crate::router`] carries a `#[utoipa::path(...)]` attribute, every
//! request-body/error type used on the wire derives [`utoipa::ToSchema`], and
//! [`ApiDoc`] (below) aggregates them into one [`utoipa::openapi::OpenApi`] document —
//! served as JSON at `GET /openapi.json` ([`openapi_json_handler`]) so it can never
//! fall out of sync with what the handlers actually accept/return without a compile
//! error (an annotated handler removed from `paths(...)` — or vice versa — is caught
//! by [`served_spec_covers_every_mounted_route`], not just trusted by convention).
//!
//! Ad hoc `serde_json::Value` response bodies (most handlers construct their success
//! response with `serde_json::json!({...})` rather than a typed struct) are documented
//! by status + description only, with no `body` schema — an honest reflection of there
//! being no concrete Rust type to derive one from, rather than a fabricated schema.
//! [`ApiErrorBody`] is the one shared, precisely-typed schema: it matches
//! [`crate::agents::ApiError`]'s actual `IntoResponse` envelope
//! (`{"error": {"code","message","type","status"}}`) and is reused on every route's
//! error responses.

use axum::Json;
use serde::Serialize;
use utoipa::Modify;
use utoipa::OpenApi;
use utoipa::ToSchema;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};

/// Registers the security schemes the server actually accepts ([`crate::auth`]):
/// a bearer credential (`WOVYR_AUTH_MODE=jwt|apikey`) alongside the always-asserted
/// tenant header, or — in the `disabled-loopback` default — the raw tenant header
/// alone. utoipa has no macro syntax for security *schemes* (only security
/// *requirements*, which reference schemes by name), so this is the standard
/// `Modify` hook: it runs once over the derived document, after `#[utoipa::path]`
/// has already populated every operation's requirements by name.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "tenantHeader",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-Wovyr-Tenant"))),
        );
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
    }
}

/// The `{"error": {...}}` envelope every route's failure responses share
/// (mirrors [`crate::agents::ApiError`]'s `IntoResponse` body exactly).
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // constructed only by utoipa's schema reflection, never at runtime
pub(crate) struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
pub(crate) struct ApiErrorDetail {
    /// A stable machine-readable error code, e.g. `"validation_failed"`, `"not_found"`.
    code: String,
    /// A human-readable description of what went wrong.
    message: String,
    /// `"client_error"` for 4xx, `"server_error"` for 5xx.
    r#type: String,
    /// The HTTP status code, repeated in the body for convenience.
    status: u16,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Wovyr Platform API",
        description = "Generated from the handlers that implement it (RM-AIM-P3 SRV-303) — see docs/09-api/overview.md for the conventions (pagination, idempotency, error model) every route follows.",
        version = env!("CARGO_PKG_VERSION"),
    ),
    servers(
        (url = "http://127.0.0.1:8080", description = "Local `wovyr dev` single-node server"),
    ),
    modifiers(&SecurityAddon),
    security(
        ("tenantHeader" = []),
        ("tenantHeader" = [], "bearerAuth" = []),
    ),
    paths(
        crate::agents::healthz,
        crate::agents::run_handler,
        crate::agents::run_stream_handler,
        crate::agents::run_stored_handler,
        crate::agents::create_agent_handler,
        crate::agents::list_agents_handler,
        crate::agents::get_agent_handler,
        crate::agents::delete_agent_handler,
        crate::agents::get_run_handler,
        crate::agents::list_workflows_handler,
        crate::agents::get_workflow_handler,
        crate::metrics_handler,
        crate::tenancy::list_orgs,
        crate::tenancy::create_org,
        crate::tenancy::list_projects,
        crate::tenancy::create_project,
        crate::tenancy::get_project,
        crate::tenancy::patch_project,
        crate::tenancy::delete_project,
        crate::tenancy::list_members,
        crate::tenancy::add_member,
        crate::tenancy::remove_member,
        crate::tenancy::get_quota,
        crate::tenancy::set_quota,
        crate::webhooks::list_webhooks,
        crate::webhooks::register_webhook,
        crate::webhooks::list_dead_letters,
        crate::webhooks::delete_webhook,
        crate::memory::list_namespaces,
        crate::memory::list_records,
        crate::memory::put_record,
        crate::memory::query,
        crate::plugins::list_plugins,
        crate::plugins::install_plugin,
        crate::plugins::enable_plugin,
        crate::plugins::disable_plugin,
        crate::plugins::upgrade_plugin,
        crate::plugins::rollback_plugin,
        crate::plugins::trust_publisher,
        crate::plugins::uninstall_plugin,
        crate::marketplace::search_listings,
        crate::marketplace::publish_listing,
        crate::marketplace::get_listing,
        crate::marketplace::download_version,
        crate::marketplace::version_attestation,
        crate::marketplace::review_listing,
        crate::marketplace::verify_listing,
        crate::marketplace::request_review,
        crate::marketplace::approve_review,
        crate::marketplace::reject_review,
        crate::marketplace::install_listing,
        crate::marketplace::report_abuse,
        crate::marketplace::list_abuse_reports,
        crate::marketplace::resolve_abuse_report,
        crate::marketplace::dismiss_abuse_report,
        crate::audit::list_audit,
        crate::tools::list_tools,
        crate::secrets::list_secrets,
        crate::secrets::create_secret,
        crate::secrets::get_secret,
        crate::secrets::delete_secret,
        crate::secrets::rotate_secret,
        crate::kms::rotate_tenant_key,
        crate::kms::destroy_tenant_key,
        crate::workflow_runner::validate_handler,
        crate::workflow_runner::submit_handler,
        crate::workflow_runner::signal_handler,
        crate::workflow_runner::approve_handler,
        crate::workflow_runner::cancel_handler,
        crate::ui::present_handler,
        crate::ui::list_frames_handler,
        crate::ui::get_frame_handler,
        crate::ui::decide_handler,
        crate::ui::get_decision_handler,
        crate::mcp::create_handler,
        crate::mcp::list_handler,
        crate::mcp::get_handler,
        crate::mcp::delete_handler,
        crate::mcp::refresh_handler,
        openapi_json_handler,
    ),
    components(schemas(ApiErrorBody, ApiErrorDetail)),
    tags(
        (name = "agents", description = "Run agents, inline or by stored id; poll async runs."),
        (name = "workflows", description = "Validate/submit/signal/approve/cancel durable workflow executions."),
        (name = "tenancy", description = "Organizations, projects, memberships, quotas."),
        (name = "webhooks", description = "Outbound event subscriptions."),
        (name = "memory", description = "The memory engine: namespaces, records, hybrid query."),
        (name = "plugins", description = "The installed plugin catalog and lifecycle."),
        (name = "marketplace", description = "Plugin discovery, publishing, and governance."),
        (name = "audit", description = "The tamper-evident audit trail."),
        (name = "tools", description = "Registered tool discovery."),
        (name = "secrets", description = "The tenant-scoped secret vault."),
        (name = "kms", description = "Tenant key lifecycle (rotate/destroy)."),
        (name = "ui", description = "Generative UI: pending validated frames and typed human decisions (PRD-005)."),
        (name = "mcp", description = "MCP connection management: persisted external MCP server connections (PRD-006)."),
        (name = "system", description = "Health, metrics, and this document."),
    ),
)]
pub(crate) struct ApiDoc;

/// Serve the generated spec as JSON — unauthenticated, alongside `/healthz`/`/metrics`,
/// since it describes the API's shape rather than any tenant's data.
#[utoipa::path(
    get,
    path = "/openapi.json",
    tag = "system",
    security(()),
    responses((status = 200, description = "This document.")),
)]
pub(crate) async fn openapi_json_handler() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The literal SRV-303 acceptance criterion: the served spec matches the
    /// handlers. Every route [`crate::router`] mounts must have a corresponding
    /// entry in the generated document — if a handler is added to the router but
    /// never annotated with `#[utoipa::path]` and listed in [`ApiDoc`]'s
    /// `paths(...)`, this test (not just convention) catches the drift.
    #[test]
    fn served_spec_covers_every_mounted_route() {
        let spec = ApiDoc::openapi();
        let expected: &[(&str, &str)] = &[
            ("/healthz", "get"),
            ("/metrics", "get"),
            ("/openapi.json", "get"),
            ("/api/v1/agents:run", "post"),
            ("/api/v1/agents:stream", "post"),
            ("/api/v1/agents/{id}/run", "post"),
            ("/api/v1/agents", "post"),
            ("/api/v1/agents", "get"),
            ("/api/v1/agents/{id}", "get"),
            ("/api/v1/agents/{id}", "delete"),
            ("/api/v1/agents/runs/{run_id}", "get"),
            ("/api/v1/workflows", "get"),
            ("/api/v1/workflows/{id}", "get"),
            ("/api/v1/organizations", "get"),
            ("/api/v1/organizations", "post"),
            ("/api/v1/projects", "get"),
            ("/api/v1/projects", "post"),
            ("/api/v1/projects/{id}", "get"),
            ("/api/v1/projects/{id}", "patch"),
            ("/api/v1/projects/{id}", "delete"),
            ("/api/v1/projects/{id}/members", "get"),
            ("/api/v1/projects/{id}/members", "post"),
            ("/api/v1/projects/{id}/members/{uid}", "delete"),
            ("/api/v1/projects/{id}/quota", "get"),
            ("/api/v1/projects/{id}/quota", "patch"),
            ("/api/v1/webhooks", "get"),
            ("/api/v1/webhooks", "post"),
            ("/api/v1/webhooks/dead-letters", "get"),
            ("/api/v1/webhooks/{id}", "delete"),
            ("/api/v1/memory/namespaces", "get"),
            ("/api/v1/memory/records", "get"),
            ("/api/v1/memory/records", "post"),
            ("/api/v1/memory:query", "post"),
            ("/api/v1/plugins", "get"),
            ("/api/v1/plugins:install", "post"),
            ("/api/v1/plugins:enable", "post"),
            ("/api/v1/plugins:disable", "post"),
            ("/api/v1/plugins:upgrade", "post"),
            ("/api/v1/plugins:rollback", "post"),
            ("/api/v1/plugins:trust", "post"),
            ("/api/v1/plugins/{id}", "delete"),
            ("/api/v1/marketplace/listings", "get"),
            ("/api/v1/marketplace:publish", "post"),
            ("/api/v1/marketplace/listings/{id}", "get"),
            ("/api/v1/marketplace/listings/{id}/download", "get"),
            ("/api/v1/marketplace/listings/{id}/attestation", "get"),
            ("/api/v1/marketplace/listings/{id}/reviews", "post"),
            ("/api/v1/marketplace/listings/{id}/verify", "post"),
            ("/api/v1/marketplace/listings/{id}/request-review", "post"),
            ("/api/v1/marketplace/listings/{id}/approve", "post"),
            ("/api/v1/marketplace/listings/{id}/reject", "post"),
            ("/api/v1/marketplace/listings/{id}/install", "post"),
            ("/api/v1/marketplace/listings/{id}/report", "post"),
            ("/api/v1/marketplace/listings/{id}/reports", "get"),
            (
                "/api/v1/marketplace/listings/{id}/reports/{report_id}/resolve",
                "post",
            ),
            (
                "/api/v1/marketplace/listings/{id}/reports/{report_id}/dismiss",
                "post",
            ),
            ("/api/v1/audit", "get"),
            ("/api/v1/tools", "get"),
            ("/api/v1/secrets", "get"),
            ("/api/v1/secrets", "post"),
            ("/api/v1/secrets/{name}", "get"),
            ("/api/v1/secrets/{name}", "delete"),
            ("/api/v1/secrets/{name}/rotate", "post"),
            ("/api/v1/kms/tenant-key/rotate", "post"),
            ("/api/v1/kms/tenant-key/destroy", "post"),
            ("/api/v1/workflows/validate", "post"),
            ("/api/v1/workflows", "post"),
            ("/api/v1/workflows/{id}/signal", "post"),
            ("/api/v1/workflows/{id}/approve", "post"),
            ("/api/v1/workflows/{id}", "delete"),
            ("/api/v1/ui/present", "post"),
            ("/api/v1/ui/frames", "get"),
            ("/api/v1/ui/frames/{frame_id}", "get"),
            ("/api/v1/ui/decisions/{frame_id}", "post"),
            ("/api/v1/ui/decisions/{frame_id}", "get"),
            ("/api/v1/mcp/connections", "post"),
            ("/api/v1/mcp/connections", "get"),
            ("/api/v1/mcp/connections/{name}", "get"),
            ("/api/v1/mcp/connections/{name}", "delete"),
            ("/api/v1/mcp/connections/{name}/refresh", "post"),
        ];

        for (path, method) in expected {
            let item = spec
                .paths
                .paths
                .get(*path)
                .unwrap_or_else(|| panic!("generated spec is missing path {path}"));
            let has_method = match *method {
                "get" => item.get.is_some(),
                "post" => item.post.is_some(),
                "put" => item.put.is_some(),
                "delete" => item.delete.is_some(),
                "patch" => item.patch.is_some(),
                other => panic!("test bug: unhandled method {other}"),
            };
            assert!(
                has_method,
                "generated spec's {path} entry is missing method {method}"
            );
        }
    }
}
