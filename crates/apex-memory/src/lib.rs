//! Memory engine: durable storage with hybrid retrieval and ranking.
//!
//! Implements the v0.2 core of the
//! [Memory Engine](../../docs/06-memory-engine/overview.md): records are embedded
//! (via the [LLM Gateway](apex_provider)) and stored, then served by **hybrid
//! retrieval** — vector similarity fused with keyword search via Reciprocal Rank
//! Fusion ([retrieval §4](../../docs/06-memory-engine/retrieval.md)) — and a
//! weighted [ranker](../../docs/06-memory-engine/ranking.md) (relevance + recency +
//! importance) with a transparent score breakdown.
//!
//! v0.2 slice scope: namespaces, hybrid/vector/keyword strategies, metadata
//! filters, deterministic ranking, and in-memory + file stores. **Deferred:**
//! Postgres/Qdrant/Redis backends, the knowledge graph, MMR diversification,
//! compression, and policy/ABAC filtering.

#[cfg(feature = "tiered")]
mod backends;
mod engine;
mod record;
mod store;

#[cfg(feature = "tiered")]
pub use backends::{PostgresStore, QdrantStore, TieredStore};
pub use engine::MemoryEngine;
pub use record::{
    MemoryQuery, MemoryRecord, MemoryType, RankingWeights, RetrievalStrategy, ScoreBreakdown,
    ScoredMemory,
};
pub use store::{FileStore, InMemoryStore, MemoryStore, ScoredId};
