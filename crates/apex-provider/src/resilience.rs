//! Gateway resilience primitives: retry/backoff, circuit breaking, response
//! caching, and cost events.
//!
//! Implements the building blocks from the
//! [Resilience spec](../../docs/05-llm-gateway/resilience.md) (retry → failover →
//! circuit break → error map) and the [Caching spec](../../docs/05-llm-gateway/caching.md)
//! (exact cache with cost/savings events on hit). The [`Gateway`](crate::Gateway)
//! wires these together.
//!
//! v0.2 slice: in-process exact cache and per-provider breakers, plus an optional
//! **Redis-shared breaker** so a fleet of gateways reacts to a failing provider
//! consistently ([resilience §6](../../docs/05-llm-gateway/resilience.md)). Semantic
//! caching and hedging are deferred.

use crate::types::ChatResponse;
use apex_common::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Retry policy for a single provider ([resilience §4](../../docs/05-llm-gateway/resilience.md)).
#[derive(Clone, Copy, Debug)]
pub struct RetryConfig {
    /// Max attempts against one provider (including the first).
    pub max_attempts: u32,
    /// Base backoff delay in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum backoff delay in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 200,
            max_delay_ms: 4_000,
        }
    }
}

impl RetryConfig {
    /// Exponential backoff before the given 1-based `attempt`, capped at `max_delay_ms`.
    pub fn backoff(&self, attempt: u32) -> Duration {
        let exp = self.base_delay_ms as f64 * 2f64.powi(attempt.saturating_sub(1) as i32);
        Duration::from_millis(exp.min(self.max_delay_ms as f64) as u64)
    }
}

/// Response cache mode ([caching §2](../../docs/05-llm-gateway/caching.md)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheMode {
    /// No lookup, no store (safe default).
    Off,
    /// Lookup/store on an exact normalized-request hash.
    Exact,
    /// Exact lookup first, then by embedding similarity; store in both on a miss
    /// ([caching §4](../../docs/05-llm-gateway/caching.md)).
    Semantic,
}

/// Cache configuration.
#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    /// Cache mode.
    pub mode: CacheMode,
    /// Entry time-to-live in milliseconds.
    pub ttl_ms: u64,
    /// Minimum cosine similarity for a semantic hit (a higher value trades hit
    /// rate for safety; ignored unless `mode` is `Semantic`).
    pub similarity_threshold: f32,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mode: CacheMode::Off,
            ttl_ms: 3_600_000,
            similarity_threshold: 0.95,
        }
    }
}

/// A metered cost (or savings) event ([caching §9](../../docs/05-llm-gateway/caching.md)).
#[derive(Clone, Debug)]
pub struct CostEvent {
    /// Provider that served (or would have served) the request.
    pub provider: String,
    /// Concrete model.
    pub model: String,
    /// Prompt tokens.
    pub prompt_tokens: u32,
    /// Completion tokens.
    pub completion_tokens: u32,
    /// Actual cost charged (0 on a cache hit).
    pub cost_usd: f64,
    /// Cache disposition: `None` (live), or `Some("exact")` on a hit.
    pub cache: Option<String>,
    /// On a cache hit, the cost the live call would have incurred.
    pub estimated_savings_usd: f64,
}

/// Receives cost events emitted by the gateway.
pub trait CostObserver: Send + Sync {
    /// Handle one cost event.
    fn on_cost(&self, event: CostEvent);
}

// ---------------------------------------------------------------------------
// Circuit breaker
// ---------------------------------------------------------------------------

/// Circuit breaker configuration ([resilience §6](../../docs/05-llm-gateway/resilience.md)).
#[derive(Clone, Copy, Debug)]
pub struct BreakerConfig {
    /// Consecutive failures that trip the breaker open.
    pub failure_threshold: u32,
    /// Cool-down before a trial (half-open) request is allowed.
    pub open_duration_ms: u64,
    /// Trial requests allowed while half-open.
    pub half_open_max_calls: u32,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_duration_ms: 15_000,
            half_open_max_calls: 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BreakerStatus {
    Closed,
    Open,
    HalfOpen,
}

/// A per-provider circuit breaker. The gateway consults one per candidate provider:
/// `allow` gates dispatch, and `on_success`/`on_failure` feed the state machine.
///
/// Time is passed in as `now_ms` (a monotonic millisecond clock) so the logic is
/// testable without sleeping. Methods are async so an implementation may consult
/// shared state (e.g. Redis) — see [`SharedCircuitBreaker`]; the default
/// [`LocalCircuitBreaker`] keeps everything in process.
#[async_trait]
pub trait CircuitBreaker: Send + Sync {
    /// Whether a request may be sent to this provider now.
    async fn allow(&self, now_ms: u64) -> bool;
    /// Record a success: close the breaker.
    async fn on_success(&self);
    /// Record a failure at `now_ms`: trip open on threshold (or any half-open fail).
    async fn on_failure(&self, now_ms: u64);
}

/// An in-process per-provider circuit breaker (the default, offline path).
pub struct LocalCircuitBreaker {
    cfg: BreakerConfig,
    inner: Mutex<Inner>,
}

struct Inner {
    status: BreakerStatus,
    consecutive_failures: u32,
    open_until_ms: u64,
    half_open_calls: u32,
}

impl LocalCircuitBreaker {
    /// Construct a closed breaker.
    pub fn new(cfg: BreakerConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(Inner {
                status: BreakerStatus::Closed,
                consecutive_failures: 0,
                open_until_ms: 0,
                half_open_calls: 0,
            }),
        }
    }
}

#[async_trait]
impl CircuitBreaker for LocalCircuitBreaker {
    async fn allow(&self, now_ms: u64) -> bool {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        match s.status {
            BreakerStatus::Closed => true,
            BreakerStatus::Open => {
                if now_ms >= s.open_until_ms {
                    s.status = BreakerStatus::HalfOpen;
                    s.half_open_calls = 1;
                    true
                } else {
                    false
                }
            }
            BreakerStatus::HalfOpen => {
                if s.half_open_calls < self.cfg.half_open_max_calls {
                    s.half_open_calls += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    async fn on_success(&self) {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        s.status = BreakerStatus::Closed;
        s.consecutive_failures = 0;
        s.half_open_calls = 0;
    }

    async fn on_failure(&self, now_ms: u64) {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        s.consecutive_failures += 1;
        if s.status == BreakerStatus::HalfOpen
            || s.consecutive_failures >= self.cfg.failure_threshold
        {
            s.status = BreakerStatus::Open;
            s.open_until_ms = now_ms + self.cfg.open_duration_ms;
            s.half_open_calls = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Shared (fleet-wide) circuit breaker over a small key-value primitive
// ---------------------------------------------------------------------------

/// Minimal atomic key-value operations a [`SharedCircuitBreaker`] needs from a
/// shared store (Redis in production; [`InMemoryKv`] for tests/single-process).
///
/// Counters use server-side atomic increment so concurrent gateways don't race;
/// TTLs bound how long failure state and half-open trial budgets live.
#[async_trait]
pub trait BreakerKv: Send + Sync {
    /// Atomically increment the integer at `key`, returning the new value. On
    /// first creation the key is given a `ttl_ms` lifetime.
    async fn incr(&self, key: &str, ttl_ms: u64) -> Result<i64>;
    /// Read an integer value, or `None` if the key is absent/expired.
    async fn get_i64(&self, key: &str) -> Result<Option<i64>>;
    /// Set an integer value with a `ttl_ms` lifetime.
    async fn set_i64(&self, key: &str, value: i64, ttl_ms: u64) -> Result<()>;
    /// Delete the given keys (missing keys are ignored).
    async fn del(&self, keys: &[&str]) -> Result<()>;
}

/// A circuit breaker whose state lives in a shared [`BreakerKv`], so every gateway
/// in a fleet sees the same provider health. The consecutive-failure state machine
/// mirrors [`LocalCircuitBreaker`], expressed with atomic counters:
///
/// - `…:fails` — consecutive failures (atomic `INCR`; cleared on success).
/// - `…:open`  — millisecond timestamp the breaker stays open until.
/// - `…:half`  — half-open trial budget consumed since the cool-down elapsed.
///
/// The breaker is **advisory**: any KV error fails *open* (request allowed, error
/// logged) so a store outage never blackholes traffic.
pub struct SharedCircuitBreaker {
    cfg: BreakerConfig,
    kv: std::sync::Arc<dyn BreakerKv>,
    prefix: String,
}

impl SharedCircuitBreaker {
    /// A breaker keyed under `prefix` (typically `apex:breaker:<provider>`).
    pub fn new(
        cfg: BreakerConfig,
        kv: std::sync::Arc<dyn BreakerKv>,
        prefix: impl Into<String>,
    ) -> Self {
        Self {
            cfg,
            kv,
            prefix: prefix.into(),
        }
    }

    fn k_fails(&self) -> String {
        format!("{}:fails", self.prefix)
    }
    fn k_open(&self) -> String {
        format!("{}:open", self.prefix)
    }
    fn k_half(&self) -> String {
        format!("{}:half", self.prefix)
    }

    /// Failure counters live a few cool-down windows so stale state self-heals.
    fn fails_ttl_ms(&self) -> u64 {
        (self.cfg.open_duration_ms.saturating_mul(4)).max(30_000)
    }
}

#[async_trait]
impl CircuitBreaker for SharedCircuitBreaker {
    async fn allow(&self, now_ms: u64) -> bool {
        match self.kv.get_i64(&self.k_open()).await {
            Ok(None) => true,                                            // closed
            Ok(Some(open_until)) if now_ms < open_until as u64 => false, // cooling down
            Ok(Some(_)) => {
                // Cool-down elapsed → half-open: allow a bounded number of trials.
                match self
                    .kv
                    .incr(&self.k_half(), self.cfg.open_duration_ms)
                    .await
                {
                    Ok(n) => n as u32 <= self.cfg.half_open_max_calls,
                    Err(e) => {
                        tracing::warn!("breaker kv incr failed, allowing: {e}");
                        true
                    }
                }
            }
            Err(e) => {
                tracing::warn!("breaker kv get failed, allowing: {e}");
                true
            }
        }
    }

    async fn on_success(&self) {
        // Close and reset all state.
        if let Err(e) = self
            .kv
            .del(&[&self.k_fails(), &self.k_open(), &self.k_half()])
            .await
        {
            tracing::warn!("breaker kv del failed on success: {e}");
        }
    }

    async fn on_failure(&self, now_ms: u64) {
        let fails = match self.kv.incr(&self.k_fails(), self.fails_ttl_ms()).await {
            Ok(n) => n as u32,
            Err(e) => {
                tracing::warn!("breaker kv incr failed on failure: {e}");
                return;
            }
        };
        // A failure while half-open (cool-down already elapsed) reopens immediately.
        let in_half_open = matches!(
            self.kv.get_i64(&self.k_open()).await,
            Ok(Some(open_until)) if now_ms >= open_until as u64
        );
        if in_half_open || fails >= self.cfg.failure_threshold {
            let open_until = now_ms + self.cfg.open_duration_ms;
            let _ = self
                .kv
                .set_i64(&self.k_open(), open_until as i64, self.fails_ttl_ms())
                .await;
            let _ = self.kv.del(&[&self.k_half()]).await;
        }
    }
}

/// An in-process [`BreakerKv`] backed by a `HashMap` — for tests and single-process
/// multi-gateway setups. TTLs are accepted but not enforced (no expiry sweep).
#[derive(Default)]
pub struct InMemoryKv {
    map: Mutex<HashMap<String, i64>>,
}

impl InMemoryKv {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BreakerKv for InMemoryKv {
    async fn incr(&self, key: &str, _ttl_ms: u64) -> Result<i64> {
        let mut m = self.map.lock().expect("kv mutex poisoned");
        let v = m.entry(key.to_string()).or_insert(0);
        *v += 1;
        Ok(*v)
    }
    async fn get_i64(&self, key: &str) -> Result<Option<i64>> {
        Ok(self
            .map
            .lock()
            .expect("kv mutex poisoned")
            .get(key)
            .copied())
    }
    async fn set_i64(&self, key: &str, value: i64, _ttl_ms: u64) -> Result<()> {
        self.map
            .lock()
            .expect("kv mutex poisoned")
            .insert(key.to_string(), value);
        Ok(())
    }
    async fn del(&self, keys: &[&str]) -> Result<()> {
        let mut m = self.map.lock().expect("kv mutex poisoned");
        for k in keys {
            m.remove(*k);
        }
        Ok(())
    }
}

/// A cached chat response with its insertion time.
pub(crate) struct CacheEntry {
    pub response: ChatResponse,
    pub created_ms: u64,
}

/// A semantic-cache entry: the response plus the request embedding and a
/// param-compatibility key it may be served for ([caching §4](../../docs/05-llm-gateway/caching.md)).
pub(crate) struct SemanticEntry {
    pub embedding: Vec<f32>,
    pub param_key: String,
    pub response: ChatResponse,
    pub created_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let r = RetryConfig {
            max_attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 500,
        };
        assert_eq!(r.backoff(1), Duration::from_millis(100));
        assert_eq!(r.backoff(2), Duration::from_millis(200));
        assert_eq!(r.backoff(3), Duration::from_millis(400));
        assert_eq!(r.backoff(4), Duration::from_millis(500)); // capped
    }

    #[tokio::test]
    async fn breaker_trips_after_threshold_and_recovers() {
        let cb = LocalCircuitBreaker::new(BreakerConfig {
            failure_threshold: 3,
            open_duration_ms: 1_000,
            half_open_max_calls: 1,
        });
        assert!(cb.allow(0).await);
        cb.on_failure(0).await;
        cb.on_failure(0).await;
        assert!(cb.allow(0).await, "still closed after 2 failures");
        cb.on_failure(0).await; // third → trips open
        assert!(
            !cb.allow(10).await,
            "open: requests blocked during cooldown"
        );

        // After cooldown, one trial is allowed (half-open).
        assert!(cb.allow(1_000).await);
        // Success closes it again.
        cb.on_success().await;
        assert!(cb.allow(2_000).await);
    }

    #[tokio::test]
    async fn half_open_failure_reopens() {
        let cb = LocalCircuitBreaker::new(BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 100,
            half_open_max_calls: 1,
        });
        cb.on_failure(0).await; // trips open
        assert!(!cb.allow(50).await);
        assert!(cb.allow(100).await); // half-open trial
        cb.on_failure(100).await; // trial fails → reopen
        assert!(!cb.allow(150).await);
    }

    /// Two breakers sharing one `InMemoryKv` stand in for two gateway nodes: a trip
    /// driven through one is observed by the other (the point of Redis-shared state).
    #[tokio::test]
    async fn shared_breaker_state_is_visible_across_instances() {
        let cfg = BreakerConfig {
            failure_threshold: 3,
            open_duration_ms: 1_000,
            half_open_max_calls: 1,
        };
        let kv = std::sync::Arc::new(InMemoryKv::new());
        let node_a = SharedCircuitBreaker::new(cfg, kv.clone(), "apex:breaker:openai");
        let node_b = SharedCircuitBreaker::new(cfg, kv.clone(), "apex:breaker:openai");

        // Node A drives the provider to its failure threshold.
        node_a.on_failure(0).await;
        node_a.on_failure(0).await;
        assert!(node_b.allow(0).await, "node B sees closed after 2 failures");
        node_a.on_failure(0).await; // third → trips open fleet-wide

        // Node B (a different instance) immediately sees the open breaker.
        assert!(!node_b.allow(10).await, "node B sees the shared open state");

        // After the cool-down, node B gets the single half-open trial; a success
        // there closes the breaker for the whole fleet, including node A.
        assert!(node_b.allow(1_000).await, "half-open trial on node B");
        assert!(
            !node_a.allow(1_000).await,
            "trial budget is shared: node A is blocked"
        );
        node_b.on_success().await;
        assert!(node_a.allow(2_000).await, "success closes it fleet-wide");
    }

    /// A half-open failure (after cool-down) reopens the shared breaker.
    #[tokio::test]
    async fn shared_breaker_half_open_failure_reopens() {
        let cfg = BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 100,
            half_open_max_calls: 1,
        };
        let kv = std::sync::Arc::new(InMemoryKv::new());
        let cb = SharedCircuitBreaker::new(cfg, kv, "apex:breaker:p");
        cb.on_failure(0).await; // trips open
        assert!(!cb.allow(50).await);
        assert!(cb.allow(100).await); // half-open trial
        cb.on_failure(100).await; // trial fails → reopen
        assert!(!cb.allow(150).await);
    }
}
