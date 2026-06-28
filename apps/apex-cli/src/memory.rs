//! `apex memory` commands: store and query memories locally.
//!
//! Persists records under `~/.apex/memory/<namespace>.jsonl` (a [`FileStore`]) and
//! embeds via the [LLM Gateway](apex_provider) — the mock provider offline, or a
//! real model when `OPENAI_API_KEY` is set. Maps to the
//! [Memory API](../../docs/09-api/memory.md) `put`/`query` verbs.

use crate::config;
use apex_agent::{ContextRetriever, MemorySpec, RetrievedContext};
use apex_memory::{
    CompactionPolicy, FileStore, MemoryEngine, MemoryQuery, MemoryStore, MemoryType,
    RetrievalStrategy,
};
use async_trait::async_trait;
use std::sync::Arc;

use apex_provider::Gateway;

/// Build a memory engine. Uses the durable tiered backend (Postgres + Qdrant) when
/// the `tiered-memory` feature is built and both `APEX_MEMORY_POSTGRES_URL` and
/// `APEX_MEMORY_QDRANT_URL` are set; otherwise a local `~/.apex/memory` file store.
async fn engine() -> apex_common::Result<MemoryEngine> {
    Ok(MemoryEngine::new(Gateway::from_env(), open_store().await?))
}

/// The local JSON-lines file store under `~/.apex/memory`.
fn file_store() -> apex_common::Result<Arc<dyn MemoryStore>> {
    let dir = config::config_dir()?.join("memory");
    Ok(Arc::new(FileStore::new(dir)?))
}

/// Select the tiered backend when configured, else the file store.
#[cfg(feature = "tiered-memory")]
async fn open_store() -> apex_common::Result<Arc<dyn MemoryStore>> {
    use apex_memory::TieredStore;
    match (
        std::env::var("APEX_MEMORY_POSTGRES_URL"),
        std::env::var("APEX_MEMORY_QDRANT_URL"),
    ) {
        (Ok(pg), Ok(qdrant)) => {
            let collection = std::env::var("APEX_MEMORY_QDRANT_COLLECTION")
                .unwrap_or_else(|_| "apex_memory".to_string());
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
async fn open_store() -> apex_common::Result<Arc<dyn MemoryStore>> {
    file_store()
}

/// Adapts the local [`MemoryEngine`] to the agent runtime's [`ContextRetriever`],
/// so `apex agents run` can ground answers in `~/.apex/memory`.
pub struct EngineRetriever {
    engine: MemoryEngine,
}

impl EngineRetriever {
    /// Open a retriever over the configured memory store.
    pub async fn open() -> apex_common::Result<Self> {
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
    ) -> apex_common::Result<Vec<RetrievedContext>> {
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

/// `apex memory put` — store a memory.
pub async fn put_cmd(
    namespace: &str,
    content: &str,
    importance: f32,
    tags: Vec<String>,
    require_scopes: Vec<String>,
) -> apex_common::Result<()> {
    let id = engine()
        .await?
        .remember_scoped(
            namespace,
            content,
            MemoryType::Semantic,
            importance,
            tags,
            require_scopes,
        )
        .await?;
    println!("stored {id}");
    Ok(())
}

/// `apex memory query` — retrieve ranked memories.
pub async fn query_cmd(
    text: &str,
    namespace: Option<String>,
    limit: usize,
    diversity: f32,
    grants: Vec<String>,
) -> apex_common::Result<()> {
    let mut query = MemoryQuery::new(text);
    query.namespace = namespace;
    query.limit = limit;
    query.diversity = diversity;
    if !grants.is_empty() {
        query.access = Some(apex_memory::AccessContext::new(grants));
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

/// `apex memory compact` — consolidate stale, low-importance memories into a summary.
pub async fn compact_cmd(
    namespace: &str,
    max_importance: f32,
    keep_recent: usize,
) -> apex_common::Result<()> {
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
