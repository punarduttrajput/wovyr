//! Performance test: memory warm retrieval.
//!
//! Asserts the documented NFR — warm retrieval **< 30 ms p95**
//! ([performance §3](../../../docs/15-testing/performance-tests.md#3-targets-examples),
//! [memory NFRs](../../../docs/06-memory-engine/overview.md)). The store is warmed
//! with records, then `query` latency is sampled. Offline, the mock provider's
//! embeddings are non-semantic, so this exercises the in-process embed + hybrid
//! fusion + ranking path over a populated namespace.

use apex_memory::{InMemoryStore, MemoryEngine, MemoryQuery, MemoryType};
use apex_provider::{Gateway, MockProvider};
use std::sync::Arc;
use std::time::Instant;

/// The `q`-quantile of `samples` (in microseconds).
fn pct(samples: &[u128], q: f64) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = (((sorted.len() as f64) * q) as usize).min(sorted.len() - 1);
    sorted[idx]
}

#[tokio::test]
async fn warm_retrieval_p95_under_30ms() {
    // A warm working-set sized for the latency NFR (scale testing — millions of
    // records — is a separate, out-of-scope concern per performance-tests §6).
    const RECORDS: usize = 250;
    const WARMUP: usize = 20;
    const SAMPLES: usize = 300;
    const NFR_P95_US: u128 = 30_000; // 30 ms

    let engine = MemoryEngine::new(
        Gateway::new(Box::new(MockProvider::new())),
        Arc::new(InMemoryStore::new()),
    );

    for i in 0..RECORDS {
        engine
            .remember(
                "kb",
                format!("fact number {i} about topic {}", i % 20),
                MemoryType::Semantic,
                0.5,
                vec![],
            )
            .await
            .unwrap();
    }

    let mut query = MemoryQuery::new("a fact about topic 7");
    query.namespace = Some("kb".to_string());

    for _ in 0..WARMUP {
        engine.query(&query).await.unwrap();
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let results = engine.query(&query).await.unwrap();
        samples.push(start.elapsed().as_micros());
        assert!(!results.is_empty());
    }

    let (p50, p95, p99) = (
        pct(&samples, 0.50),
        pct(&samples, 0.95),
        pct(&samples, 0.99),
    );
    eprintln!(
        "memory retrieval ({RECORDS} records): p50={p50}us p95={p95}us p99={p99}us (NFR p95 < {NFR_P95_US}us)"
    );

    assert!(
        p95 < NFR_P95_US,
        "memory retrieval p95 {p95}us exceeds the {NFR_P95_US}us NFR"
    );
}
