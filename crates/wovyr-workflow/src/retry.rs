//! Retry policy and backoff strategies.
//!
//! Implements the deterministic backoff from the
//! [Retry Engine spec](../../docs/03-workflow-engine/retry-engine.md): fixed,
//! linear, and exponential strategies, capped at a maximum delay. Whether a
//! failure is retryable is decided by [`crate::ActivityError`] (the spec's failure
//! classifier, [§8](../../docs/03-workflow-engine/retry-engine.md)).
//!
//! Jitter is intentionally **not** applied in this layer: workflow orchestration
//! must be deterministic for replay ([execution model §14](../../docs/03-workflow-engine/execution-model.md)).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Backoff strategy between retry attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RetryStrategy {
    /// Constant delay every attempt.
    Fixed,
    /// Delay grows linearly with the attempt number.
    Linear,
    /// Delay doubles (× `multiplier`) each attempt.
    #[default]
    Exponential,
}

/// A retry policy for an activity ([spec §6](../../docs/03-workflow-engine/retry-engine.md)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of attempts (including the first).
    #[serde(default = "default_max_attempts", alias = "maxAttempts")]
    pub max_attempts: u32,
    /// Backoff strategy.
    #[serde(default)]
    pub strategy: RetryStrategy,
    /// Base delay in milliseconds.
    #[serde(default = "default_initial_delay_ms", alias = "initialDelayMs")]
    pub initial_delay_ms: u64,
    /// Maximum delay in milliseconds (the backoff is capped here).
    #[serde(default = "default_max_delay_ms", alias = "maxDelayMs")]
    pub max_delay_ms: u64,
    /// Growth factor for the exponential strategy.
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
}

fn default_max_attempts() -> u32 {
    3
}
fn default_initial_delay_ms() -> u64 {
    100
}
fn default_max_delay_ms() -> u64 {
    30_000
}
fn default_multiplier() -> f64 {
    2.0
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            strategy: RetryStrategy::default(),
            initial_delay_ms: default_initial_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            multiplier: default_multiplier(),
        }
    }
}

impl RetryPolicy {
    /// Delay before the given `attempt` (1-based: `attempt = 1` is the first retry,
    /// i.e. the delay after the initial attempt failed). Capped at `max_delay_ms`.
    pub fn next_delay(&self, attempt: u32) -> Duration {
        let n = attempt.max(1);
        let ms = match self.strategy {
            RetryStrategy::Fixed => self.initial_delay_ms as f64,
            RetryStrategy::Linear => self.initial_delay_ms as f64 * n as f64,
            RetryStrategy::Exponential => {
                self.initial_delay_ms as f64 * self.multiplier.powi((n - 1) as i32)
            }
        };
        let capped = ms.min(self.max_delay_ms as f64);
        Duration::from_millis(capped as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_grows_and_caps() {
        let p = RetryPolicy {
            max_attempts: 10,
            strategy: RetryStrategy::Exponential,
            initial_delay_ms: 100,
            max_delay_ms: 1000,
            multiplier: 2.0,
        };
        assert_eq!(p.next_delay(1), Duration::from_millis(100));
        assert_eq!(p.next_delay(2), Duration::from_millis(200));
        assert_eq!(p.next_delay(3), Duration::from_millis(400));
        assert_eq!(p.next_delay(4), Duration::from_millis(800));
        // Capped at max_delay_ms.
        assert_eq!(p.next_delay(5), Duration::from_millis(1000));
        assert_eq!(p.next_delay(20), Duration::from_millis(1000));
    }

    #[test]
    fn fixed_and_linear() {
        let fixed = RetryPolicy {
            strategy: RetryStrategy::Fixed,
            initial_delay_ms: 50,
            max_delay_ms: 10_000,
            ..RetryPolicy::default()
        };
        assert_eq!(fixed.next_delay(1), Duration::from_millis(50));
        assert_eq!(fixed.next_delay(5), Duration::from_millis(50));

        let linear = RetryPolicy {
            strategy: RetryStrategy::Linear,
            initial_delay_ms: 50,
            max_delay_ms: 10_000,
            ..RetryPolicy::default()
        };
        assert_eq!(linear.next_delay(1), Duration::from_millis(50));
        assert_eq!(linear.next_delay(3), Duration::from_millis(150));
    }
}
