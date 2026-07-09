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

mod embeddings;
mod gateway;
mod image;
#[cfg(feature = "mistralrs")]
mod mistralrs_provider;
mod mock;
mod openai;
mod pricing;
mod provider;
#[cfg(feature = "redis")]
mod redis_breaker;
mod resilience;
#[cfg(feature = "qdrant")]
mod semantic_qdrant;
mod types;

pub use embeddings::{EmbeddingRequest, EmbeddingResponse, cosine_similarity};
pub use gateway::{Gateway, ModelSelector};
pub use image::{ImageGenRequest, ImageGenResponse};
#[cfg(feature = "mistralrs")]
pub use mistralrs_provider::MistralRsProvider;
pub use mock::MockProvider;
pub use openai::OpenAiProvider;
pub use pricing::{ModelPrice, PriceBook};
pub use provider::{AIProvider, ChatStream, ChatStreamEvent};
#[cfg(feature = "redis")]
pub use redis_breaker::RedisKv;
pub use resilience::{
    BreakerConfig, BreakerKv, CacheConfig, CacheMode, CircuitBreaker, CostEvent, CostObserver,
    HedgeConfig, InMemoryKv, InMemorySemanticCache, LocalCircuitBreaker, RetryConfig,
    SemanticCacheStore, SharedCircuitBreaker,
};
#[cfg(feature = "qdrant")]
pub use semantic_qdrant::QdrantSemanticCache;
pub use types::{ChatRequest, ChatResponse, Message, Role, ToolCall, ToolSpec};
