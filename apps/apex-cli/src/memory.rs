//! `apex memory` commands: store and query memories locally.
//!
//! Persists records under `~/.apex/memory/<namespace>.jsonl` (a [`FileStore`]) and
//! embeds via the [LLM Gateway](apex_provider) — the mock provider offline, or a
//! real model when `OPENAI_API_KEY` is set. Maps to the
//! [Memory API](../../docs/09-api/memory.md) `put`/`query` verbs.

use crate::config;
use apex_memory::{FileStore, MemoryEngine, MemoryQuery, MemoryType};
use apex_provider::Gateway;
use std::sync::Arc;

/// Build a memory engine backed by `~/.apex/memory`.
fn engine() -> apex_common::Result<MemoryEngine> {
    let dir = config::config_dir()?.join("memory");
    let store = FileStore::new(dir)?;
    Ok(MemoryEngine::new(Gateway::from_env(), Arc::new(store)))
}

/// `apex memory put` — store a memory.
pub async fn put_cmd(
    namespace: &str,
    content: &str,
    importance: f32,
    tags: Vec<String>,
) -> apex_common::Result<()> {
    let id = engine()?
        .remember(namespace, content, MemoryType::Semantic, importance, tags)
        .await?;
    println!("stored {id}");
    Ok(())
}

/// `apex memory query` — retrieve ranked memories.
pub async fn query_cmd(
    text: &str,
    namespace: Option<String>,
    limit: usize,
) -> apex_common::Result<()> {
    let mut query = MemoryQuery::new(text);
    query.namespace = namespace;
    query.limit = limit;

    let results = engine()?.query(&query).await?;
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
