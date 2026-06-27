//! In-memory tool registry.
//!
//! The runtime authoritative catalog of available tools
//! ([Tool Framework spec §12/§57](../../docs/04-agent-framework/tool-framework.md)).
//! v0.1 is a simple in-process map keyed by tool id; distributed/versioned
//! registries arrive in later milestones.

use crate::tool::Tool;
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

    /// A registry pre-populated with the v0.1 built-in tools.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(crate::builtin::EchoTool));
        r.register(Arc::new(crate::builtin::FsReadTool));
        r.register(Arc::new(crate::builtin::HttpGetTool::new()));
        r
    }

    /// Register a tool, overwriting any existing tool with the same id.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let id = tool.metadata().id;
        self.tools.insert(id, tool);
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
        assert_eq!(r.ids().len(), 3);
    }
}
