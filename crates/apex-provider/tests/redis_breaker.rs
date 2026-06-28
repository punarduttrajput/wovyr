//! Live integration test for the Redis-shared circuit breaker.
//!
//! Capability-gated like the tool sandbox / memory tiered tests: it reads
//! `APEX_REDIS_URL` and returns early (logging a skip) when unset or unreachable, so
//! the suite still passes on offline CI without a Redis. The shared state machine
//! itself is unit-tested offline with `InMemoryKv` (see `src/resilience.rs`); this
//! file verifies the real `RedisKv` adapter and cross-instance visibility.
//!
//! Only compiled with `--features redis`. To run locally:
//!
//! ```bash
//! APEX_REDIS_URL=redis://127.0.0.1:6379 \
//!   cargo test -p apex-provider --features redis --test redis_breaker -- --nocapture
//! ```

#![cfg(feature = "redis")]

use apex_provider::{BreakerConfig, CircuitBreaker, RedisKv, SharedCircuitBreaker};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A unique key prefix per run so repeated runs (and parallel tests) don't collide.
fn prefix() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("apex:it:breaker:{nonce}")
}

/// Connect a shared `RedisKv`, or `None` (logging a skip) when unconfigured/unreachable.
async fn kv() -> Option<Arc<RedisKv>> {
    let url = match std::env::var("APEX_REDIS_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: APEX_REDIS_URL not set");
            return None;
        }
    };
    match RedisKv::connect(&url).await {
        Ok(k) => Some(Arc::new(k)),
        Err(e) => {
            eprintln!("skipping: redis unreachable at {url}: {e}");
            None
        }
    }
}

fn cfg() -> BreakerConfig {
    BreakerConfig {
        failure_threshold: 3,
        open_duration_ms: 1_000,
        half_open_max_calls: 1,
    }
}

#[tokio::test]
async fn redis_breaker_trips_and_recovers_across_instances() {
    let Some(kv) = kv().await else { return };
    let p = prefix();

    // Two breakers over the same Redis prefix = two gateway nodes in a fleet.
    let node_a = SharedCircuitBreaker::new(cfg(), kv.clone(), p.clone());
    let node_b = SharedCircuitBreaker::new(cfg(), kv.clone(), p.clone());

    assert!(node_a.allow(0).await, "starts closed");
    node_a.on_failure(0).await;
    node_a.on_failure(0).await;
    assert!(node_b.allow(0).await, "still closed after 2 failures");

    node_a.on_failure(0).await; // third → trips open fleet-wide
    assert!(
        !node_b.allow(10).await,
        "node B observes the shared open state"
    );

    // After the cool-down, the single half-open trial is shared across nodes.
    assert!(node_b.allow(1_000).await, "half-open trial");
    assert!(
        !node_a.allow(1_000).await,
        "trial budget is shared: node A blocked"
    );

    // A success closes the breaker for the whole fleet.
    node_b.on_success().await;
    assert!(node_a.allow(2_000).await, "success closes it fleet-wide");
}

#[tokio::test]
async fn redis_breaker_half_open_failure_reopens() {
    let Some(kv) = kv().await else { return };
    let cb = SharedCircuitBreaker::new(
        BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 1_000,
            half_open_max_calls: 1,
        },
        kv,
        prefix(),
    );

    cb.on_failure(0).await; // trips open
    assert!(!cb.allow(50).await, "open during cool-down");
    assert!(cb.allow(1_000).await, "half-open trial after cool-down");
    cb.on_failure(1_000).await; // trial fails → reopen
    assert!(!cb.allow(1_050).await, "reopened after half-open failure");
}
