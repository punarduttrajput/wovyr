//! `wovyr memory` commands: store and query memories locally.
//!
//! Persists records under `~/.wovyr/memory/<namespace>.jsonl` (a [`FileStore`]) and
//! embeds via the [LLM Gateway](wovyr_provider) — the mock provider offline, or a
//! real model when `OPENAI_API_KEY` is set. Maps to the
//! [Memory API](../../docs/09-api/memory.md) `put`/`query` verbs.

use crate::config;
use async_trait::async_trait;
use std::sync::Arc;
use wovyr_agent::{ContextRetriever, MemorySpec, RetrievedContext};
use wovyr_memory::{
    CompactionPolicy, EncryptingMemoryStore, FileStore, MemoryEngine, MemoryQuery, MemoryStore,
    MemoryType, RetrievalStrategy,
};

use wovyr_provider::Gateway;

/// Build a memory engine. Uses the durable tiered backend (Postgres + Qdrant) when
/// the `tiered-memory` feature is built and both `WOVYR_MEMORY_POSTGRES_URL` and
/// `WOVYR_MEMORY_QDRANT_URL` are set; otherwise a local `~/.wovyr/memory` file store.
/// Always wrapped in [`EncryptingMemoryStore`] — transparent unless a caller marks a
/// record `--sensitive`, so existing plaintext memories are unaffected.
async fn engine() -> wovyr_common::Result<MemoryEngine> {
    let store: Arc<dyn MemoryStore> = Arc::new(EncryptingMemoryStore::new(
        open_store().await?,
        config::kms(),
    ));
    // AIC-301: fail loud with an actionable message when no embedding provider
    // is configured (e.g. Anthropic-only, no OPENAI_API_KEY), rather than
    // erroring deep inside the first put/query or memory-grounded agent run.
    // `with_rrf_k_from_env` so the CLI and the server agree on hybrid fusion
    // tuning; a corpus small enough to need a lower `k` needs it on both paths.
    Ok(MemoryEngine::try_new(Gateway::from_env(), store)?.with_rrf_k_from_env())
}

/// The local JSON-lines file store under `~/.wovyr/memory`.
fn file_store() -> wovyr_common::Result<Arc<dyn MemoryStore>> {
    let dir = config::config_dir()?.join("memory");
    Ok(Arc::new(FileStore::new(dir)?))
}

/// Select the tiered backend when configured, else the file store.
#[cfg(feature = "tiered-memory")]
async fn open_store() -> wovyr_common::Result<Arc<dyn MemoryStore>> {
    use wovyr_memory::TieredStore;
    match (
        std::env::var("WOVYR_MEMORY_POSTGRES_URL"),
        std::env::var("WOVYR_MEMORY_QDRANT_URL"),
    ) {
        (Ok(pg), Ok(qdrant)) => {
            let collection = std::env::var("WOVYR_MEMORY_QDRANT_COLLECTION")
                .unwrap_or_else(|_| "wovyr_memory".to_string());
            Ok(Arc::new(
                TieredStore::connect(&pg, &qdrant, &collection).await?,
            ))
        }
        // Tiered support compiled in but not configured → fall back to the file store.
        _ => file_store(),
    }
}

/// Without the `tiered-memory` feature there is only the file store.
#[cfg(not(feature = "tiered-memory"))]
async fn open_store() -> wovyr_common::Result<Arc<dyn MemoryStore>> {
    file_store()
}

/// Adapts the local [`MemoryEngine`] to the agent runtime's [`ContextRetriever`],
/// so `wovyr agents run` can ground answers in `~/.wovyr/memory`.
pub struct EngineRetriever {
    engine: MemoryEngine,
}

impl EngineRetriever {
    /// Open a retriever over the configured memory store.
    pub async fn open() -> wovyr_common::Result<Self> {
        Ok(Self {
            engine: engine().await?,
        })
    }
}

#[async_trait]
impl ContextRetriever for EngineRetriever {
    async fn retrieve(
        &self,
        query: &str,
        spec: &MemorySpec,
    ) -> wovyr_common::Result<Vec<RetrievedContext>> {
        let mut q = MemoryQuery::new(query);
        q.namespace = spec.namespace.clone();
        q.tags = spec.tags.clone();
        if let Some(limit) = spec.retrieval.limit {
            q.limit = limit;
        }
        if let Some(strategy) = &spec.retrieval.strategy {
            q.strategy = match strategy.to_lowercase().as_str() {
                "vector" => RetrievalStrategy::Vector,
                "keyword" => RetrievalStrategy::Keyword,
                _ => RetrievalStrategy::Hybrid,
            };
        }

        let results = self.engine.query(&q).await?;
        Ok(results
            .into_iter()
            .map(|r| RetrievedContext {
                source: r.record.id,
                content: r.record.content,
                score: r.score,
            })
            .collect())
    }
}

/// `wovyr memory put` — store a memory.
#[allow(clippy::too_many_arguments)] // mirrors MemoryEngine::remember_full's positional-arg style
pub async fn put_cmd(
    namespace: &str,
    content: &str,
    importance: f32,
    tags: Vec<String>,
    require_scopes: Vec<String>,
    sensitive: bool,
) -> wovyr_common::Result<()> {
    let id = engine()
        .await?
        .remember_full(
            namespace,
            content,
            MemoryType::Semantic,
            importance,
            tags,
            require_scopes,
            sensitive,
        )
        .await?;
    println!("stored {id}");
    Ok(())
}

/// `wovyr memory query` — retrieve ranked memories.
pub async fn query_cmd(
    text: &str,
    namespace: Option<String>,
    limit: usize,
    diversity: f32,
    strategy: Option<String>,
    grants: Vec<String>,
) -> wovyr_common::Result<()> {
    let mut query = MemoryQuery::new(text);
    query.namespace = namespace;
    query.limit = limit;
    query.diversity = diversity;
    // Retrieval strategy (default hybrid). Pick `keyword` for offline use: the mock
    // embeddings are non-semantic, so the vector half of hybrid is noise.
    if let Some(s) = strategy {
        query.strategy = match s.to_lowercase().as_str() {
            "vector" => RetrievalStrategy::Vector,
            "keyword" => RetrievalStrategy::Keyword,
            _ => RetrievalStrategy::Hybrid,
        };
    }
    if !grants.is_empty() {
        query.access = Some(wovyr_memory::AccessContext::new(grants));
    }

    let results = engine().await?.query(&query).await?;
    if results.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    for r in results {
        println!("{:.3}  [{}]  {}", r.score, r.record.id, r.record.content);
        println!(
            "       relevance={:.2} recency={:.2} importance={:.2}",
            r.breakdown.relevance, r.breakdown.recency, r.breakdown.importance
        );
    }
    Ok(())
}

/// `wovyr memory compact` — consolidate stale, low-importance memories into a summary.
pub async fn compact_cmd(
    namespace: &str,
    max_importance: f32,
    keep_recent: usize,
) -> wovyr_common::Result<()> {
    let policy = CompactionPolicy {
        max_importance,
        keep_recent,
        ..CompactionPolicy::default()
    };
    let outcome = engine().await?.compress(namespace, policy).await?;
    match outcome.summary_id {
        Some(id) => println!("compacted {} memories into {id}", outcome.compacted),
        None => println!("nothing to compact (fewer than the minimum candidates)"),
    }
    Ok(())
}
