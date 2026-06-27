//! The LLM gateway: provider selection and model resolution.
//!
//! Agents declare *what kind* of model they need via a [`ModelSelector`]
//! (`capability` + `class`) rather than pinning a vendor model — see the
//! [hello agent](../../docs/16-examples/hello-agent.md) and
//! [routing spec](../../docs/05-llm-gateway/routing.md). The gateway turns that
//! intent into a concrete model on a concrete provider.
//!
//! v0.1 keeps routing deliberately simple: one active provider chosen from the
//! environment, with a small selector→model mapping. Cost/latency-based routing
//! and failover come later.

use crate::mock::MockProvider;
use crate::openai::OpenAiProvider;
use crate::provider::AIProvider;
use crate::types::{ChatRequest, ChatResponse};
use apex_common::Result;
use serde::{Deserialize, Serialize};

/// Declarative model requirement: pick a model by capability and class instead
/// of pinning a vendor model id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelSelector {
    /// Required capability, e.g. `chat` or `embeddings`.
    #[serde(default = "default_capability")]
    pub capability: String,
    /// Desired class/tier, e.g. `fast`, `balanced`, `frontier`.
    #[serde(default = "default_class")]
    pub class: String,
}

fn default_capability() -> String {
    "chat".to_string()
}
fn default_class() -> String {
    "fast".to_string()
}

impl Default for ModelSelector {
    fn default() -> Self {
        Self {
            capability: default_capability(),
            class: default_class(),
        }
    }
}

/// Routes chat requests to a backing [`AIProvider`] and resolves model selectors.
pub struct Gateway {
    provider: Box<dyn AIProvider>,
}

impl Gateway {
    /// Construct a gateway over an explicit provider.
    pub fn new(provider: Box<dyn AIProvider>) -> Self {
        Self { provider }
    }

    /// Build a gateway from the environment.
    ///
    /// Uses the OpenAI-compatible provider when `OPENAI_API_KEY` is set; otherwise
    /// falls back to the offline [`MockProvider`] so local runs work with no setup.
    pub fn from_env() -> Self {
        match OpenAiProvider::from_env() {
            Ok(p) => {
                tracing::info!("llm gateway: using openai-compatible provider");
                Self::new(Box::new(p))
            }
            Err(_) => {
                tracing::info!("llm gateway: OPENAI_API_KEY not set, using mock provider");
                Self::new(Box::new(MockProvider::new()))
            }
        }
    }

    /// Name of the active provider (for tracing / the run header).
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// Resolve a selector (and optional pinned model) to a concrete model id.
    ///
    /// A pinned model always wins. Otherwise the class maps to a default model id
    /// per provider; unknown providers fall back to a generic `chat` class name.
    pub fn resolve_model(&self, pinned: Option<&str>, selector: &ModelSelector) -> String {
        if let Some(model) = pinned {
            return model.to_string();
        }
        match self.provider.name() {
            "openai" => match selector.class.as_str() {
                "fast" => "gpt-4o-mini",
                "balanced" => "gpt-4o",
                "frontier" => "gpt-4o",
                _ => "gpt-4o-mini",
            }
            .to_string(),
            // Mock and unknown providers echo a descriptive synthetic id.
            other => format!("{other}-{}-{}", selector.capability, selector.class),
        }
    }

    /// Execute a chat completion via the active provider.
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.provider.chat(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_model_wins() {
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let m = gw.resolve_model(Some("gpt-5"), &ModelSelector::default());
        assert_eq!(m, "gpt-5");
    }

    #[test]
    fn mock_selector_resolves_descriptively() {
        let gw = Gateway::new(Box::new(MockProvider::new()));
        let m = gw.resolve_model(None, &ModelSelector::default());
        assert_eq!(m, "mock-chat-fast");
    }
}
