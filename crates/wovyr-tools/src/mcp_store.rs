//! Persisted MCP connection configuration (RM-MCX-P1-101, PRD-006 MCX-101).
//!
//! [`McpConnectionStore`] is the tenant-scoped, durable record of *configured*
//! MCP servers — distinct from [`crate::McpClient`] (`mcp.rs`), which is the
//! live wire connection. A [`McpConnection`] here is inert configuration;
//! nothing in this module spawns a process or dials a socket, and nothing
//! here enforces the `Stdio`-transport hosted-safety gate or the `mcp:admin`
//! RBAC tier — those are MCX-102/103, enforced by the API layer that will
//! call this store, exactly as `wovyr-secrets`'s `Vault` separates storage from
//! the access-control layer in front of it.
//!
//! Persistence follows the same shape every other durable store in this
//! workspace uses (`wovyr-tenancy`'s `FileTenancyStore` is the closest
//! relative): one JSON document per store, guarded by a process-local
//! `Mutex` **and** a cross-process [`wovyr_common::fs::FileLock`] spanning the
//! whole load→mutate→save cycle (RM-GA-P2 DUR-403), written via
//! [`wovyr_common::fs::atomic_write`] (DUR-401) so a crash never leaves a torn
//! file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use wovyr_common::{Error, Result};
use wovyr_secrets::{SecretAccess, SecretRef, Vault};

use crate::mcp::{HttpTransport, McpClient, StdioTransport, is_valid_mcp_server_name};
use crate::tool::ToolError;

/// How to reach an MCP server — configuration only, mirroring
/// [`crate::McpTransport`]'s two shipped transports with no live connection
/// attached. `Stdio`'s arbitrary local command execution is materially
/// higher-privilege than `Http`; see
/// [ADR-0012](../../../docs/17-adr/ADR-0012-mcp-connection-trust-boundary.md) —
/// this type only models the distinction, it does not enforce anything.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransportConfig {
    /// Spawn a local process (`StdioTransport::spawn(command, args)`).
    Stdio { command: String, args: Vec<String> },
    /// JSON-RPC POSTs to a streamable-HTTP endpoint (`HttpTransport::new(url)`).
    Http { url: String },
}

impl McpTransportConfig {
    /// Whether this is the higher-privilege `Stdio` transport — the fact
    /// the API layer's hosted-safety gate and `mcp:admin` RBAC tier
    /// (MCX-103, ADR-0012) key off.
    pub fn is_stdio(&self) -> bool {
        matches!(self, McpTransportConfig::Stdio { .. })
    }
}

/// One configured MCP server connection — inert configuration, not a live
/// client. `name` is exactly the identifier [`crate::McpClient::connect`]'s
/// `server` argument takes (it namespaces proxied tool ids `mcp__<name>__<tool>`
/// and the default `mcp:<name>` permission), so it is validated identically
/// via [`is_valid_mcp_server_name`] — this store must never persist a
/// connection whose name the client would then refuse to use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConnection {
    pub name: String,
    pub transport: McpTransportConfig,
    /// An optional credential for the server (an HTTP auth header, typically)
    /// — the wire form of a `SecretRef` (`secret://<namespace>/<name>`,
    /// validated via [`SecretRef::parse`] at write time), **never** an inline
    /// value (MCX-105). `wovyr_secrets::SecretRef` doesn't derive
    /// `Serialize`/`Deserialize` (it's constructed on the fly everywhere else
    /// in this workspace, never embedded in a document) — storing the
    /// canonical string form here, the same shape references already take in
    /// manifests/plugin permissions, avoids adding that derive just for this
    /// one caller. Resolving it into an actual value is the tool-call path's
    /// job, not this store's.
    #[serde(default)]
    pub secret_ref: Option<String>,
    /// `Stdio` only: the env var name the *spawned server itself* expects its
    /// credential as (e.g. `GITHUB_TOKEN`) — this is the admin's choice, not
    /// ours to invent, since only they know what the third-party server they
    /// are configuring actually reads. Required (fail-closed) whenever
    /// `secret_ref` is set on a `Stdio` connection; ignored for `Http` (which
    /// always injects as `Authorization: Bearer <value>`, MCX-105).
    #[serde(default)]
    pub secret_env_var: Option<String>,
    /// Overrides the default `["mcp:<name>"]` permission every tool this
    /// connection's server proxies would otherwise declare
    /// (`McpClient::with_tool_permissions`).
    #[serde(default)]
    pub tool_permissions: Option<Vec<String>>,
    /// Epoch milliseconds, stamped by the caller (the API layer) — this
    /// store reads no clock itself, the house determinism rule.
    pub created_ms: u64,
    pub updated_ms: u64,
}

impl McpConnection {
    /// Dial this connection for real: resolve `secret_ref` (if set) through
    /// `vault`, build the guarded transport (MCX-104's `connect_guarded` for
    /// `Http`, `spawn_with_env` for `Stdio` with the resolved credential
    /// injected under `secret_env_var`), and run the MCP `initialize`
    /// handshake — RM-MCX-P1-105, the tool-call path's half of secret
    /// resolution (the store itself never touches a real value, only the
    /// reference).
    ///
    /// `vault` is `None` for a tenant-less/local caller (mirrors
    /// `resolve_secret_env`'s "empty tenant injects nothing" stance) — a
    /// connection with `secret_ref` set then fails closed rather than
    /// silently connecting without its credential.
    pub async fn connect(
        &self,
        tenant: &str,
        vault: Option<&Vault>,
    ) -> std::result::Result<McpClient, ToolError> {
        let secret_value = match &self.secret_ref {
            None => None,
            Some(raw) => {
                let Some(vault) = vault else {
                    return Err(ToolError::PermissionDenied(format!(
                        "connection `{}` needs its secret_ref resolved but no vault/tenant \
                         context is available for this run",
                        self.name
                    )));
                };
                let reference = SecretRef::parse(raw)
                    .map_err(|e| ToolError::Internal(format!("secret reference: {e}")))?;
                let access = SecretAccess::new(tenant, vec![reference.read_permission()]);
                let value = vault
                    .resolve(&reference, &access)
                    .map_err(|e| ToolError::PermissionDenied(format!("resolve secret: {e}")))?;
                Some(value.expose().to_string())
            }
        };

        let transport_result = match (&self.transport, secret_value) {
            (McpTransportConfig::Http { url }, secret) => {
                let mut transport = HttpTransport::connect_guarded(url).await?;
                if let Some(token) = secret {
                    transport = transport.with_bearer_token(token);
                }
                Ok(Box::new(transport) as Box<dyn crate::mcp::McpTransport>)
            }
            (McpTransportConfig::Stdio { command, args }, secret) => {
                let envs: Vec<(String, String)> = match (secret, &self.secret_env_var) {
                    (Some(value), Some(var)) => vec![(var.clone(), value)],
                    (Some(_), None) => {
                        // Already refused at `put()` time, but a persisted document
                        // could in principle be hand-edited — fail closed here too.
                        return Err(ToolError::Internal(format!(
                            "connection `{}` has a secret_ref but no secret_env_var",
                            self.name
                        )));
                    }
                    (None, _) => Vec::new(),
                };
                StdioTransport::spawn_with_env(command, args, envs)
                    .map(|t| Box::new(t) as Box<dyn crate::mcp::McpTransport>)
            }
        };

        let transport = transport_result?;
        McpClient::connect(self.name.clone(), BoxedTransport(transport)).await
    }
}

/// `McpClient::connect` takes `impl McpTransport + 'static` by value, not a
/// trait object, so [`McpConnection::connect`] (which must return either
/// transport kind from one function) forwards through a boxed
/// `dyn McpTransport` wrapped in this newtype instead of duplicating the
/// dial logic per transport kind.
struct BoxedTransport(Box<dyn crate::mcp::McpTransport>);

#[async_trait::async_trait]
impl crate::mcp::McpTransport for BoxedTransport {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> std::result::Result<serde_json::Value, ToolError> {
        self.0.request(method, params).await
    }

    async fn notify(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> std::result::Result<(), ToolError> {
        self.0.notify(method, params).await
    }
}

/// The full connection catalog, serialized as one document: tenant id →
/// connection name → connection. Nested maps (rather than a single map keyed
/// by a composite string) sidestep any ambiguity from a tenant id or
/// connection name containing a separator character.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct McpConnectionsState {
    #[serde(default)]
    tenants: BTreeMap<String, BTreeMap<String, McpConnection>>,
}

impl McpConnectionsState {
    fn put(&mut self, tenant: &str, connection: McpConnection) -> Result<()> {
        if !is_valid_mcp_server_name(&connection.name) {
            return Err(Error::invalid(format!(
                "MCP connection name `{}` must be a non-empty [A-Za-z0-9_-]+ identifier — \
                 it namespaces tool ids and permissions",
                connection.name
            )));
        }
        if let Some(raw) = &connection.secret_ref {
            SecretRef::parse(raw).map_err(|e| {
                Error::invalid(format!(
                    "MCP connection `secret_ref` `{raw}` is invalid: {e}"
                ))
            })?;
            if connection.transport.is_stdio() && connection.secret_env_var.is_none() {
                return Err(Error::invalid(
                    "a Stdio connection with a secret_ref must also set secret_env_var — \
                     only the admin configuring it knows what env var the spawned server expects"
                        .to_string(),
                ));
            }
        }
        self.tenants
            .entry(tenant.to_string())
            .or_default()
            .insert(connection.name.clone(), connection);
        Ok(())
    }

    fn get(&self, tenant: &str, name: &str) -> Option<McpConnection> {
        self.tenants.get(tenant)?.get(name).cloned()
    }

    fn list(&self, tenant: &str) -> Vec<McpConnection> {
        self.tenants
            .get(tenant)
            .map(|byname| byname.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Removes the connection; `false` if it didn't exist (idempotent, not an
    /// error — matches `wovyr-secrets`'s delete semantics).
    fn delete(&mut self, tenant: &str, name: &str) -> bool {
        self.tenants
            .get_mut(tenant)
            .map(|byname| byname.remove(name).is_some())
            .unwrap_or(false)
    }
}

/// A tenant-scoped, file-backed, crash-safe store of configured MCP
/// connections — the durable half of PRD-006's connection-management layer.
pub struct McpConnectionStore {
    dir: PathBuf,
    path: PathBuf,
    lock: Mutex<()>,
}

impl McpConnectionStore {
    /// Open (or create) a store under `dir`, holding `dir/connections.json`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("connections.json"),
            dir,
            lock: Mutex::new(()),
        })
    }

    fn load(&self) -> Result<McpConnectionsState> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::invalid(format!("corrupt MCP connection store: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(McpConnectionsState::default())
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn save(&self, state: &McpConnectionsState) -> Result<()> {
        wovyr_common::fs::atomic_write(&self.path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    /// Runs `f` against the loaded state under both locks, persisting on
    /// success. The cross-process lock spans load→mutate→save so a
    /// concurrent writer (this or another process) can't silently lose the
    /// other's update (DUR-403).
    fn with_mut<T>(&self, f: impl FnOnce(&mut McpConnectionsState) -> Result<T>) -> Result<T> {
        let _guard = self
            .lock
            .lock()
            .expect("mcp connection store lock poisoned");
        let _flock = wovyr_common::fs::FileLock::acquire(&self.dir)
            .map_err(|e| Error::config(format!("lock MCP connection store: {e}")))?;
        let mut state = self.load()?;
        let out = f(&mut state)?;
        self.save(&state)?;
        Ok(out)
    }

    fn with_ref<T>(&self, f: impl FnOnce(&McpConnectionsState) -> T) -> Result<T> {
        let _guard = self
            .lock
            .lock()
            .expect("mcp connection store lock poisoned");
        Ok(f(&self.load()?))
    }

    /// Insert or replace a connection (by name) for `tenant` — registering
    /// overwrites an existing one, the same collision behavior
    /// `ToolRegistry::register`/`McpClient::register_into` already use.
    pub fn put(&self, tenant: &str, connection: McpConnection) -> Result<()> {
        self.with_mut(|state| state.put(tenant, connection))
    }

    /// One tenant's connection by name.
    pub fn get(&self, tenant: &str, name: &str) -> Result<Option<McpConnection>> {
        self.with_ref(|state| state.get(tenant, name))
    }

    /// All of a tenant's connections, ordered by name (`BTreeMap` iteration).
    pub fn list(&self, tenant: &str) -> Result<Vec<McpConnection>> {
        self.with_ref(|state| state.list(tenant))
    }

    /// Remove a connection; `Ok(false)` if it didn't exist.
    pub fn delete(&self, tenant: &str, name: &str) -> Result<bool> {
        self.with_mut(|state| Ok(state.delete(tenant, name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wovyr_mcp_store_test_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn conn(name: &str) -> McpConnection {
        McpConnection {
            name: name.to_string(),
            transport: McpTransportConfig::Http {
                url: "https://example.com/mcp".to_string(),
            },
            secret_ref: None,
            secret_env_var: None,
            tool_permissions: None,
            created_ms: 1,
            updated_ms: 1,
        }
    }

    #[test]
    fn a_connection_survives_a_fresh_store_instance_over_the_same_directory() {
        let dir = scratch_dir("restart");
        {
            let store = McpConnectionStore::new(&dir).unwrap();
            store.put("acme", conn("docs")).unwrap();
        }
        // A brand-new instance, standing in for a process restart.
        let reopened = McpConnectionStore::new(&dir).unwrap();
        let found = reopened.get("acme", "docs").unwrap();
        assert_eq!(found.unwrap().name, "docs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connections_are_scoped_per_tenant() {
        let dir = scratch_dir("tenant_scope");
        let store = McpConnectionStore::new(&dir).unwrap();
        store.put("acme", conn("docs")).unwrap();
        store.put("other-tenant", conn("docs")).unwrap();

        assert_eq!(store.list("acme").unwrap().len(), 1);
        assert_eq!(store.list("other-tenant").unwrap().len(), 1);
        assert!(store.get("a-third-tenant", "docs").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_is_an_upsert() {
        let dir = scratch_dir("upsert");
        let store = McpConnectionStore::new(&dir).unwrap();
        store.put("acme", conn("docs")).unwrap();
        let mut updated = conn("docs");
        updated.updated_ms = 2;
        store.put("acme", updated).unwrap();

        let list = store.list("acme").unwrap();
        assert_eq!(list.len(), 1, "put must replace, not duplicate");
        assert_eq!(list[0].updated_ms, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_is_idempotent_and_reports_whether_it_existed() {
        let dir = scratch_dir("delete");
        let store = McpConnectionStore::new(&dir).unwrap();
        store.put("acme", conn("docs")).unwrap();

        assert!(store.delete("acme", "docs").unwrap());
        assert!(!store.delete("acme", "docs").unwrap(), "already gone");
        assert!(store.get("acme", "docs").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_invalid_connection_name_is_rejected_fail_closed() {
        let dir = scratch_dir("invalid_name");
        let store = McpConnectionStore::new(&dir).unwrap();
        for bad in ["", "has space", "dot.dot", "semi;colon"] {
            let err = store.put("acme", conn(bad)).unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{bad}: {err}");
        }
        // Nothing should have been persisted from any of those attempts.
        assert!(store.list("acme").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stdio_transport_connection_round_trips_with_its_command_and_args() {
        let dir = scratch_dir("stdio_roundtrip");
        let store = McpConnectionStore::new(&dir).unwrap();
        let mut c = conn("fs");
        c.transport = McpTransportConfig::Stdio {
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                "/tmp/docs".to_string(),
            ],
        };
        assert!(c.transport.is_stdio());
        store.put("acme", c.clone()).unwrap();

        let reopened = McpConnectionStore::new(&dir).unwrap();
        let found = reopened.get("acme", "fs").unwrap().unwrap();
        assert_eq!(found.transport, c.transport);
        assert!(found.transport.is_stdio());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_secret_ref_round_trips_and_the_store_never_needs_the_real_value() {
        let dir = scratch_dir("secret_ref");
        let store = McpConnectionStore::new(&dir).unwrap();
        let mut c = conn("weather");
        c.secret_ref = Some("secret://acme/weather-api-key".to_string());
        store.put("acme", c).unwrap();

        let found = store.get("acme", "weather").unwrap().unwrap();
        assert_eq!(
            found.secret_ref.as_deref(),
            Some("secret://acme/weather-api-key")
        );
        // The stored reference is a real, parseable SecretRef, not just a string.
        let parsed = SecretRef::parse(&found.secret_ref.unwrap()).unwrap();
        assert_eq!(parsed.namespace, "acme");
        assert_eq!(parsed.name, "weather-api-key");

        // The on-disk document holds only the reference string, never a value.
        let raw = std::fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(raw.contains("secret://acme/weather-api-key"));
        assert!(!raw.to_lowercase().contains("api_key_value"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_secret_ref_is_rejected_fail_closed() {
        let dir = scratch_dir("bad_secret_ref");
        let store = McpConnectionStore::new(&dir).unwrap();
        let mut c = conn("weather");
        c.secret_ref = Some("not-a-secret-uri".to_string());
        let err = store.put("acme", c).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        assert!(store.list("acme").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deny_unknown_fields_rejects_a_document_with_an_unexpected_key() {
        let bad_json = r#"{"tenants":{"acme":{"docs":{"name":"docs","transport":{"kind":"http","url":"https://x"},"created_ms":1,"updated_ms":1,"totally_unexpected":true}}}}"#;
        let err = serde_json::from_str::<McpConnectionsState>(bad_json).unwrap_err();
        assert!(
            err.to_string().contains("unknown field")
                || err.to_string().contains("totally_unexpected")
        );
    }

    // --- RM-MCX-P1-105: secret resolution wiring ---------------------------------

    #[test]
    fn a_stdio_connection_with_a_secret_ref_but_no_secret_env_var_is_rejected_at_put_time() {
        let dir = scratch_dir("stdio_secret_no_env_var");
        let store = McpConnectionStore::new(&dir).unwrap();
        let mut c = conn("fs");
        c.transport = McpTransportConfig::Stdio {
            command: "npx".to_string(),
            args: vec![],
        };
        c.secret_ref = Some("secret://acme/some-token".to_string());
        // secret_env_var deliberately left None.
        let err = store.put("acme", c).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn connect_fails_closed_when_a_secret_ref_needs_resolving_but_no_vault_is_given() {
        let mut c = conn("weather");
        c.secret_ref = Some("secret://acme/weather-api-key".to_string());
        let err = c.connect("acme", None).await.unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn connect_refuses_an_http_connection_pointed_at_a_private_address() {
        let mut c = conn("internal");
        c.transport = McpTransportConfig::Http {
            url: "http://10.1.2.3:9/mcp".to_string(),
        };
        let err = c.connect("acme", None).await.unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    /// The end-to-end proof this ticket exists for: a real vault-resolved
    /// secret actually reaches a real spawned OS process under the admin's
    /// chosen env var name — verified through a genuine MCP `initialize`
    /// handshake with a minimal real Node "server" (not a mock of
    /// `StdioTransport` — the whole point is proving the *process
    /// environment* the child actually saw), which echoes the env var back
    /// as its `serverInfo.version` so the test can observe it from the
    /// client side without needing raw stdout access (`StdioTransport`
    /// exposes only the framed JSON-RPC channel).
    #[tokio::test]
    async fn connect_injects_the_resolved_secret_as_the_configured_env_var_on_a_real_process() {
        let vault = Vault::new(std::sync::Arc::new(
            wovyr_secrets::InMemorySecretStore::new(),
        ));
        vault
            .create("acme", "weather-api-key", "s3cr3t-value")
            .unwrap();

        let script = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {
    const resp = {
      jsonrpc: '2.0', id: msg.id,
      result: { serverInfo: { name: 'echo-env-test', version: process.env.WOVYR_TEST_WEATHER_KEY || 'MISSING' } },
    };
    process.stdout.write(JSON.stringify(resp) + '\n');
  }
});
"#;

        let mut c = conn("weather");
        c.transport = McpTransportConfig::Stdio {
            command: "node".to_string(),
            args: vec!["-e".to_string(), script.to_string()],
        };
        c.secret_ref = Some("secret://acme/weather-api-key".to_string());
        c.secret_env_var = Some("WOVYR_TEST_WEATHER_KEY".to_string());

        let client = c.connect("acme", Some(&vault)).await.unwrap();
        assert_eq!(
            client.server_version(),
            "s3cr3t-value",
            "the spawned process must have actually seen the resolved secret \
             under the configured env var name"
        );
    }

    #[tokio::test]
    async fn connect_works_with_no_secret_ref_at_all() {
        let script = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { serverInfo: { name: 'x', version: '1.2.3' } } }) + '\n');
  }
});
"#;
        let mut c = conn("plain");
        c.transport = McpTransportConfig::Stdio {
            command: "node".to_string(),
            args: vec!["-e".to_string(), script.to_string()],
        };
        let client = c.connect("acme", None).await.unwrap();
        assert_eq!(client.server_version(), "1.2.3");
    }
}
