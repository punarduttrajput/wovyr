//! Memory records, queries, and scoring types.

use serde::{Deserialize, Serialize};

/// The kind of memory, which governs recency decay
/// ([ranking §4](../../docs/06-memory-engine/ranking.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
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
    /// Content-derived access scopes a reader must hold to see this record
    /// ([ranking §8](../../docs/06-memory-engine/ranking.md)). Empty = public.
    #[serde(default)]
    pub required_scopes: Vec<String>,
    /// Whether `content` should be sealed at rest via `apex-kms` (tenant =
    /// `namespace`) — [Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption)'s
    /// "memory records flagged sensitive". A plain [`MemoryStore`](crate::MemoryStore)
    /// ignores this flag entirely; only
    /// [`EncryptingMemoryStore`](crate::EncryptingMemoryStore) acts on it.
    #[serde(default)]
    pub sensitive: bool,
    /// For a chunk record (RM-AIM-P2 RAG-201): the id of the parent document
    /// it was split from. `None` for ordinary memories and parent documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// True for a parent-document record: it holds the full document verbatim
    /// and exists only for expansion ([`MemoryQuery::expand_parents`]) — it is
    /// never returned as a direct retrieval hit (its one-vector embedding is
    /// exactly the diluted representation chunking exists to avoid).
    #[serde(default)]
    pub is_parent: bool,
    /// Monotonic insertion sequence (assigned by the store; used for recency).
    #[serde(default)]
    pub seq: u64,
}

/// The reader's access context for an ABAC policy pass: the scopes a principal
/// holds ([ranking §8](../../docs/06-memory-engine/ranking.md)). A record is
/// visible only if every one of its `required_scopes` is granted here.
#[derive(Debug, Clone, Default)]
pub struct AccessContext {
    /// Scopes the principal is granted.
    pub grants: Vec<String>,
}

impl AccessContext {
    /// An access context granting `grants`.
    pub fn new(grants: Vec<String>) -> Self {
        Self { grants }
    }
}

/// Which retrieval method to use ([retrieval §2](../../docs/06-memory-engine/retrieval.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
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
    /// Reader access context for ABAC filtering ([ranking §8](../../docs/06-memory-engine/ranking.md)).
    /// `None` grants nothing, so any scope-protected record is hidden (fail-closed);
    /// public records (no required scopes) are always visible.
    pub access: Option<AccessContext>,
    /// Parent-document expansion (RM-AIM-P2 RAG-201): when `true`, a result
    /// that is a chunk of a larger document gets its full parent document
    /// attached as [`ScoredMemory::parent`] — retrieval stays chunk-precise
    /// while the caller can ground on the whole document. The parent must
    /// itself pass the query's ABAC check (fail-closed). Off by default.
    pub expand_parents: bool,
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
            access: None,
            expand_parents: false,
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

/// Policy for a compaction pass ([compression](../../docs/06-memory-engine/overview.md)):
/// which memories to consolidate into a summary.
#[derive(Debug, Clone, Copy)]
pub struct CompactionPolicy {
    /// Only consolidate records with importance **below** this — high-value memories
    /// are kept verbatim.
    pub max_importance: f32,
    /// Never consolidate the most recent `keep_recent` records (by sequence).
    pub keep_recent: usize,
    /// Skip compaction unless at least this many candidates qualify.
    pub min_candidates: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            max_importance: 0.5,
            keep_recent: 5,
            min_candidates: 2,
        }
    }
}

/// The result of a compaction pass.
#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    /// Number of source records consolidated and removed.
    pub compacted: usize,
    /// Id of the new summary memory, if one was written.
    pub summary_id: Option<String>,
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
    /// The full parent document, when `record` is a chunk and the query set
    /// [`MemoryQuery::expand_parents`] (RM-AIM-P2 RAG-201). Absent otherwise —
    /// including when the parent exists but the query's access context does
    /// not pass its ABAC check (fail-closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<MemoryRecord>,
}

/// The outcome of ingesting a document via
/// [`MemoryEngine::remember_document`](crate::MemoryEngine::remember_document)
/// (RM-AIM-P2 RAG-201).
#[derive(Debug, Clone)]
pub struct DocumentIngest {
    /// Id of the stored record holding the full document. When the document
    /// fit in a single chunk window, this is an ordinary memory record (no
    /// linkage was needed) and `chunk_ids` is empty.
    pub parent_id: String,
    /// Ids of the chunk records linked to the parent, in document order.
    pub chunk_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RM-GA-P4 API-702: every `MemoryType` variant round-trips through JSON as
    /// `snake_case` — the platform-wide casing policy, even though it was
    /// already `rename_all = "lowercase"` before this change (identical output
    /// for these single-word variants; normalized for policy consistency with
    /// every other wire enum). `apex-server`'s response serialization used to
    /// re-derive this same string by hand via `{:?}` (Debug) + `to_lowercase()`
    /// instead of relying on this derive at all.
    #[test]
    fn memory_type_round_trips_as_snake_case_json() {
        let cases = [
            (MemoryType::Conversation, "\"conversation\""),
            (MemoryType::Workflow, "\"workflow\""),
            (MemoryType::Episodic, "\"episodic\""),
            (MemoryType::Semantic, "\"semantic\""),
        ];
        for (ty, wire) in cases {
            let json = serde_json::to_string(&ty).unwrap();
            assert_eq!(json, wire, "{ty:?} must serialize to {wire}");
            let back: MemoryType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ty, "{wire} must deserialize back to {ty:?}");
        }
    }
}
