//! Plugin routes: full lifecycle over the durable catalog the CLI manages under
//! `~/.wovyr/plugins` (`catalog.json` + `trust.json` + `staging/`).
//!
//! Routes: list, install, enable, disable, upgrade, rollback, uninstall, trust.
//! All reads/writes are to the same files `wovyr plugin *` CLI commands use, so
//! changes made here are immediately visible to the CLI and vice versa.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use utoipa::ToSchema;
use wovyr_plugin::{
    CapabilityKind, CapabilityRuntime, InstalledPlugin, NotLoadedRuntime, Package, PluginEngine,
    TrustStore,
};
use wovyr_tools::ToolRegistry;

use crate::ApiError;
use crate::AppState;
use crate::hardening::{PageQuery, paginate};
use axum::extract::{Query, State};

fn plugins_dir() -> Option<PathBuf> {
    wovyr_config::paths::plugins_dir().ok()
}

fn staging_dir() -> Option<PathBuf> {
    wovyr_config::paths::staging_dir().ok()
}

/// Acquire the cross-process advisory lock over `~/.wovyr/plugins` (RM-GA-P2
/// DUR-403), held for the duration of a mutating handler. Every lifecycle
/// handler here does load-trust/catalog → mutate the in-memory `PluginEngine`
/// → save-trust/catalog, all against files the CLI's `wovyr plugin` commands
/// touch too; without a lock spanning that whole sequence, a concurrent writer
/// (this or another process) could act on the same stale load and have its
/// update silently clobbered by whichever saves last.
fn acquire_lock() -> Result<wovyr_common::fs::FileLock, ApiError> {
    let dir = plugins_dir().ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "no home directory for plugin store",
        )
    })?;
    wovyr_common::fs::FileLock::acquire(&dir).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            format!("lock plugin store: {e}"),
        )
    })
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
/// `~/.wovyr/plugins/keyless.json` (`{"root": …, "policy": …}`), shared by the plugin
/// engine and the marketplace registry. Absent ⇒ keyless disabled (publisher-key
/// trust only).
#[derive(serde::Deserialize)]
pub(crate) struct KeylessConfig {
    pub root: wovyr_plugin::KeylessRoot,
    pub policy: wovyr_plugin::IdentityPolicy,
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
        .and_then(|_| wovyr_common::fs::atomic_write(dir.join("catalog.json"), bytes))
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
        .and_then(|_| wovyr_common::fs::atomic_write(dir.join("trust.json"), bytes))
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
fn run_capability_runtime(vault: &wovyr_secrets::Vault) -> Arc<dyn CapabilityRuntime> {
    #[cfg(feature = "plugin-wasi")]
    if let Ok(rt) = wovyr_plugin::WasiCapabilityRuntime::new() {
        return Arc::new(rt.with_secrets(vault.clone()));
    }
    let _ = vault;
    Arc::new(NotLoadedRuntime)
}

/// Register every **enabled** plugin's tool capabilities from the durable catalog into
/// `registry`, routing execution through a [secret-aware](run_capability_runtime) runtime
/// so an agent/workflow run can call them with their tenant-scoped secrets injected.
/// Best-effort: a missing or corrupt catalog registers nothing (the server still starts).
pub(crate) fn register_enabled_tools(registry: &mut ToolRegistry, vault: &wovyr_secrets::Vault) {
    let trust = load_trust().unwrap_or_else(|_| TrustStore::new());
    let catalog = load_catalog().unwrap_or_default();
    register_catalog(registry, catalog, trust, staging_dir(), vault);
}

/// Register the enabled plugins of `catalog` into `registry` through the run runtime.
/// Split out from [`register_enabled_tools`] (which sources the catalog from disk) so the
/// registration path is unit-testable without touching `~/.wovyr/plugins`.
fn register_catalog(
    registry: &mut ToolRegistry,
    catalog: Vec<InstalledPlugin>,
    trust: TrustStore,
    staging: Option<PathBuf>,
    vault: &wovyr_secrets::Vault,
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
        // `PluginState` already derives a `snake_case` Serialize — this used to
        // re-derive the same two strings by hand (RM-GA-P4 API-702).
        "state": p.state,
        "permissions": p.manifest.permissions,
        "granted": p.granted_permissions,
        "capabilities": caps,
        "platform_api": p.manifest.compatibility.platform_api,
    })
}

/// `GET /api/v1/plugins` — the installed plugin catalog, cursor-paginated
/// (overview §6, RM-GA-P4 API-701).
#[utoipa::path(
    get,
    path = "/api/v1/plugins",
    tag = "plugins",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "The installed plugin catalog.")),
)]
pub(crate) async fn list_plugins(Query(page): Query<PageQuery>) -> Result<Json<Value>, ApiError> {
    let catalog = load_catalog()?;
    let items: Vec<Value> = catalog.iter().map(plugin_json).collect();
    Ok(Json(paginate(items, &page.page())))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct PluginRef {
    id: String,
}

/// `POST /api/v1/plugins:enable` — enable a plugin (routes its capabilities live).
///
/// `plugins:admin`-gated ([RM-GA-P1 SEC-103](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
/// an enabled tool runs inside *every* tenant's agent runs with tenant-scoped
/// secrets injected, so this is a platform-admin-tier action, not a per-tenant one.
#[utoipa::path(
    post,
    path = "/api/v1/plugins:enable",
    tag = "plugins",
    request_body = PluginRef,
    responses(
        (status = 200, description = "Plugin enabled."),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn enable_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PluginRef>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let _lock = acquire_lock()?;
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.enable(&req.id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "plugin.enable",
        "plugin",
        &req.id,
    );
    Ok(Json(json!({ "id": req.id, "state": "enabled" })))
}

/// `POST /api/v1/plugins:disable` — disable a plugin (withdraws its capabilities).
/// `plugins:admin`-gated (SEC-103).
#[utoipa::path(
    post,
    path = "/api/v1/plugins:disable",
    tag = "plugins",
    request_body = PluginRef,
    responses(
        (status = 200, description = "Plugin disabled."),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn disable_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PluginRef>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let _lock = acquire_lock()?;
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.disable(&req.id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "plugin.disable",
        "plugin",
        &req.id,
    );
    Ok(Json(json!({ "id": req.id, "state": "disabled" })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct InstallReq {
    /// Base64-encoded `.wovyrpkg` file contents.
    wovyrpkg: String,
    #[serde(default)]
    grants: Vec<String>,
}

/// `POST /api/v1/plugins:install` — install a plugin from a base64-encoded `.wovyrpkg`.
///
/// The package must carry a valid ed25519 signature from a trusted publisher
/// (`POST /api/v1/plugins:trust` registers publishers). On success the plugin is
/// installed in the *disabled* state; call `:enable` to activate it. `plugins:admin`-
/// gated (SEC-103).
#[utoipa::path(
    post,
    path = "/api/v1/plugins:install",
    tag = "plugins",
    request_body = InstallReq,
    responses(
        (status = 200, description = "Plugin installed (disabled)."),
        (status = 400, description = "Invalid package or unsigned/untrusted publisher.", body = crate::openapi::ApiErrorBody),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn install_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<InstallReq>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let bytes = base64_decode(&req.wovyrpkg)?;
    let package = Package::from_wovyrpkg(&bytes)?;
    let installed = install_package(&package, &req.grants)?;
    let id = installed["id"].as_str().unwrap_or_default();
    crate::audit::audit(&state, &headers, &tenant, "plugin.install", "plugin", id);
    Ok(Json(installed))
}

/// Install a verified [`Package`] into the durable catalog (disabled), returning its
/// catalog JSON. Shared by the HTTP install route and the marketplace install bridge.
pub(crate) fn install_package(package: &Package, grants: &[String]) -> Result<Value, ApiError> {
    let _lock = acquire_lock()?;
    let mut engine = engine()?;
    let installed = engine.install(package, grants)?;
    let resp = plugin_json(installed);
    save_catalog(&engine.catalog())?;
    Ok(resp)
}

/// `POST /api/v1/plugins:upgrade` — upgrade an installed plugin to a new version.
///
/// Retains the prior version for rollback. Any new permissions beyond what was
/// previously granted must be listed in `grants`. `plugins:admin`-gated (SEC-103).
#[utoipa::path(
    post,
    path = "/api/v1/plugins:upgrade",
    tag = "plugins",
    request_body = InstallReq,
    responses(
        (status = 200, description = "Plugin upgraded (prior version retained for rollback)."),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn upgrade_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<InstallReq>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let bytes = base64_decode(&req.wovyrpkg)?;
    let package = Package::from_wovyrpkg(&bytes)?;
    let _lock = acquire_lock()?;
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.upgrade(&package, &req.grants, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    let id = package.manifest()?.qualified_id();
    crate::audit::audit(&state, &headers, &tenant, "plugin.upgrade", "plugin", &id);
    Ok(Json(json!({ "id": id, "status": "upgraded" })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct RollbackReq {
    id: String,
}

/// `POST /api/v1/plugins:rollback` — revert a plugin to its retained prior version.
/// `plugins:admin`-gated (SEC-103).
#[utoipa::path(
    post,
    path = "/api/v1/plugins:rollback",
    tag = "plugins",
    request_body = RollbackReq,
    responses(
        (status = 200, description = "Plugin rolled back."),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn rollback_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RollbackReq>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let _lock = acquire_lock()?;
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.rollback(&req.id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "plugin.rollback",
        "plugin",
        &req.id,
    );
    Ok(Json(json!({ "id": req.id, "status": "rolled_back" })))
}

/// `DELETE /api/v1/plugins/{id}` — uninstall a plugin.
///
/// `id` is URL-encoded `publisher/name` (e.g. `acme%2Fmy-plugin`). `plugins:admin`-
/// gated (SEC-103).
#[utoipa::path(
    delete,
    path = "/api/v1/plugins/{id}",
    tag = "plugins",
    params(("id" = String, Path, description = "URL-encoded `publisher/name`.")),
    responses(
        (status = 200, description = "Plugin uninstalled."),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn uninstall_plugin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let _lock = acquire_lock()?;
    let mut engine = engine()?;
    let mut scratch = ToolRegistry::new();
    engine.uninstall(&id, &mut scratch)?;
    save_catalog(&engine.catalog())?;
    crate::audit::audit(&state, &headers, &tenant, "plugin.uninstall", "plugin", &id);
    Ok(Json(json!({ "id": id, "status": "uninstalled" })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct TrustReq {
    publisher: String,
    /// Hex-encoded ed25519 public key (32 bytes = 64 hex chars).
    public_key_hex: String,
}

/// `POST /api/v1/plugins:trust` — register a publisher's ed25519 public key.
///
/// After this, packages signed by that publisher can be installed. `plugins:admin`-
/// gated (SEC-103): trusting a publisher is the root of the plugin supply chain.
#[utoipa::path(
    post,
    path = "/api/v1/plugins:trust",
    tag = "plugins",
    request_body = TrustReq,
    responses(
        (status = 200, description = "Publisher trusted."),
        (status = 403, description = "Caller lacks plugins:admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn trust_publisher(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TrustReq>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "plugins:admin")?;
    let key_bytes = hex::decode(&req.public_key_hex).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "public_key_hex must be valid hex (64 chars for a 32-byte ed25519 key)",
        )
    })?;
    let _lock = acquire_lock()?;
    let mut trust = load_trust()?;
    trust.trust(req.publisher.clone(), key_bytes);
    save_trust(&trust)?;
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "plugin.trust",
        "publisher",
        &req.publisher,
    );
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
                "wovyrpkg must be valid base64-encoded bytes",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wovyr_plugin::{PluginManifest, PluginState};
    use wovyr_secrets::{InMemorySecretStore, Vault};

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
apiVersion: plugin.wovyr.io/v1
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

    // --- RM-GA-P1 SEC-103: plugin lifecycle routes are RBAC-gated -------------------

    use axum::http::{Request, StatusCode as HttpStatus};
    use tower::ServiceExt;

    async fn call(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        principal: &str,
        body: Value,
    ) -> HttpStatus {
        let resp = crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("x-wovyr-principal", principal)
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        resp.status()
    }

    /// An anonymous caller can no longer trust their own key, install, or enable a
    /// plugin (PP-03) — every mutating route requires `plugins:admin`, held here only
    /// by an org-admin membership (deliberately not `WOVYR_PLATFORM_ADMINS`, a
    /// process-global env var that would race against this crate's other tests).
    #[tokio::test]
    async fn plugin_lifecycle_routes_require_plugins_admin() {
        use wovyr_tenancy::{
            InMemoryTenancyStore, MemberScope, Membership, Organization, Role, TenancyStore,
        };

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org = tenancy
            .create_org(Organization::new("default", "Plugins Admin Co"))
            .unwrap();
        tenancy
            .add_membership(Membership {
                user: "admin".to_string(),
                role: Role::OrgAdmin,
                scope: MemberScope::Organization(org.id.clone()),
            })
            .unwrap();
        let state = Arc::new(crate::AppState::from_env().await.with_tenancy(tenancy));

        // A non-admin principal cannot trust a publisher key. (Deliberately not
        // exercised for the org admin here too: unlike enable/disable/rollback on an
        // unknown id, `:trust` writes unconditionally — this test must not touch the
        // real `~/.wovyr/plugins/trust.json` on whatever machine runs it.)
        let status = call(
            &state,
            "POST",
            "/api/v1/plugins:trust",
            "mallory",
            json!({ "publisher": "acme", "public_key_hex": "00".repeat(32) }),
        )
        .await;
        assert_eq!(status, HttpStatus::FORBIDDEN);

        // Same gating for enable/disable/rollback: a non-admin is refused before
        // authz even considers whether the (unknown) id exists — a 403, not a 404.
        // Safe for the admin side too: the engine errors out on an unknown id via
        // `?`, before ever reaching `save_catalog`, so nothing is persisted.
        for (method, uri) in [
            ("POST", "/api/v1/plugins:enable"),
            ("POST", "/api/v1/plugins:disable"),
            ("POST", "/api/v1/plugins:rollback"),
        ] {
            let status = call(&state, method, uri, "mallory", json!({ "id": "acme/demo" })).await;
            assert_eq!(status, HttpStatus::FORBIDDEN, "{method} {uri}");
            let status = call(&state, method, uri, "admin", json!({ "id": "acme/demo" })).await;
            assert_ne!(status, HttpStatus::FORBIDDEN, "{method} {uri}");
            assert_ne!(status, HttpStatus::UNAUTHORIZED, "{method} {uri}");
        }
        let status = call(
            &state,
            "DELETE",
            "/api/v1/plugins/acme%2Fdemo",
            "mallory",
            Value::Null,
        )
        .await;
        assert_eq!(status, HttpStatus::FORBIDDEN);
        let status = call(
            &state,
            "DELETE",
            "/api/v1/plugins/acme%2Fdemo",
            "admin",
            Value::Null,
        )
        .await;
        assert_ne!(status, HttpStatus::FORBIDDEN);
        assert_ne!(status, HttpStatus::UNAUTHORIZED);
    }
}
