//! [`EvalReport`]: the aggregate result of running an [`crate::EvalSuite`].
//! `PartialEq` on both types is what lets a test assert two runs produced a
//! byte-identical report — the reproducibility claim this crate exists to
//! prove.

use apex_common::Usage;
use serde::{Deserialize, Serialize};

/// One case's result within a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaseResult {
    pub id: String,
    pub passed: bool,
    pub detail: String,
    pub usage: Usage,
}

/// The full result of running a suite once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalReport {
    pub suite: String,
    pub cases: Vec<CaseResult>,
    pub passed: usize,
    pub total: usize,
    pub pass_rate: f64,
    pub usage: Usage,
}

impl EvalReport {
    /// Build a report from a suite name and its per-case results, computing
    /// the aggregate pass count/rate and total usage. Pure — no I/O, no clock.
    pub fn from_cases(suite: impl Into<String>, cases: Vec<CaseResult>) -> Self {
        let total = cases.len();
        let passed = cases.iter().filter(|c| c.passed).count();
        let pass_rate = if total == 0 {
            0.0
        } else {
            passed as f64 / total as f64
        };
        let mut usage = Usage::default();
        for case in &cases {
            usage.add(case.usage);
        }
        Self {
            suite: suite.into(),
            cases,
            passed,
            total,
            pass_rate,
            usage,
        }
    }

    /// The ids of every failing case, in suite order.
    pub fn failing_case_ids(&self) -> Vec<&str> {
        self.cases
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, passed: bool) -> CaseResult {
        CaseResult {
            id: id.to_string(),
            passed,
            detail: String::new(),
            usage: Usage::new(1, 1, 0.0),
        }
    }

    #[test]
    fn aggregates_pass_rate_and_usage() {
        let report = EvalReport::from_cases("s", vec![case("a", true), case("b", false)]);
        assert_eq!(report.passed, 1);
        assert_eq!(report.total, 2);
        assert_eq!(report.pass_rate, 0.5);
        assert_eq!(report.usage.total_tokens, 4);
        assert_eq!(report.failing_case_ids(), vec!["b"]);
    }

    #[test]
    fn empty_suite_has_zero_pass_rate_not_nan() {
        let report = EvalReport::from_cases("empty", vec![]);
        assert_eq!(report.pass_rate, 0.0);
    }
}
