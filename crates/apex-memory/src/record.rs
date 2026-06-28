//! Memory records, queries, and scoring types.

use serde::{Deserialize, Serialize};

/// The kind of memory, which governs recency decay
/// ([ranking §4](../../docs/06-memory-engine/ranking.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// Short-lived conversational memory (fast decay).
    Conversation,
    /// Workflow-scoped memory.
    Workflow,
    /// Episodic memory of past events.
    Episodic,
    /// Durable semantic knowledge — does not decay.
    #[default]
    Semantic,
}

impl MemoryType {
    /// Recency half-life in sequence units; `None` means no decay (semantic).
    /// Sequence distance is used as a deterministic proxy for age
    /// ([determinism §11](../../docs/06-memory-engine/retrieval.md)).
    pub fn half_life(self) -> Option<f32> {
        match self {
            MemoryType::Conversation => Some(2.0),
            MemoryType::Workflow => Some(14.0),
            MemoryType::Episodic => Some(90.0),
            MemoryType::Semantic => None,
        }
    }
}

/// A stored memory record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    /// Unique id (assigned by the store).
    pub id: String,
    /// Namespace the record belongs to.
    pub namespace: String,
    /// The memory text.
    pub content: String,
    /// Embedding vector of `content`.
    pub embedding: Vec<f32>,
    /// Memory type (drives recency).
    #[serde(default)]
    pub memory_type: MemoryType,
    /// Intrinsic importance in `[0,1]`.
    #[serde(default)]
    pub importance: f32,
    /// Free-form tags for metadata filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Monotonic insertion sequence (assigned by the store; used for recency).
    #[serde(default)]
    pub seq: u64,
}

/// Which retrieval method to use ([retrieval §2](../../docs/06-memory-engine/retrieval.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalStrategy {
    /// Embedding similarity only.
    Vector,
    /// Keyword/term-overlap only.
    Keyword,
    /// Vector + keyword fused (the default).
    #[default]
    Hybrid,
}

/// Ranking signal weights ([ranking §3](../../docs/06-memory-engine/ranking.md)).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RankingWeights {
    /// Weight of query relevance.
    pub relevance: f32,
    /// Weight of recency.
    pub recency: f32,
    /// Weight of intrinsic importance.
    pub importance: f32,
}

impl Default for RankingWeights {
    fn default() -> Self {
        // Defaults from the ranking spec (frequency/proximity are not yet scored).
        Self {
            relevance: 0.55,
            recency: 0.20,
            importance: 0.15,
        }
    }
}

/// A memory retrieval query.
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    /// Query text.
    pub text: String,
    /// Restrict to a namespace (all namespaces if `None`).
    pub namespace: Option<String>,
    /// Retrieval strategy.
    pub strategy: RetrievalStrategy,
    /// Maximum results to return.
    pub limit: usize,
    /// Only records carrying at least one of these tags (no filter if empty).
    pub tags: Vec<String>,
    /// Minimum importance.
    pub min_importance: f32,
    /// Ranking weights.
    pub weights: RankingWeights,
    /// Result diversification in `[0,1]` via MMR ([ranking §7](../../docs/06-memory-engine/ranking.md)):
    /// `0.0` ranks by pure relevance (default), higher values trade relevance for
    /// less redundancy among the returned memories.
    pub diversity: f32,
}

impl MemoryQuery {
    /// A hybrid query for `text` with sensible defaults.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            namespace: None,
            strategy: RetrievalStrategy::default(),
            limit: 5,
            tags: Vec::new(),
            min_importance: 0.0,
            weights: RankingWeights::default(),
            diversity: 0.0,
        }
    }
}

/// Per-result explanation of the composite score
/// ([ranking §9](../../docs/06-memory-engine/ranking.md)).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    /// Normalized relevance (fusion / similarity).
    pub relevance: f32,
    /// Recency decay factor.
    pub recency: f32,
    /// Importance contribution.
    pub importance: f32,
    /// Final weighted score.
    pub total: f32,
}

/// A ranked memory result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMemory {
    /// The matched record.
    pub record: MemoryRecord,
    /// Final ranking score.
    pub score: f32,
    /// Why it scored that way.
    pub breakdown: ScoreBreakdown,
}
