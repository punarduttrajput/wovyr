//! The memory engine: ingestion, hybrid retrieval, and ranking.

use crate::record::{
    MemoryQuery, MemoryRecord, MemoryType, RetrievalStrategy, ScoreBreakdown, ScoredMemory,
};
use crate::store::MemoryStore;
use apex_common::{Error, Result};
use apex_provider::{EmbeddingRequest, Gateway, cosine_similarity};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// RRF smoothing constant ([retrieval §4](../../docs/06-memory-engine/retrieval.md)).
const RRF_K: f32 = 60.0;

/// Ties memory storage to embeddings (via the [`Gateway`]) and serves ranked
/// hybrid retrieval.
pub struct MemoryEngine {
    gateway: Gateway,
    store: Arc<dyn MemoryStore>,
}

impl MemoryEngine {
    /// Build an engine over a gateway (for embeddings) and a store.
    pub fn new(gateway: Gateway, store: Arc<dyn MemoryStore>) -> Self {
        Self { gateway, store }
    }

    /// Embed `content` and store it as a memory; returns the new record id.
    pub async fn remember(
        &self,
        namespace: impl Into<String>,
        content: impl Into<String>,
        memory_type: MemoryType,
        importance: f32,
        tags: Vec<String>,
    ) -> Result<String> {
        let content = content.into();
        let embedding = self.embed(&content).await?;
        let record = MemoryRecord {
            id: String::new(),
            namespace: namespace.into(),
            content,
            embedding,
            memory_type,
            importance: importance.clamp(0.0, 1.0),
            tags,
            seq: 0,
        };
        self.store.put(record).await
    }

    /// Retrieve and rank memories for a query.
    pub async fn query(&self, q: &MemoryQuery) -> Result<Vec<ScoredMemory>> {
        let mut candidates = self.store.all(q.namespace.as_deref()).await?;
        candidates.retain(|r| {
            r.importance >= q.min_importance
                && (q.tags.is_empty() || q.tags.iter().any(|t| r.tags.contains(t)))
        });
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let relevance = self.relevance(q, &candidates).await?;
        let max_seq = candidates.iter().map(|r| r.seq).max().unwrap_or(0);

        let mut scored: Vec<ScoredMemory> = candidates
            .into_iter()
            .map(|r| {
                let rel = relevance.get(&r.id).copied().unwrap_or(0.0);
                let rec = recency_decay(max_seq.saturating_sub(r.seq), r.memory_type);
                let imp = r.importance;
                let total = q.weights.relevance * rel
                    + q.weights.recency * rec
                    + q.weights.importance * imp;
                ScoredMemory {
                    breakdown: ScoreBreakdown {
                        relevance: rel,
                        recency: rec,
                        importance: imp,
                        total,
                    },
                    score: total,
                    record: r,
                }
            })
            .collect();

        // Deterministic ordering: score desc, then id asc as a tiebreaker.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.record.id.cmp(&b.record.id))
        });
        scored.truncate(q.limit.max(1));
        Ok(scored)
    }

    /// Compute the normalized relevance of each candidate per the query strategy.
    async fn relevance(
        &self,
        q: &MemoryQuery,
        candidates: &[MemoryRecord],
    ) -> Result<HashMap<String, f32>> {
        match q.strategy {
            RetrievalStrategy::Keyword => Ok(keyword_relevance(&q.text, candidates)),
            RetrievalStrategy::Vector => {
                let qv = self.embed(&q.text).await?;
                Ok(vector_relevance(&qv, candidates))
            }
            RetrievalStrategy::Hybrid => {
                let qv = self.embed(&q.text).await?;
                let vlist = ranked_ids(vector_relevance(&qv, candidates));
                let klist = ranked_ids(keyword_relevance(&q.text, candidates));
                Ok(reciprocal_rank_fusion(&[vlist, klist]))
            }
        }
    }

    /// Embed a single string via the gateway.
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let model = self.gateway.resolve_embedding_model(None);
        let resp = self
            .gateway
            .embed(EmbeddingRequest::new(model, vec![text.to_string()]))
            .await?;
        resp.vectors
            .into_iter()
            .next()
            .ok_or_else(|| Error::provider("embedding response was empty"))
    }
}

/// Normalized cosine relevance per record (`(cos + 1) / 2` → `[0,1]`).
fn vector_relevance(query: &[f32], candidates: &[MemoryRecord]) -> HashMap<String, f32> {
    candidates
        .iter()
        .map(|r| {
            let cos = cosine_similarity(query, &r.embedding);
            (r.id.clone(), (cos + 1.0) / 2.0)
        })
        .collect()
}

/// Keyword relevance from query-term overlap, normalized by the best overlap.
fn keyword_relevance(query: &str, candidates: &[MemoryRecord]) -> HashMap<String, f32> {
    let q_terms = tokenize(query);
    let overlaps: Vec<(String, f32)> = candidates
        .iter()
        .map(|r| {
            let terms = tokenize(&r.content);
            let overlap = q_terms.iter().filter(|t| terms.contains(*t)).count() as f32;
            (r.id.clone(), overlap)
        })
        .collect();
    let max = overlaps.iter().map(|(_, o)| *o).fold(0.0_f32, f32::max);
    overlaps
        .into_iter()
        .map(|(id, o)| (id, if max > 0.0 { o / max } else { 0.0 }))
        .collect()
}

/// Sort candidate ids by score descending (id asc tiebreak) for fusion.
fn ranked_ids(scores: HashMap<String, f32>) -> Vec<String> {
    let mut pairs: Vec<(String, f32)> = scores.into_iter().collect();
    pairs.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    pairs.into_iter().map(|(id, _)| id).collect()
}

/// Reciprocal Rank Fusion over several ranked id lists, normalized to `[0,1]`.
fn reciprocal_rank_fusion(lists: &[Vec<String>]) -> HashMap<String, f32> {
    let mut fused: HashMap<String, f32> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *fused.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + (rank as f32 + 1.0));
        }
    }
    let max = fused.values().copied().fold(0.0_f32, f32::max);
    if max > 0.0 {
        for v in fused.values_mut() {
            *v /= max;
        }
    }
    fused
}

/// Exponential recency decay using sequence distance as a deterministic age proxy.
fn recency_decay(age: u64, memory_type: MemoryType) -> f32 {
    match memory_type.half_life() {
        None => 1.0, // semantic facts do not decay
        Some(half_life) => (-(age as f32) / half_life).exp(),
    }
}

/// Lowercase alphanumeric tokenization into a set.
fn tokenize(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{MemoryQuery, RetrievalStrategy};
    use crate::store::InMemoryStore;
    use apex_provider::MockProvider;

    fn engine() -> MemoryEngine {
        let gateway = Gateway::new(Box::new(MockProvider::new()));
        MemoryEngine::new(gateway, Arc::new(InMemoryStore::new()))
    }

    #[tokio::test]
    async fn retrieves_most_relevant_record_first() {
        let eng = engine();
        eng.remember(
            "kb",
            "The refund window is 30 days.",
            MemoryType::Semantic,
            0.9,
            vec![],
        )
        .await
        .unwrap();
        eng.remember(
            "kb",
            "Our office is in Berlin.",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
        eng.remember(
            "kb",
            "Support hours are 9 to 5.",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();

        let mut q = MemoryQuery::new("what is the refund window");
        q.namespace = Some("kb".into());
        let results = eng.query(&q).await.unwrap();

        assert!(!results.is_empty());
        assert!(
            results[0].record.content.contains("refund"),
            "expected the refund memory to rank first, got: {}",
            results[0].record.content
        );
        assert!(results[0].breakdown.total > 0.0);
    }

    #[tokio::test]
    async fn metadata_filters_by_tag_and_importance() {
        let eng = engine();
        eng.remember(
            "kb",
            "tagged fact",
            MemoryType::Semantic,
            0.9,
            vec!["policy".into()],
        )
        .await
        .unwrap();
        eng.remember("kb", "untagged trivia", MemoryType::Semantic, 0.1, vec![])
            .await
            .unwrap();

        let mut q = MemoryQuery::new("fact");
        q.namespace = Some("kb".into());
        q.tags = vec!["policy".into()];
        let results = eng.query(&q).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.content, "tagged fact");
    }

    #[tokio::test]
    async fn keyword_strategy_works_without_semantic_embeddings() {
        let eng = engine();
        eng.remember("kb", "alpha beta gamma", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
        eng.remember("kb", "delta epsilon", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();

        let mut q = MemoryQuery::new("gamma");
        q.namespace = Some("kb".into());
        q.strategy = RetrievalStrategy::Keyword;
        let results = eng.query(&q).await.unwrap();
        assert_eq!(results[0].record.content, "alpha beta gamma");
    }
}
