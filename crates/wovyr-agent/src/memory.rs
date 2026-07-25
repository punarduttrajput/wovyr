//! Memory retrieval hook for the agent loop.
//!
//! When an agent enables memory ([`MemorySpec`]), the runtime retrieves relevant
//! context before the model call and grounds the prompt in it
//! ([RAG agent example](../../docs/16-examples/rag-agent.md)). To keep the crate
//! spine one-directional (the agent must not depend on `wovyr-memory`), retrieval is
//! abstracted behind the [`ContextRetriever`] trait; a concrete adapter over the
//! Memory Engine lives in the CLI, which depends on both crates.

use crate::definition::MemorySpec;
use async_trait::async_trait;
use wovyr_common::Result;

/// A single piece of context retrieved for grounding.
#[derive(Debug, Clone)]
pub struct RetrievedContext {
    /// A short, citable source label (e.g. a title or record id).
    pub source: String,
    /// The retrieved text.
    pub content: String,
    /// The retrieval score (higher = more relevant).
    pub score: f32,
}

/// Supplies grounding context for a query, per an agent's [`MemorySpec`].
///
/// `Send + Sync` so the runtime can hold it across `.await` and drive it from
/// `Send` futures (e.g. the Axum handler), matching [`RunEventSink`](crate::RunEventSink).
#[async_trait]
pub trait ContextRetriever: Send + Sync {
    /// Retrieve context relevant to `query` under the agent's memory configuration.
    /// Returns results best-first; an empty vec means nothing relevant was found.
    async fn retrieve(&self, query: &str, spec: &MemorySpec) -> Result<Vec<RetrievedContext>>;
}
