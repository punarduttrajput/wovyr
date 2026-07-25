//! Memory engine: durable storage with hybrid retrieval and ranking.
//!
//! Implements the v0.2 core of the
//! [Memory Engine](../../docs/06-memory-engine/overview.md): records are embedded
//! (via the [LLM Gateway](wovyr_provider)) and stored, then served by **hybrid
//! retrieval** — vector similarity fused with keyword search via Reciprocal Rank
//! Fusion ([retrieval §4](../../docs/06-memory-engine/retrieval.md)) — and a
//! weighted [ranker](../../docs/06-memory-engine/ranking.md) (relevance + recency +
//! importance) with a transparent score breakdown.
//!
//! Implemented beyond the v0.2 core: MMR diversification, ABAC filtering,
//! compression, sensitive-record encryption ([`EncryptingMemoryStore`]), the
//! tiered Postgres+Qdrant backend (`tiered` feature), document chunking
//! with parent linkage ([`MemoryEngine::remember_document`], RM-AIM-P2
//! RAG-201), and an opt-in second-stage reranker
//! ([`MemoryEngine::with_reranker`], RAG-202). **Deferred:** the knowledge
//! graph.

#[cfg(feature = "tiered")]
mod backends;
mod chunk;
mod clock;
mod encrypting_store;
mod engine;
mod record;
mod rerank;
mod store;

#[cfg(feature = "tiered")]
pub use backends::{PostgresStore, QdrantStore, TieredStore};
pub use chunk::{ChunkPolicy, split};
pub use clock::{Clock, ManualClock, SystemClock};
pub use encrypting_store::EncryptingMemoryStore;
pub use engine::MemoryEngine;
pub use record::{
    AccessContext, CompactionOutcome, CompactionPolicy, DocumentIngest, EmbeddingMigrationReport,
    MemoryQuery, MemoryRecord, MemoryType, RankingWeights, RetrievalStrategy, ScoreBreakdown,
    ScoredMemory,
};
pub use rerank::{LlmReranker, Reranker};
pub use store::{FileStore, InMemoryStore, MemoryStore, ScoredId};
