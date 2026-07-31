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
            Some(limit) if spent_today + adding > limit => {
                // One precision for all three figures, so the message reads as the
                // comparison it is. Per-figure precision would print
                // `0.00000360 + 0.00 exceeds limit 0.00000100` — the admission check
                // passes `adding = 0.0`, and a bare `0.00` beside eight-decimal
                // siblings invites the reader to line up digits that don't line up.
                let p = usd_precision(&[spent_today, adding, limit]);
                Err(Error::quota_exceeded(format!(
                    "llm_cost_per_day_usd: {spent_today:.p$} + {adding:.p$} exceeds limit {limit:.p$}"
                )))
            }
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

/// Decimal places that render every one of `values` readably *at its own scale* —
/// the widest requirement across the set wins, so a group of related figures is
/// formatted consistently and can actually be compared digit by digit.
///
/// A fixed `{:.4}` turned every figure a real deployment produces into noise: a
/// single `gpt-4o-mini` reply costs on the order of `$0.000008`, so a breach of a
/// micro-dollar budget read `llm_cost_per_day_usd: 0.0000 + 0.0000 exceeds limit
/// 0.0000` — three zeros, and no way to tell what was spent, what was asked for, or
/// what the ceiling was. Human-scale money stays at two decimals (`0.50`); anything
/// below a cent gets the places it needs to show three significant digits, capped at
/// 8 (a tenth of a microdollar, below any real per-call price). Zero carries no
/// scale of its own and never widens the group.
///
/// Pure: `f64` arithmetic only, no clock or config.
fn usd_precision(values: &[f64]) -> usize {
    values
        .iter()
        .map(|v| {
            let magnitude = v.abs();
            if magnitude == 0.0 || magnitude >= 0.01 {
                2
            } else {
                // First significant digit sits at 10^floor(log10(m)); three digits
                // from there means 0.000008 -> 8 places, 0.005 -> 5.
                let leading_zeros = -magnitude.log10().floor() as i32;
                (leading_zeros + 2).clamp(2, 8) as usize
            }
        })
        .max()
        .unwrap_or(2)
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

    /// A micro-dollar breach must say what was actually spent. Under the old fixed
    /// `{:.4}` every figure at real per-call scale rendered `0.0000`, so the message
    /// named the metric and then told the operator nothing.
    #[test]
    fn a_micro_dollar_breach_reports_real_figures_not_rounded_zeros() {
        let q = QuotaLimits {
            llm_cost_per_day_usd: Some(0.000001),
            ..Default::default()
        };
        let msg = q
            .check_llm_cost(0.0000078, 0.0000066)
            .unwrap_err()
            .to_string();
        assert!(
            !msg.contains("0.0000 "),
            "figures must not collapse to rounded zeros: {msg}"
        );
        for figure in ["0.00000780", "0.00000660", "0.00000100"] {
            assert!(msg.contains(figure), "expected {figure} in: {msg}");
        }
    }

    /// The admission check runs *before* the call, so `adding` is `0.0`. It must
    /// still print at the group's precision — `0.00` beside eight-decimal figures
    /// reads like a different unit.
    #[test]
    fn a_zero_delta_is_formatted_at_the_same_scale_as_its_siblings() {
        let q = QuotaLimits {
            llm_cost_per_day_usd: Some(0.000001),
            ..Default::default()
        };
        let msg = q.check_llm_cost(0.0000036, 0.0).unwrap_err().to_string();
        assert!(
            msg.contains("0.00000360 + 0.00000000 exceeds limit 0.00000100"),
            "got: {msg}"
        );
    }

    #[test]
    fn usd_precision_scales_to_the_amount_and_is_shared_across_a_group() {
        // Human-scale money stays at two decimals.
        assert_eq!(usd_precision(&[0.0]), 2);
        assert_eq!(usd_precision(&[12.5]), 2);
        assert_eq!(usd_precision(&[0.01]), 2);
        // Below a cent, enough places for three significant digits.
        assert_eq!(usd_precision(&[0.005]), 5);
        assert_eq!(usd_precision(&[0.000008]), 8);
        // Capped, so an absurdly small value can't produce an endless tail.
        assert_eq!(usd_precision(&[0.00000000001]), 8);
        // The widest requirement wins, so a group formats consistently — and a zero
        // (which has no scale of its own) never drags the group back down to 2.
        assert_eq!(usd_precision(&[0.00000360, 0.0, 0.00000100]), 8);
        assert_eq!(usd_precision(&[10.0, 0.5]), 2);
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
