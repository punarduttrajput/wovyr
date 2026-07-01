//! Declarative agent definition and its YAML loader.
//!
//! Accepts the Kubernetes-style manifest from the
//! [hello agent](../../docs/16-examples/hello-agent.md) example
//! (`apiVersion`/`kind`/`metadata`/`spec`). Fields beyond v0.1's needs
//! (memory, planning strategy, policy) are tolerated but ignored for now;
//! `#[serde(deny_unknown_fields)]` is intentionally *not* set so richer manifests
//! from the [full spec](../../docs/04-agent-framework/agent-definition.md) still load.

use apex_common::{Error, Result};
use apex_provider::ModelSelector;
use serde::Deserialize;

/// A parsed agent manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentDefinition {
    /// Manifest API version, e.g. `agent.apex.io/v1`.
    #[serde(rename = "apiVersion", default)]
    pub api_version: Option<String>,
    /// Manifest kind; expected to be `Agent`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Identifying metadata.
    pub metadata: Metadata,
    /// Behavioral specification.
    pub spec: Spec,
}

/// Agent identity.
#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    /// Agent name.
    pub name: String,
}

/// Agent behavior and runtime configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    /// System instructions injected as the system prompt.
    pub instructions: String,
    /// Pinned model id; takes precedence over `model_selector`.
    #[serde(default)]
    pub model: Option<String>,
    /// Declarative model requirement (capability + class).
    #[serde(default, alias = "modelSelector")]
    pub model_selector: Option<ModelSelector>,
    /// Sampling temperature.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Max output tokens.
    #[serde(default, alias = "maxTokens")]
    pub max_tokens: Option<u32>,
    /// Default cap on model/tool iterations for a run of this agent (overrides the
    /// runtime's built-in default; a caller-supplied [`crate::RunOptions::max_steps`]
    /// still takes precedence over this).
    #[serde(default, alias = "maxSteps")]
    pub max_steps: Option<usize>,
    /// Tool ids this agent is allowed to call (must exist in the registry).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Permissions granted to this agent, enforced against each tool's declared
    /// permissions ([tool framework §47](../../docs/04-agent-framework/tool-framework.md)).
    /// Absent → unrestricted; `[]` → grant nothing (deny permissioned tools).
    #[serde(default)]
    pub permissions: Option<Vec<String>>,
    /// Memory/retrieval configuration. When enabled, the runtime grounds the prompt
    /// in retrieved context ([RAG agent](../../docs/16-examples/rag-agent.md)).
    #[serde(default)]
    pub memory: Option<MemorySpec>,
}

/// Memory retrieval configuration for an agent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MemorySpec {
    /// Whether retrieval-augmented grounding is on.
    #[serde(default)]
    pub enabled: bool,
    /// Memory namespace (knowledge base) to retrieve from.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Retrieval tuning (strategy, result cap).
    #[serde(default)]
    pub retrieval: Retrieval,
    /// Only retrieve records carrying at least one of these tags (no filter if empty).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Retrieval tuning knobs ([retrieval spec](../../docs/06-memory-engine/retrieval.md)).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Retrieval {
    /// `hybrid` (default), `vector`, or `keyword`. Interpreted by the retriever.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Maximum memories to inject (defaults to a small budget in the retriever).
    #[serde(default)]
    pub limit: Option<usize>,
}

impl AgentDefinition {
    /// Parse a definition from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let def: AgentDefinition = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid agent manifest: {e}")))?;
        def.validate()?;
        Ok(def)
    }

    /// Load and parse a definition from a file path.
    pub fn from_file(path: &str) -> Result<Self> {
        let yaml = std::fs::read_to_string(path)
            .map_err(|e| Error::config(format!("could not read agent file {path}: {e}")))?;
        Self::from_yaml(&yaml)
    }

    /// The model selector, defaulting to `{capability: chat, class: fast}`.
    pub fn selector(&self) -> ModelSelector {
        self.spec.model_selector.clone().unwrap_or_default()
    }

    fn validate(&self) -> Result<()> {
        if let Some(kind) = &self.kind
            && kind != "Agent"
        {
            return Err(Error::invalid(format!(
                "unsupported kind `{kind}`, expected `Agent`"
            )));
        }
        if self.metadata.name.trim().is_empty() {
            return Err(Error::invalid("metadata.name must not be empty"));
        }
        if self.spec.instructions.trim().is_empty() {
            return Err(Error::invalid("spec.instructions must not be empty"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = r#"
apiVersion: agent.apex.io/v1
kind: Agent
metadata:
  name: hello
spec:
  model_selector: { capability: chat, class: fast }
  instructions: |
    You are a friendly assistant. Greet the user and answer briefly.
"#;

    #[test]
    fn parses_hello_manifest() {
        let def = AgentDefinition::from_yaml(HELLO).unwrap();
        assert_eq!(def.metadata.name, "hello");
        assert_eq!(def.selector().class, "fast");
        assert!(def.spec.tools.is_empty());
        assert!(def.spec.model.is_none());
    }

    #[test]
    fn rejects_wrong_kind() {
        let yaml = "kind: Workflow\nmetadata:\n  name: x\nspec:\n  instructions: hi\n";
        assert!(AgentDefinition::from_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_empty_instructions() {
        let yaml = "metadata:\n  name: x\nspec:\n  instructions: \"  \"\n";
        assert!(AgentDefinition::from_yaml(yaml).is_err());
    }

    #[test]
    fn parses_max_steps_and_its_camel_case_alias() {
        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: x\nspec:\n  instructions: hi\n  max_steps: 20\n",
        )
        .unwrap();
        assert_eq!(def.spec.max_steps, Some(20));

        let def = AgentDefinition::from_yaml(
            "metadata:\n  name: x\nspec:\n  instructions: hi\n  maxSteps: 12\n",
        )
        .unwrap();
        assert_eq!(def.spec.max_steps, Some(12));

        // Absent → no agent-level override.
        assert_eq!(
            AgentDefinition::from_yaml(HELLO).unwrap().spec.max_steps,
            None
        );
    }
}
