//! Live integration tests for the tiered durable backend (Postgres + Qdrant).
//!
//! These hit **real** services and so are **capability-gated**, like the tool
//! sandbox tests (`apex-tools/tests/sandbox_backends.rs`): each test reads
//! `APEX_MEMORY_POSTGRES_URL` / `APEX_MEMORY_QDRANT_URL` and returns early (logging a
//! skip) when they are unset or unreachable, so the suite still passes on offline CI
//! nodes without a database. The pure construction logic (`make_id`, `point_id`,
//! type mapping, URL handling) is covered by the unit tests in `src/backends.rs`;
//! this file verifies the store actually persists, indexes, and retrieves.
//!
//! Only compiled with `--features tiered`. To run locally:
//!
//! ```bash
//! APEX_MEMORY_POSTGRES_URL=postgres://apex:apex@127.0.0.1:5433/apex \
//! APEX_MEMORY_QDRANT_URL=http://127.0.0.1:6333 \
//!   cargo test -p apex-memory --features tiered --test tiered_backend -- --nocapture
//! ```
//!
//! Data is isolated by a per-run **nonce namespace**, so repeated runs are
//! independent and don't collide. A single shared Qdrant collection (`apex_it_mem`)
//! is reused across runs; points are keyed off the unique record ids.

#![cfg(feature = "tiered")]

use apex_memory::{
    MemoryEngine, MemoryQuery, MemoryRecord, MemoryStore, MemoryType, PostgresStore,
    RetrievalStrategy, TieredStore,
};
use apex_provider::{Gateway, MockProvider};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const COLLECTION: &str = "apex_it_mem";

/// A unique nanosecond nonce, used to namespace each run's data.
fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Connect a `PostgresStore`, or `None` (logging a skip) when unconfigured/unreachable.
async fn pg() -> Option<PostgresStore> {
    let url = match std::env::var("APEX_MEMORY_POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: APEX_MEMORY_POSTGRES_URL not set");
            return None;
        }
    };
    match PostgresStore::connect(&url).await {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("skipping: postgres unreachable at {url}: {e}");
            None
        }
    }
}

/// Connect a `TieredStore`, or `None` (logging a skip) when either tier is unconfigured.
async fn tiered() -> Option<TieredStore> {
    let (Ok(pg_url), Ok(qd_url)) = (
        std::env::var("APEX_MEMORY_POSTGRES_URL"),
        std::env::var("APEX_MEMORY_QDRANT_URL"),
    ) else {
        eprintln!("skipping: APEX_MEMORY_POSTGRES_URL / APEX_MEMORY_QDRANT_URL not both set");
        return None;
    };
    match TieredStore::connect(&pg_url, &qd_url, COLLECTION).await {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("skipping: tiered backend unreachable: {e}");
            None
        }
    }
}

fn rec(ns: &str, content: &str, embedding: Vec<f32>) -> MemoryRecord {
    MemoryRecord {
        id: String::new(),
        namespace: ns.to_string(),
        content: content.to_string(),
        embedding,
        memory_type: MemoryType::Semantic,
        importance: 0.5,
        tags: Vec::new(),
        seq: 0,
    }
}

#[tokio::test]
async fn postgres_put_get_and_namespace_filter() {
    let Some(store) = pg().await else { return };
    let ns_a = format!("it-pg-a-{}", nonce());
    let ns_b = format!("it-pg-b-{}", nonce());

    let id1 = store
        .put(rec(&ns_a, "alpha record", vec![0.1, 0.2, 0.3]))
        .await
        .unwrap();
    let id2 = store
        .put(rec(&ns_a, "beta record", vec![0.4, 0.5, 0.6]))
        .await
        .unwrap();
    let id_other = store
        .put(rec(&ns_b, "gamma record", vec![0.7, 0.8, 0.9]))
        .await
        .unwrap();

    // Ids are namespaced and distinct.
    assert!(id1.starts_with(&format!("mem-{ns_a}-")));
    assert_ne!(id1, id2);

    // get() round-trips by id, preserving content + embedding.
    let mut fetched = store.get(&[id1.clone(), id2.clone()]).await.unwrap();
    fetched.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(fetched.len(), 2);
    let alpha = fetched.iter().find(|r| r.id == id1).unwrap();
    assert_eq!(alpha.content, "alpha record");
    assert_eq!(alpha.embedding, vec![0.1, 0.2, 0.3]);

    // all(ns) is scoped to the namespace — the other namespace's record is excluded.
    let in_a = store.all(Some(&ns_a)).await.unwrap();
    let ids_a: Vec<&str> = in_a.iter().map(|r| r.id.as_str()).collect();
    assert!(ids_a.contains(&id1.as_str()) && ids_a.contains(&id2.as_str()));
    assert!(!ids_a.contains(&id_other.as_str()));
}

#[tokio::test]
async fn postgres_keyword_search_matches_and_ranks() {
    let Some(store) = pg().await else { return };
    assert!(store.supports_pushdown());
    let ns = format!("it-pg-kw-{}", nonce());

    let hit = store
        .put(rec(
            &ns,
            "the capital of France is Paris",
            vec![0.1, 0.0, 0.0],
        ))
        .await
        .unwrap();
    let _miss = store
        .put(rec(
            &ns,
            "rust is a systems programming language",
            vec![0.0, 0.1, 0.0],
        ))
        .await
        .unwrap();

    let hits = store
        .keyword_search(Some(&ns), "Paris France capital", 10)
        .await
        .unwrap()
        .expect("postgres advertises keyword pushdown");

    // The France/Paris record matches the full-text query; the Rust one does not.
    assert!(!hits.is_empty(), "expected a tsvector match");
    assert_eq!(hits[0].0, hit, "the matching record should rank first");
    assert!(hits[0].1 > 0.0, "ts_rank should be positive");
    assert!(
        hits.iter().all(|(id, _)| *id == hit),
        "only the matching record should be returned"
    );
}

#[tokio::test]
async fn tiered_vector_search_finds_upserted_record() {
    let store = match tiered().await {
        Some(s) => Arc::new(s),
        None => return,
    };
    assert!(store.supports_pushdown());
    let ns = format!("it-vec-{}", nonce());

    // Drive the write through the engine so the embedding has the collection's
    // dimensionality; reuse the same store handle for the direct index reads.
    let engine = MemoryEngine::new(Gateway::new(Box::new(MockProvider::new())), store.clone());
    let id = engine
        .remember(
            &ns,
            "vector indexed memory",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();

    // Read the stored embedding back, then query Qdrant with it: the record must be
    // its own nearest neighbour (cosine ~1.0).
    let stored = store.get(std::slice::from_ref(&id)).await.unwrap();
    assert_eq!(stored.len(), 1);
    let query_vec = stored[0].embedding.clone();

    let hits = store
        .vector_search(Some(&ns), &query_vec, 5)
        .await
        .unwrap()
        .expect("qdrant advertises vector pushdown");

    assert!(
        hits.iter().any(|(hid, _)| *hid == id),
        "the upserted record should appear in vector results"
    );
    let self_score = hits.iter().find(|(hid, _)| *hid == id).unwrap().1;
    assert!(
        self_score > 0.99,
        "a vector should match itself near-perfectly, got {self_score}"
    );
}

#[tokio::test]
async fn tiered_hybrid_query_ranks_keyword_match_via_engine() {
    let Some(store) = tiered().await else { return };
    let ns = format!("it-hybrid-{}", nonce());
    let engine = MemoryEngine::new(Gateway::new(Box::new(MockProvider::new())), Arc::new(store));

    engine
        .remember(
            &ns,
            "the capital of France is Paris",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();
    engine
        .remember(
            &ns,
            "rust is a systems programming language",
            MemoryType::Semantic,
            0.5,
            vec![],
        )
        .await
        .unwrap();

    // Hybrid pushdown: Qdrant (vector) + Postgres (keyword) fused via RRF. Offline
    // embeddings are non-semantic, so the keyword branch carries precision.
    let mut q = MemoryQuery::new("France Paris capital");
    q.namespace = Some(ns.clone());
    q.strategy = RetrievalStrategy::Hybrid;
    q.limit = 5;

    let results = engine.query(&q).await.unwrap();
    assert!(!results.is_empty(), "expected hybrid retrieval results");
    assert!(
        results[0].record.content.contains("Paris"),
        "the keyword-matching record should rank first, got {:?}",
        results[0].record.content
    );
}
