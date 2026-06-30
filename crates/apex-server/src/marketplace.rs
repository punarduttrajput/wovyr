//! Marketplace routes: the hosted plugin registry — publish, discover, download, rate,
//! verify, and a publish→install bridge ([Marketplace](../../docs/08-plugin-sdk/marketplace.md)).
//!
//! Listings persist to a durable [`FileRegistryStore`] at `~/.apex/marketplace/registry.json`.
//! Publishing re-verifies the package signature against the **same** trust store the
//! plugin lifecycle uses (`~/.apex/plugins/trust.json`), so only trusted publishers can
//! list a plugin. Operator curation ([§7]) is loaded from
//! `~/.apex/marketplace/policy.json` when present (else the permissive default). The
//! install bridge downloads a listed package and installs it into the local plugin
//! catalog via the shared [`plugins::install_package`](crate::plugins) helper.

use apex_marketplace::{FileRegistryStore, Registry, RegistryPolicy, Review, SearchQuery};
use apex_plugin::{CapabilityKind, Package};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
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
