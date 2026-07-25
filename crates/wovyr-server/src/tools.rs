//! Tool-discovery route: `GET /api/v1/tools`.
//!
//! Lists the run registry's tools — the built-ins **and** any enabled plugin
//! capabilities registered at startup — with id, description, and category, so UIs (the
//! dashboard's Agent Studio tool picker) can offer them without hardcoding names. Pure
//! discovery metadata (no values), so the built-in/plugin listing stays unauthenticated
//! like `/healthz` and `/metrics`.
//!
//! **MCX-202 (PRD-006):** when the caller can be authorized for `mcp:read` against a
//! tenant (`crate::tenancy::tenant_authorize`), the response also includes that
//! tenant's currently-configured MCP connections' live-discovered tools
//! (`mcp__<server>__<tool>`, MCX-106's client cache keeps repeated calls cheap) — the
//! same ids `spec.mcp_servers` resolution (RM-MCX-P2-201) registers into a run. An
//! unauthenticated/unauthorized caller still sees the built-in/plugin catalog exactly as
//! before; they just see no MCP-sourced entries, matching the platform's default-deny
//! stance without breaking this route's existing anonymous-friendly contract. A
//! connection that fails to dial is skipped rather than failing the whole listing — a
//! momentarily-unreachable MCP server shouldn't take down tool discovery for everything
//! else.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::AppState;
use crate::hardening::{PageQuery, paginate};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/tools", get(list_tools))
}

/// `GET /api/v1/tools` — the registered tool catalog (id + description +
/// category), cursor-paginated (overview §6, RM-GA-P4 API-701).
#[utoipa::path(
    get,
    path = "/api/v1/tools",
    tag = "tools",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "The registered tool catalog (built-ins + enabled plugin tools + the caller's tenant's MCP-sourced tools).")),
)]
pub(crate) async fn list_tools(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<PageQuery>,
) -> Json<Value> {
    let mut tools: Vec<Value> = state
        .registry
        .metadata()
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id,
                "description": m.description,
                "category": m.category,
                "permissions": m.permissions,
            })
        })
        .collect();

    if let Ok(tenant) = crate::tenancy::tenant_authorize(&state, &headers, "mcp:read") {
        tools.extend(mcp_tool_metadata(&state, &tenant).await);
    }

    Json(paginate(tools, &page.page()))
}

/// `tenant`'s currently-configured MCP connections' live-discovered tools, as the
/// same `id`/`description`/`category`/`permissions` shape a built-in tool reports.
/// A connection that fails to dial (unreachable server, gated `Stdio` without the
/// operator opt-in having ever been reachable to configure it, etc.) is silently
/// skipped — best-effort discovery, never a hard failure of the whole listing.
async fn mcp_tool_metadata(state: &AppState, tenant: &str) -> Vec<Value> {
    let Ok(connections) = state.mcp.store().list(tenant) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for connection in connections {
        let Ok(client) = state
            .mcp
            .cache()
            .get_or_connect(tenant, &connection, Some(&state.secrets))
            .await
        else {
            continue;
        };
        let Ok(discovered) = client.discover_tools().await else {
            continue;
        };
        out.extend(discovered.into_iter().map(|t| {
            let m = t.metadata();
            json!({
                "id": m.id,
                "description": m.description,
                "category": m.category,
                "permissions": m.permissions,
            })
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpRuntime;
    use crate::state::AppState;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn ensure_admin_env() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
    }

    /// A real server state with an isolated, scratch-directory-backed MCP
    /// runtime (never the real `~/.wovyr/mcp`).
    async fn test_state() -> Arc<AppState> {
        let base = AppState::from_env().await;
        let mcp = Arc::new(McpRuntime::in_memory());
        Arc::new(base.with_mcp(mcp))
    }

    async fn get(app: axum::Router, principal: &str, tenant: Option<&str>) -> (StatusCode, Value) {
        ensure_admin_env();
        let mut builder = Request::builder()
            .method("GET")
            .uri("/api/v1/tools")
            .header("x-wovyr-principal", principal);
        if let Some(t) = tenant {
            builder = builder.header("x-wovyr-tenant", t);
        }
        let resp = app
            .oneshot(
                builder
                    .body(axum::body::Body::empty())
                    .expect("valid request"),
            )
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

    fn ids_of(body: &Value) -> Vec<String> {
        body["data"]
            .as_array()
            .expect("data array")
            .iter()
            .map(|t| t["id"].as_str().expect("id").to_string())
            .collect()
    }

    /// A connection whose real spawned server answers `tools/list` with one
    /// tool named `echo` — enough to prove a discovered MCP tool's metadata
    /// makes it into this route's response.
    fn conn(name: &str) -> wovyr_tools::McpConnection {
        wovyr_tools::McpConnection {
            name: name.to_string(),
            transport: wovyr_tools::McpTransportConfig::Stdio {
                command: "node".to_string(),
                args: vec![
                    "-e".to_string(),
                    r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { serverInfo: { name: 'x', version: '1' } } }) + '\n');
  } else if (msg.method === 'tools/list') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { tools: [{ name: 'echo', description: 'echoes' }] } }) + '\n');
  }
});
"#
                    .to_string(),
                ],
            },
            secret_ref: None,
            secret_env_var: None,
            tool_permissions: None,
            created_ms: 1,
            updated_ms: 1,
        }
    }

    #[tokio::test]
    async fn lists_built_in_tools_with_no_tenant_context_at_all() {
        let state = test_state().await;
        let (status, body) = get(crate::router(state), "", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(ids_of(&body).contains(&"echo".to_string()));
    }

    /// RM-MCX-P2-202: an authorized caller's response includes the tenant's
    /// currently-configured MCP connections' live-discovered tools, alongside
    /// the built-ins.
    #[tokio::test]
    async fn includes_the_callers_tenants_configured_mcp_connections_tools() {
        let state = test_state().await;
        state.mcp.store().put("acme", conn("docs")).unwrap();

        let (status, body) = get(crate::router(state), "root", Some("acme")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids = ids_of(&body);
        assert!(ids.contains(&"mcp__docs__echo".to_string()), "{ids:?}");
        assert!(ids.contains(&"echo".to_string()), "built-ins still present");
    }

    /// A caller who can't be authorized for `mcp:read` in the asserted tenant
    /// (no membership, not a platform admin) still gets the built-in catalog —
    /// this route's existing anonymous-friendly contract is unchanged — but
    /// sees zero MCP-sourced entries, never another tenant's tool names.
    #[tokio::test]
    async fn an_unauthorized_caller_sees_the_built_ins_but_no_mcp_tools() {
        let state = test_state().await;
        state.mcp.store().put("acme", conn("docs")).unwrap();

        let (status, body) = get(crate::router(state), "someone-with-no-roles", Some("acme")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids = ids_of(&body);
        assert!(ids.contains(&"echo".to_string()));
        assert!(!ids.iter().any(|id| id.starts_with("mcp__")), "{ids:?}");
    }

    /// A connection configured for one tenant must never surface under a
    /// different tenant's listing, even for a fully-privileged caller.
    #[tokio::test]
    async fn a_different_tenants_mcp_connections_are_never_leaked() {
        let state = test_state().await;
        state.mcp.store().put("acme", conn("docs")).unwrap();

        let (status, body) = get(crate::router(state), "root", Some("other-tenant")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids = ids_of(&body);
        assert!(!ids.iter().any(|id| id.starts_with("mcp__")), "{ids:?}");
    }
}
