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
    /// deliberately withholds (SEC-301). Uses a **native-only** sandbox manager, so a
    /// verified/untrusted run fails closed (no strong backend to run it in). For
    /// trusted first-party/local contexts (e.g. the CLI's `agents run --local`); a
    /// server that has probed the node's capabilities should use
    /// [`Self::with_shell_using`] instead so verified/untrusted work runs in a
    /// container when one is available (SBX-101).
    pub fn with_shell(mut self) -> Self {
        self.register(Arc::new(crate::builtin::ShellTool::native_only()));
        self
    }

    /// Register `shell` driven by the node's **detected** sandbox capabilities
    /// (RM-AIM-P1 SBX-101): a verified/untrusted run selects the strongest available
    /// backend (container/gVisor) rather than failing closed. Build `manager` from
    /// [`crate::SandboxManager::detect`] at startup.
    pub fn with_shell_using(mut self, manager: crate::SandboxManager) -> Self {
        self.register(Arc::new(crate::builtin::ShellTool::with_manager(manager)));
        self
    }

    /// Register `fs_write` into this registry — like `shell`, `with_builtins()`
    /// deliberately withholds write access (SBX-301): confined to the run's
    /// workspace root the same way `fs_read` is, but a much bigger blast radius
    /// than read-only access to hand every agent by default.
    pub fn with_fs_write(mut self) -> Self {
        self.register(Arc::new(crate::builtin::FsWriteTool));
        self
    }

    /// Register `code_execute` into this registry — like `shell`, `with_builtins()`
    /// deliberately withholds it (SBX-302): arbitrary code execution, just in a
    /// language runtime rather than a shell command line. Native-only, so a
    /// verified/untrusted run fails closed; a server that has probed the node's
    /// capabilities should use [`Self::with_code_execute_using`] instead.
    pub fn with_code_execute(mut self) -> Self {
        self.register(Arc::new(crate::builtin::CodeExecuteTool::native_only()));
        self
    }

    /// Register `code_execute` driven by the node's **detected** sandbox
    /// capabilities (SBX-101), mirroring [`Self::with_shell_using`].
    pub fn with_code_execute_using(mut self, manager: crate::SandboxManager) -> Self {
        self.register(Arc::new(crate::builtin::CodeExecuteTool::with_manager(
            manager,
        )));
        self
    }

    /// `with_builtins()` plus `shell`, `fs_write`, and `code_execute` — a
    /// convenience for trusted first-party/local contexts that want the full
    /// v0.1 built-in set, including arbitrary command/code execution and
    /// confined write access (SEC-301, SBX-301, SBX-302).
    pub fn with_privileged_builtins() -> Self {
        Self::with_builtins()
            .with_shell()
            .with_fs_write()
            .with_code_execute()
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
        // shell/fs_write/code_execute are not default builtins
        // (SEC-301/SBX-301/SBX-302) — explicit opt-in only.
        assert!(!r.contains("shell"));
        assert!(!r.contains("fs_write"));
        assert!(!r.contains("code_execute"));
        assert_eq!(r.ids().len(), 3);
    }

    #[test]
    fn code_execute_is_an_explicit_opt_in() {
        assert!(!ToolRegistry::with_builtins().contains("code_execute"));
        assert!(
            ToolRegistry::with_builtins()
                .with_code_execute()
                .contains("code_execute")
        );
    }

    #[test]
    fn fs_write_is_an_explicit_opt_in() {
        assert!(!ToolRegistry::with_builtins().contains("fs_write"));
        assert!(
            ToolRegistry::with_builtins()
                .with_fs_write()
                .contains("fs_write")
        );
    }

    #[test]
    fn shell_is_an_explicit_opt_in() {
        assert!(!ToolRegistry::with_builtins().contains("shell"));
        assert!(ToolRegistry::with_builtins().with_shell().contains("shell"));
        let privileged = ToolRegistry::with_privileged_builtins();
        for id in [
            "echo",
            "fs_read",
            "http_get",
            "shell",
            "fs_write",
            "code_execute",
        ] {
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
