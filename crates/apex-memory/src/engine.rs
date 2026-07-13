//! The memory engine: ingestion, hybrid retrieval, and ranking.

use crate::chunk::ChunkPolicy;
use crate::record::{
    CompactionOutcome, CompactionPolicy, DocumentIngest, MemoryQuery, MemoryRecord, MemoryType,
    RetrievalStrategy, ScoreBreakdown, ScoredMemory,
};
use crate::store::MemoryStore;
use apex_common::{Error, Result};
use apex_provider::{
    ChatRequest, EmbeddingRequest, Gateway, Message, ModelSelector, cosine_similarity,
};
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

    /// Embed `content` and store it as a (public) memory; returns the new record id.
    pub async fn remember(
        &self,
        namespace: impl Into<String>,
        content: impl Into<String>,
        memory_type: MemoryType,
        importance: f32,
        tags: Vec<String>,
    ) -> Result<String> {
        self.remember_scoped(
            namespace,
            content,
            memory_type,
            importance,
            tags,
            Vec::new(),
        )
        .await
    }

    /// Like [`Self::remember`], but the record is gated behind `required_scopes`: a
    /// query may only retrieve it if its access context grants all of them
    /// ([ranking §8](../../docs/06-memory-engine/ranking.md)).
    pub async fn remember_scoped(
        &self,
        namespace: impl Into<String>,
        content: impl Into<String>,
        memory_type: MemoryType,
        importance: f32,
        tags: Vec<String>,
        required_scopes: Vec<String>,
    ) -> Result<String> {
        self.remember_full(
            namespace,
            content,
            memory_type,
            importance,
            tags,
            required_scopes,
            false,
        )
        .await
    }

    /// Like [`Self::remember_scoped`], additionally marking the record
    /// `sensitive` — [Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption):
    /// a plain store ignores the flag, but an
    /// [`EncryptingMemoryStore`](crate::EncryptingMemoryStore) seals `content`
    /// through `apex-kms` before it reaches disk.
    #[allow(clippy::too_many_arguments)] // mirrors remember_scoped's existing positional-arg style
    pub async fn remember_full(
        &self,
        namespace: impl Into<String>,
        content: impl Into<String>,
        memory_type: MemoryType,
        importance: f32,
        tags: Vec<String>,
        required_scopes: Vec<String>,
        sensitive: bool,
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
            required_scopes,
            sensitive,
            parent_id: None,
            is_parent: false,
            seq: 0,
        };
        self.store.put(record).await
    }

    /// Ingest a **document** with chunking + parent linkage (RM-AIM-P2 RAG-201).
    ///
    /// Splits `content` into overlapping windows per `policy`
    /// ([`crate::chunk::split`]), stores the full document as a *parent*
    /// record (never a direct retrieval hit — see
    /// [`MemoryRecord::is_parent`]), and stores each chunk as its own
    /// embedded retrieval unit linked back via `parent_id`. Chunks inherit
    /// the document's metadata (type/importance/tags/scopes/`sensitive`), so
    /// ABAC and at-rest encryption apply to every piece identically. A
    /// document that fits one window is stored as an ordinary memory — no
    /// linkage overhead for short content.
    ///
    /// Chunk embeddings are computed in **one** batched gateway call.
    /// Ingestion is not transactional: a failure partway can leave a parent
    /// with fewer chunks than intended (the stored pieces remain valid).
    #[allow(clippy::too_many_arguments)] // mirrors remember_full's positional-arg style
    pub async fn remember_document(
        &self,
        namespace: impl Into<String>,
        content: impl Into<String>,
        memory_type: MemoryType,
        importance: f32,
        tags: Vec<String>,
        required_scopes: Vec<String>,
        sensitive: bool,
        policy: &ChunkPolicy,
    ) -> Result<DocumentIngest> {
        let namespace = namespace.into();
        let content = content.into();
        let chunks = crate::chunk::split(&content, policy);
        if chunks.len() <= 1 {
            let parent_id = self
                .remember_full(
                    namespace,
                    content,
                    memory_type,
                    importance,
                    tags,
                    required_scopes,
                    sensitive,
                )
                .await?;
            return Ok(DocumentIngest {
                parent_id,
                chunk_ids: Vec::new(),
            });
        }

        let importance = importance.clamp(0.0, 1.0);
        // The parent holds the verbatim document with no embedding: it is
        // excluded from retrieval by construction, so indexing its diluted
        // one-vector representation would only waste space (the tiered
        // backend skips the vector index entirely for parent records).
        let parent_id = self
            .store
            .put(MemoryRecord {
                id: String::new(),
                namespace: namespace.clone(),
                content,
                embedding: Vec::new(),
                memory_type,
                importance,
                tags: tags.clone(),
                required_scopes: required_scopes.clone(),
                sensitive,
                parent_id: None,
                is_parent: true,
                seq: 0,
            })
            .await?;

        let embeddings = self.embed_batch(&chunks).await?;
        let mut chunk_ids = Vec::with_capacity(chunks.len());
        for (chunk, embedding) in chunks.into_iter().zip(embeddings) {
            let id = self
                .store
                .put(MemoryRecord {
                    id: String::new(),
                    namespace: namespace.clone(),
                    content: chunk,
                    embedding,
                    memory_type,
                    importance,
                    tags: tags.clone(),
                    required_scopes: required_scopes.clone(),
                    sensitive,
                    parent_id: Some(parent_id.clone()),
                    is_parent: false,
                    seq: 0,
                })
                .await?;
            chunk_ids.push(id);
        }
        Ok(DocumentIngest {
            parent_id,
            chunk_ids,
        })
    }

    /// Consolidate stale, low-importance memories in `namespace` into a single
    /// summary memory, then delete the originals ([compression](../../docs/06-memory-engine/overview.md)).
    ///
    /// Candidates are records older than the `keep_recent` newest whose importance is
    /// below `max_importance`; the summary inherits the **union** of their tags and
    /// `required_scopes` (so access is never widened) and the highest importance. A
    /// no-op (returns `compacted: 0`) when fewer than `min_candidates` qualify.
    pub async fn compress(
        &self,
        namespace: &str,
        policy: CompactionPolicy,
    ) -> Result<CompactionOutcome> {
        let mut records = self.store.all(Some(namespace)).await?;
        records.sort_by_key(|r| r.seq);
        // Protect the most recent `keep_recent` from compaction. Document
        // records (parents and their chunks, RAG-201) are excluded outright:
        // compacting one half would tear the parent↔chunk linkage (dangling
        // `parent_id`s or an unreachable parent).
        let cutoff = records.len().saturating_sub(policy.keep_recent);
        let candidates: Vec<MemoryRecord> = records
            .into_iter()
            .take(cutoff)
            .filter(|r| {
                r.importance < policy.max_importance && !r.is_parent && r.parent_id.is_none()
            })
            .collect();

        if candidates.len() < policy.min_candidates {
            return Ok(CompactionOutcome {
                compacted: 0,
                summary_id: None,
            });
        }

        let contents: Vec<&str> = candidates.iter().map(|r| r.content.as_str()).collect();
        let summary = self.summarize(&contents).await?;

        // Merge metadata conservatively.
        let mut tags = BTreeSet::new();
        let mut scopes = BTreeSet::new();
        let mut importance = 0.0_f32;
        for r in &candidates {
            tags.extend(r.tags.iter().cloned());
            scopes.extend(r.required_scopes.iter().cloned());
            importance = importance.max(r.importance);
        }

        let summary_id = self
            .remember_scoped(
                namespace,
                summary,
                MemoryType::Semantic,
                importance,
                tags.into_iter().collect(),
                scopes.into_iter().collect(),
            )
            .await?;

        let ids: Vec<String> = candidates.iter().map(|r| r.id.clone()).collect();
        self.store.delete(&ids).await?;

        Ok(CompactionOutcome {
            compacted: candidates.len(),
            summary_id: Some(summary_id),
        })
    }

    /// Summarize memory contents into a single note via the gateway.
    async fn summarize(&self, contents: &[&str]) -> Result<String> {
        let joined = contents
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n");
        let model = self.gateway.resolve_model(None, &ModelSelector::default());
        let messages = vec![
            Message::system(
                "Summarize the following memories into a single concise note that \
                 preserves the key facts.",
            ),
            Message::user(joined),
        ];
        let resp = self.gateway.chat(ChatRequest::new(model, messages)).await?;
        Ok(resp.message.content.unwrap_or_default())
    }

    /// Retrieve and rank memories for a query.
    ///
    /// When the store has a purpose-built index ([`MemoryStore::supports_pushdown`]),
    /// candidate ids come from the store (vector ANN / keyword search); otherwise
    /// the engine scans all records and computes relevance in-process.
    pub async fn query(&self, q: &MemoryQuery) -> Result<Vec<ScoredMemory>> {
        let mut results = if self.store.supports_pushdown()
            && let Some(scored) = self.query_pushdown(q).await?
        {
            scored
        } else {
            self.query_in_process(q).await?
        };
        if q.expand_parents {
            self.attach_parents(&mut results, q).await?;
        }
        Ok(results)
    }

    /// Attach the full parent document to each chunk result (RM-AIM-P2
    /// RAG-201). A parent that fails the query's ABAC check is *not* attached
    /// (fail-closed), and a dangling `parent_id` (parent since deleted) is
    /// silently skipped — the chunk itself is still a valid result.
    async fn attach_parents(&self, results: &mut [ScoredMemory], q: &MemoryQuery) -> Result<()> {
        let ids: Vec<String> = results
            .iter()
            .filter_map(|r| r.record.parent_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let parents: HashMap<String, MemoryRecord> = self
            .store
            .get(&ids)
            .await?
            .into_iter()
            .filter(|p| abac_allows(p, q))
            .map(|p| (p.id.clone(), p))
            .collect();
        for r in results.iter_mut() {
            if let Some(pid) = &r.record.parent_id {
                r.parent = parents.get(pid).cloned();
            }
        }
        Ok(())
    }

    /// In-process retrieval: scan `all()` and score every candidate.
    async fn query_in_process(&self, q: &MemoryQuery) -> Result<Vec<ScoredMemory>> {
        let mut candidates = self.store.all(q.namespace.as_deref()).await?;
        candidates.retain(|r| passes_filters(r, q));
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let relevance = self.relevance(q, &candidates).await?;
        Ok(rank(candidates, &relevance, q))
    }

    /// Pushdown retrieval: ask the store's index for candidate ids per the strategy,
    /// fetch those records, then apply the weighted ranker. Returns `None` if the
    /// store cannot satisfy the requested strategy (caller falls back to in-process).
    async fn query_pushdown(&self, q: &MemoryQuery) -> Result<Option<Vec<ScoredMemory>>> {
        // Over-fetch so metadata filtering still leaves enough to rank.
        let k = q.limit.saturating_mul(4).max(50);
        let ns = q.namespace.as_deref();

        let relevance: HashMap<String, f32> = match q.strategy {
            RetrievalStrategy::Vector => {
                let qv = self.embed(&q.text).await?;
                match self.store.vector_search(ns, &qv, k).await? {
                    Some(hits) => normalize(hits),
                    None => return Ok(None),
                }
            }
            RetrievalStrategy::Keyword => match self.store.keyword_search(ns, &q.text, k).await? {
                Some(hits) => normalize(hits),
                None => return Ok(None),
            },
            RetrievalStrategy::Hybrid => {
                let qv = self.embed(&q.text).await?;
                let vector = self.store.vector_search(ns, &qv, k).await?;
                let keyword = self.store.keyword_search(ns, &q.text, k).await?;
                match (vector, keyword) {
                    (Some(v), Some(kw)) => reciprocal_rank_fusion(&[ids_of(&v), ids_of(&kw)]),
                    // Partial support → let the in-process path handle it.
                    _ => return Ok(None),
                }
            }
        };

        if relevance.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let ids: Vec<String> = relevance.keys().cloned().collect();
        let mut records = self.store.get(&ids).await?;
        records.retain(|r| passes_filters(r, q));
        Ok(Some(rank(records, &relevance, q)))
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

    /// Embed several strings in one gateway call (chunk ingestion, RAG-201).
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let model = self.gateway.resolve_embedding_model(None);
        let resp = self
            .gateway
            .embed(EmbeddingRequest::new(model, texts.to_vec()))
            .await?;
        if resp.vectors.len() != texts.len() {
            return Err(Error::provider(format!(
                "embedding response returned {} vectors for {} inputs",
                resp.vectors.len(),
                texts.len()
            )));
        }
        Ok(resp.vectors)
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

/// Whether a record passes the query's metadata filters (importance + tags) and the
/// ABAC access policy. Parent-document records (RAG-201) never pass: they exist
/// only for [`MemoryQuery::expand_parents`] expansion — their chunks are the
/// retrieval units.
fn passes_filters(r: &MemoryRecord, q: &MemoryQuery) -> bool {
    !r.is_parent
        && r.importance >= q.min_importance
        && (q.tags.is_empty() || q.tags.iter().any(|t| r.tags.contains(t)))
        && abac_allows(r, q)
}

/// ABAC policy pass ([ranking §8](../../docs/06-memory-engine/ranking.md)): a record
/// is visible only if the query's access context grants **every** scope the record
/// requires. Public records (no required scopes) always pass; a protected record with
/// no access context is denied (fail-closed).
fn abac_allows(r: &MemoryRecord, q: &MemoryQuery) -> bool {
    if r.required_scopes.is_empty() {
        return true;
    }
    match &q.access {
        Some(ctx) => r
            .required_scopes
            .iter()
            .all(|scope| ctx.grants.contains(scope)),
        None => false,
    }
}

/// Apply the weighted ranker (relevance + recency + importance) to `records` and
/// return them best-first, truncated to the query limit. When `q.diversity > 0`,
/// the final selection is diversified with MMR ([ranking §7](../../docs/06-memory-engine/ranking.md)).
fn rank(
    records: Vec<MemoryRecord>,
    relevance: &HashMap<String, f32>,
    q: &MemoryQuery,
) -> Vec<ScoredMemory> {
    let max_seq = records.iter().map(|r| r.seq).max().unwrap_or(0);
    let mut scored: Vec<ScoredMemory> = records
        .into_iter()
        .map(|r| {
            let rel = relevance.get(&r.id).copied().unwrap_or(0.0);
            let rec = recency_decay(max_seq.saturating_sub(r.seq), r.memory_type);
            let imp = r.importance;
            let total =
                q.weights.relevance * rel + q.weights.recency * rec + q.weights.importance * imp;
            ScoredMemory {
                breakdown: ScoreBreakdown {
                    relevance: rel,
                    recency: rec,
                    importance: imp,
                    total,
                },
                score: total,
                record: r,
                parent: None,
            }
        })
        .collect();
    // Deterministic base order: score desc, then id asc as a tiebreaker.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record.id.cmp(&b.record.id))
    });

    let limit = q.limit.max(1);
    if q.diversity > 0.0 {
        mmr_select(scored, q.diversity.clamp(0.0, 1.0), limit)
    } else {
        scored.truncate(limit);
        scored
    }
}

/// Greedily reorder `scored` (already in deterministic relevance order) by **Maximal
/// Marginal Relevance**: each pick maximizes `λ·score − (1−λ)·maxSim`, where `λ =
/// 1 − diversity` and `maxSim` is the cosine similarity to the most similar
/// already-selected memory. Picks `limit` results; ties favor the relevance order.
fn mmr_select(mut remaining: Vec<ScoredMemory>, diversity: f32, limit: usize) -> Vec<ScoredMemory> {
    let lambda = 1.0 - diversity;
    let mut selected: Vec<ScoredMemory> = Vec::with_capacity(limit.min(remaining.len()));
    while !remaining.is_empty() && selected.len() < limit {
        let mut best_idx = 0;
        let mut best_mmr = f32::NEG_INFINITY;
        for (i, cand) in remaining.iter().enumerate() {
            let max_sim = selected
                .iter()
                .map(|s| cosine_similarity(&cand.record.embedding, &s.record.embedding))
                .fold(0.0_f32, f32::max);
            let mmr = lambda * cand.score - (1.0 - lambda) * max_sim;
            // Strict `>` keeps the first (highest-relevance) candidate on ties, so
            // the result stays deterministic for the sorted input.
            if mmr > best_mmr {
                best_mmr = mmr;
                best_idx = i;
            }
        }
        selected.push(remaining.remove(best_idx));
    }
    selected
}

/// Extract the ranked id list (best-first) from pushdown hits.
fn ids_of(hits: &[(String, f32)]) -> Vec<String> {
    hits.iter().map(|(id, _)| id.clone()).collect()
}

/// Normalize pushdown hit scores into a `[0,1]` relevance map (divide by the max).
fn normalize(hits: Vec<(String, f32)>) -> HashMap<String, f32> {
    let max = hits.iter().map(|(_, s)| *s).fold(0.0_f32, f32::max);
    hits.into_iter()
        .map(|(id, s)| (id, if max > 0.0 { s / max } else { 0.0 }))
        .collect()
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

    // --- MMR diversification -------------------------------------------------

    use crate::record::RankingWeights;

    fn rec(id: &str, embedding: Vec<f32>) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            namespace: "kb".to_string(),
            content: id.to_string(),
            embedding,
            memory_type: MemoryType::Semantic, // recency_decay == 1.0 (no decay)
            importance: 0.0,
            tags: Vec::new(),
            required_scopes: Vec::new(),
            sensitive: false,
            parent_id: None,
            is_parent: false,
            seq: 0,
        }
    }

    fn scored(id: &str, score: f32, embedding: Vec<f32>) -> ScoredMemory {
        ScoredMemory {
            record: rec(id, embedding),
            score,
            breakdown: ScoreBreakdown {
                relevance: score,
                recency: 0.0,
                importance: 0.0,
                total: score,
            },
            parent: None,
        }
    }

    fn ids(results: &[ScoredMemory]) -> Vec<&str> {
        results.iter().map(|r| r.record.id.as_str()).collect()
    }

    #[test]
    fn mmr_demotes_near_duplicate_for_a_diverse_candidate() {
        // a (best) and b (near-duplicate of a) vs c (orthogonal/diverse). Pure
        // relevance returns [a, b]; MMR should fill the 2nd slot with the diverse c.
        let scored = vec![
            scored("a", 0.90, vec![1.0, 0.0, 0.0]),
            scored("b", 0.85, vec![1.0, 0.0, 0.0]),
            scored("c", 0.70, vec![0.0, 1.0, 0.0]),
        ];
        let out = mmr_select(scored, 0.5, 2);
        assert_eq!(
            ids(&out),
            vec!["a", "c"],
            "near-duplicate b should be demoted"
        );
    }

    #[test]
    fn mmr_with_zero_diversity_is_pure_relevance() {
        let scored = vec![
            scored("a", 0.90, vec![1.0, 0.0, 0.0]),
            scored("b", 0.85, vec![1.0, 0.0, 0.0]),
            scored("c", 0.70, vec![0.0, 1.0, 0.0]),
        ];
        // diversity 0 → λ=1 → ranks by score only, keeping the near-duplicate.
        let out = mmr_select(scored, 0.0, 2);
        assert_eq!(ids(&out), vec!["a", "b"]);
    }

    #[test]
    fn rank_diversifies_only_when_diversity_is_set() {
        let records = vec![
            rec("a", vec![1.0, 0.0, 0.0]),
            rec("b", vec![1.0, 0.0, 0.0]),
            rec("c", vec![0.0, 1.0, 0.0]),
        ];
        let relevance: HashMap<String, f32> = [("a", 1.0), ("b", 0.95), ("c", 0.60)]
            .into_iter()
            .map(|(id, s)| (id.to_string(), s))
            .collect();

        // Isolate relevance: weight recency/importance to zero so score == relevance.
        let mut q = MemoryQuery::new("x");
        q.limit = 2;
        q.weights = RankingWeights {
            relevance: 1.0,
            recency: 0.0,
            importance: 0.0,
        };

        // Default (diversity 0): pure relevance keeps the near-duplicate b.
        let base = rank(records.clone(), &relevance, &q);
        assert_eq!(ids(&base), vec!["a", "b"]);

        // Diversity on: the orthogonal c displaces the near-duplicate b.
        q.diversity = 0.6;
        let diversified = rank(records, &relevance, &q);
        assert_eq!(ids(&diversified), vec!["a", "c"]);
    }

    // --- ABAC access filtering -----------------------------------------------

    use crate::record::AccessContext;

    /// Seed one public memory and one gated behind the `pii` scope.
    async fn seed_abac(eng: &MemoryEngine) {
        eng.remember(
            "kb",
            "office is in Berlin",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
        eng.remember_scoped(
            "kb",
            "customer SSN is 123",
            MemoryType::Semantic,
            0.9,
            vec![],
            vec!["pii".to_string()],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn abac_hides_protected_records_without_grants() {
        let eng = engine();
        seed_abac(&eng).await;

        // No access context → the pii-gated record is invisible (fail-closed).
        let mut q = MemoryQuery::new("customer");
        q.namespace = Some("kb".into());
        let results = eng.query(&q).await.unwrap();
        assert!(
            results.iter().all(|r| !r.record.content.contains("SSN")),
            "protected record must not surface without grants"
        );
        // The public record is still retrievable.
        assert!(results.iter().any(|r| r.record.content.contains("Berlin")));
    }

    #[tokio::test]
    async fn abac_reveals_protected_records_with_matching_grant() {
        let eng = engine();
        seed_abac(&eng).await;

        let mut q = MemoryQuery::new("customer SSN");
        q.namespace = Some("kb".into());
        q.access = Some(AccessContext::new(vec!["pii".to_string()]));
        let results = eng.query(&q).await.unwrap();
        assert!(
            results.iter().any(|r| r.record.content.contains("SSN")),
            "granting `pii` must reveal the protected record"
        );
    }

    #[test]
    fn abac_requires_all_scopes() {
        // A record requiring two scopes is denied unless the context grants both.
        let mut r = rec("x", vec![1.0]);
        r.required_scopes = vec!["pii".into(), "legal".into()];

        let mut q = MemoryQuery::new("x");
        q.access = Some(AccessContext::new(vec!["pii".into()]));
        assert!(!abac_allows(&r, &q), "missing `legal` scope must deny");

        q.access = Some(AccessContext::new(vec!["pii".into(), "legal".into()]));
        assert!(abac_allows(&r, &q), "both scopes granted must allow");
    }

    // --- Document chunking + parent linkage (RM-AIM-P2 RAG-201) ---------------

    /// A two-topic document long enough to split at the test's window size:
    /// a refund-policy section followed by an office-logistics section.
    fn two_topic_document() -> String {
        let refunds = "Refunds are honored within a thirty day refund window. \
                       A refund request needs the original receipt. Refund \
                       processing takes five business days once approved."
            .to_string();
        let office = "The office is located in Berlin near the station. \
                      Visitors must sign in at the front desk on arrival. \
                      Parking spaces are available in the basement garage.";
        format!("{refunds} {office}")
    }

    fn doc_policy() -> ChunkPolicy {
        // Small windows so the two topics land in different chunks.
        ChunkPolicy {
            max_chars: 160,
            overlap_chars: 20,
        }
    }

    async fn ingest_two_topic_doc(eng: &MemoryEngine) -> DocumentIngest {
        eng.remember_document(
            "kb",
            two_topic_document(),
            MemoryType::Semantic,
            0.5,
            vec![],
            vec![],
            false,
            &doc_policy(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_long_document_is_split_into_linked_chunks() {
        let eng = engine();
        let ingest = ingest_two_topic_doc(&eng).await;

        assert!(
            ingest.chunk_ids.len() > 1,
            "expected multiple chunks, got {}",
            ingest.chunk_ids.len()
        );
        let all = eng.store.all(Some("kb")).await.unwrap();
        let parent = all.iter().find(|r| r.id == ingest.parent_id).unwrap();
        assert!(parent.is_parent, "the document record is marked as parent");
        assert_eq!(
            parent.content,
            two_topic_document(),
            "parent holds the full document verbatim"
        );
        for cid in &ingest.chunk_ids {
            let chunk = all.iter().find(|r| &r.id == cid).unwrap();
            assert_eq!(
                chunk.parent_id.as_ref(),
                Some(&ingest.parent_id),
                "every chunk links back to the parent"
            );
            assert!(!chunk.is_parent);
            assert!(!chunk.embedding.is_empty(), "chunks are embedded");
        }
    }

    /// RAG-201 acceptance: retrieval scores the relevant chunk above an
    /// irrelevant chunk from the same document.
    #[tokio::test]
    async fn retrieval_scores_the_relevant_chunk_above_an_irrelevant_one() {
        let eng = engine();
        let ingest = ingest_two_topic_doc(&eng).await;

        let mut q = MemoryQuery::new("what is the refund window");
        q.namespace = Some("kb".into());
        let results = eng.query(&q).await.unwrap();

        assert!(!results.is_empty());
        let top = &results[0];
        assert!(
            top.record.content.contains("refund"),
            "the refund chunk must rank first, got: {}",
            top.record.content
        );
        assert!(
            ingest.chunk_ids.contains(&top.record.id),
            "the top hit is one of the document's chunks"
        );
        // The office chunk (same document, different topic) scores lower.
        let office = results.iter().find(|r| r.record.content.contains("Berlin"));
        if let Some(office) = office {
            assert!(
                top.score > office.score,
                "relevant chunk ({}) must outscore the irrelevant one ({})",
                top.score,
                office.score
            );
        }
    }

    #[tokio::test]
    async fn parent_documents_never_surface_as_direct_hits() {
        let eng = engine();
        let ingest = ingest_two_topic_doc(&eng).await;

        // Even a query matching the whole document never returns the parent.
        let mut q = MemoryQuery::new("refund window office Berlin");
        q.namespace = Some("kb".into());
        q.limit = 50;
        let results = eng.query(&q).await.unwrap();
        assert!(!results.is_empty());
        assert!(
            results.iter().all(|r| r.record.id != ingest.parent_id),
            "the parent document must be expansion-only"
        );
    }

    #[tokio::test]
    async fn expand_parents_attaches_the_full_document() {
        let eng = engine();
        let ingest = ingest_two_topic_doc(&eng).await;

        let mut q = MemoryQuery::new("what is the refund window");
        q.namespace = Some("kb".into());

        // Off by default: no parent attached.
        let plain = eng.query(&q).await.unwrap();
        assert!(plain[0].parent.is_none());

        // Opted in: the chunk hit carries the full parent document.
        q.expand_parents = true;
        let expanded = eng.query(&q).await.unwrap();
        let top = &expanded[0];
        let parent = top.parent.as_ref().expect("parent attached");
        assert_eq!(parent.id, ingest.parent_id);
        assert_eq!(parent.content, two_topic_document());
    }

    #[tokio::test]
    async fn parent_expansion_is_abac_fail_closed() {
        let eng = engine();
        // Craft a pathological store state directly: a public chunk whose
        // parent is scope-protected (normal ingestion gives both the same
        // scopes; this guards the expansion path itself).
        let mut parent = rec("ignored", vec![1.0, 0.0]);
        parent.content = "full secret document".into();
        parent.is_parent = true;
        parent.required_scopes = vec!["pii".into()];
        let parent_id = eng.store.put(parent).await.unwrap();

        let mut chunk = rec("ignored", vec![1.0, 0.0]);
        chunk.content = "public chunk about refunds".into();
        chunk.parent_id = Some(parent_id);
        eng.store.put(chunk).await.unwrap();

        let mut q = MemoryQuery::new("refunds");
        q.namespace = Some("kb".into());
        q.expand_parents = true;
        let results = eng.query(&q).await.unwrap();
        assert!(!results.is_empty());
        assert!(
            results[0].parent.is_none(),
            "an ungranted parent must not be attached (fail-closed)"
        );

        // With the grant, the same query attaches it.
        q.access = Some(AccessContext::new(vec!["pii".into()]));
        let granted = eng.query(&q).await.unwrap();
        assert!(granted[0].parent.is_some());
    }

    #[tokio::test]
    async fn a_short_document_stores_as_an_ordinary_memory() {
        let eng = engine();
        let ingest = eng
            .remember_document(
                "kb",
                "a short note that fits one window",
                MemoryType::Semantic,
                0.5,
                vec![],
                vec![],
                false,
                &ChunkPolicy::default(),
            )
            .await
            .unwrap();
        assert!(ingest.chunk_ids.is_empty(), "no chunks for a short doc");
        let all = eng.store.all(Some("kb")).await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(!all[0].is_parent, "no linkage overhead for short content");
        assert!(all[0].parent_id.is_none());
    }

    #[tokio::test]
    async fn compress_leaves_document_records_alone() {
        let eng = engine();
        ingest_two_topic_doc(&eng).await;
        // Low importance + keep_recent 0 would compact everything if document
        // records were eligible.
        let eng2 = &eng;
        let outcome = eng2
            .compress(
                "kb",
                CompactionPolicy {
                    max_importance: 1.0,
                    keep_recent: 0,
                    min_candidates: 1,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.compacted, 0,
            "parents and chunks must be excluded from compaction"
        );
    }

    // --- Compression / compaction -------------------------------------------

    use crate::record::CompactionPolicy;

    #[tokio::test]
    async fn compress_consolidates_stale_low_importance_memories() {
        let eng = engine();
        // 4 stale low-importance memories + 1 recent high-importance one.
        for i in 0..4 {
            eng.remember(
                "kb",
                format!("trivia {i}"),
                MemoryType::Episodic,
                0.2,
                vec![],
            )
            .await
            .unwrap();
        }
        eng.remember("kb", "critical fact", MemoryType::Semantic, 0.9, vec![])
            .await
            .unwrap();

        let policy = CompactionPolicy {
            max_importance: 0.5,
            keep_recent: 1, // protect only the newest
            min_candidates: 2,
        };
        let outcome = eng.compress("kb", policy).await.unwrap();
        assert_eq!(outcome.compacted, 4, "all 4 stale trivia consolidated");
        assert!(outcome.summary_id.is_some());

        // After compaction: the summary + the protected recent record remain; the
        // four originals are gone.
        let mut all = eng.store.all(Some("kb")).await.unwrap();
        all.sort_by_key(|r| r.seq);
        let contents: Vec<&str> = all.iter().map(|r| r.content.as_str()).collect();
        assert!(contents.contains(&"critical fact"), "recent record kept");
        assert!(
            !contents.iter().any(|c| c.starts_with("trivia")),
            "originals were deleted, got {contents:?}"
        );
        assert!(
            all.iter()
                .any(|r| Some(&r.id) == outcome.summary_id.as_ref())
        );
    }

    #[tokio::test]
    async fn compress_is_a_noop_below_min_candidates() {
        let eng = engine();
        eng.remember("kb", "lonely trivia", MemoryType::Episodic, 0.2, vec![])
            .await
            .unwrap();
        let policy = CompactionPolicy {
            max_importance: 0.5,
            keep_recent: 0,
            min_candidates: 2,
        };
        let outcome = eng.compress("kb", policy).await.unwrap();
        assert_eq!(outcome.compacted, 0);
        assert!(outcome.summary_id.is_none());
        assert_eq!(eng.store.all(Some("kb")).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn compress_unions_required_scopes_so_access_is_not_widened() {
        let eng = engine();
        eng.remember_scoped(
            "kb",
            "public-ish trivia",
            MemoryType::Episodic,
            0.1,
            vec![],
            vec![],
        )
        .await
        .unwrap();
        eng.remember_scoped(
            "kb",
            "pii trivia",
            MemoryType::Episodic,
            0.1,
            vec![],
            vec!["pii".to_string()],
        )
        .await
        .unwrap();

        let policy = CompactionPolicy {
            max_importance: 0.5,
            keep_recent: 0,
            min_candidates: 2,
        };
        let outcome = eng.compress("kb", policy).await.unwrap();
        let summary_id = outcome.summary_id.unwrap();
        let summary = eng.store.get(&[summary_id]).await.unwrap().pop().unwrap();
        assert!(
            summary.required_scopes.contains(&"pii".to_string()),
            "summary must inherit the pii scope so it stays protected"
        );
    }
}
