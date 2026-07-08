//! Marketplace routes: the hosted plugin registry — publish, discover, download,
//! attest, rate, verify, and a publish→install bridge
//! ([Marketplace](../../docs/08-plugin-sdk/marketplace.md)). The **attestation** route
//! surfaces a version's supply-chain posture (permission risk, SBOM, build provenance,
//! content digest, signature verification) so an operator sees it before granting.
//!
//! Listings persist to a durable [`FileRegistryStore`] at `~/.apex/marketplace/registry.json`
//! by default, or — when built with the `postgres` cargo feature and
//! `APEX_MARKETPLACE_POSTGRES_URL` is set — a `PostgresRegistryStore` shared across every
//! node running this server, so a fleet publishes/discovers/downloads against one durable
//! catalog instead of each node holding its own file. Publishing re-verifies the package
//! signature against the **same** trust store the plugin lifecycle uses
//! (`~/.apex/plugins/trust.json`), so only trusted publishers can list a plugin. Operator
//! curation ([§7]) is loaded from `~/.apex/marketplace/policy.json` when present (else the
//! permissive default). The install bridge downloads a listed package and installs it into
//! the local plugin catalog via the shared [`plugins::install_package`](crate::plugins)
//! helper.

use apex_marketplace::{
    FileRegistryStore, PermissionRisk, Registry, RegistryPolicy, RegistryStore, Review, SearchQuery,
};
use apex_plugin::{CapabilityKind, Package, TrustStore};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

use crate::{ApiError, AppState, plugins};

fn marketplace_dir() -> Result<PathBuf, ApiError> {
    apex_config::paths::marketplace_dir().map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "no home directory for the marketplace registry",
        )
    })
}

/// Operator curation policy from `~/.apex/marketplace/policy.json`, or the default.
fn load_policy() -> Result<RegistryPolicy, ApiError> {
    let path = marketplace_dir()?.join("policy.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("corrupt marketplace policy: {e}"),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RegistryPolicy::default()),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )),
    }
}

/// Open the durable registry store: `PostgresRegistryStore` when this binary was built
/// with the `postgres` feature *and* `APEX_MARKETPLACE_POSTGRES_URL` is set (a shared
/// catalog across every node running this server), else the single-node
/// `FileRegistryStore` at `~/.apex/marketplace/registry.json`.
fn open_store() -> Result<Box<dyn RegistryStore>, ApiError> {
    #[cfg(feature = "postgres")]
    if let Some(url) = apex_config::env::marketplace_postgres_url() {
        let store = apex_marketplace::PostgresRegistryStore::connect(&url)?;
        return Ok(Box::new(store));
    }
    Ok(Box::new(FileRegistryStore::new(
        marketplace_dir()?.join("registry.json"),
    )))
}

/// Build a registry over the durable store, sharing the plugin trust store (and the
/// keyless trust config, when present) and applying the operator policy.
fn registry() -> Result<Registry<Box<dyn RegistryStore>>, ApiError> {
    let trust = plugins::load_trust()?;
    let store = open_store()?;
    let mut reg = Registry::new(store, trust).with_policy(load_policy()?);
    if let Some(keyless) = plugins::load_keyless()? {
        reg = reg.with_keyless(keyless.root, keyless.policy);
    }
    Ok(reg)
}

/// Run a synchronous registry operation on a dedicated blocking-pool thread.
///
/// The sync `postgres` crate's `Client` (used by `PostgresRegistryStore` when
/// `APEX_MARKETPLACE_POSTGRES_URL` is set) drives its own internal Tokio runtime
/// via `block_on` for *every* call, including `connect` — calling it directly
/// from an Axum handler panics ("Cannot start a runtime from within a runtime")
/// because the handler is already running on one of this server's own runtime
/// worker threads. `spawn_blocking` moves the whole registry construction +
/// operation onto a plain OS thread outside that runtime, where the nested
/// `block_on` is fine. (The file-store path doesn't need this, but paying a
/// thread hop for it too keeps every handler on one code path.)
async fn with_registry<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce(&mut Registry<Box<dyn RegistryStore>>) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut reg = registry()?;
        f(&mut reg)
    })
    .await
    .unwrap_or_else(|e| {
        Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            format!("registry task panicked: {e}"),
        ))
    })
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/marketplace/listings", get(search_listings))
        .route("/api/v1/marketplace:publish", post(publish_listing))
        .route("/api/v1/marketplace/listings/{id}", get(get_listing))
        .route(
            "/api/v1/marketplace/listings/{id}/download",
            get(download_version),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/attestation",
            get(version_attestation),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/reviews",
            post(review_listing),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/verify",
            post(verify_listing),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/request-review",
            post(request_review),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/approve",
            post(approve_review),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/reject",
            post(reject_review),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/install",
            post(install_listing),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/report",
            post(report_abuse),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/reports",
            get(list_abuse_reports),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/reports/{report_id}/resolve",
            post(resolve_abuse_report),
        )
        .route(
            "/api/v1/marketplace/listings/{id}/reports/{report_id}/dismiss",
            post(dismiss_abuse_report),
        )
}

fn parse_capability(s: &str) -> Option<CapabilityKind> {
    match s {
        "tool" => Some(CapabilityKind::Tool),
        "provider" => Some(CapabilityKind::Provider),
        "memory_backend" => Some(CapabilityKind::MemoryBackend),
        "policy" => Some(CapabilityKind::Policy),
        "workflow_activity" => Some(CapabilityKind::WorkflowActivity),
        _ => None,
    }
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
    category: Option<String>,
    capability: Option<String>,
    /// `limit` + `cursor` (overview §6, RM-GA-P4 API-701).
    #[serde(flatten)]
    page: crate::hardening::PageQuery,
}

/// `GET /api/v1/marketplace/listings?q=&category=&capability=` — search/browse,
/// cursor-paginated (overview §6).
async fn search_listings(Query(params): Query<SearchParams>) -> Result<Json<Value>, ApiError> {
    let page = params.page.page();
    let query = SearchQuery {
        text: params.q,
        category: params.category,
        capability: params.capability.as_deref().and_then(parse_capability),
    };
    let listings = with_registry(move |reg| Ok(reg.search(&query)?)).await?;
    let items: Vec<Value> = listings
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
        .collect();
    Ok(Json(crate::hardening::paginate(items, &page)))
}

/// `GET /api/v1/marketplace/listings/{id}` — one listing's detail (`id` URL-encoded
/// `publisher%2Fname`).
async fn get_listing(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let listing = with_registry(move |reg| {
        reg.get(&id)?.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("listing `{id}` not found"),
            )
        })
    })
    .await?;
    Ok(Json(json!(listing)))
}

#[derive(Deserialize)]
struct PublishReq {
    /// Base64-encoded `.apexpkg` file contents.
    apexpkg: String,
    #[serde(default)]
    categories: Vec<String>,
    /// Channel to publish to (default `stable`).
    channel: Option<String>,
}

/// `POST /api/v1/marketplace:publish` — publish a signed package to the registry.
///
/// The package must carry a valid signature from a trusted publisher and satisfy the
/// operator policy (allow-list, permission-risk ceiling, blocklist, and — when a
/// `block_scan_severity` ceiling is configured — the security scan). The response
/// carries the scan report so the publisher sees advisory findings. Emits
/// `plugin.published` on success.
async fn publish_listing(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PublishReq>,
) -> Result<Json<Value>, ApiError> {
    require_authenticated_principal(&headers)?;
    let bytes = base64_decode(&req.apexpkg)?;
    let categories = req.categories;
    let channel = req.channel;
    let out =
        with_registry(move |reg| Ok(reg.publish(&bytes, &categories, channel.as_deref())?)).await?;

    let tenant = headers
        .get("x-apex-tenant")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("public");
    crate::webhooks::emit(
        &state,
        "plugin.published",
        tenant,
        json!({ "listing": out.listing_id, "reference": out.reference, "channel": out.channel }),
    );

    Ok(Json(json!({
        "listing": out.listing_id,
        "reference": out.reference,
        "channel": out.channel,
        "status": "published",
        "scan": out.scan,
    })))
}

#[derive(Deserialize)]
struct DownloadParams {
    version: Option<String>,
}

/// `GET /api/v1/marketplace/listings/{id}/download?version=` — fetch the `.apexpkg`
/// bytes (base64) for a version (latest stable if omitted).
async fn download_version(
    Path(id): Path<String>,
    Query(params): Query<DownloadParams>,
) -> Result<Json<Value>, ApiError> {
    let lookup_id = id.clone();
    let bytes =
        with_registry(move |reg| Ok(reg.download(&lookup_id, params.version.as_deref())?)).await?;
    Ok(Json(json!({ "id": id, "apexpkg": base64_encode(&bytes) })))
}

/// `GET /api/v1/marketplace/listings/{id}/attestation?version=` — the supply-chain
/// attestation for a version: permission risk, SBOM, build provenance, content digest,
/// the operator verified badge, whether the package signature verifies against the
/// trust store, and a live security-scan report (against the current operator
/// deny-list). Derived on demand from the stored (signed) package, so it reflects
/// exactly what `download` serves. Latest stable version if `version` is omitted.
async fn version_attestation(
    Path(id): Path<String>,
    Query(params): Query<DownloadParams>,
) -> Result<Json<Value>, ApiError> {
    let value = with_registry(move |reg| {
        let listing = reg.get(&id)?.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("listing `{id}` not found"),
            )
        })?;
        let bytes = reg.download(&id, params.version.as_deref())?;
        let trust = plugins::load_trust()?;
        attestation_json(
            &id,
            &bytes,
            &trust,
            listing.verified,
            &reg.policy().deny_components,
        )
    })
    .await?;
    Ok(Json(value))
}

/// Build the supply-chain attestation JSON for a package: permission risk, SBOM, build
/// provenance, content digest, whether the signature verifies against `trust`, and the
/// security-scan report (re-run live so it reflects the *current* `deny_components`,
/// not the list at publish time). Pure over its inputs (no registry store, HTTP, or
/// `HOME`), so it is unit-testable directly.
fn attestation_json(
    id: &str,
    apexpkg: &[u8],
    trust: &TrustStore,
    verified: bool,
    deny_components: &[String],
) -> Result<Value, ApiError> {
    let package = Package::from_apexpkg(apexpkg)?;
    // `verify` re-checks the detached signature against the trust store; a failure here
    // (untrusted/unknown publisher, tampered manifest) is surfaced, not fatal — the panel
    // shows the package as unverified rather than hiding the attestation.
    let signature_verified = package.verify(trust).is_ok();
    let manifest = package.manifest()?;
    let risk = PermissionRisk::classify(&manifest.permissions);
    let scan = apex_marketplace::scan(&package, &manifest, deny_components);
    let package_digest = format!("sha256:{:x}", Sha256::digest(apexpkg));
    Ok(json!({
        "id": id,
        "version": manifest.metadata.version,
        "publisher": manifest.metadata.publisher,
        "verified": verified,
        "signature_verified": signature_verified,
        "risk": risk,
        "permissions": manifest.permissions,
        "package_digest": package_digest,
        "sbom": manifest.sbom,
        "provenance": manifest.provenance,
        "scan": scan,
    }))
}

#[derive(Deserialize)]
struct ReviewReq {
    author: String,
    rating: u8,
    #[serde(default)]
    body: String,
}

/// `POST /api/v1/marketplace/listings/{id}/reviews` — add a 1–5 star review.
async fn review_listing(
    Path(id): Path<String>,
    Json(req): Json<ReviewReq>,
) -> Result<Json<Value>, ApiError> {
    let id_for_resp = id.clone();
    let review = Review {
        author: req.author,
        rating: req.rating,
        body: req.body,
    };
    with_registry(move |reg| Ok(reg.review(&id, review)?)).await?;
    Ok(Json(json!({ "id": id_for_resp, "status": "reviewed" })))
}

#[derive(Deserialize)]
struct VerifyReq {
    #[serde(default = "default_true")]
    verified: bool,
}

fn default_true() -> bool {
    true
}

/// `POST /api/v1/marketplace/listings/{id}/verify` — operator sets the verified badge
/// directly, bypassing the request/approve/reject workflow below (e.g. an immediate
/// takedown, or back-compat with a pre-workflow verified listing). `marketplace:moderate`-
/// gated ([RM-GA-P1 SEC-104](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)).
async fn verify_listing(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<VerifyReq>,
) -> Result<Json<Value>, ApiError> {
    crate::tenancy::tenant_authorize(&state, &headers, "marketplace:moderate")?;
    let id_for_resp = id.clone();
    let verified = req.verified;
    with_registry(move |reg| Ok(reg.set_verified(&id, verified)?)).await?;
    Ok(Json(json!({ "id": id_for_resp, "verified": verified })))
}

/// `POST /api/v1/marketplace/listings/{id}/request-review` — the publisher requests
/// human review of the listing's current latest version, the step gating the
/// **verified** badge ([Marketplace §6]). Does not gate `publish` itself.
async fn request_review(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let id_for_resp = id.clone();
    with_registry(move |reg| Ok(reg.request_review(&id)?)).await?;
    Ok(Json(json!({ "id": id_for_resp, "status": "pending" })))
}

#[derive(Deserialize)]
struct ReviewDecisionReq {
    /// The reviewing identity, when the caller didn't send `X-Apex-Principal`.
    #[serde(default)]
    reviewer: Option<String>,
}

/// The acting identity for a reviewer/moderator decision, or a report submission:
/// `X-Apex-Principal` if present, else the request body's own identity field, else
/// `default` (anonymous back-compat).
fn actor_identity(headers: &HeaderMap, body_value: Option<&str>, default: &str) -> String {
    let principal = crate::tenancy::principal(headers);
    if !principal.is_empty() {
        return principal;
    }
    body_value
        .filter(|r| !r.is_empty())
        .unwrap_or(default)
        .to_string()
}

/// Require a non-anonymous, verified principal ([RM-GA-P1 SEC-104](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
/// `publish` must be attributable to a real caller, not an anonymous string —
/// distinct from RBAC (any authenticated principal may publish; moderation actions
/// additionally require `marketplace:moderate`).
fn require_authenticated_principal(headers: &HeaderMap) -> Result<String, ApiError> {
    let principal = crate::tenancy::principal(headers);
    if principal.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "marketplace:publish requires an authenticated principal",
        ));
    }
    Ok(principal)
}

/// `POST /api/v1/marketplace/listings/{id}/approve` — a reviewer approves the
/// listing's pending review, setting the verified badge ([Marketplace §6]).
/// `marketplace:moderate`-gated (SEC-104).
async fn approve_review(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ReviewDecisionReq>,
) -> Result<Json<Value>, ApiError> {
    crate::tenancy::tenant_authorize(&state, &headers, "marketplace:moderate")?;
    let reviewer = actor_identity(&headers, req.reviewer.as_deref(), "operator");
    let id_for_resp = id.clone();
    let reviewer_for_task = reviewer.clone();
    with_registry(move |reg| Ok(reg.approve_review(&id, &reviewer_for_task)?)).await?;
    Ok(Json(
        json!({ "id": id_for_resp, "verified": true, "reviewer": reviewer }),
    ))
}

#[derive(Deserialize)]
struct RejectReviewReq {
    /// The reviewing identity, when the caller didn't send `X-Apex-Principal`.
    #[serde(default)]
    reviewer: Option<String>,
    /// Actionable feedback explaining the rejection.
    reason: String,
}

/// `POST /api/v1/marketplace/listings/{id}/reject` — a reviewer rejects the listing's
/// pending review with actionable feedback ([Marketplace §6]), clearing the verified
/// badge; the publisher may address it and request review again. `marketplace:moderate`-
/// gated (SEC-104).
async fn reject_review(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RejectReviewReq>,
) -> Result<Json<Value>, ApiError> {
    crate::tenancy::tenant_authorize(&state, &headers, "marketplace:moderate")?;
    let reviewer = actor_identity(&headers, req.reviewer.as_deref(), "operator");
    let id_for_resp = id.clone();
    let reviewer_for_task = reviewer.clone();
    let reason = req.reason;
    let reason_for_task = reason.clone();
    with_registry(move |reg| Ok(reg.reject_review(&id, &reviewer_for_task, &reason_for_task)?))
        .await?;
    Ok(Json(json!({
        "id": id_for_resp,
        "verified": false,
        "reviewer": reviewer,
        "reason": reason,
    })))
}

#[derive(Deserialize)]
struct InstallReq {
    version: Option<String>,
    #[serde(default)]
    grants: Vec<String>,
}

/// `POST /api/v1/marketplace/listings/{id}/install` — download a listed package and
/// install it into the local plugin catalog (disabled), then bump the install count.
async fn install_listing(
    Path(id): Path<String>,
    Json(req): Json<InstallReq>,
) -> Result<Json<Value>, ApiError> {
    let id_for_resp = id.clone();
    let installed = with_registry(move |reg| {
        let bytes = reg.download(&id, req.version.as_deref())?;
        let package = Package::from_apexpkg(&bytes)?;
        let installed = plugins::install_package(&package, &req.grants)?;
        reg.record_install(&id)?;
        Ok(installed)
    })
    .await?;
    Ok(Json(
        json!({ "id": id_for_resp, "status": "installed", "plugin": installed }),
    ))
}

#[derive(Deserialize)]
struct AbuseReportReq {
    /// The reporting principal, when the caller didn't send `X-Apex-Principal`.
    #[serde(default)]
    reporter: Option<String>,
    /// Why the listing is being reported (malware, IP infringement, deceptive
    /// metadata, etc.).
    reason: String,
}

/// `POST /api/v1/marketplace/listings/{id}/report` — file an abuse report against a
/// listing ([Marketplace §8]). Feeds moderation and can trigger delisting via the
/// `resolve` route below. Emits `plugin.abuse_reported`.
async fn report_abuse(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<AbuseReportReq>,
) -> Result<Json<Value>, ApiError> {
    let reporter = actor_identity(&headers, req.reporter.as_deref(), "anonymous");
    let id_for_resp = id.clone();
    let reporter_for_task = reporter.clone();
    let reason = req.reason;
    let reason_for_task = reason.clone();
    let report_id = with_registry(move |reg| {
        Ok(reg.report_abuse(&id, &reporter_for_task, &reason_for_task)?)
    })
    .await?;

    let tenant = headers
        .get("x-apex-tenant")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("public");
    crate::webhooks::emit(
        &state,
        "plugin.abuse_reported",
        tenant,
        json!({ "listing": id_for_resp, "report_id": report_id, "reporter": reporter, "reason": reason }),
    );

    Ok(Json(json!({
        "id": id_for_resp,
        "report_id": report_id,
        "reporter": reporter,
        "status": "open",
    })))
}

/// `GET /api/v1/marketplace/listings/{id}/reports` — the abuse reports filed against
/// a listing, for moderator review.
async fn list_abuse_reports(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let reports = with_registry(move |reg| Ok(reg.abuse_reports(&id)?)).await?;
    Ok(Json(json!({ "reports": reports })))
}

#[derive(Deserialize)]
struct ResolveAbuseReportReq {
    /// The moderating identity, when the caller didn't send `X-Apex-Principal`.
    #[serde(default)]
    moderator: Option<String>,
    /// Whether resolving the report also delists the listing (removes it from
    /// discovery/download).
    #[serde(default)]
    delist: bool,
}

/// `POST /api/v1/marketplace/listings/{id}/reports/{report_id}/resolve` — a
/// moderator resolves an open abuse report as valid ([Marketplace §8]), optionally
/// delisting the listing. Emits `plugin.delisted` when `delist` is `true`.
/// `marketplace:moderate`-gated (SEC-104).
async fn resolve_abuse_report(
    State(state): State<Arc<AppState>>,
    Path((id, report_id)): Path<(String, u64)>,
    headers: HeaderMap,
    Json(req): Json<ResolveAbuseReportReq>,
) -> Result<Json<Value>, ApiError> {
    crate::tenancy::tenant_authorize(&state, &headers, "marketplace:moderate")?;
    let moderator = actor_identity(&headers, req.moderator.as_deref(), "operator");
    let id_for_resp = id.clone();
    let moderator_for_task = moderator.clone();
    let delist = req.delist;
    with_registry(move |reg| {
        Ok(reg.resolve_abuse_report(&id, report_id, &moderator_for_task, delist)?)
    })
    .await?;

    if delist {
        let tenant = headers
            .get("x-apex-tenant")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("public");
        crate::webhooks::emit(
            &state,
            "plugin.delisted",
            tenant,
            json!({ "listing": id_for_resp, "report_id": report_id, "moderator": moderator }),
        );
    }

    Ok(Json(json!({
        "id": id_for_resp,
        "report_id": report_id,
        "moderator": moderator,
        "delisted": delist,
    })))
}

#[derive(Deserialize)]
struct DismissAbuseReportReq {
    /// The moderating identity, when the caller didn't send `X-Apex-Principal`.
    #[serde(default)]
    moderator: Option<String>,
    /// Why the report was found not actionable.
    reason: String,
}

/// `POST /api/v1/marketplace/listings/{id}/reports/{report_id}/dismiss` — a
/// moderator dismisses an open abuse report as not actionable ([Marketplace §8]).
/// `marketplace:moderate`-gated (SEC-104).
async fn dismiss_abuse_report(
    State(state): State<Arc<AppState>>,
    Path((id, report_id)): Path<(String, u64)>,
    headers: HeaderMap,
    Json(req): Json<DismissAbuseReportReq>,
) -> Result<Json<Value>, ApiError> {
    crate::tenancy::tenant_authorize(&state, &headers, "marketplace:moderate")?;
    let moderator = actor_identity(&headers, req.moderator.as_deref(), "operator");
    let id_for_resp = id.clone();
    let moderator_for_task = moderator.clone();
    let reason = req.reason;
    let reason_for_task = reason.clone();
    with_registry(move |reg| {
        Ok(reg.dismiss_abuse_report(&id, report_id, &moderator_for_task, &reason_for_task)?)
    })
    .await?;

    Ok(Json(json!({
        "id": id_for_resp,
        "report_id": report_id,
        "moderator": moderator,
        "reason": reason,
    })))
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ApiError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "apexpkg must be valid base64-encoded bytes",
            )
        })
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest requesting a wildcard permission (⇒ High risk) and carrying both SBOM
    /// and build provenance — the attestation surface the panel renders.
    const ATTESTED: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: hello, version: 2.1.0, publisher: acme }
permissions:
  - net:egress:*
provenance:
  builder: github-actions
  source: github.com/acme/hello@v2.1.0
  built_at: "2026-07-02T09:00:00Z"
sbom:
  components:
    - { name: serde, version: "1.0.0", license: MIT }
    - { name: reqwest, version: "0.12.0" }
"#;

    #[test]
    fn attestation_extracts_risk_sbom_provenance_and_digest() {
        // Unsigned package (empty signature) checked against an empty trust store: the
        // attestation is still produced, just flagged unverified.
        let apexpkg = Package::new(ATTESTED, Vec::new()).to_apexpkg().unwrap();
        let v = attestation_json("acme/hello", &apexpkg, &TrustStore::new(), true, &[]).unwrap();

        assert_eq!(v["id"], "acme/hello");
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["publisher"], "acme");
        assert_eq!(v["verified"], true); // operator badge, passed through
        assert_eq!(v["signature_verified"], false); // untrusted signer
        assert_eq!(v["risk"], "high"); // wildcard permission
        assert_eq!(v["permissions"][0], "net:egress:*");
        assert_eq!(v["provenance"]["builder"], "github-actions");
        assert_eq!(v["sbom"]["components"].as_array().unwrap().len(), 2);
        assert_eq!(v["sbom"]["components"][0]["name"], "serde");
        assert!(
            v["package_digest"].as_str().unwrap().starts_with("sha256:"),
            "digest is content-addressed"
        );
        // Live scan: the wildcard permission and the unlicensed `reqwest` component.
        let codes: Vec<&str> = v["scan"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["permission.broad", "component.unlicensed"]);

        // A deny-listed SBOM component surfaces as a critical finding.
        let v = attestation_json(
            "acme/hello",
            &apexpkg,
            &TrustStore::new(),
            true,
            &["serde@1.0.0".to_string()],
        )
        .unwrap();
        assert!(
            v["scan"]["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["code"] == "component.denied" && f["severity"] == "critical")
        );
    }

    #[test]
    fn attestation_without_sbom_or_provenance_is_null() {
        const BARE: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: bare, version: 1.0.0, publisher: acme }
"#;
        let apexpkg = Package::new(BARE, Vec::new()).to_apexpkg().unwrap();
        let v = attestation_json("acme/bare", &apexpkg, &TrustStore::new(), false, &[]).unwrap();
        assert_eq!(v["risk"], "low"); // no permissions
        assert!(v["sbom"].is_null());
        assert!(v["provenance"].is_null());
        // The missing attestation shows up as advisory scan findings.
        let codes: Vec<&str> = v["scan"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["sbom.missing", "provenance.missing"]);
    }

    #[test]
    fn actor_identity_prefers_the_principal_header_then_body_then_default() {
        let mut headers = HeaderMap::new();
        headers.insert("x-apex-principal", "alice".parse().unwrap());
        assert_eq!(actor_identity(&headers, Some("bob"), "operator"), "alice");
        assert_eq!(actor_identity(&headers, None, "operator"), "alice");

        let empty = HeaderMap::new();
        assert_eq!(actor_identity(&empty, Some("bob"), "operator"), "bob");
        assert_eq!(actor_identity(&empty, None, "operator"), "operator");
        assert_eq!(actor_identity(&empty, Some(""), "operator"), "operator");
        assert_eq!(actor_identity(&empty, None, "anonymous"), "anonymous");
    }

    // --- RM-GA-P1 SEC-104: moderation + publish routes are RBAC-gated ---------------

    use apex_tenancy::{
        InMemoryTenancyStore, MemberScope, Membership, Organization, Role, TenancyStore,
    };
    use axum::http::{Request, StatusCode as HttpStatus};
    use tower::ServiceExt;

    async fn call(state: &Arc<AppState>, method: &str, uri: &str, principal: &str) -> HttpStatus {
        let resp = crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("x-apex-principal", principal)
                    .body(axum::body::Body::from(
                        json!({ "reason": "test" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        resp.status()
    }

    /// Every moderation route requires `marketplace:moderate` — a non-member (even
    /// against a listing that doesn't exist) is refused *before* the registry is ever
    /// consulted, so this needs no real published listing to prove.
    #[tokio::test]
    async fn moderation_routes_require_marketplace_moderate() {
        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org = tenancy
            .create_org(Organization::new("default", "Moderation Test Co"))
            .unwrap();
        tenancy
            .add_membership(Membership {
                user: "admin".to_string(),
                role: Role::OrgAdmin,
                scope: MemberScope::Organization(org.id.clone()),
            })
            .unwrap();
        let state = Arc::new(crate::AppState::from_env().await.with_tenancy(tenancy));

        for uri in [
            "/api/v1/marketplace/listings/nonexistent%2Flisting/verify",
            "/api/v1/marketplace/listings/nonexistent%2Flisting/approve",
            "/api/v1/marketplace/listings/nonexistent%2Flisting/reject",
            "/api/v1/marketplace/listings/nonexistent%2Flisting/reports/1/resolve",
            "/api/v1/marketplace/listings/nonexistent%2Flisting/reports/1/dismiss",
        ] {
            assert_eq!(
                call(&state, "POST", uri, "mallory").await,
                HttpStatus::FORBIDDEN,
                "{uri}"
            );
            // The org admin passes the RBAC gate (whatever the registry itself then
            // does with a nonexistent listing is a separate, not-found concern).
            let status = call(&state, "POST", uri, "admin").await;
            assert_ne!(status, HttpStatus::FORBIDDEN, "{uri}");
            assert_ne!(status, HttpStatus::UNAUTHORIZED, "{uri}");
        }
    }

    /// `publish` requires a real, non-anonymous principal (PP-03) — independent of
    /// `marketplace:moderate`, which only gates moderation actions.
    #[tokio::test]
    async fn publish_requires_an_authenticated_principal() {
        let headers = HeaderMap::new();
        assert!(require_authenticated_principal(&headers).is_err());

        let mut headers = HeaderMap::new();
        headers.insert("x-apex-principal", "alice".parse().unwrap());
        assert_eq!(require_authenticated_principal(&headers).unwrap(), "alice");
    }
}
