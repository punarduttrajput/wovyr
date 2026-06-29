//! The core provider trait.

use crate::embeddings::{EmbeddingRequest, EmbeddingResponse};
use crate::types::{ChatRequest, ChatResponse};
use apex_common::{Error, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;

/// An event in a streamed chat completion ([provider SDK §streaming](../../docs/04-agent-framework/provider-sdk.md)).
pub enum ChatStreamEvent {
    /// An incremental piece of the assistant's text content.
    Delta(String),
    /// The completed response — full message (incl. any tool calls), usage, and
    /// finish reason. Always the final event of a successful stream.
    Done(ChatResponse),
}

/// A boxed stream of chat events.
pub type ChatStream = BoxStream<'static, Result<ChatStreamEvent>>;

/// A vendor-neutral LLM provider.
///
/// Business logic talks to this trait, never a vendor SDK
/// ([Provider SDK spec §21](../../docs/04-agent-framework/provider-sdk.md)).
#[async_trait]
pub trait AIProvider: Send + Sync {
    /// Stable provider identifier (e.g. `mock`, `openai`), used in traces.
    fn name(&self) -> &str;

    /// Execute a chat completion, returning the assistant message and usage.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;

    /// Stream a chat completion as incremental [`ChatStreamEvent`]s, ending with a
    /// [`ChatStreamEvent::Done`]. The default emits the whole content as one delta
    /// then `Done` (non-streaming providers need not override); real providers
    /// override to surface tokens as they arrive.
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let response = self.chat(request).await?;
        let mut events: Vec<Result<ChatStreamEvent>> = Vec::new();
        if let Some(content) = response.message.content.as_ref().filter(|c| !c.is_empty()) {
            events.push(Ok(ChatStreamEvent::Delta(content.clone())));
        }
        events.push(Ok(ChatStreamEvent::Done(response)));
        Ok(Box::pin(futures::stream::iter(events)))
    }

    /// Embed one or more texts.
    ///
    /// Defaults to an "unsupported" error so providers that only do chat need not
    /// implement it; embedding-capable providers override this.
    async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        Err(Error::provider(format!(
            "provider `{}` does not support embeddings",
            self.name()
        )))
    }
}
