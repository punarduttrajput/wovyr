//! Plugin routes: full lifecycle over the durable catalog the CLI manages under
//! `~/.apex/plugins` (`catalog.json` + `trust.json` + `staging/`).
//!
//! Routes: list, install, enable, disable, upgrade, rollback, uninstall, trust.
//! All reads/writes are to the same files `apex plugin *` CLI commands use, so
//! changes made here are immediately visible to the CLI and vice versa.

use apex_plugin::{CapabilityKind, InstalledPlugin, Package, PluginEngine, PluginState, TrustStore};
use apex_tools::ToolRegistry;
use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    routing::{delete, get},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

use crate::ApiError;
use crate::AppState;

fn plugins_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".apex").join("plugins"))
}

fn staging_dir() -> Option<PathBuf> {
    plugins_dir().map(|d| d.join("staging"))
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: PathBuf) -> Result<T, ApiError> {
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("corrupt {}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )),
    }
}

fn load_catalog() -> Result<Vec<InstalledPlugin>, ApiError> {
    match plugins_dir() {
        Some(dir) => read_json(dir.join("catalog.json")),
        None => Ok(Vec::new()),
    }
}

fn load_trust() -> Result<TrustStore, ApiError> {
    let path = match plugins_dir() {
        Some(dir) => dir.join("trust.json"),
        None => return Ok(TrustStore::new()),
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("corrupt trust store: {e}"),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustStore::new()),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )),
    }
}

fn save_catalog(catalog: &[InstalledPlugin]) -> Result<(), ApiError> {
    let dir = plugins_dir().ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "no home directory for plugin catalog",
        )
    })?;
    let bytes = serde_json::to_vec_pretty(catalog).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )
    })?;
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join("catalog.json"), bytes))
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            )
        })
}

fn save_trust(trust: &TrustStore) -> Result<(), ApiError> {
    let dir = plugins_dir().ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "no home directory for trust store",
        )
    })?;
    let bytes = serde_json::to_vec_pretty(trust).map_err(|e| {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e.to_string())
    })?;
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join("trust.json"), bytes))
        .map_err(|e| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e.to_string())
        })
}

/// Build an engine from the durable catalog (platform API 1.0.0, matching the CLI).
fn engine() -> Result<PluginEngine, ApiError> {
    let mut e = PluginEngine::new(semver::Version::new(1, 0, 0), load_trust()?)
        .with_catalog(load_catalog()?);
    if let Some(staging) = staging_dir() {
        e = e.with_staging_dir(staging);
    }
    Ok(e)
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route("/api/v1/plugins:install", axum::routing::post(install_plugin))
        .route("/api/v1/plugins:enable", axum::routing::post(enable_plugin))
        .route("/api/v1/plugins:disable", axum::routing::post(disable_plugin))
        .route("/api/v1/plugins:upgrade", axum::routing::post(upgrade_plugin))
        .route("/api/v1/plugins:rollback", axum::routing::post(rollback_plugin))
        .route("/api/v1/plugins:trust", axum::routing::post(trust_publisher))
        .route("/api/v1/plugins/{id}", delete(uninstall_plugin))
}

fn kind_str(k: CapabilityKind) -> &'static str {
    match k {
        CapabilityKind::Tool => "tool",
        CapabilityKind::Provider => "provider",
        CapabilityKind::MemoryBackend => "memory_backend",
        CapabilityKind::Policy => "policy",
        CapabilityKind::WorkflowActivity => "workflow_activity",
    }
}

fn plugin_json(p: &InstalledPlugin) -> Value {
    let caps: Vec<Value> = p
        .manifest
        .capabilities
        .iter()
        .map(|c| json!({ "kind": kind_str(c.kind), "id": c.id }))
        .collect();
    json!({
        "id": p.manifest.qualified_id(),
        "name": p.manifest.metadata.name,
        "version": p.manifest.metadata.version,
        "publisher": p.manifest.metadata.publisher,
        "description": p.manifest.metadata.description,
        "state": match p.state { PluginState::Enabled => "enabled", PluginState::Disabled => "disabled" },
        "permissions": p.manifest.permissions,
        "granted": p.granted_permissions,
        "capabilities": caps,
        "platform_api": p.manifest.compatibility.platform_api,
    })
}

/// `GET /api/v1/plugins` — the installed plugin catalog.
async fn list_plugins() -> Result<Json<Value>, ApiError> {
    let catalog = load_catalog()?;
    let items: Vec<Value> = catalog.iter().map(plugin_json).collect();
    Ok(Json(json!({ "plugins": items, "total": items.len() })))
}

#[derive(Deserialize)]
struct PluginRef {
    id: String,
}

/// `POST /api/v1/plugins:enable` — enable a plugin (routes its capabilities live).
async fn enable_plugin(Json(req): Json<PluginRef>) -> Result<Json<Value>, ApiError> {
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.enable(&req.id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    Ok(Json(json!({ "id": req.id, "state": "enabled" })))
}

/// `POST /api/v1/plugins:disable` — disable a plugin (withdraws its capabilities).
async fn disable_plugin(Json(req): Json<PluginRef>) -> Result<Json<Value>, ApiError> {
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.disable(&req.id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    Ok(Json(json!({ "id": req.id, "state": "disabled" })))
}

#[derive(Deserialize)]
struct InstallReq {
    /// Base64-encoded `.apexpkg` file contents.
    apexpkg: String,
    #[serde(default)]
    grants: Vec<String>,
}

/// `POST /api/v1/plugins:install` — install a plugin from a base64-encoded `.apexpkg`.
///
/// The package must carry a valid ed25519 signature from a trusted publisher
/// (`POST /api/v1/plugins:trust` registers publishers). On success the plugin is
/// installed in the *disabled* state; call `:enable` to activate it.
async fn install_plugin(Json(req): Json<InstallReq>) -> Result<Json<Value>, ApiError> {
    let bytes = base64_decode(&req.apexpkg)?;
    let package = Package::from_apexpkg(&bytes)?;
    let mut engine = engine()?;
    let installed = engine.install(&package, &req.grants)?;
    let resp = plugin_json(installed);
    save_catalog(&engine.catalog())?;
    Ok(Json(resp))
}

/// `POST /api/v1/plugins:upgrade` — upgrade an installed plugin to a new version.
///
/// Retains the prior version for rollback. Any new permissions beyond what was
/// previously granted must be listed in `grants`.
async fn upgrade_plugin(Json(req): Json<InstallReq>) -> Result<Json<Value>, ApiError> {
    let bytes = base64_decode(&req.apexpkg)?;
    let package = Package::from_apexpkg(&bytes)?;
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.upgrade(&package, &req.grants, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    let id = package.manifest()?.qualified_id();
    Ok(Json(json!({ "id": id, "status": "upgraded" })))
}

#[derive(Deserialize)]
struct RollbackReq {
    id: String,
}

/// `POST /api/v1/plugins:rollback` — revert a plugin to its retained prior version.
async fn rollback_plugin(Json(req): Json<RollbackReq>) -> Result<Json<Value>, ApiError> {
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.rollback(&req.id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    Ok(Json(json!({ "id": req.id, "status": "rolled_back" })))
}

/// `DELETE /api/v1/plugins/{id}` — uninstall a plugin.
///
/// `id` is URL-encoded `publisher/name` (e.g. `acme%2Fmy-plugin`).
async fn uninstall_plugin(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.uninstall(&id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    Ok(Json(json!({ "id": id, "status": "uninstalled" })))
}

#[derive(Deserialize)]
struct TrustReq {
    publisher: String,
    /// Hex-encoded ed25519 public key (32 bytes = 64 hex chars).
    public_key_hex: String,
}

/// `POST /api/v1/plugins:trust` — register a publisher's ed25519 public key.
///
/// After this, packages signed by that publisher can be installed.
async fn trust_publisher(Json(req): Json<TrustReq>) -> Result<Json<Value>, ApiError> {
    let key_bytes = hex::decode(&req.public_key_hex).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "public_key_hex must be valid hex (64 chars for a 32-byte ed25519 key)",
        )
    })?;
    let mut trust = load_trust()?;
    trust.trust(req.publisher.clone(), key_bytes);
    save_trust(&trust)?;
    Ok(Json(json!({ "publisher": req.publisher, "status": "trusted" })))
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
