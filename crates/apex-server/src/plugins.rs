//! Plugin routes: list the installed catalog and enable/disable plugins.
//!
//! Reads/writes the same durable catalog the CLI manages under `~/.apex/plugins`
//! (`catalog.json` + `trust.json`), so plugins installed via `apex plugin install`
//! show up here and lifecycle changes persist. This surfaces the **installed** set;
//! hosted marketplace discovery is a later slice (see apex-plugin docs).

use apex_plugin::{CapabilityKind, InstalledPlugin, PluginEngine, PluginState, TrustStore};
use apex_tools::ToolRegistry;
use axum::{Json, Router, http::StatusCode, routing::get};
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

/// Build an engine from the durable catalog (platform API 1.0.0, matching the CLI).
fn engine() -> Result<PluginEngine, ApiError> {
    Ok(
        PluginEngine::new(semver::Version::new(1, 0, 0), load_trust()?)
            .with_catalog(load_catalog()?),
    )
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route("/api/v1/plugins:enable", axum::routing::post(enable_plugin))
        .route(
            "/api/v1/plugins:disable",
            axum::routing::post(disable_plugin),
        )
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
