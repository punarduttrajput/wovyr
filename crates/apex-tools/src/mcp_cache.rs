//! A per-connection live-client cache (RM-MCX-P1-106, PRD-006).
//!
//! Every call through a persisted [`crate::McpConnection`] would otherwise
//! mean re-dialing it — for `Http`, a fresh TCP/TLS handshake per call; for
//! `Stdio`, spawning a brand-new OS process per call. [`McpClientCache`]
//! keeps a bounded set of live [`crate::McpClient`]s warm, keyed by
//! `(tenant, connection name)`, reused across calls within an idle window
//! and evicted (dropped — `Stdio`'s `kill_on_drop` reaps the process) once
//! stale.
//!
//! Eviction is **lazy**, checked on the next access rather than via a
//! background task: simpler, and avoids a reaper task's own lifecycle
//! (start/stop, panics) for what is, at this scale, an infrequent check.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::mcp::McpClient;
use crate::mcp_store::McpConnection;
use crate::tool::ToolError;
use apex_secrets::Vault;

/// Default idle window before a cached client is dropped and a fresh one
/// dialed on next use — generous enough that a burst of calls in one agent
/// run reuses the same client, short enough that a long-idle connection
/// doesn't hold a spawned process open indefinitely.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct CachedEntry {
    client: Arc<McpClient>,
    last_used: Instant,
}

/// A bounded, tenant-scoped cache of live [`McpClient`]s over persisted
/// connections. One instance is meant to be shared (behind an `Arc`) across
/// an entire server process.
pub struct McpClientCache {
    entries: Mutex<HashMap<(String, String), CachedEntry>>,
    idle_timeout: Duration,
}

impl Default for McpClientCache {
    fn default() -> Self {
        Self::new(DEFAULT_IDLE_TIMEOUT)
    }
}

impl McpClientCache {
    /// A cache evicting entries idle longer than `idle_timeout`.
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            idle_timeout,
        }
    }

    /// Get a live client for `connection` under `tenant`, reusing a cached
    /// one if it's still within the idle window, otherwise dialing a fresh
    /// one (via [`McpConnection::connect`]) and caching it.
    pub async fn get_or_connect(
        &self,
        tenant: &str,
        connection: &McpConnection,
        vault: Option<&Vault>,
    ) -> Result<Arc<McpClient>, ToolError> {
        let key = (tenant.to_string(), connection.name.clone());
        {
            let mut entries = self.entries.lock().await;
            if let Some(entry) = entries.get(&key) {
                if entry.last_used.elapsed() < self.idle_timeout {
                    let client = entry.client.clone();
                    entries.get_mut(&key).expect("just checked").last_used = Instant::now();
                    return Ok(client);
                }
                // Stale — drop it (releasing a Stdio connection's spawned
                // process via kill_on_drop) before dialing a replacement.
                entries.remove(&key);
            }
        }

        let client = Arc::new(connection.connect(tenant, vault).await?);
        let mut entries = self.entries.lock().await;
        entries.insert(
            key,
            CachedEntry {
                client: client.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(client)
    }

    /// Resolve an agent's declared `spec.mcp_servers` allow-list (PRD-006,
    /// RM-MCX-P2-201) into live, registry-ready tools: each connection name is
    /// looked up in `store` for `tenant`, dialed (or reused from this cache),
    /// and its currently-served tools registered into `registry`. Returns the
    /// registered tool ids so the caller can extend the agent's advertised
    /// `spec.tools` with them — `run_agent`'s own tool-resolution step only
    /// advertises ids already listed in `spec.tools`, and an MCP server's
    /// tools are discovered live, not knowable when the manifest was authored.
    ///
    /// Fails closed on an unknown connection name: an agent naming a
    /// connection expects it to exist, so a name matching nothing is a
    /// configuration error, not a silent no-op (mirrors `resolve_tools`'s own
    /// fail-closed stance on an unknown *tool* id).
    pub async fn resolve_agent_mcp_tools(
        &self,
        store: &crate::mcp_store::McpConnectionStore,
        vault: Option<&Vault>,
        tenant: &str,
        connection_names: &[String],
        registry: &mut crate::ToolRegistry,
    ) -> Result<Vec<String>, ToolError> {
        let mut ids = Vec::new();
        for name in connection_names {
            let connection = store
                .get(tenant, name)
                .map_err(|e| ToolError::Internal(format!("MCP connection store: {e}")))?
                .ok_or_else(|| {
                    ToolError::Validation(format!(
                        "no configured MCP connection named `{name}` for this tenant"
                    ))
                })?;
            let client = self.get_or_connect(tenant, &connection, vault).await?;
            ids.extend(client.register_into(registry).await?);
        }
        Ok(ids)
    }

    /// Evict a connection's cached client immediately — used when a
    /// connection is edited or deleted (MCX-102), so a revoked/changed
    /// connection takes effect on the very next call rather than waiting out
    /// the idle window.
    pub async fn invalidate(&self, tenant: &str, name: &str) {
        self.entries
            .lock()
            .await
            .remove(&(tenant.to_string(), name.to_string()));
    }

    /// Number of currently-cached (not necessarily still-fresh) entries —
    /// for tests and operational visibility.
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Whether the cache holds no entries.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_store::McpTransportConfig;

    fn conn(name: &str, command: &str) -> McpConnection {
        McpConnection {
            name: name.to_string(),
            transport: McpTransportConfig::Stdio {
                command: command.to_string(),
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
    async fn a_second_get_within_the_idle_window_reuses_the_same_client() {
        let cache = McpClientCache::new(Duration::from_secs(60));
        let c = conn("fs", "node");
        let a = cache.get_or_connect("acme", &c, None).await.unwrap();
        let b = cache.get_or_connect("acme", &c, None).await.unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "expected the cached client to be reused"
        );
        assert_eq!(cache.len().await, 1);
    }

    #[tokio::test]
    async fn different_tenants_never_share_a_cached_client_for_the_same_connection_name() {
        let cache = McpClientCache::new(Duration::from_secs(60));
        let c = conn("fs", "node");
        let a = cache.get_or_connect("acme", &c, None).await.unwrap();
        let b = cache
            .get_or_connect("other-tenant", &c, None)
            .await
            .unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "different tenants must never reuse each other's cached client"
        );
        assert_eq!(cache.len().await, 2);
    }

    #[tokio::test]
    async fn an_expired_entry_is_redialed_not_reused() {
        // A near-zero idle timeout means the second call always finds the
        // first entry already stale.
        let cache = McpClientCache::new(Duration::from_millis(1));
        let c = conn("fs", "node");
        let a = cache.get_or_connect("acme", &c, None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let b = cache.get_or_connect("acme", &c, None).await.unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "an expired entry must be redialed, not reused"
        );
        // Still exactly one live entry — the stale one was replaced, not appended.
        assert_eq!(cache.len().await, 1);
    }

    #[tokio::test]
    async fn invalidate_forces_a_fresh_dial_on_the_next_call() {
        let cache = McpClientCache::new(Duration::from_secs(60));
        let c = conn("fs", "node");
        let a = cache.get_or_connect("acme", &c, None).await.unwrap();
        cache.invalidate("acme", "fs").await;
        assert!(cache.is_empty().await);
        let b = cache.get_or_connect("acme", &c, None).await.unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "invalidate must force a fresh dial even within the idle window"
        );
    }

    // --- RM-MCX-P2-201: agent manifest `spec.mcp_servers` resolution --------

    /// A connection whose real spawned server also answers `tools/list` with
    /// one tool — what `resolve_agent_mcp_tools` needs to actually have
    /// something to register.
    fn conn_with_one_tool(name: &str) -> McpConnection {
        McpConnection {
            name: name.to_string(),
            transport: McpTransportConfig::Stdio {
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
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { tools: [{ name: 'search_docs', description: 'search' }] } }) + '\n');
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

    fn store_with(
        dir_label: &str,
        tenant: &str,
        connections: &[McpConnection],
    ) -> crate::mcp_store::McpConnectionStore {
        let dir = std::env::temp_dir().join(format!(
            "apex_mcp_resolve_test_{dir_label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = crate::mcp_store::McpConnectionStore::new(&dir).unwrap();
        for c in connections {
            store.put(tenant, c.clone()).unwrap();
        }
        store
    }

    #[tokio::test]
    async fn resolve_agent_mcp_tools_registers_the_connections_currently_served_tools() {
        let store = store_with("happy", "acme", &[conn_with_one_tool("docs")]);
        let cache = McpClientCache::default();
        let mut registry = crate::ToolRegistry::new();

        let ids = cache
            .resolve_agent_mcp_tools(&store, None, "acme", &["docs".to_string()], &mut registry)
            .await
            .unwrap();

        assert_eq!(ids, vec!["mcp__docs__search_docs".to_string()]);
        assert!(registry.contains("mcp__docs__search_docs"));
    }

    #[tokio::test]
    async fn resolve_agent_mcp_tools_fails_closed_on_an_unknown_connection_name() {
        let store = store_with("unknown", "acme", &[]);
        let cache = McpClientCache::default();
        let mut registry = crate::ToolRegistry::new();

        let err = cache
            .resolve_agent_mcp_tools(
                &store,
                None,
                "acme",
                &["does-not-exist".to_string()],
                &mut registry,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)), "{err:?}");
        assert!(registry.ids().is_empty());
    }

    #[tokio::test]
    async fn resolve_agent_mcp_tools_is_scoped_to_the_calling_tenant() {
        let store = store_with("tenant_scope", "acme", &[conn_with_one_tool("docs")]);
        let cache = McpClientCache::default();
        let mut registry = crate::ToolRegistry::new();

        // `docs` exists for `acme`, not for `other-tenant`.
        let err = cache
            .resolve_agent_mcp_tools(
                &store,
                None,
                "other-tenant",
                &["docs".to_string()],
                &mut registry,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)), "{err:?}");
    }
}
