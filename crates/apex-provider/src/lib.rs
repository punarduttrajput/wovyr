//! Provider SDK and LLM gateway.
//!
//! Implements the vendor-neutral provider abstraction from the
//! [Provider SDK spec](../../docs/04-agent-framework/provider-sdk.md): business
//! logic talks to the [`AIProvider`] trait and never to a vendor SDK directly.
//!
//! v0.1 scope (per the [roadmap](../../docs/18-roadmap/v0.1.md)): chat completion
//! with function/tool calling, a deterministic [`MockProvider`] for offline runs,
//! and an OpenAI-compatible [`OpenAiProvider`]. Routing is handled by the
//! [`Gateway`], which resolves a [`ModelSelector`] to a concrete model.

mod gateway;
mod mock;
mod openai;
mod provider;
mod types;

pub use gateway::{Gateway, ModelSelector};
pub use mock::MockProvider;
pub use openai::OpenAiProvider;
pub use provider::AIProvider;
pub use types::{ChatRequest, ChatResponse, Message, Role, ToolCall, ToolSpec};
