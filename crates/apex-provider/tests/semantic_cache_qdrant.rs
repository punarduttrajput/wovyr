//! Live integration test for the Qdrant-backed distributed semantic cache.
//!
//! Capability-gated like the other backend tests: reads `APEX_QDRANT_URL` and skips
//! (logging) when unset/unreachable, so offline CI still passes. The in-process
//! semantic cache and the gateway lookup/store wiring are unit-tested in
//! `src/gateway.rs`; this verifies the real `QdrantSemanticCache` round-trip.
//!
//! Only compiled with `--features qdrant`. To run locally:
//!
//! ```bash
//! APEX_QDRANT_URL=http://127.0.0.1:6333 \
//!   cargo test -p apex-provider --features qdrant --test semantic_cache_qdrant -- --nocapture
//! ```
//!
//! Data is isolated by a per-run nonce `param_key`; a shared collection
//! (`apex_it_semcache`) is reused across runs (points keyed off param_key+vector).

#![cfg(feature = "qdrant")]

use apex_provider::{ChatResponse, Message, QdrantSemanticCache, SemanticCacheStore};
use std::time::{SystemTime, UNIX_EPOCH};

const COLLECTION: &str = "apex_it_semcache";

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// A cache over the configured Qdrant, or `None` (logging a skip) when unconfigured.
fn cache() -> Option<QdrantSemanticCache> {
    match std::env::var("APEX_QDRANT_URL") {
        Ok(url) => Some(QdrantSemanticCache::new(url, COLLECTION)),
        Err(_) => {
            eprintln!("skipping: APEX_QDRANT_URL not set");
            None
        }
    }
}

fn response(text: &str) -> ChatResponse {
    ChatResponse {
        message: Message::assistant(text),
        model: "m".to_string(),
        usage: apex_common::Usage::new(3, 2, 0.01),
        finish_reason: "stop".to_string(),
    }
}

/// The embedding-model id used by these fixtures (RM-AIM-P2 RAG-203).
const EMB: &str = "it-embedding-model";

#[tokio::test]
async fn qdrant_semantic_cache_round_trip_and_gating() {
    let Some(cache) = cache() else { return };
    let pk = format!("m|None|{}", nonce());
    let vec_a = vec![1.0_f32, 0.0, 0.0];

    // Miss before anything is stored.
    let miss = cache.lookup(&pk, EMB, &vec_a, 0.95, 60_000, 1_000).await;
    match miss {
        Ok(None) => {}
        Ok(Some(_)) => panic!("unexpected hit before store"),
        Err(e) => {
            eprintln!("skipping: qdrant unreachable: {e}");
            return;
        }
    }

    // Store, then an identical-vector lookup is a hit returning the same response.
    cache
        .store(&pk, EMB, &vec_a, &response("paris"), 1_000)
        .await
        .expect("store");
    let hit = cache
        .lookup(&pk, EMB, &vec_a, 0.95, 60_000, 1_500)
        .await
        .expect("lookup");
    assert_eq!(
        hit.and_then(|r| r.message.content).as_deref(),
        Some("paris"),
        "identical vector should hit and return the cached response"
    );

    // A different param_key (incompatible params) does not see the entry.
    let other_pk = format!("m|Some(0.7)|{}", nonce());
    let cross = cache
        .lookup(&other_pk, EMB, &vec_a, 0.95, 60_000, 1_500)
        .await
        .expect("lookup");
    assert!(cross.is_none(), "param-incompatible key must not hit");

    // A different embedding model does not see the entry (RAG-203), even for
    // the identical vector and param key.
    let cross_model = cache
        .lookup(&pk, "some-other-model", &vec_a, 0.95, 60_000, 1_500)
        .await
        .expect("lookup");
    assert!(
        cross_model.is_none(),
        "an entry from a different embedding model must not be served"
    );

    // An expired TTL (created at 1_000, now far later) is a miss.
    let expired = cache
        .lookup(&pk, EMB, &vec_a, 0.95, 10, 1_000_000)
        .await
        .expect("lookup");
    assert!(expired.is_none(), "entry past its TTL must not hit");

    // A dissimilar vector below threshold is a miss.
    let dissimilar = cache
        .lookup(&pk, EMB, &[0.0, 1.0, 0.0], 0.95, 60_000, 1_500)
        .await
        .expect("lookup");
    assert!(dissimilar.is_none(), "below-threshold similarity must miss");
}
