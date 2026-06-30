//! Delivery retry policy
//! ([retry strategy §18](../../docs/02-architecture/event-driven-architecture.md#18-retry-strategy)):
//! exponential backoff, a configurable maximum number of attempts, and a delay cap.
//!
//! The delay computation is **pure** (`delay_ms(attempt)`); jitter and the actual sleep
//! are applied by the delivery worker at the I/O boundary, so this stays deterministic
//! and unit-testable. After [`max_attempts`](BackoffPolicy::max_attempts) the event is
//! dead-lettered.

use serde::{Deserialize, Serialize};

/// An exponential backoff policy for webhook delivery retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackoffPolicy {
    /// Base delay before the first retry, milliseconds.
    pub base_ms: u64,
    /// Maximum delivery attempts before dead-lettering.
    pub max_attempts: u32,
    /// Cap on any single backoff delay, milliseconds.
    pub max_delay_ms: u64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        // 1s, 2s, 4s, … capped at 5m, up to 6 attempts.
        Self {
            base_ms: 1_000,
            max_attempts: 6,
            max_delay_ms: 300_000,
        }
    }
}

impl BackoffPolicy {
    /// The base delay before retrying after a failed `attempt` (1-based), capped at
    /// [`max_delay_ms`](Self::max_delay_ms). The caller adds jitter. Exponential:
    /// `base * 2^(attempt-1)`.
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let shift = attempt.saturating_sub(1).min(63);
        let scaled = self.base_ms.saturating_mul(1u64 << shift);
        scaled.min(self.max_delay_ms)
    }

    /// Whether another attempt should be made after `attempts` have already been tried.
    pub fn should_retry(&self, attempts: u32) -> bool {
        attempts < self.max_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_grows_exponentially_then_caps() {
        let p = BackoffPolicy {
            base_ms: 1_000,
            max_attempts: 10,
            max_delay_ms: 8_000,
        };
        assert_eq!(p.delay_ms(1), 1_000);
        assert_eq!(p.delay_ms(2), 2_000);
        assert_eq!(p.delay_ms(3), 4_000);
        assert_eq!(p.delay_ms(4), 8_000);
        assert_eq!(p.delay_ms(5), 8_000); // capped
        assert_eq!(p.delay_ms(99), 8_000); // no overflow
    }

    #[test]
    fn should_retry_respects_max_attempts() {
        let p = BackoffPolicy {
            max_attempts: 3,
            ..Default::default()
        };
        assert!(p.should_retry(0) && p.should_retry(2));
        assert!(!p.should_retry(3) && !p.should_retry(4));
    }
}
