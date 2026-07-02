//! Marketplace routes: the hosted plugin registry — publish, discover, download,
//! attest, rate, verify, and a publish→install bridge
//! ([Marketplace](../../docs/08-plugin-sdk/marketplace.md)). The **attestation** route
//! surfaces a version's supply-chain posture (permission risk, SBOM, build provenance,
//! content digest, signature verification) so an operator sees it before granting.
//!
//! Listings persist to a durable [`FileRegistryStore`] at `~/.apex/marketplace/registry.json`.
//! Publishing re-verifies the package signature against the **same** trust store the
//! plugin lifecycle uses (`~/.apex/plugins/trust.json`), so only trusted publishers can
//! list a plugin. Operator curation ([§7]) is loaded from
//! `~/.apex/marketplace/policy.json` when present (else the permissive default). The
//! install bridge downloads a listed package and installs it into the local plugin
//! catalog via the shared [`plugins::install_package`](crate::plugins) helper.

use apex_marketplace::{
    FileRegistryStore, PermissionRisk, Registry, RegistryPolicy, Review, SearchQuery,
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
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".apex").join("marketplace"))
        .ok_or_else(|| {
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

/// Build a registry over the durable store, sharing the plugin trust store and applying
/// the operator policy.
fn registry() -> Result<Registry<FileRegistryStore>, ApiError> {
    let trust = plugins::load_trust()?;
    let store = FileRegistryStore::new(marketplace_dir()?.join("registry.json"));
    Ok(Registry::new(store, trust).with_policy(load_policy()?))
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
            "/api/v1/marketplace/listings/{id}/install",
            post(install_listing),
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
}

/// `GET /api/v1/marketplace/listings?q=&category=&capability=` — search/browse.
async fn search_listings(Query(params): Query<SearchParams>) -> Result<Json<Value>, ApiError> {
    let query = SearchQuery {
        text: params.q,
        category: params.category,
        capability: params.capability.as_deref().and_then(parse_capability),
    };
    let listings = registry()?.search(&query)?;
    let total = listings.len();
    Ok(Json(json!({ "listings": listings, "total": total })))
}

/// `GET /api/v1/marketplace/listings/{id}` — one listing's detail (`id` URL-encoded
/// `publisher%2Fname`).
async fn get_listing(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    match registry()?.get(&id)? {
        Some(listing) => Ok(Json(json!(listing))),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("listing `{id}` not found"),
        )),
    }
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
/// operator policy (allow-list, permission-risk ceiling, blocklist). Emits
/// `plugin.published` on success.
async fn publish_listing(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PublishReq>,
) -> Result<Json<Value>, ApiError> {
    let bytes = base64_decode(&req.apexpkg)?;
    let out = registry()?.publish(&bytes, &req.categories, req.channel.as_deref())?;

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
    let bytes = registry()?.download(&id, params.version.as_deref())?;
    Ok(Json(json!({ "id": id, "apexpkg": base64_encode(&bytes) })))
}

/// `GET /api/v1/marketplace/listings/{id}/attestation?version=` — the supply-chain
/// attestation for a version: permission risk, SBOM, build provenance, content digest,
/// the operator verified badge, and whether the package signature verifies against the
/// trust store. Derived on demand from the stored (signed) package, so it reflects
/// exactly what `download` serves. Latest stable version if `version` is omitted.
async fn version_attestation(
    Path(id): Path<String>,
    Query(params): Query<DownloadParams>,
) -> Result<Json<Value>, ApiError> {
    let reg = registry()?;
    let listing = reg.get(&id)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("listing `{id}` not found"),
        )
    })?;
    let bytes = reg.download(&id, params.version.as_deref())?;
    let trust = plugins::load_trust()?;
    Ok(Json(attestation_json(
        &id,
        &bytes,
        &trust,
        listing.verified,
    )?))
}

/// Build the supply-chain attestation JSON for a package: permission risk, SBOM, build
/// provenance, content digest, and whether the signature verifies against `trust`. Pure
/// over its inputs (no registry store, HTTP, or `HOME`), so it is unit-testable directly.
fn attestation_json(
    id: &str,
    apexpkg: &[u8],
    trust: &TrustStore,
    verified: bool,
) -> Result<Value, ApiError> {
    let package = Package::from_apexpkg(apexpkg)?;
    // `verify` re-checks the detached signature against the trust store; a failure here
    // (untrusted/unknown publisher, tampered manifest) is surfaced, not fatal — the panel
    // shows the package as unverified rather than hiding the attestation.
    let signature_verified = package.verify(trust).is_ok();
    let manifest = package.manifest()?;
    let risk = PermissionRisk::classify(&manifest.permissions);
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
    registry()?.review(
        &id,
        Review {
            author: req.author,
            rating: req.rating,
            body: req.body,
        },
    )?;
    Ok(Json(json!({ "id": id, "status": "reviewed" })))
}

#[derive(Deserialize)]
struct VerifyReq {
    #[serde(default = "default_true")]
    verified: bool,
}

fn default_true() -> bool {
    true
}

/// `POST /api/v1/marketplace/listings/{id}/verify` — operator sets the verified badge.
async fn verify_listing(
    Path(id): Path<String>,
    Json(req): Json<VerifyReq>,
) -> Result<Json<Value>, ApiError> {
    registry()?.set_verified(&id, req.verified)?;
    Ok(Json(json!({ "id": id, "verified": req.verified })))
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
    let reg = registry()?;
    let bytes = reg.download(&id, req.version.as_deref())?;
    let package = Package::from_apexpkg(&bytes)?;
    let installed = plugins::install_package(&package, &req.grants)?;
    reg.record_install(&id)?;
    Ok(Json(
        json!({ "id": id, "status": "installed", "plugin": installed }),
    ))
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
        let v = attestation_json("acme/hello", &apexpkg, &TrustStore::new(), true).unwrap();

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
    }

    #[test]
    fn attestation_without_sbom_or_provenance_is_null() {
        const BARE: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: bare, version: 1.0.0, publisher: acme }
"#;
        let apexpkg = Package::new(BARE, Vec::new()).to_apexpkg().unwrap();
        let v = attestation_json("acme/bare", &apexpkg, &TrustStore::new(), false).unwrap();
        assert_eq!(v["risk"], "low"); // no permissions
        assert!(v["sbom"].is_null());
        assert!(v["provenance"].is_null());
    }
}
