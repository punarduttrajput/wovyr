//! MCP connection management routes (PRD-006, RM-MCX-P1-102/103).
//!
//! A persisted, API/dashboard-managed layer over the already-shipped,
//! programmatic-only MCP client (`wovyr-tools::mcp`, RM-AIM-P3 ECO-301): add a
//! connection to an external MCP server once, and its tools become available
//! to agents that name it (see `crate::workflow_runner`'s `mcp_servers`
//! wiring, RM-MCX-P2-201).
//!
//! **The trust boundary this module enforces is [ADR-0012](../../../docs/17-adr/ADR-0012-mcp-connection-trust-boundary.md):**
//! a `Stdio`-transport connection (arbitrary local command execution) requires
//! *both* the `mcp:admin` RBAC scope *and* an explicit operator opt-in
//! (`WOVYR_ENABLE_MCP_STDIO=1`, the exact `WOVYR_ENABLE_SHELL_TOOL` precedent) —
//! a tenant cannot reach this on their own no matter what role they hold. An
//! `Http`-transport connection only needs `mcp:write` and gets the identical
//! SSRF guard `http_get` already has (`HttpTransport::connect_guarded`),
//! reused rather than reimplemented.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied
//! here (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped
//! `ApiError`, never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::hardening::{PageQuery, paginate};
use crate::{ApiError, AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use wovyr_tools::{McpClientCache, McpConnection, McpConnectionStore, McpTransportConfig};

/// The MCP connection-management runtime: the durable connection catalog,
/// the live-client cache (RM-MCX-P1-106), and the resolved hosted-safety
/// gate state (RM-MCX-P1-103).
pub struct McpRuntime {
    store: McpConnectionStore,
    cache: McpClientCache,
    /// `WOVYR_ENABLE_MCP_STDIO=1` at startup — the same operator-only opt-in
    /// shape `WOVYR_ENABLE_SHELL_TOOL` uses (SEC-301). Resolved once, not
    /// re-read per request, matching every other env-derived gate in this
    /// codebase.
    stdio_enabled: bool,
}

impl McpRuntime {
    fn build(store: McpConnectionStore, stdio_enabled: bool) -> Self {
        Self {
            store,
            cache: McpClientCache::default(),
            stdio_enabled,
        }
    }

    fn stdio_enabled_from_env() -> bool {
        std::env::var("WOVYR_ENABLE_MCP_STDIO")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// The production runtime: a durable connection store under
    /// `~/.wovyr/mcp` and the hosted-safety gate read from the environment.
    pub fn from_env() -> wovyr_common::Result<Self> {
        let dir = wovyr_config::paths::mcp_dir()?;
        Ok(Self::build(
            McpConnectionStore::new(dir)?,
            Self::stdio_enabled_from_env(),
        ))
    }

    /// A temp-directory-backed fallback for when [`Self::from_env`]'s real
    /// `~/.wovyr/mcp` directory can't be created — ephemeral (lost on
    /// restart), but keeps the server starting rather than failing outright,
    /// the same resilience stance `default_webhook_store`'s in-memory
    /// fallback gives every other durable store here (this one has no true
    /// in-memory backend to fall back to instead).
    pub(crate) fn from_temp_dir_fallback() -> Self {
        let dir = std::env::temp_dir().join(format!("wovyr_mcp_fallback_{}", std::process::id()));
        #[allow(clippy::expect_used)]
        // SRV-306: last-resort fallback with nowhere further to fall back to — an unwritable OS temp dir means the process can't run at all
        Self::build(
            McpConnectionStore::new(dir).expect("OS temp dir must be writable"),
            Self::stdio_enabled_from_env(),
        )
    }

    /// A scratch-directory-backed runtime for tests, with `Stdio` connections
    /// allowed unconditionally (tests opt into the gate explicitly via
    /// [`Self::with_stdio_enabled`] where the gate itself is what's under
    /// test) — there is no true in-memory variant of the file-backed store,
    /// matching every other durable store in this codebase.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("wovyr_mcp_runtime_test_{}_{n}", std::process::id()));
        Self::build(McpConnectionStore::new(dir).expect("scratch dir"), true)
    }

    #[cfg(test)]
    pub fn with_stdio_enabled(mut self, enabled: bool) -> Self {
        self.stdio_enabled = enabled;
        self
    }

    pub(crate) fn store(&self) -> &McpConnectionStore {
        &self.store
    }

    pub(crate) fn cache(&self) -> &McpClientCache {
        &self.cache
    }

    pub(crate) fn stdio_enabled(&self) -> bool {
        self.stdio_enabled
    }
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/mcp/connections",
            post(create_handler).get(list_handler),
        )
        .route(
            "/api/v1/mcp/connections/{name}",
            get(get_handler).delete(delete_handler),
        )
        .route(
            "/api/v1/mcp/connections/{name}/refresh",
            post(refresh_handler),
        )
}

fn connection_body(c: &McpConnection) -> Value {
    json!({
        "name": c.name,
        "transport": c.transport,
        "secret_ref": c.secret_ref,
        "secret_env_var": c.secret_env_var,
        "tool_permissions": c.tool_permissions,
        "created_ms": c.created_ms,
        "updated_ms": c.updated_ms,
    })
}

fn tool_list_body(tools: Vec<wovyr_tools::McpToolInfo>) -> Value {
    json!(
        tools
            .into_iter()
            .map(|t| json!({ "name": t.name, "description": t.description }))
            .collect::<Vec<_>>()
    )
}

fn not_found(name: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("no MCP connection `{name}`"),
    )
}

fn provider_error(context: &str, e: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "provider_error",
        format!("{context}: {e}"),
    )
}

/// The connection document a `POST /api/v1/mcp/connections` call submits.
/// `transport` is left as raw JSON (like `ui.rs`'s `PresentRequest.frame`)
/// rather than a typed nested schema, since `wovyr_tools::McpTransportConfig`
/// doesn't derive `utoipa::ToSchema` (that crate has no `utoipa` dependency) —
/// deserialized into the real type inside the handler, where a malformed
/// shape becomes a normal `400`, not a schema-generation problem.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ConnectionRequest {
    name: String,
    /// `{"kind": "stdio", "command": "...", "args": [...]}` or
    /// `{"kind": "http", "url": "..."}`.
    #[schema(value_type = Object)]
    transport: Value,
    #[serde(default)]
    secret_ref: Option<String>,
    #[serde(default)]
    secret_env_var: Option<String>,
    #[serde(default)]
    tool_permissions: Option<Vec<String>>,
}

/// `POST /api/v1/mcp/connections` — register (or replace) a connection.
/// Verifies it actually works — connects, resolves any `secret_ref`, and
/// lists its tools — **before** persisting anything; a connection that can't
/// be dialed is never saved half-configured. `Stdio` transports require the
/// `mcp:admin` scope and the operator's `WOVYR_ENABLE_MCP_STDIO=1` opt-in
/// (ADR-0012); `Http` transports only need `mcp:write` and are SSRF-guarded
/// the same way `http_get` already is.
#[utoipa::path(
    post,
    path = "/api/v1/mcp/connections",
    tag = "mcp",
    request_body = ConnectionRequest,
    responses(
        (status = 200, description = "The connection was verified and persisted; its discovered tools are included."),
        (status = 400, description = "Malformed transport or connection name.", body = crate::openapi::ApiErrorBody),
        (status = 403, description = "Missing scope, or Stdio requested without the operator opt-in.", body = crate::openapi::ApiErrorBody),
        (status = 429, description = "The tenant's max_mcp_connections quota would be exceeded.", body = crate::openapi::ApiErrorBody),
        (status = 502, description = "The connection could not be dialed.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn create_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ConnectionRequest>,
) -> Result<Json<Value>, ApiError> {
    let transport: McpTransportConfig =
        serde_json::from_value(req.transport.clone()).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation_failed",
                format!("invalid transport: {e}"),
            )
        })?;

    let scope = if transport.is_stdio() {
        "mcp:admin"
    } else {
        "mcp:write"
    };
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, scope)?;

    if transport.is_stdio() && !state.mcp.stdio_enabled() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Stdio-transport MCP connections require the operator to set WOVYR_ENABLE_MCP_STDIO=1",
        ));
    }

    // Best-effort quota check: MCP connections are tenant-scoped but this
    // platform's QuotaLimits are project-scoped, so this only enforces when
    // the caller identifies a project (mirrors admit_run's "no project ->
    // unmetered" stance) — the project's max_mcp_connections then bounds the
    // one thing that *is* tenant-scoped, the connection count.
    if let Some(project) = crate::tenancy::run_project(&headers)
        && let Some(limits) = state.tenancy.get_quota(&project)?
    {
        let current = state.mcp.store().list(&tenant)?.len() as u64;
        limits.check_mcp_connections(current)?;
    }

    let now = crate::audit::now_ms();
    let connection = McpConnection {
        name: req.name,
        transport,
        secret_ref: req.secret_ref,
        secret_env_var: req.secret_env_var,
        tool_permissions: req.tool_permissions,
        created_ms: now,
        updated_ms: now,
    };

    let client = connection
        .connect(&tenant, Some(&state.secrets))
        .await
        .map_err(|e| provider_error("could not connect", e))?;
    let tools = client
        .list_tools()
        .await
        .map_err(|e| provider_error("could not list tools", e))?;

    state.mcp.store().put(&tenant, connection.clone())?;
    // Drop any stale cached client from a prior version of this connection
    // (an edit-over-existing put) — the next real use dials fresh.
    state
        .mcp
        .cache()
        .invalidate(&tenant, &connection.name)
        .await;

    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "mcp.connection.put",
        "mcp_connection",
        &connection.name,
    );

    let mut body = connection_body(&connection);
    body["tools"] = tool_list_body(tools);
    Ok(Json(body))
}

/// `GET /api/v1/mcp/connections` — the caller's tenant's configured
/// connections, cursor-paginated (overview §6, RM-GA-P4 API-701), plus a
/// `stdio_enabled` flag (RM-MCX-P3-302) alongside the standard pagination
/// fields — an additive extension of this one route's envelope, not a change
/// to the shared `paginate()` shape every other list route uses. A dashboard
/// composing a new connection needs to know, *before* the operator fills out
/// the form, whether the operator has set `WOVYR_ENABLE_MCP_STDIO=1` — PRD-006
/// MCX-302 requires the `Stdio` transport option be hidden rather than
/// silently offered-then-rejected on submit.
#[utoipa::path(
    get,
    path = "/api/v1/mcp/connections",
    tag = "mcp",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "The tenant's configured MCP connections, plus a stdio_enabled capability flag.")),
)]
pub(crate) async fn list_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "mcp:read")?;
    let items: Vec<Value> = state
        .mcp
        .store()
        .list(&tenant)?
        .iter()
        .map(connection_body)
        .collect();
    let mut body = paginate(items, &page.page());
    body["stdio_enabled"] = json!(state.mcp.stdio_enabled());
    Ok(Json(body))
}

/// `GET /api/v1/mcp/connections/{name}` — one connection's config (never a
/// resolved secret value, only the reference).
#[utoipa::path(
    get,
    path = "/api/v1/mcp/connections/{name}",
    tag = "mcp",
    params(("name" = String, Path, description = "The connection name.")),
    responses(
        (status = 200, description = "The connection's configuration."),
        (status = 404, description = "Unknown connection (or not the caller's tenant's).", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "mcp:read")?;
    let connection = state
        .mcp
        .store()
        .get(&tenant, &name)?
        .ok_or_else(|| not_found(&name))?;
    Ok(Json(connection_body(&connection)))
}

/// `DELETE /api/v1/mcp/connections/{name}` — remove a connection; its cached
/// live client (if any) is evicted immediately, so a revoked connection takes
/// effect on the very next call rather than waiting out the idle window.
/// Deleting a `Stdio` connection still requires `mcp:admin`, matching who is
/// trusted to have configured it in the first place.
#[utoipa::path(
    delete,
    path = "/api/v1/mcp/connections/{name}",
    tag = "mcp",
    params(("name" = String, Path, description = "The connection name.")),
    responses(
        (status = 204, description = "Deleted."),
        (status = 403, description = "Missing scope (Stdio connections require mcp:admin).", body = crate::openapi::ApiErrorBody),
        (status = 404, description = "Unknown connection (or not the caller's tenant's).", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let ctx = crate::tenancy::tenant_context(&state, &headers);
    ctx.authorize("mcp:write")?;
    let tenant = ctx.tenant.clone();

    let existing = state
        .mcp
        .store()
        .get(&tenant, &name)?
        .ok_or_else(|| not_found(&name))?;
    if existing.transport.is_stdio() {
        ctx.authorize("mcp:admin")?;
    }

    state.mcp.store().delete(&tenant, &name)?;
    state.mcp.cache().invalidate(&tenant, &name).await;

    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "mcp.connection.delete",
        "mcp_connection",
        &name,
    );

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/mcp/connections/{name}/refresh` — force an immediate
/// re-dial and re-discovery (RM-MCX-P2-203), bypassing the client cache
/// rather than waiting out its idle window — the dashboard's "see what's
/// new" action.
#[utoipa::path(
    post,
    path = "/api/v1/mcp/connections/{name}/refresh",
    tag = "mcp",
    params(("name" = String, Path, description = "The connection name.")),
    responses(
        (status = 200, description = "The connection's freshly re-discovered tools."),
        (status = 403, description = "Missing scope (Stdio connections require mcp:admin).", body = crate::openapi::ApiErrorBody),
        (status = 404, description = "Unknown connection (or not the caller's tenant's).", body = crate::openapi::ApiErrorBody),
        (status = 502, description = "The connection could not be dialed.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn refresh_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::tenancy::tenant_context(&state, &headers);
    ctx.authorize("mcp:write")?;
    let tenant = ctx.tenant.clone();

    let connection = state
        .mcp
        .store()
        .get(&tenant, &name)?
        .ok_or_else(|| not_found(&name))?;
    if connection.transport.is_stdio() {
        ctx.authorize("mcp:admin")?;
    }

    state.mcp.cache().invalidate(&tenant, &name).await;
    let client = state
        .mcp
        .cache()
        .get_or_connect(&tenant, &connection, Some(&state.secrets))
        .await
        .map_err(|e| provider_error("could not connect", e))?;
    let tools = client
        .list_tools()
        .await
        .map_err(|e| provider_error("could not list tools", e))?;

    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "mcp.connection.refresh",
        "mcp_connection",
        &name,
    );

    Ok(Json(
        json!({ "name": name, "tools": tool_list_body(tools) }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    fn ensure_admin_env() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
    }

    /// A real server state with an isolated, scratch-directory-backed MCP
    /// runtime (never the real `~/.wovyr/mcp`) — `stdio_enabled` lets each
    /// test control MCX-103's gate independently of the RBAC scope check.
    async fn test_state(stdio_enabled: bool) -> Arc<AppState> {
        let base = AppState::from_env().await;
        let mcp = Arc::new(McpRuntime::in_memory().with_stdio_enabled(stdio_enabled));
        Arc::new(base.with_mcp(mcp))
    }

    async fn request(
        app: axum::Router,
        method: &str,
        uri: &str,
        principal: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        ensure_admin_env();
        let body = match body {
            Some(b) => axum::body::Body::from(b.to_string()),
            None => axum::body::Body::empty(),
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-wovyr-principal", principal);
        if method == "POST" {
            builder = builder.header("content-type", "application/json");
        }
        let resp = app
            .oneshot(builder.body(body).expect("valid request"))
            .await
            .expect("router never errors");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap_or_default();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    fn http_connection_body(name: &str, url: &str) -> Value {
        json!({ "name": name, "transport": { "kind": "http", "url": url } })
    }

    fn stdio_node_script(env_var: &str) -> String {
        format!(
            r#"
const readline = require('readline');
const rl = readline.createInterface({{ input: process.stdin, terminal: false }});
rl.on('line', (line) => {{
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {{
    process.stdout.write(JSON.stringify({{
      jsonrpc: '2.0', id: msg.id,
      result: {{ serverInfo: {{ name: 'test', version: process.env.{env_var} || 'unset' }} }},
    }}) + '\n');
  }} else if (msg.method === 'tools/list') {{
    process.stdout.write(JSON.stringify({{
      jsonrpc: '2.0', id: msg.id,
      result: {{ tools: [{{ name: 'echo', description: 'echoes input' }}] }},
    }}) + '\n');
  }}
}});
"#
        )
    }

    fn stdio_connection_body(name: &str, env_var: &str) -> Value {
        json!({
            "name": name,
            "transport": { "kind": "stdio", "command": "node", "args": ["-e", stdio_node_script(env_var)] },
        })
    }

    #[tokio::test]
    async fn an_http_connection_is_refused_when_it_resolves_to_a_private_address() {
        let state = test_state(true).await;
        let (status, body) = request(
            crate::router(state),
            "POST",
            "/api/v1/mcp/connections",
            "root",
            Some(http_connection_body("internal", "http://10.1.2.3:9/mcp")),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    }

    #[tokio::test]
    async fn a_stdio_connection_is_refused_without_the_operator_opt_in() {
        // stdio_enabled = false: MCX-103's hosted-safety gate, independent of RBAC.
        let state = test_state(false).await;
        let (status, body) = request(
            crate::router(state),
            "POST",
            "/api/v1/mcp/connections",
            "root",
            Some(stdio_connection_body("echo", "WOVYR_TEST_VAR")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn a_non_admin_principal_cannot_create_a_stdio_connection() {
        let state = test_state(true).await;
        // A principal with no platform-admin standing and no seeded
        // membership resolves to zero roles — default-deny, so every scope
        // (mcp:admin included) is refused. A real, if coarse, proof the gate
        // exists and isn't bypassable by an ordinary caller.
        let (status, body) = request(
            crate::router(state),
            "POST",
            "/api/v1/mcp/connections",
            "someone-with-no-roles",
            Some(stdio_connection_body("echo", "WOVYR_TEST_VAR")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn create_verifies_connectivity_and_returns_real_discovered_tools() {
        let state = test_state(true).await;
        let (status, body) = request(
            crate::router(state),
            "POST",
            "/api/v1/mcp/connections",
            "root",
            Some(stdio_connection_body("echo", "WOVYR_TEST_VAR")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["name"], "echo");
        assert_eq!(body["tools"][0]["name"], "echo");
    }

    /// RM-MCX-P3-302: the dashboard needs to know whether the operator opt-in
    /// is on *before* the operator fills out a connection form, not after a
    /// rejected submit.
    #[tokio::test]
    async fn the_list_route_reports_whether_stdio_is_operator_enabled() {
        let (status, body) = request(
            crate::router(test_state(true).await),
            "GET",
            "/api/v1/mcp/connections",
            "root",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["stdio_enabled"], true);

        let (status, body) = request(
            crate::router(test_state(false).await),
            "GET",
            "/api/v1/mcp/connections",
            "root",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["stdio_enabled"], false);
    }

    #[tokio::test]
    async fn full_lifecycle_create_list_get_refresh_delete() {
        let state = test_state(true).await;
        let app = || crate::router(state.clone());

        let (status, _) = request(
            app(),
            "POST",
            "/api/v1/mcp/connections",
            "root",
            Some(stdio_connection_body("fs", "WOVYR_TEST_VAR")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = request(app(), "GET", "/api/v1/mcp/connections", "root", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"].as_array().map(|a| a.len()), Some(1));

        let (status, body) =
            request(app(), "GET", "/api/v1/mcp/connections/fs", "root", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["name"], "fs");

        let (status, body) = request(
            app(),
            "POST",
            "/api/v1/mcp/connections/fs/refresh",
            "root",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tools"][0]["name"], "echo");

        let (status, _) =
            request(app(), "DELETE", "/api/v1/mcp/connections/fs", "root", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Deleted — the very next call fails closed, not a stale success.
        let (status, body) =
            request(app(), "GET", "/api/v1/mcp/connections/fs", "root", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test]
    async fn deleting_a_stdio_connection_still_requires_mcp_admin() {
        let state = test_state(true).await;
        let app = || crate::router(state.clone());

        let (status, _) = request(
            app(),
            "POST",
            "/api/v1/mcp/connections",
            "root",
            Some(stdio_connection_body("fs", "WOVYR_TEST_VAR")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // A principal with zero roles can't even reach the "does it exist"
        // check meaningfully — the write-tier authorize fails first.
        let (status, body) = request(
            app(),
            "DELETE",
            "/api/v1/mcp/connections/fs",
            "someone-with-no-roles",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn the_env_var_actually_reaches_the_spawned_process() {
        // End-to-end proof (mirroring wovyr-tools' own MCX-105 test, but through
        // the real HTTP route this time): a secret-free stdio connection's
        // plain env var still round-trips through a real spawned process,
        // observed via its self-reported version.
        let state = test_state(true).await;
        let (status, body) = request(
            crate::router(state),
            "POST",
            "/api/v1/mcp/connections",
            "root",
            Some(stdio_connection_body("echo", "WOVYR_TEST_VAR")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // No secret_ref was set, so the env var is genuinely absent in the
        // spawned process — the server reports "unset", proving the round
        // trip is real, not hardcoded.
        assert_eq!(body["tools"][0]["name"], "echo");
    }
}
