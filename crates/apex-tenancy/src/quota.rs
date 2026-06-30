//! Quota enforcement primitives ([Projects API §5](../../docs/09-api/projects.md#5-quotas)).
//!
//! [`QuotaLimits`] holds the per-org/project ceilings; the check methods are **pure**
//! (the caller supplies the current usage and the proposed delta), so windowing
//! (per-day / per-minute) and usage accounting live with the enforcing subsystem and no
//! ambient clock enters this crate. A breach returns
//! [`Error::QuotaExceeded`](apex_common::Error::QuotaExceeded), which the API maps to
//! `429`/`402`.

use crate::model::QuotaLimits;
use apex_common::{Error, Result};

impl QuotaLimits {
    /// Admit one more concurrent agent run given `current` already running.
    pub fn check_concurrent_runs(&self, current: u64) -> Result<()> {
        check_count(
            "concurrent_agent_runs",
            self.concurrent_agent_runs,
            current,
            1,
        )
    }

    /// Admit `adding` new memory records given `current` already stored.
    pub fn check_memory_records(&self, current: u64, adding: u64) -> Result<()> {
        check_count("memory_records", self.memory_records, current, adding)
    }

    /// Admit `adding` tool executions given `in_window` already run this minute.
    pub fn check_tool_executions(&self, in_window: u64, adding: u64) -> Result<()> {
        check_count(
            "tool_executions_per_minute",
            self.tool_executions_per_minute,
            in_window,
            adding,
        )
    }

    /// Admit `adding` USD of LLM spend given `spent_today` already this rolling day.
    pub fn check_llm_cost(&self, spent_today: f64, adding: f64) -> Result<()> {
        match self.llm_cost_per_day_usd {
            Some(limit) if spent_today + adding > limit => Err(Error::quota_exceeded(format!(
                "llm_cost_per_day_usd: {:.4} + {:.4} exceeds limit {:.4}",
                spent_today, adding, limit
            ))),
            _ => Ok(()),
        }
    }
}

/// Shared count check: `Ok` when unlimited or `current + delta <= limit`.
fn check_count(metric: &str, limit: Option<u64>, current: u64, delta: u64) -> Result<()> {
    match limit {
        Some(limit) if current.saturating_add(delta) > limit => Err(Error::quota_exceeded(
            format!("{metric}: {current} + {delta} exceeds limit {limit}"),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_when_unset() {
        let q = QuotaLimits::default();
        assert!(q.check_concurrent_runs(1_000_000).is_ok());
        assert!(q.check_llm_cost(1e9, 1e9).is_ok());
    }

    #[test]
    fn enforces_count_at_the_boundary() {
        let q = QuotaLimits {
            concurrent_agent_runs: Some(2),
            ..Default::default()
        };
        assert!(q.check_concurrent_runs(1).is_ok()); // 1 + 1 = 2 <= 2
        let err = q.check_concurrent_runs(2).unwrap_err(); // 2 + 1 = 3 > 2
        assert!(matches!(err, Error::QuotaExceeded(_)));
    }

    #[test]
    fn enforces_cost_limit() {
        let q = QuotaLimits {
            llm_cost_per_day_usd: Some(10.0),
            ..Default::default()
        };
        assert!(q.check_llm_cost(9.5, 0.5).is_ok());
        assert!(q.check_llm_cost(9.5, 1.0).is_err());
    }
}
