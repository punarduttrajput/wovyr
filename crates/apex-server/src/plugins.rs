//! Plugin routes: full lifecycle over the durable catalog the CLI manages under
//! `~/.apex/plugins` (`catalog.json` + `trust.json` + `staging/`).
//!
//! Routes: list, install, enable, disable, upgrade, rollback, uninstall, trust.
//! All reads/writes are to the same files `apex plugin *` CLI commands use, so
//! changes made here are immediately visible to the CLI and vice versa.

use apex_plugin::{
    CapabilityKind, CapabilityRuntime, InstalledPlugin, NotLoadedRuntime, Package, PluginEngine,
    PluginState, TrustStore,
};
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

/// Operator keyless-trust configuration ([ADR-0009]) from
/// `~/.apex/plugins/keyless.json` (`{"root": …, "policy": …}`), shared by the plugin
/// engine and the marketplace registry. Absent ⇒ keyless disabled (publisher-key
/// trust only).
#[derive(serde::Deserialize)]
pub(crate) struct KeylessConfig {
    pub root: apex_plugin::KeylessRoot,
    pub policy: apex_plugin::IdentityPolicy,
}

pub(crate) fn load_keyless() -> Result<Option<KeylessConfig>, ApiError> {
    let path = match plugins_dir() {
        Some(dir) => dir.join("keyless.json"),
        None => return Ok(None),
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("corrupt keyless trust config: {e}"),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )),
    }
}

pub(crate) fn load_trust() -> Result<TrustStore, ApiError> {
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
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )
    })?;
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join("trust.json"), bytes))
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
    let mut e = PluginEngine::new(semver::Version::new(1, 0, 0), load_trust()?)
        .with_catalog(load_catalog()?);
    if let Some(staging) = staging_dir() {
        e = e.with_staging_dir(staging);
    }
    if let Some(keyless) = load_keyless()? {
        e = e.with_keyless(keyless.root, keyless.policy);
    }
    Ok(e)
}

/// The capability runtime plugin tools registered into the server's *run* registry use:
/// the secret-aware WASM loader when built with `plugin-wasi`, else the not-loaded
/// placeholder (tools are visible with correct metadata but error on call).
fn run_capability_runtime(vault: &apex_secrets::Vault) -> Arc<dyn CapabilityRuntime> {
    #[cfg(feature = "plugin-wasi")]
    if let Ok(rt) = apex_plugin::WasiCapabilityRuntime::new() {
        return Arc::new(rt.with_secrets(vault.clone()));
    }
    let _ = vault;
    Arc::new(NotLoadedRuntime)
}

/// Register every **enabled** plugin's tool capabilities from the durable catalog into
/// `registry`, routing execution through a [secret-aware](run_capability_runtime) runtime
/// so an agent/workflow run can call them with their tenant-scoped secrets injected.
/// Best-effort: a missing or corrupt catalog registers nothing (the server still starts).
pub(crate) fn register_enabled_tools(registry: &mut ToolRegistry, vault: &apex_secrets::Vault) {
    let trust = load_trust().unwrap_or_else(|_| TrustStore::new());
    let catalog = load_catalog().unwrap_or_default();
    register_catalog(registry, catalog, trust, staging_dir(), vault);
}

/// Register the enabled plugins of `catalog` into `registry` through the run runtime.
/// Split out from [`register_enabled_tools`] (which sources the catalog from disk) so the
/// registration path is unit-testable without touching `~/.apex/plugins`.
fn register_catalog(
    registry: &mut ToolRegistry,
    catalog: Vec<InstalledPlugin>,
    trust: TrustStore,
    staging: Option<PathBuf>,
    vault: &apex_secrets::Vault,
) {
    let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust)
        .with_catalog(catalog)
        .with_runtime(run_capability_runtime(vault));
    if let Some(staging) = staging {
        engine = engine.with_staging_dir(staging);
    }
    engine.register_enabled(registry);
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route(
            "/api/v1/plugins:install",
            axum::routing::post(install_plugin),
        )
        .route("/api/v1/plugins:enable", axum::routing::post(enable_plugin))
        .route(
            "/api/v1/plugins:disable",
            axum::routing::post(disable_plugin),
        )
        .route(
            "/api/v1/plugins:upgrade",
            axum::routing::post(upgrade_plugin),
        )
        .route(
            "/api/v1/plugins:rollback",
            axum::routing::post(rollback_plugin),
        )
        .route(
            "/api/v1/plugins:trust",
            axum::routing::post(trust_publisher),
        )
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
    Ok(Json(install_package(&package, &req.grants)?))
}

/// Install a verified [`Package`] into the durable catalog (disabled), returning its
/// catalog JSON. Shared by the HTTP install route and the marketplace install bridge.
pub(crate) fn install_package(package: &Package, grants: &[String]) -> Result<Value, ApiError> {
    let mut engine = engine()?;
    let installed = engine.install(package, grants)?;
    let resp = plugin_json(installed);
    save_catalog(&engine.catalog())?;
    Ok(resp)
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
    Ok(Json(
        json!({ "publisher": req.publisher, "status": "trusted" }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use apex_plugin::PluginManifest;
    use apex_secrets::{InMemorySecretStore, Vault};

    fn vault() -> Vault {
        Vault::new(Arc::new(InMemorySecretStore::new()))
    }

    fn enabled(manifest_yaml: &str) -> InstalledPlugin {
        InstalledPlugin {
            manifest: PluginManifest::from_yaml(manifest_yaml).unwrap(),
            state: PluginState::Enabled,
            granted_permissions: vec![],
            artifact_dir: None,
            previous: None,
        }
    }

    const DEMO: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: demo, version: 1.0.0, publisher: acme }
capabilities:
  - { kind: tool, id: demo.run }
"#;

    #[test]
    fn registers_enabled_plugin_tools_into_the_run_registry() {
        let mut registry = ToolRegistry::with_builtins();
        register_catalog(
            &mut registry,
            vec![enabled(DEMO)],
            TrustStore::new(),
            None,
            &vault(),
        );
        // The enabled plugin's tool capability is now callable from a run.
        assert!(registry.contains("demo.run"));
        // Built-ins remain.
        assert!(registry.contains("echo"));
    }

    #[test]
    fn disabled_plugins_are_not_registered() {
        let mut plugin = enabled(DEMO);
        plugin.state = PluginState::Disabled;
        let mut registry = ToolRegistry::with_builtins();
        register_catalog(
            &mut registry,
            vec![plugin],
            TrustStore::new(),
            None,
            &vault(),
        );
        assert!(!registry.contains("demo.run"));
    }
}
