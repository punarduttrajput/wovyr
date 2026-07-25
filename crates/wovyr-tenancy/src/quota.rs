//! Quota enforcement primitives ([Projects API §5](../../docs/09-api/projects.md#5-quotas)).
//!
//! [`QuotaLimits`] holds the per-org/project ceilings; the check methods are **pure**
//! (the caller supplies the current usage and the proposed delta), so windowing
//! (per-day / per-minute) and usage accounting live with the enforcing subsystem and no
//! ambient clock enters this crate. A breach returns
//! [`Error::QuotaExceeded`](wovyr_common::Error::QuotaExceeded), which the API maps to
//! `429`/`402`.

use crate::model::QuotaLimits;
use wovyr_common::{Error, Result};

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

    /// Admit `adding` LLM tokens given `used_today` already this rolling day
    /// (RM-AIM-P2 SRV-202).
    pub fn check_llm_tokens(&self, used_today: u64, adding: u64) -> Result<()> {
        check_count(
            "llm_tokens_per_day",
            self.llm_tokens_per_day,
            used_today,
            adding,
        )
    }

    /// Admit one more configured MCP connection given `current` already
    /// configured for the tenant (RM-MCX-P1-106, PRD-006).
    pub fn check_mcp_connections(&self, current: u64) -> Result<()> {
        check_count("max_mcp_connections", self.max_mcp_connections, current, 1)
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

    #[test]
    fn enforces_token_budget_at_the_threshold() {
        let q = QuotaLimits {
            llm_tokens_per_day: Some(1_000),
            ..Default::default()
        };
        assert!(q.check_llm_tokens(900, 100).is_ok()); // exactly at the limit
        let err = q.check_llm_tokens(900, 101).unwrap_err();
        assert!(matches!(err, Error::QuotaExceeded(_)));
        // Unset = unlimited, same contract as every other dimension.
        assert!(
            QuotaLimits::default()
                .check_llm_tokens(u64::MAX / 2, 1)
                .is_ok()
        );
    }

    #[test]
    fn enforces_mcp_connection_count_at_the_boundary() {
        let q = QuotaLimits {
            max_mcp_connections: Some(3),
            ..Default::default()
        };
        assert!(q.check_mcp_connections(2).is_ok()); // 2 + 1 = 3 <= 3
        let err = q.check_mcp_connections(3).unwrap_err(); // 3 + 1 = 4 > 3
        assert!(matches!(err, Error::QuotaExceeded(_)));
        // Unset = unlimited, same contract as every other dimension.
        assert!(QuotaLimits::default().check_mcp_connections(1_000).is_ok());
    }

    /// SRV-202: a stored quota written before the dead dimensions were removed
    /// still deserializes — the unknown fields are ignored, not an error.
    #[test]
    fn legacy_quota_json_with_removed_fields_still_loads() {
        let legacy = r#"{
            "llm_cost_per_day_usd": 10.0,
            "tool_executions_per_minute": 60,
            "memory_records": 1000,
            "concurrent_agent_runs": 5
        }"#;
        let q: QuotaLimits = serde_json::from_str(legacy).unwrap();
        assert_eq!(q.llm_cost_per_day_usd, Some(10.0));
        assert_eq!(q.concurrent_agent_runs, Some(5));
        assert_eq!(q.llm_tokens_per_day, None);
    }
}
