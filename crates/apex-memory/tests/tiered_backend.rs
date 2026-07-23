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
//! Only compiled with `--features tiered`. `PostgresStore::connect` only ever
//! *reads* the schema version (RM-GA-P3 MIG-A1) — migrate first, or every test
//! here skips with a "not migrated" reason instead of running. To run locally:
//!
//! ```bash
//! cargo run -p apex-cli --features tiered-memory -- admin migrate --target memory \
//!   --database-url postgres://apex:apex@127.0.0.1:5433/apex
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
        embedding_model: String::new(),
        memory_type: MemoryType::Semantic,
        importance: 0.5,
        tags: Vec::new(),
        required_scopes: Vec::new(),
        sensitive: false,
        parent_id: None,
        is_parent: false,
        created_ms: 0,
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
async fn tiered_delete_removes_from_both_tiers() {
    let store = match tiered().await {
        Some(s) => Arc::new(s),
        None => return,
    };
    let ns = format!("it-del-{}", nonce());
    // Put through the engine so the embedding matches the shared collection's dim.
    let engine = MemoryEngine::new(Gateway::new(Box::new(MockProvider::new())), store.clone());
    let id = engine
        .remember(&ns, "ephemeral memory", MemoryType::Semantic, 0.5, vec![])
        .await
        .unwrap();

    // Present in Postgres (get) and Qdrant (vector_search) before deletion.
    let stored = store.get(std::slice::from_ref(&id)).await.unwrap();
    assert_eq!(stored.len(), 1);
    let query_vec = stored[0].embedding.clone();
    let before = store
        .vector_search(Some(&ns), &query_vec, 5)
        .await
        .unwrap()
        .unwrap();
    assert!(before.iter().any(|(hid, _)| *hid == id));

    store.delete(std::slice::from_ref(&id)).await.unwrap();

    // Gone from both tiers.
    assert!(
        store
            .get(std::slice::from_ref(&id))
            .await
            .unwrap()
            .is_empty()
    );
    let after = store
        .vector_search(Some(&ns), &query_vec, 5)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !after.iter().any(|(hid, _)| *hid == id),
        "vector index purged"
    );
}

#[tokio::test]
async fn postgres_round_trips_required_scopes_for_abac() {
    let Some(store) = pg().await else { return };
    let ns = format!("it-pg-abac-{}", nonce());

    let mut scoped = rec(&ns, "pii protected", vec![0.1, 0.2, 0.3]);
    scoped.required_scopes = vec!["pii".to_string(), "legal".to_string()];
    let id = store.put(scoped).await.unwrap();

    // The ABAC scopes must survive the Postgres TEXT[] round-trip so the engine can
    // enforce them after fetch.
    let fetched = store.get(&[id]).await.unwrap();
    assert_eq!(fetched.len(), 1);
    let mut scopes = fetched[0].required_scopes.clone();
    scopes.sort();
    assert_eq!(scopes, vec!["legal".to_string(), "pii".to_string()]);
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

/// RAG-204 parity smoke: on a shared fixture, the in-process BM25 keyword
/// branch and Postgres's real FTS (`ts_rank`) agree on the top result. The
/// fixture is built so term frequency decides — a set-overlap scorer would
/// tie the first two documents, so agreement here means the in-process path
/// now has the FTS backend's ranking character, not just its match set.
#[tokio::test]
async fn in_process_bm25_agrees_with_postgres_fts_on_the_top_result() {
    use apex_memory::InMemoryStore;

    let Some(store) = pg().await else { return };
    let ns = format!("it-bm25-parity-{}", nonce());
    let fixture = [
        "refund mentioned once alongside office parking badges visitors",
        "refund policy refund window refund processing takes five days",
        "office parking and visitor badges and sign in procedures",
    ];
    let query = "refund window";

    // Postgres side: seed and rank via real FTS, mapping ids back to content.
    for content in fixture {
        store.put(rec(&ns, content, vec![0.0])).await.unwrap();
    }
    let fts_hits = store
        .keyword_search(Some(&ns), query, 10)
        .await
        .unwrap()
        .expect("postgres advertises keyword pushdown");
    assert!(!fts_hits.is_empty(), "FTS must match the fixture");
    let fts_top = store
        .get(std::slice::from_ref(&fts_hits[0].0))
        .await
        .unwrap()
        .pop()
        .unwrap()
        .content;

    // In-process side: identical fixture through the engine's keyword branch.
    let engine = MemoryEngine::new(
        Gateway::new(Box::new(MockProvider::new())),
        Arc::new(InMemoryStore::new()),
    );
    for content in fixture {
        engine
            .remember(&ns, content, MemoryType::Semantic, 0.5, vec![])
            .await
            .unwrap();
    }
    let mut q = MemoryQuery::new(query);
    q.namespace = Some(ns.clone());
    q.strategy = RetrievalStrategy::Keyword;
    let results = engine.query(&q).await.unwrap();

    assert_eq!(
        results[0].record.content, fts_top,
        "BM25 and Postgres FTS must agree on the top result for this fixture"
    );
    assert!(
        fts_top.contains("policy"),
        "and that top result is the term-frequency-heavy document"
    );
}
