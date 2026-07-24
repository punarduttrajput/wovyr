//! The core provider trait.

use crate::embeddings::{EmbeddingRequest, EmbeddingResponse};
use crate::image::{ImageGenRequest, ImageGenResponse};
use crate::types::{ChatRequest, ChatResponse};
use apex_common::{Error, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;

/// An event in a streamed chat completion ([provider SDK §streaming](../../docs/04-agent-framework/provider-sdk.md)).
pub enum ChatStreamEvent {
    /// An incremental piece of the assistant's text content.
    Delta(String),
    /// An incremental fragment of a requested tool call's JSON arguments as the
    /// model composes it (AIC-202). `id`/`name` carry the accumulated values known
    /// so far (both protocols send them at the call's start), so consumers never
    /// need to join fragments across events; `arguments` is this event's fragment
    /// only — empty on the announcement event that opens a call. The complete,
    /// assembled call still arrives in the terminal [`Done`](Self::Done) response,
    /// which remains the source of truth the agent loop executes from.
    ToolCallDelta {
        /// Position of the call within the assistant turn (OpenAI `index` /
        /// Anthropic content-block order).
        index: usize,
        /// The call id accumulated so far (may be empty early in the stream).
        id: String,
        /// The tool name accumulated so far (may be empty early in the stream).
        name: String,
        /// This event's incremental piece of the JSON arguments.
        arguments: String,
    },
    /// An incremental piece of the model's reasoning/thinking channel, where the
    /// provider exposes one (AIC-202) — Anthropic `thinking_delta`s, or an
    /// OpenAI-compatible server's `delta.reasoning_content`. Display-only: the
    /// terminal [`Done`](Self::Done) message never includes it.
    ReasoningDelta(String),
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

    /// Whether this provider can produce embeddings (RM-AR-P1 AIC-301).
    ///
    /// Defaults to `false`, matching the default [`embed`](Self::embed) that
    /// returns an "unsupported" error; a provider that overrides `embed` also
    /// overrides this to `true`. Lets the gateway and memory engine detect a
    /// non-embedding deployment (e.g. Anthropic-only) at construction/startup
    /// and fail loud, rather than erroring per-call deep inside a run.
    fn supports_embeddings(&self) -> bool {
        false
    }

    /// Embed one or more texts.
    ///
    /// Defaults to an "unsupported" error so providers that only do chat need not
    /// implement it; embedding-capable providers override this (and
    /// [`supports_embeddings`](Self::supports_embeddings)).
    async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        Err(Error::provider(format!(
            "provider `{}` does not support embeddings",
            self.name()
        )))
    }

    /// Generate one or more images from a text prompt.
    ///
    /// Defaults to an "unsupported" error so chat-only providers need not
    /// implement it; image-capable providers override this.
    async fn generate_image(&self, _request: ImageGenRequest) -> Result<ImageGenResponse> {
        Err(Error::provider(format!(
            "provider `{}` does not support image generation",
            self.name()
        )))
    }
}
