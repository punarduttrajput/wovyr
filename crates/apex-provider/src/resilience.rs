//! Gateway resilience primitives: retry/backoff, circuit breaking, response
//! caching, and cost events.
//!
//! Implements the building blocks from the
//! [Resilience spec](../../docs/05-llm-gateway/resilience.md) (retry → failover →
//! circuit break → error map) and the [Caching spec](../../docs/05-llm-gateway/caching.md)
//! (exact cache with cost/savings events on hit). The [`Gateway`](crate::Gateway)
//! wires these together.
//!
//! v0.2 slice: in-process exact cache and per-provider breakers. Redis-shared
//! breaker state, semantic caching, and hedging are deferred.

use crate::types::ChatResponse;
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
}

/// Cache configuration.
#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    /// Cache mode.
    pub mode: CacheMode,
    /// Entry time-to-live in milliseconds.
    pub ttl_ms: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mode: CacheMode::Off,
            ttl_ms: 3_600_000,
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

/// A per-provider circuit breaker.
///
/// Time is passed in as `now_ms` (a monotonic millisecond clock) so the logic is
/// fully testable without sleeping.
pub struct CircuitBreaker {
    cfg: BreakerConfig,
    inner: Mutex<Inner>,
}

struct Inner {
    status: BreakerStatus,
    consecutive_failures: u32,
    open_until_ms: u64,
    half_open_calls: u32,
}

impl CircuitBreaker {
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

    /// Whether a request may be sent to this provider now.
    pub fn allow(&self, now_ms: u64) -> bool {
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

    /// Record a success: close the breaker.
    pub fn on_success(&self) {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        s.status = BreakerStatus::Closed;
        s.consecutive_failures = 0;
        s.half_open_calls = 0;
    }

    /// Record a failure at `now_ms`: trip open on threshold (or any half-open fail).
    pub fn on_failure(&self, now_ms: u64) {
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

/// A cached chat response with its insertion time.
pub(crate) struct CacheEntry {
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

    #[test]
    fn breaker_trips_after_threshold_and_recovers() {
        let cb = CircuitBreaker::new(BreakerConfig {
            failure_threshold: 3,
            open_duration_ms: 1_000,
            half_open_max_calls: 1,
        });
        assert!(cb.allow(0));
        cb.on_failure(0);
        cb.on_failure(0);
        assert!(cb.allow(0), "still closed after 2 failures");
        cb.on_failure(0); // third → trips open
        assert!(!cb.allow(10), "open: requests blocked during cooldown");

        // After cooldown, one trial is allowed (half-open).
        assert!(cb.allow(1_000));
        // Success closes it again.
        cb.on_success();
        assert!(cb.allow(2_000));
    }

    #[test]
    fn half_open_failure_reopens() {
        let cb = CircuitBreaker::new(BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 100,
            half_open_max_calls: 1,
        });
        cb.on_failure(0); // trips open
        assert!(!cb.allow(50));
        assert!(cb.allow(100)); // half-open trial
        cb.on_failure(100); // trial fails → reopen
        assert!(!cb.allow(150));
    }
}
