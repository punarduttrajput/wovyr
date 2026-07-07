//! In-memory tool registry.
//!
//! The runtime authoritative catalog of available tools
//! ([Tool Framework spec §12/§57](../../docs/04-agent-framework/tool-framework.md)).
//! v0.1 is a simple in-process map keyed by tool id; distributed/versioned
//! registries arrive in later milestones.

use crate::tool::{Tool, ToolContext, ToolError, ToolRequest, ToolResponse, check_permissions};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A catalog of tools available to agents, keyed by tool id.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry pre-populated with the **safe-by-default** built-in tools
    /// ([RM-GA-P1 SEC-301](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
    /// `echo`, `fs_read`, `http_get` — no arbitrary command execution. `shell` is a
    /// separate, explicit opt-in ([`Self::with_shell`]/[`Self::with_privileged_builtins`]):
    /// a hosted/server context must not hand every agent a shell by default.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(crate::builtin::EchoTool));
        r.register(Arc::new(crate::builtin::FsReadTool));
        r.register(Arc::new(crate::builtin::HttpGetTool::new()));
        r
    }

    /// Register `shell` into this registry — the explicit opt-in `with_builtins()`
    /// deliberately withholds (SEC-301). For trusted first-party/local contexts only
    /// (e.g. the CLI's `agents run --local`, or a server explicitly setting
    /// `APEX_ENABLE_SHELL_TOOL=1`).
    pub fn with_shell(mut self) -> Self {
        self.register(Arc::new(crate::builtin::ShellTool));
        self
    }

    /// `with_builtins()` plus `shell` — a convenience for trusted first-party/local
    /// contexts that want the full v0.1 built-in set, including arbitrary command
    /// execution (SEC-301).
    pub fn with_privileged_builtins() -> Self {
        Self::with_builtins().with_shell()
    }

    /// Register a tool, overwriting any existing tool with the same id.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let id = tool.metadata().id;
        self.tools.insert(id, tool);
    }

    /// Remove tool `id`, returning it if present. Used when a capability is
    /// withdrawn (e.g. a plugin is disabled or uninstalled).
    pub fn unregister(&mut self, id: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(id)
    }

    /// Look up a tool by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(id).cloned()
    }

    /// Whether a tool id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.tools.contains_key(id)
    }

    /// All registered tool ids, sorted.
    pub fn ids(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Metadata for every registered tool, sorted by id — backs a tool-discovery API
    /// (e.g. the dashboard's tool picker), covering built-ins **and** enabled plugin tools.
    pub fn metadata(&self) -> Vec<crate::tool::ToolMetadata> {
        self.tools.values().map(|t| t.metadata()).collect()
    }

    /// Execute tool `id`, enforcing its declared permissions against the context's
    /// grants ([spec §47](../../docs/04-agent-framework/tool-framework.md)): an
    /// unknown tool is a validation error, and a tool requiring a permission the
    /// caller wasn't granted is denied **before** it runs (fail-closed).
    pub async fn execute(
        &self,
        id: &str,
        ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let tool = self
            .get(id)
            .ok_or_else(|| ToolError::Validation(format!("unknown tool `{id}`")))?;
        check_permissions(&tool.metadata().permissions, ctx)?;
        tool.execute(ctx, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_registered() {
        let r = ToolRegistry::with_builtins();
        assert!(r.contains("echo"));
        assert!(r.contains("fs_read"));
        assert!(r.contains("http_get"));
        // shell is not a default builtin (SEC-301) — explicit opt-in only.
        assert!(!r.contains("shell"));
        assert_eq!(r.ids().len(), 3);
    }

    #[test]
    fn shell_is_an_explicit_opt_in() {
        assert!(!ToolRegistry::with_builtins().contains("shell"));
        assert!(ToolRegistry::with_builtins().with_shell().contains("shell"));
        let privileged = ToolRegistry::with_privileged_builtins();
        for id in ["echo", "fs_read", "http_get", "shell"] {
            assert!(privileged.contains(id), "missing {id}");
        }
    }

    #[test]
    fn unregister_removes_a_tool() {
        let mut r = ToolRegistry::with_builtins();
        assert!(r.unregister("echo").is_some());
        assert!(!r.contains("echo"));
        // Removing a missing tool is a no-op (None).
        assert!(r.unregister("echo").is_none());
    }

    fn ctx(granted: Option<&[&str]>) -> ToolContext {
        ToolContext {
            granted_permissions: granted.map(|g| g.iter().map(|s| s.to_string()).collect()),
            ..ToolContext::default()
        }
    }

    fn echo_request() -> ToolRequest {
        ToolRequest::new(serde_json::json!({ "message": "hi" }))
    }

    #[tokio::test]
    async fn execute_denies_ungranted_permission() {
        let r = ToolRegistry::with_privileged_builtins();
        // `shell` requires `shell.execute`; a caller granted only `net.egress` is denied.
        let err = r
            .execute("shell", &ctx(Some(&["net.egress"])), echo_request())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::PermissionDenied(m) if m.contains("shell.execute")),
            "expected a permission denial, got {err:?}"
        );
    }

    #[tokio::test]
    async fn execute_allows_granted_and_unpermissioned_tools() {
        let r = ToolRegistry::with_builtins();
        // `echo` declares no permissions → allowed even under an empty grant set.
        assert!(
            r.execute("echo", &ctx(Some(&[])), echo_request())
                .await
                .is_ok()
        );
        // A caller granted the required permission may run the tool.
        assert!(
            r.execute("echo", &ctx(Some(&["anything"])), echo_request())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn execute_is_unrestricted_without_a_policy() {
        let r = ToolRegistry::with_builtins();
        // `None` grants = no policy → permissioned tools run (back-compat).
        assert!(r.execute("echo", &ctx(None), echo_request()).await.is_ok());
    }

    #[tokio::test]
    async fn execute_rejects_unknown_tool() {
        let r = ToolRegistry::with_builtins();
        let err = r
            .execute("nope", &ctx(None), echo_request())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
