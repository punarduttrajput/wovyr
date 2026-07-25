//! Token and cost accounting.
//!
//! Every model response carries [`Usage`] so the runtime can report tokens and
//! estimated cost to the caller — see the
//! [token management](../../docs/05-llm-gateway/token-management.md) spec and the
//! `done · tokens: N, cost_usd: X` line in the
//! [hello agent](../../docs/16-examples/hello-agent.md) example.

use serde::{Deserialize, Serialize};

/// Token counts and estimated cost for one or more model calls.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the prompt (input).
    pub prompt_tokens: u32,
    /// Tokens produced by the model (output).
    pub completion_tokens: u32,
    /// `prompt_tokens + completion_tokens`.
    pub total_tokens: u32,
    /// Estimated cost in US dollars.
    pub cost_usd: f64,
}

impl Usage {
    /// Build a [`Usage`], deriving `total_tokens` from the two components.
    pub fn new(prompt_tokens: u32, completion_tokens: u32, cost_usd: f64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cost_usd,
        }
    }

    /// Accumulate another usage record into this one (for multi-step runs).
    pub fn add(&mut self, other: Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.cost_usd += other.cost_usd;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_derives_total() {
        let u = Usage::new(10, 5, 0.001);
        assert_eq!(u.total_tokens, 15);
    }

    #[test]
    fn add_accumulates() {
        let mut a = Usage::new(10, 5, 0.001);
        a.add(Usage::new(20, 10, 0.002));
        assert_eq!(a.prompt_tokens, 30);
        assert_eq!(a.completion_tokens, 15);
        assert_eq!(a.total_tokens, 45);
        assert!((a.cost_usd - 0.003).abs() < f64::EPSILON);
    }
}
