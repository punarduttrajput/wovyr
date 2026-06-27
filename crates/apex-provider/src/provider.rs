//! The core provider trait.

use crate::types::{ChatRequest, ChatResponse};
use apex_common::Result;
use async_trait::async_trait;

/// A vendor-neutral LLM provider.
///
/// This is the Rust interface from the
/// [Provider SDK spec §21](../../docs/04-agent-framework/provider-sdk.md). v0.1
/// requires only chat completion; embeddings, images, and streaming are added in
/// later milestones.
#[async_trait]
pub trait AIProvider: Send + Sync {
    /// Stable provider identifier (e.g. `mock`, `openai`), used in traces.
    fn name(&self) -> &str;

    /// Execute a chat completion, returning the assistant message and usage.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
}
