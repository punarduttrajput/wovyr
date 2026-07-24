//! The memory engine: ingestion, hybrid retrieval, and ranking.

use crate::chunk::ChunkPolicy;
use crate::clock::{Clock, SystemClock};
use crate::record::{
    CompactionOutcome, CompactionPolicy, DocumentIngest, EmbeddingMigrationReport, MemoryQuery,
    MemoryRecord, MemoryType, RetrievalStrategy, ScoreBreakdown, ScoredMemory,
};
use crate::rerank::Reranker;
use crate::store::MemoryStore;
use apex_common::{Error, Result};
use apex_provider::{
    ChatRequest, EmbeddingRequest, Gateway, Message, ModelSelector, cosine_similarity,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Default RRF smoothing constant ([retrieval §4](../../docs/06-memory-engine/retrieval.md));
/// override per engine via [`MemoryEngine::with_rrf_k`] (RM-AIM-P2 RAG-202).
const DEFAULT_RRF_K: f32 = 60.0;

/// Default cap on how many fused candidates the optional [`Reranker`] re-scores.
const DEFAULT_RERANK_TOP_N: usize = 20;

/// Ties memory storage to embeddings (via the [`Gateway`]) and serves ranked
/// hybrid retrieval.
pub struct MemoryEngine {
    gateway: Gateway,
    store: Arc<dyn MemoryStore>,
    /// Optional second-stage reranker over the fused top-N (RAG-202). `None`
    /// (the default) keeps single-stage retrieval exactly as before.
    reranker: Option<Arc<dyn Reranker>>,
    /// How many fused candidates the reranker re-scores (top-N by fused
    /// relevance; the rest keep their fused scores).
    rerank_top_n: usize,
    /// RRF smoothing constant used by hybrid fusion.
    rrf_k: f32,
    /// Wall-clock source (RM-AIM-P2 RAG-205), read only at the boundaries:
    /// ingestion stamps `created_ms`, a query reads "now" once for recency +
    /// time filters. [`SystemClock`] by default; injectable for tests.
    clock: Arc<dyn Clock>,
}

impl MemoryEngine {
    /// Build an engine over a gateway (for embeddings) and a store.
    ///
    /// Does **not** check that the gateway can embed — callers that construct
    /// with a known embedding-capable gateway (tests, internal use) use this.
    /// Deployment wiring (server/CLI) should prefer [`try_new`](Self::try_new),
    /// which fails loud when no embedding provider is configured.
    pub fn new(gateway: Gateway, store: Arc<dyn MemoryStore>) -> Self {
        Self {
            gateway,
            store,
            reranker: None,
            rerank_top_n: DEFAULT_RERANK_TOP_N,
            rrf_k: DEFAULT_RRF_K,
            clock: Arc::new(SystemClock),
        }
    }

    /// Build an engine, failing closed (`Error::Config`) when the gateway has no
    /// embedding provider (RM-AR-P1 AIC-301). Memory ingestion and retrieval both
    /// embed, so an embedding-less deployment (e.g. Anthropic-only, no
    /// `OPENAI_API_KEY`) cannot serve memory at all; this surfaces that at
    /// construction/startup with an actionable message instead of erroring deep
    /// inside the first `remember`/`query`.
    pub fn try_new(gateway: Gateway, store: Arc<dyn MemoryStore>) -> Result<Self> {
        if !gateway.supports_embeddings() {
            return Err(Error::config(format!(
                "memory/RAG requires an embedding provider, but the configured LLM \
                 provider `{}` cannot embed. Set OPENAI_API_KEY (its provider serves both \
                 chat and embeddings), attach a dedicated embedding provider, or run \
                 without a memory-enabled agent (RM-AR-P1 AIC-301).",
                gateway.provider_name()
            )));
        }
        Ok(Self::new(gateway, store))
    }

    /// Inject a wall-clock source (RM-AIM-P2 RAG-205); tests use
    /// [`ManualClock`](crate::ManualClock) for deterministic timestamps and
    /// recency.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Attach a second-stage [`Reranker`] (RM-AIM-P2 RAG-202) applied to the
    /// fused top-N candidates before the weighted ranker. Opt-in: without
    /// this, retrieval behavior is unchanged. A reranker failure degrades to
    /// the fused order with a warning (availability over quality — the same
    /// stance as the gateway's semantic-cache degradation), never a failed
    /// query.
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Cap how many fused candidates the reranker re-scores (default 20).
    /// The effective N is never below the query's own `limit`.
    pub fn with_rerank_top_n(mut self, top_n: usize) -> Self {
        self.rerank_top_n = top_n.max(1);
        self
    }

    /// Override the RRF smoothing constant `k` (default 60) used by hybrid
    /// fusion: a smaller `k` weights top ranks more heavily (RM-AIM-P2
    /// RAG-202 — previously hardcoded).
    pub fn with_rrf_k(mut self, k: f32) -> Self {
        self.rrf_k = k.max(0.0);
        self
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
            embedding_model: self.gateway.resolve_embedding_model(None),
            memory_type,
            importance: importance.clamp(0.0, 1.0),
            tags,
            required_scopes,
            sensitive,
            parent_id: None,
            is_parent: false,
            created_ms: self.clock.now_ms(),
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
        // One clock read for the whole document: the parent and every chunk
        // share a single creation instant.
        let created_ms = self.clock.now_ms();
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
                // Non-embedded by construction — no model to attribute.
                embedding_model: String::new(),
                memory_type,
                importance,
                tags: tags.clone(),
                required_scopes: required_scopes.clone(),
                sensitive,
                parent_id: None,
                is_parent: true,
                created_ms,
                seq: 0,
            })
            .await?;

        let embeddings = self.embed_batch(&chunks).await?;
        let embedding_model = self.gateway.resolve_embedding_model(None);
        let mut chunk_ids = Vec::with_capacity(chunks.len());
        for (chunk, embedding) in chunks.into_iter().zip(embeddings) {
            let id = self
                .store
                .put(MemoryRecord {
                    id: String::new(),
                    namespace: namespace.clone(),
                    content: chunk,
                    embedding,
                    embedding_model: embedding_model.clone(),
                    memory_type,
                    importance,
                    tags: tags.clone(),
                    required_scopes: required_scopes.clone(),
                    sensitive,
                    parent_id: Some(parent_id.clone()),
                    is_parent: false,
                    created_ms,
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

    /// Migrate `namespace`'s embeddings to the gateway's **current** embedding
    /// model (RM-AIM-P3 RAG-301).
    ///
    /// Incremental by design: only *stale* records are touched — a record whose
    /// [`embedding_model`](MemoryRecord::embedding_model) already matches the
    /// current model is skipped, so re-running after a partial failure (or on a
    /// cron cadence after a model change) only pays for what's left. A record
    /// with an **empty** model id (written before model ids were recorded) is
    /// treated as stale rather than assumed current — an unknown provenance
    /// can't be trusted to share the new model's vector space. Parent documents
    /// (non-embedded by construction) are skipped.
    ///
    /// Each migrated record is re-embedded from its stored `content` (batched
    /// gateway calls) and rewritten **in place** via [`MemoryStore::update`] —
    /// same `id`/`seq`/timestamps, so chunk→parent links and recency ordering
    /// survive. Requires a store with an in-place write path (the in-memory and
    /// file stores; a store without one fails closed on the first write).
    /// After a run that reports no failures, every embedded record in the
    /// namespace carries the same model id — and therefore one uniform
    /// dimensionality.
    pub async fn migrate_embeddings(&self, namespace: &str) -> Result<EmbeddingMigrationReport> {
        /// Records re-embedded per gateway call — bounds request size while
        /// still amortizing the per-call overhead.
        const BATCH: usize = 32;

        let target_model = self.gateway.resolve_embedding_model(None);
        let records = self.store.all(Some(namespace)).await?;

        let mut report = EmbeddingMigrationReport {
            namespace: namespace.to_string(),
            target_model: target_model.clone(),
            scanned: records.len(),
            migrated: 0,
            already_current: 0,
            parents_skipped: 0,
        };
        let stale: Vec<MemoryRecord> = records
            .into_iter()
            .filter(|r| {
                if r.is_parent {
                    report.parents_skipped += 1;
                    false
                } else if r.embedding_model == target_model {
                    report.already_current += 1;
                    false
                } else {
                    true
                }
            })
            .collect();

        for batch in stale.chunks(BATCH) {
            let texts: Vec<String> = batch.iter().map(|r| r.content.clone()).collect();
            let embeddings = self.embed_batch(&texts).await?;
            for (record, embedding) in batch.iter().zip(embeddings) {
                let mut updated = record.clone();
                updated.embedding = embedding;
                updated.embedding_model = target_model.clone();
                self.store.update(updated).await?;
                report.migrated += 1;
            }
        }
        Ok(report)
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
        // Stage 1: retrieve + fuse into (candidates, relevance).
        let (records, mut relevance) = if self.store.supports_pushdown()
            && let Some(fused) = self.fused_pushdown(q).await?
        {
            fused
        } else {
            self.fused_in_process(q).await?
        };
        // Stage 2 (opt-in, RAG-202): re-score the fused top-N with the
        // reranker; degrade to the fused order on any failure.
        if self.reranker.is_some() {
            self.apply_rerank(q, &records, &mut relevance).await;
        }
        // Stage 3: weighted ranking (+ optional MMR), then parent expansion.
        // One clock read for the whole ranking pass (RAG-205).
        let mut results = rank(records, &relevance, q, self.clock.now_ms());
        if q.expand_parents {
            self.attach_parents(&mut results, q).await?;
        }
        Ok(results)
    }

    /// Re-score the top-N candidates (by fused relevance) through the
    /// configured [`Reranker`], overwriting their relevance in place.
    /// Candidates beyond N keep their fused scores. Never fails the query:
    /// a reranker error or a wrong-shaped response logs a warning and leaves
    /// the fused scores untouched.
    async fn apply_rerank(
        &self,
        q: &MemoryQuery,
        records: &[MemoryRecord],
        relevance: &mut HashMap<String, f32>,
    ) {
        let Some(reranker) = &self.reranker else {
            return;
        };
        let mut ordered: Vec<&MemoryRecord> = records.iter().collect();
        // Same deterministic order the ranker uses: fused score desc, id asc.
        ordered.sort_by(|a, b| {
            let sa = relevance.get(&a.id).copied().unwrap_or(0.0);
            let sb = relevance.get(&b.id).copied().unwrap_or(0.0);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        let top: Vec<&MemoryRecord> = ordered
            .into_iter()
            .take(self.rerank_top_n.max(q.limit))
            .collect();
        if top.is_empty() {
            return;
        }
        let contents: Vec<&str> = top.iter().map(|r| r.content.as_str()).collect();
        match reranker.rerank(&q.text, &contents).await {
            Ok(scores) if scores.len() == top.len() => {
                for (record, score) in top.iter().zip(scores) {
                    relevance.insert(record.id.clone(), score.clamp(0.0, 1.0));
                }
            }
            Ok(scores) => tracing::warn!(
                "reranker returned {} scores for {} candidates; keeping fused order",
                scores.len(),
                top.len()
            ),
            Err(e) => tracing::warn!("reranker failed, keeping fused order: {e}"),
        }
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

    /// In-process retrieval: scan `all()` and score every candidate, returning
    /// the filtered candidates with their fused relevance.
    async fn fused_in_process(
        &self,
        q: &MemoryQuery,
    ) -> Result<(Vec<MemoryRecord>, HashMap<String, f32>)> {
        let mut candidates = self.store.all(q.namespace.as_deref()).await?;
        candidates.retain(|r| passes_filters(r, q));
        if candidates.is_empty() {
            return Ok((Vec::new(), HashMap::new()));
        }
        let relevance = self.relevance(q, &candidates).await?;
        Ok((candidates, relevance))
    }

    /// Pushdown retrieval: ask the store's index for candidate ids per the strategy
    /// and fetch those records, returning them with their fused relevance. `None`
    /// if the store cannot satisfy the requested strategy (caller falls back to
    /// in-process).
    async fn fused_pushdown(
        &self,
        q: &MemoryQuery,
    ) -> Result<Option<(Vec<MemoryRecord>, HashMap<String, f32>)>> {
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
                    (Some(v), Some(kw)) => {
                        reciprocal_rank_fusion(&[ids_of(&v), ids_of(&kw)], self.rrf_k)
                    }
                    // Partial support → let the in-process path handle it.
                    _ => return Ok(None),
                }
            }
        };

        if relevance.is_empty() {
            return Ok(Some((Vec::new(), HashMap::new())));
        }
        let ids: Vec<String> = relevance.keys().cloned().collect();
        let mut records = self.store.get(&ids).await?;
        records.retain(|r| passes_filters(r, q));
        Ok(Some((records, relevance)))
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
                Ok(reciprocal_rank_fusion(&[vlist, klist], self.rrf_k))
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

/// BM25 `k1`: term-frequency saturation (standard default).
const BM25_K1: f32 = 1.2;
/// BM25 `b`: document-length normalization strength (standard default).
const BM25_B: f32 = 0.75;

/// In-process keyword relevance via **BM25 over stemmed tokens**, normalized
/// to `[0,1]` by the best score (RM-AIM-P2 RAG-204).
///
/// Replaces the old unnormalized set-overlap count, closing the quality gap
/// against the Postgres pushdown path's real full-text ranking: term
/// *frequency* now matters (saturating via `k1`), rare terms outweigh common
/// ones (IDF, Lucene's non-negative `ln(1 + (N - df + 0.5)/(df + 0.5))`
/// variant), long documents no longer win by sheer surface area (`b`
/// length-normalization), and light stemming matches morphological variants
/// ("refunds" ↔ "refund"). IDF is computed over the candidate set itself —
/// the same corpus the scores are compared within.
fn keyword_relevance(query: &str, candidates: &[MemoryRecord]) -> HashMap<String, f32> {
    // Query terms deduped: BM25 sums per unique term.
    let q_terms: BTreeSet<String> = tokens(query).into_iter().collect();
    if q_terms.is_empty() || candidates.is_empty() {
        return candidates.iter().map(|r| (r.id.clone(), 0.0)).collect();
    }

    // Per-document term frequencies + lengths, one tokenization pass each.
    let tf_maps: Vec<HashMap<String, f32>> = candidates
        .iter()
        .map(|r| {
            let mut tf: HashMap<String, f32> = HashMap::new();
            for t in tokens(&r.content) {
                *tf.entry(t).or_insert(0.0) += 1.0;
            }
            tf
        })
        .collect();
    let doc_lens: Vec<f32> = tf_maps.iter().map(|m| m.values().sum()).collect();
    let n = candidates.len() as f32;
    let avgdl = (doc_lens.iter().sum::<f32>() / n).max(1.0);

    let idf: HashMap<&String, f32> = q_terms
        .iter()
        .map(|t| {
            let df = tf_maps.iter().filter(|m| m.contains_key(t)).count() as f32;
            (t, (1.0 + (n - df + 0.5) / (df + 0.5)).ln())
        })
        .collect();

    let scores: Vec<(String, f32)> = candidates
        .iter()
        .zip(tf_maps.iter().zip(&doc_lens))
        .map(|(r, (tf_map, dl))| {
            let score: f32 = q_terms
                .iter()
                .filter_map(|t| {
                    let tf = *tf_map.get(t)?;
                    let norm = tf + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl);
                    Some(idf[t] * tf * (BM25_K1 + 1.0) / norm)
                })
                .sum();
            (r.id.clone(), score)
        })
        .collect();

    let max = scores.iter().map(|(_, s)| *s).fold(0.0_f32, f32::max);
    scores
        .into_iter()
        .map(|(id, s)| (id, if max > 0.0 { s / max } else { 0.0 }))
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
/// `k` is the smoothing constant: smaller values weight top ranks more heavily
/// (configurable per engine since RM-AIM-P2 RAG-202).
fn reciprocal_rank_fusion(lists: &[Vec<String>], k: f32) -> HashMap<String, f32> {
    let mut fused: HashMap<String, f32> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *fused.entry(id.clone()).or_insert(0.0) += 1.0 / (k + (rank as f32 + 1.0));
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

/// Whether a record passes the query's metadata filters (importance range +
/// tags + creation-time window, RM-AIM-P2 RAG-205) and the ABAC access policy.
/// Parent-document records (RAG-201) never pass: they exist only for
/// [`MemoryQuery::expand_parents`] expansion — their chunks are the retrieval
/// units. A legacy record without a timestamp (`created_ms == 0`) is excluded
/// whenever either time bound is set: an unknown creation time cannot be
/// placed inside a window (fail-closed).
fn passes_filters(r: &MemoryRecord, q: &MemoryQuery) -> bool {
    let in_time_window = if q.created_after.is_some() || q.created_before.is_some() {
        r.created_ms > 0
            && q.created_after.is_none_or(|t| r.created_ms >= t)
            && q.created_before.is_none_or(|t| r.created_ms <= t)
    } else {
        true
    };
    !r.is_parent
        && r.importance >= q.min_importance
        && q.max_importance.is_none_or(|m| r.importance <= m)
        && in_time_window
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
/// `now_ms` is the query-boundary clock reading recency ages against (RAG-205).
fn rank(
    records: Vec<MemoryRecord>,
    relevance: &HashMap<String, f32>,
    q: &MemoryQuery,
    now_ms: u64,
) -> Vec<ScoredMemory> {
    let max_seq = records.iter().map(|r| r.seq).max().unwrap_or(0);
    let mut scored: Vec<ScoredMemory> = records
        .into_iter()
        .map(|r| {
            let rel = relevance.get(&r.id).copied().unwrap_or(0.0);
            let rec = recency(&r, max_seq, now_ms);
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

/// Recency factor for a record (RM-AIM-P2 RAG-205): **wall-clock age** against
/// `now_ms` for records carrying a real `created_ms`, with the pre-RAG-205
/// sequence-distance proxy as the fallback for legacy records stored before
/// timestamps existed (`created_ms == 0`) — so an old store keeps ranking
/// sensibly instead of every legacy record decaying to ~0.
fn recency(r: &MemoryRecord, max_seq: u64, now_ms: u64) -> f32 {
    if r.created_ms > 0 {
        recency_decay_ms(now_ms.saturating_sub(r.created_ms), r.memory_type)
    } else {
        recency_decay_seq(max_seq.saturating_sub(r.seq), r.memory_type)
    }
}

/// Exponential recency decay over wall-clock age
/// ([ranking §4](../../docs/06-memory-engine/ranking.md): `exp(-age / half_life)`,
/// half-lives of 2/14/90 days by memory type).
fn recency_decay_ms(age_ms: u64, memory_type: MemoryType) -> f32 {
    match memory_type.half_life_ms() {
        None => 1.0, // semantic facts do not decay
        Some(half_life) => (-(age_ms as f32) / half_life).exp(),
    }
}

/// Exponential recency decay using sequence distance as the age proxy — the
/// legacy path for records without a creation timestamp.
fn recency_decay_seq(age: u64, memory_type: MemoryType) -> f32 {
    match memory_type.half_life() {
        None => 1.0, // semantic facts do not decay
        Some(half_life) => (-(age as f32) / half_life).exp(),
    }
}

/// Lowercase alphanumeric tokenization with light stemming, order/frequency
/// preserved (BM25 needs term counts, not a set).
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(stem)
        .collect()
}

/// A light English suffix-stripper (≈ Porter step 1, RM-AIM-P2 RAG-204) so
/// morphological variants match ("refunds" ↔ "refund", "policies" ↔ "policy",
/// "shipping" ↔ "shipp"…). Deliberately *light*: at most one rule fires, and a
/// minimum-stem-length guard keeps short words intact ("ring" is not "r" +
/// "-ing"). Approximate by design — a mismatch degrades to the unstemmed
/// token, never an error — mirroring how the Postgres path's `english`
/// snowball config also stems both sides.
fn stem(token: &str) -> String {
    if let Some(base) = token.strip_suffix("ies")
        && base.len() >= 2
    {
        return format!("{base}y");
    }
    if let Some(base) = token.strip_suffix("sses") {
        return format!("{base}ss");
    }
    for suffix in ["ing", "ed", "es"] {
        if let Some(base) = token.strip_suffix(suffix)
            && base.len() >= 3
        {
            return base.to_string();
        }
    }
    if let Some(base) = token.strip_suffix('s')
        && base.len() >= 3
        && !base.ends_with('s')
        && !base.ends_with('u')
        && !base.ends_with('i')
    {
        return base.to_string();
    }
    token.to_string()
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

    // --- RM-AR-P1 AIC-301: fail loud without an embedding provider -----------

    #[test]
    fn try_new_fails_closed_without_an_embedding_provider() {
        use apex_provider::AnthropicProvider;
        // An Anthropic-class chat provider can't embed; construction must fail
        // fast with a clear config error, not at the first remember/query.
        let gateway = Gateway::new(Box::new(AnthropicProvider::new(
            "https://example.invalid",
            "test-key",
        )));
        match MemoryEngine::try_new(gateway, Arc::new(InMemoryStore::new())) {
            Err(Error::Config(msg)) => assert!(
                msg.contains("embedding") && msg.contains("OPENAI_API_KEY"),
                "error must be actionable: {msg}"
            ),
            Err(other) => panic!("expected a config error, got {other:?}"),
            Ok(_) => panic!("expected fail-closed, but an engine was constructed"),
        }
    }

    #[test]
    fn try_new_succeeds_with_an_embedding_capable_gateway() {
        let gateway = Gateway::new(Box::new(MockProvider::new()));
        assert!(MemoryEngine::try_new(gateway, Arc::new(InMemoryStore::new())).is_ok());
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
            embedding_model: String::new(),
            memory_type: MemoryType::Semantic, // recency == 1.0 (no decay)
            importance: 0.0,
            tags: Vec::new(),
            required_scopes: Vec::new(),
            sensitive: false,
            parent_id: None,
            is_parent: false,
            created_ms: 0,
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
        let base = rank(records.clone(), &relevance, &q, 0);
        assert_eq!(ids(&base), vec!["a", "b"]);

        // Diversity on: the orthogonal c displaces the near-duplicate b.
        q.diversity = 0.6;
        let diversified = rank(records, &relevance, &q, 0);
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

    // --- Wall-clock recency + time/range filters (RM-AIM-P2 RAG-205) -----------

    use crate::clock::ManualClock;

    fn clocked_engine(clock: Arc<ManualClock>) -> MemoryEngine {
        let gateway = Gateway::new(Box::new(MockProvider::new()));
        MemoryEngine::new(gateway, Arc::new(InMemoryStore::new())).with_clock(clock)
    }

    /// RAG-205 acceptance (recency half): recency decays by **wall-clock age**,
    /// not sequence distance. One conversation record, queried exactly one
    /// half-life (2 days) after creation: wall-clock decay gives e⁻¹ ≈ 0.368,
    /// while the old seq proxy would give exp(0) = 1.0 (it is the only — and
    /// therefore newest — record).
    #[tokio::test]
    async fn recency_uses_wall_clock_age() {
        let clock = Arc::new(ManualClock::new(1_000_000));
        let eng = clocked_engine(clock.clone());
        eng.remember("kb", "meeting note", MemoryType::Conversation, 0.5, vec![])
            .await
            .unwrap();

        const TWO_DAYS_MS: u64 = 2 * 86_400_000;
        clock.advance(TWO_DAYS_MS);

        let mut q = MemoryQuery::new("meeting note");
        q.namespace = Some("kb".into());
        let results = eng.query(&q).await.unwrap();
        let rec = results[0].breakdown.recency;
        assert!(
            (rec - (-1.0_f32).exp()).abs() < 1e-3,
            "one half-life of wall-clock age must decay to e^-1 ≈ 0.368, got {rec}"
        );

        // A fresh query instant later: the same record decays further —
        // recency is a function of the query-time clock, not of insertions.
        clock.advance(TWO_DAYS_MS);
        let older = eng.query(&q).await.unwrap()[0].breakdown.recency;
        assert!(
            (older - (-2.0_f32).exp()).abs() < 1e-3,
            "two half-lives must decay to e^-2, got {older}"
        );
    }

    /// Legacy records without a timestamp keep the old sequence-distance decay
    /// instead of collapsing to ~0 wall-clock recency.
    #[test]
    fn legacy_records_fall_back_to_sequence_decay() {
        let mut legacy = rec("old", vec![1.0]);
        legacy.memory_type = MemoryType::Conversation;
        legacy.seq = 3; // max_seq 5 → age 2 seq units = one half-life
        assert!(
            (recency(&legacy, 5, u64::MAX) - (-1.0_f32).exp()).abs() < 1e-6,
            "created_ms == 0 must use the seq proxy regardless of the clock"
        );

        let mut stamped = legacy.clone();
        stamped.created_ms = 1_000;
        assert!(
            recency(&stamped, 5, 1_000) > 0.999,
            "a just-created stamped record is fully recent"
        );
    }

    /// RAG-205 acceptance (filter half): a time-range filter excludes
    /// out-of-window records, and a legacy record (unknown creation time) is
    /// excluded whenever a bound is set.
    #[tokio::test]
    async fn time_range_filter_excludes_out_of_window_records() {
        let clock = Arc::new(ManualClock::new(1_000));
        let eng = clocked_engine(clock.clone());
        eng.remember("kb", "early note", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
        clock.set(2_000);
        eng.remember("kb", "middle note", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
        clock.set(3_000);
        eng.remember("kb", "late note", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
        // A legacy record with no timestamp, written around the store directly.
        let mut legacy = rec("ignored", vec![1.0]);
        legacy.content = "legacy note".into();
        eng.store.put(legacy).await.unwrap();

        let mut q = MemoryQuery::new("note");
        q.namespace = Some("kb".into());

        // No bounds: everything (incl. the legacy record) is retrievable.
        assert_eq!(eng.query(&q).await.unwrap().len(), 4);

        // A window around t=2000 keeps exactly the middle record.
        q.created_after = Some(1_500);
        q.created_before = Some(2_500);
        let windowed = eng.query(&q).await.unwrap();
        assert_eq!(windowed.len(), 1);
        assert_eq!(windowed[0].record.content, "middle note");

        // A lower bound alone: middle + late, never the legacy unknown.
        q.created_before = None;
        let after: Vec<String> = eng
            .query(&q)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.record.content)
            .collect();
        assert_eq!(after.len(), 2);
        assert!(after.contains(&"middle note".to_string()));
        assert!(after.contains(&"late note".to_string()));

        // Bounds are inclusive on both ends.
        q.created_after = Some(2_000);
        q.created_before = Some(2_000);
        assert_eq!(eng.query(&q).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn max_importance_completes_the_numeric_range_filter() {
        let eng = engine();
        eng.remember("kb", "minor trivia", MemoryType::Semantic, 0.2, vec![])
            .await
            .unwrap();
        eng.remember("kb", "major policy", MemoryType::Semantic, 0.9, vec![])
            .await
            .unwrap();

        let mut q = MemoryQuery::new("note");
        q.namespace = Some("kb".into());
        q.max_importance = Some(0.5);
        let results = eng.query(&q).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.content, "minor trivia");

        // Combined with the existing lower bound: an empty band matches nothing.
        q.min_importance = 0.3;
        assert!(eng.query(&q).await.unwrap().is_empty());
    }

    // --- BM25 keyword relevance (RM-AIM-P2 RAG-204) ----------------------------

    /// RAG-204 acceptance: term frequency matters — a document mentioning the
    /// query term repeatedly outranks a same-length single-mention one. The
    /// old set-overlap scorer tied these (both "contain the term") and the
    /// heavy doc is seeded *second*, so an id-ascending tiebreak would pick
    /// the wrong one — only real tf scoring makes this pass.
    #[tokio::test]
    async fn bm25_ranks_term_frequency_above_a_single_mention() {
        let eng = engine();
        eng.remember(
            "kb",
            "refund mentioned once alongside office parking badges visitors",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
        eng.remember(
            "kb",
            "refund policy refund window refund processing takes five days",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();

        let mut q = MemoryQuery::new("refund");
        q.namespace = Some("kb".into());
        q.strategy = RetrievalStrategy::Keyword;
        let results = eng.query(&q).await.unwrap();
        assert!(
            results[0].record.content.contains("policy"),
            "the term-frequency-heavy doc must rank first, got: {}",
            results[0].record.content
        );
        assert!(
            results[0].score > results[1].score,
            "and strictly outscore the single-mention doc"
        );
    }

    /// IDF: matching a rare term is worth more than matching a ubiquitous one.
    /// Both docs match exactly one query term, so the old overlap scorer tied
    /// them (and its id tiebreak picked the wrong one).
    #[tokio::test]
    async fn bm25_weights_a_rare_term_above_a_ubiquitous_one() {
        let eng = engine();
        // "the" appears in every doc (df = 3); "zebra" in exactly one (df = 1).
        eng.remember(
            "kb",
            "the office in berlin",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
        eng.remember("kb", "the support hours", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
        // No "the" here: this doc matches exactly one query term, like the
        // others — the old overlap scorer tied all three and its id-ascending
        // tiebreak picked the first doc; only IDF separates them.
        eng.remember(
            "kb",
            "zebra spotted near zoo",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();

        let mut q = MemoryQuery::new("the zebra");
        q.namespace = Some("kb".into());
        q.strategy = RetrievalStrategy::Keyword;
        let results = eng.query(&q).await.unwrap();
        assert!(
            results[0].record.content.contains("zebra"),
            "the rare-term match must rank first, got: {}",
            results[0].record.content
        );
    }

    /// Stemming: a plural query matches a singular document (the old scorer
    /// scored zero overlap for "refunds" vs "refund").
    #[tokio::test]
    async fn stemming_matches_morphological_variants() {
        let eng = engine();
        eng.remember(
            "kb",
            "refund policy details",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
        eng.remember(
            "kb",
            "office parking rules",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();

        let mut q = MemoryQuery::new("refunds");
        q.namespace = Some("kb".into());
        q.strategy = RetrievalStrategy::Keyword;
        let results = eng.query(&q).await.unwrap();
        assert!(
            results[0].record.content.contains("refund"),
            "\"refunds\" must match \"refund\" via stemming"
        );
        assert!(
            results[0].breakdown.relevance > 0.0,
            "the match must carry positive relevance, not win by tiebreak"
        );
    }

    #[test]
    fn stem_strips_common_suffixes_with_short_word_guards() {
        let cases = [
            ("refunds", "refund"),
            ("policies", "policy"),
            ("processes", "process"),
            ("boxes", "box"),
            ("shipping", "shipp"), // light stemmer: no double-consonant undoubling
            ("walked", "walk"),
            ("ring", "ring"),     // min-stem guard: not "r" + "-ing"
            ("red", "red"),       // not "r" + "-ed"
            ("bus", "bus"),       // too short to strip "s"
            ("pass", "pass"),     // "-ss" is not a plural
            ("status", "status"), // "-us" is not a plural
            ("this", "this"),     // "-is" is not a plural
            ("zebra", "zebra"),   // nothing to strip
        ];
        for (input, want) in cases {
            assert_eq!(stem(input), want, "stem({input:?})");
        }
    }

    #[test]
    fn keyword_relevance_handles_empty_query_and_empty_corpus() {
        let candidates = vec![rec("a", vec![1.0])];
        let scores = keyword_relevance("", &candidates);
        assert_eq!(scores["a"], 0.0, "empty query scores zero, never panics");
        assert!(keyword_relevance("q", &[]).is_empty());
    }

    // --- Re-ranking stage (RM-AIM-P2 RAG-202) ---------------------------------

    /// A deterministic test reranker: scores by substring match, and records
    /// what it was asked to score.
    struct ScriptedReranker {
        /// `(substring, score)` — first match wins; unmatched candidates get 0.1.
        rules: Vec<(&'static str, f32)>,
        seen: std::sync::Mutex<Vec<Vec<String>>>,
        fail: bool,
    }

    impl ScriptedReranker {
        fn new(rules: Vec<(&'static str, f32)>) -> Arc<Self> {
            Arc::new(Self {
                rules,
                seen: std::sync::Mutex::new(Vec::new()),
                fail: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                rules: Vec::new(),
                seen: std::sync::Mutex::new(Vec::new()),
                fail: true,
            })
        }
    }

    #[async_trait::async_trait]
    impl crate::rerank::Reranker for ScriptedReranker {
        async fn rerank(&self, _query: &str, candidates: &[&str]) -> Result<Vec<f32>> {
            self.seen
                .lock()
                .unwrap()
                .push(candidates.iter().map(|c| c.to_string()).collect());
            if self.fail {
                return Err(Error::provider("reranker down"));
            }
            Ok(candidates
                .iter()
                .map(|c| {
                    self.rules
                        .iter()
                        .find(|(needle, _)| c.contains(needle))
                        .map(|(_, s)| *s)
                        .unwrap_or(0.1)
                })
                .collect())
        }
    }

    /// Weights isolating relevance so ordering is decided by (re)ranked
    /// relevance alone.
    fn relevance_only(q: &mut MemoryQuery) {
        q.weights = RankingWeights {
            relevance: 1.0,
            recency: 0.0,
            importance: 0.0,
        };
    }

    /// Two memories where the keyword-fused order puts `alpha` first for the
    /// query "alpha" (it literally contains the term).
    async fn seed_rerank(eng: &MemoryEngine) {
        eng.remember("kb", "alpha alpha alpha", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
        eng.remember(
            "kb",
            "beta document about other things",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
    }

    /// RAG-202 acceptance (half 1): the reranker reorders the fused list.
    #[tokio::test]
    async fn reranker_reorders_the_fused_candidates() {
        let reranker = ScriptedReranker::new(vec![("beta", 0.9), ("alpha", 0.2)]);
        let gateway = Gateway::new(Box::new(MockProvider::new()));
        let eng = MemoryEngine::new(gateway, Arc::new(InMemoryStore::new()))
            .with_reranker(reranker.clone());
        seed_rerank(&eng).await;

        let mut q = MemoryQuery::new("alpha");
        q.namespace = Some("kb".into());
        relevance_only(&mut q);
        let results = eng.query(&q).await.unwrap();

        assert!(
            results[0].record.content.contains("beta"),
            "reranker must override the fused order, got: {}",
            results[0].record.content
        );
        // The reranked score is what the breakdown reports.
        assert_eq!(results[0].breakdown.relevance, 0.9);
        assert_eq!(reranker.seen.lock().unwrap().len(), 1, "one rerank call");
    }

    /// RAG-202 acceptance (half 2): off by default — behavior is unchanged.
    #[tokio::test]
    async fn without_a_reranker_the_fused_order_stands() {
        let eng = engine(); // no reranker attached
        seed_rerank(&eng).await;

        let mut q = MemoryQuery::new("alpha");
        q.namespace = Some("kb".into());
        relevance_only(&mut q);
        let results = eng.query(&q).await.unwrap();
        assert!(
            results[0].record.content.contains("alpha"),
            "default behavior must be the fused order"
        );
    }

    #[tokio::test]
    async fn a_failing_reranker_degrades_to_the_fused_order() {
        let reranker = ScriptedReranker::failing();
        let gateway = Gateway::new(Box::new(MockProvider::new()));
        let eng =
            MemoryEngine::new(gateway, Arc::new(InMemoryStore::new())).with_reranker(reranker);
        seed_rerank(&eng).await;

        let mut q = MemoryQuery::new("alpha");
        q.namespace = Some("kb".into());
        relevance_only(&mut q);
        let results = eng.query(&q).await.unwrap();
        assert!(
            results[0].record.content.contains("alpha"),
            "a reranker outage must not change results, let alone fail the query"
        );
    }

    #[tokio::test]
    async fn only_the_fused_top_n_reaches_the_reranker() {
        let reranker = ScriptedReranker::new(vec![]);
        let gateway = Gateway::new(Box::new(MockProvider::new()));
        let eng = MemoryEngine::new(gateway, Arc::new(InMemoryStore::new()))
            .with_reranker(reranker.clone())
            .with_rerank_top_n(2);
        for i in 0..5 {
            eng.remember("kb", format!("note {i}"), MemoryType::Semantic, 0.5, vec![])
                .await
                .unwrap();
        }

        let mut q = MemoryQuery::new("note");
        q.namespace = Some("kb".into());
        q.limit = 2;
        eng.query(&q).await.unwrap();

        let seen = reranker.seen.lock().unwrap();
        assert_eq!(seen[0].len(), 2, "only top-N candidates are re-scored");
    }

    #[test]
    fn rrf_k_changes_the_fusion_ratio() {
        // Item `a` leads list 1, item `b` leads list 2 and also appears second
        // in list 1 — with a small k the double appearance dominates harder.
        let lists = vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["b".to_string()],
        ];
        let tight = reciprocal_rank_fusion(&lists, 1.0);
        let smooth = reciprocal_rank_fusion(&lists, 60.0);
        // `a` holds one rank-1 slot; `b` holds a rank-1 and a rank-2 slot.
        // With small k a rank-2 appearance is worth much less than rank-1, so
        // `a` closes the gap; with large k all ranks flatten toward equal
        // weight and `b`'s double appearance dominates (ratio → 0.5).
        let ratio_tight = tight["a"] / tight["b"];
        let ratio_smooth = smooth["a"] / smooth["b"];
        assert!(
            ratio_tight > ratio_smooth,
            "smaller k must weight top ranks more heavily ({ratio_tight} vs {ratio_smooth})"
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

    // --- RAG-301: incremental re-embedding migration -------------------------

    /// An embedding provider with a different identity (→ model id
    /// `mockv2-embeddings`) and a different dimensionality (4, vs the mock's
    /// 16) — the "we switched embedding models" scenario.
    struct AltEmbedProvider;

    #[async_trait::async_trait]
    impl apex_provider::AIProvider for AltEmbedProvider {
        fn name(&self) -> &str {
            "mockv2"
        }
        async fn chat(
            &self,
            _request: apex_provider::ChatRequest,
        ) -> apex_common::Result<apex_provider::ChatResponse> {
            Err(apex_common::Error::provider(
                "chat is not used in this test",
            ))
        }
        async fn embed(
            &self,
            request: apex_provider::EmbeddingRequest,
        ) -> apex_common::Result<apex_provider::EmbeddingResponse> {
            Ok(apex_provider::EmbeddingResponse {
                model: request.model,
                vectors: request
                    .input
                    .iter()
                    .map(|t| vec![t.len() as f32, 1.0, 2.0, 3.0])
                    .collect(),
                usage: apex_common::Usage::default(),
            })
        }
    }

    /// RAG-301 acceptance: a namespace ingested under one embedding model is
    /// migrated to a new model — afterwards every embedded record carries the
    /// new model id and one uniform dimensionality, ids/seqs/parent links
    /// survive, parents stay non-embedded, and a re-run is an incremental
    /// no-op.
    #[tokio::test]
    async fn migrate_embeddings_moves_a_namespace_to_the_new_model() {
        let store = Arc::new(InMemoryStore::new());

        // Ingest under the mock model (16-dim, id `mock-embeddings`): two plain
        // memories + one chunked document (parent + chunks).
        let old = MemoryEngine::new(
            Gateway::new(Box::new(MockProvider::new())),
            store.clone() as Arc<dyn MemoryStore>,
        );
        for text in ["refunds take 30 days", "the office is in Berlin"] {
            old.remember("kb", text, MemoryType::Semantic, 0.5, vec![])
                .await
                .unwrap();
        }
        let doc = old
            .remember_document(
                "kb",
                "alpha beta gamma delta epsilon zeta eta theta",
                MemoryType::Semantic,
                0.5,
                vec![],
                vec![],
                false,
                &ChunkPolicy {
                    max_chars: 20,
                    overlap_chars: 4,
                },
            )
            .await
            .unwrap();
        assert!(doc.chunk_ids.len() >= 2, "document must actually chunk");

        let before = store.all(Some("kb")).await.unwrap();
        let embedded_before = before.iter().filter(|r| !r.is_parent).count();
        assert!(
            before
                .iter()
                .filter(|r| !r.is_parent)
                .all(|r| r.embedding.len() == 16 && r.embedding_model == "mock-embeddings"),
            "precondition: everything embedded on the old model"
        );

        // Switch models: same store, new gateway → migrate.
        let new = MemoryEngine::new(
            Gateway::new(Box::new(AltEmbedProvider)),
            store.clone() as Arc<dyn MemoryStore>,
        );
        let report = new.migrate_embeddings("kb").await.unwrap();
        assert_eq!(report.target_model, "mockv2-embeddings");
        assert_eq!(report.scanned, before.len());
        assert_eq!(report.migrated, embedded_before);
        assert_eq!(report.already_current, 0);
        assert_eq!(report.parents_skipped, 1);

        // Uniform dimensionality + model id after; identity and links intact.
        let after = store.all(Some("kb")).await.unwrap();
        assert_eq!(after.len(), before.len());
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(b.id, a.id, "ids survive migration");
            assert_eq!(b.seq, a.seq, "seqs survive migration");
            assert_eq!(b.created_ms, a.created_ms, "timestamps survive migration");
            assert_eq!(b.parent_id, a.parent_id, "chunk links survive migration");
            assert_eq!(b.content, a.content, "content is never touched");
            if a.is_parent {
                assert!(a.embedding.is_empty(), "parents stay non-embedded");
            } else {
                assert_eq!(a.embedding.len(), 4, "uniform new dimensionality");
                assert_eq!(a.embedding_model, "mockv2-embeddings");
            }
        }

        // Incremental: a second run finds nothing stale.
        let rerun = new.migrate_embeddings("kb").await.unwrap();
        assert_eq!(rerun.migrated, 0);
        assert_eq!(rerun.already_current, embedded_before);

        // And retrieval still works over the migrated namespace end to end.
        let mut q = MemoryQuery::new("refunds");
        q.namespace = Some("kb".into());
        q.strategy = RetrievalStrategy::Keyword;
        let hits = new.query(&q).await.unwrap();
        assert!(!hits.is_empty(), "migrated namespace still retrieves");
    }

    /// A store without an in-place write path fails the migration closed on the
    /// first stale record — never silently re-inserting under new ids.
    #[tokio::test]
    async fn migration_fails_closed_on_a_store_without_update() {
        /// Wraps the in-memory store but hides `update` (the trait default).
        struct NoUpdateStore(InMemoryStore);
        #[async_trait::async_trait]
        impl MemoryStore for NoUpdateStore {
            async fn put(&self, record: MemoryRecord) -> apex_common::Result<String> {
                self.0.put(record).await
            }
            async fn all(&self, namespace: Option<&str>) -> apex_common::Result<Vec<MemoryRecord>> {
                self.0.all(namespace).await
            }
        }

        let store = Arc::new(NoUpdateStore(InMemoryStore::new()));
        let old = MemoryEngine::new(
            Gateway::new(Box::new(MockProvider::new())),
            store.clone() as Arc<dyn MemoryStore>,
        );
        old.remember("kb", "hello", MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();

        let new = MemoryEngine::new(Gateway::new(Box::new(AltEmbedProvider)), store);
        let err = new.migrate_embeddings("kb").await.unwrap_err();
        assert!(
            err.to_string().contains("in-place updates"),
            "clear fail-closed error, got: {err}"
        );
    }
}
