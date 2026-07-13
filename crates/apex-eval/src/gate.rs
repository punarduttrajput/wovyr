//! The regression **gate** (RM-AIM-P2 EVL-202): golden baselines, pass-rate
//! thresholds, and repeat-N variance — what turns the harness's reports into
//! a CI decision instead of log output.
//!
//! A [`Baseline`] is a committed golden file: the suite it gates, a minimum
//! pass rate, and every case's expected pass/fail. [`check`] compares a fresh
//! [`EvalReport`] against it, **pure and fail-closed**: a dropped pass rate, a
//! regressed case, a case that vanished from the suite, or a wrong-suite
//! baseline are all violations; an improved case or a brand-new case is a
//! note (refresh the baseline), never a silent pass of something ungated.
//! [`run_suite_repeated`]/[`VarianceReport`] add the repeat-N story: run the
//! same suite N times and report per-run pass rates plus how many *distinct*
//! reports appeared — zero variance is the expectation against a
//! deterministic provider, and any spread is visible evidence for FUT-006's
//! open live-model question rather than an invisible flake.

use crate::fixture::EvalSuite;
use crate::judge::Scorer;
use crate::report::EvalReport;
use crate::runner::run_suite_scored;
use apex_agent::AgentDefinition;
use apex_common::{Error, Result};
use apex_provider::Gateway;
use apex_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A committed golden baseline for one suite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    /// The suite this baseline gates — [`check`] rejects a report from any
    /// other suite rather than comparing apples to oranges.
    pub suite: String,
    /// The overall pass rate the report must meet (in `[0,1]`).
    pub min_pass_rate: f64,
    /// Every case's expected outcome at the time the baseline was taken
    /// (`true` = passed). `BTreeMap` so the serialized golden file is stable
    /// and diffs cleanly.
    pub cases: BTreeMap<String, bool>,
}

impl Baseline {
    /// Snapshot a report as the new golden baseline.
    pub fn from_report(report: &EvalReport, min_pass_rate: f64) -> Self {
        Self {
            suite: report.suite.clone(),
            min_pass_rate,
            cases: report
                .cases
                .iter()
                .map(|c| (c.id.clone(), c.passed))
                .collect(),
        }
    }

    /// Parse a baseline from JSON, failing closed on anything malformed.
    pub fn from_json(json: &str) -> Result<Self> {
        let baseline: Self = serde_json::from_str(json)
            .map_err(|e| Error::invalid(format!("invalid eval baseline: {e}")))?;
        if !(0.0..=1.0).contains(&baseline.min_pass_rate) {
            return Err(Error::invalid(format!(
                "baseline min_pass_rate outside [0,1]: {}",
                baseline.min_pass_rate
            )));
        }
        Ok(baseline)
    }

    /// Load a baseline from a JSON file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Serialize as pretty JSON (the committed golden-file format).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("baseline serialization cannot fail")
    }

    /// Write the baseline to a JSON file (the refresh flow).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_json() + "\n")?;
        Ok(())
    }
}

/// The outcome of gating a report against a [`Baseline`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateResult {
    /// `true` only when there are no violations.
    pub passed: bool,
    /// What failed the gate — empty when `passed`.
    pub violations: Vec<String>,
    /// Informational findings (improvements, new cases) that suggest
    /// refreshing the baseline but do not fail the gate.
    pub notes: Vec<String>,
}

/// Gate `report` against `baseline`. Pure — same inputs, same result.
///
/// Violations (any one fails the gate):
/// - the report is from a different suite than the baseline gates;
/// - the overall pass rate is below `min_pass_rate`;
/// - a case the baseline expects to pass now fails (**a regression**);
/// - a case in the baseline is missing from the report (a deleted fixture
///   must not silently shrink the gate's coverage).
///
/// Notes (reported, never a failure): a baseline-failing case now passes
/// (improvement — refresh the baseline to lock it in), or a case exists in
/// the report but not the baseline (new fixture — ungated until refreshed).
pub fn check(report: &EvalReport, baseline: &Baseline) -> GateResult {
    let mut violations = Vec::new();
    let mut notes = Vec::new();

    if report.suite != baseline.suite {
        violations.push(format!(
            "report is for suite `{}` but the baseline gates `{}`",
            report.suite, baseline.suite
        ));
    }
    // Strict comparison with a tolerance for f64 division noise only.
    if report.pass_rate + 1e-9 < baseline.min_pass_rate {
        violations.push(format!(
            "pass rate {:.4} is below the baseline threshold {:.4}",
            report.pass_rate, baseline.min_pass_rate
        ));
    }

    for (id, expected_pass) in &baseline.cases {
        match report.cases.iter().find(|c| &c.id == id) {
            None => violations.push(format!(
                "case `{id}` is in the baseline but missing from the report"
            )),
            Some(case) if *expected_pass && !case.passed => violations.push(format!(
                "case `{id}` regressed (baseline: pass, now: fail): {}",
                case.detail
            )),
            Some(case) if !*expected_pass && case.passed => notes.push(format!(
                "case `{id}` improved (baseline: fail, now: pass) — refresh the baseline"
            )),
            Some(_) => {}
        }
    }
    for case in &report.cases {
        if !baseline.cases.contains_key(&case.id) {
            notes.push(format!(
                "case `{}` is new (not in the baseline) — refresh to gate it",
                case.id
            ));
        }
    }

    GateResult {
        passed: violations.is_empty(),
        violations,
        notes,
    }
}

/// Per-run pass rates and spread across N repeats of the same suite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VarianceReport {
    /// How many times the suite ran.
    pub runs: usize,
    /// Each run's pass rate, in run order.
    pub pass_rates: Vec<f64>,
    pub mean_pass_rate: f64,
    pub min_pass_rate: f64,
    pub max_pass_rate: f64,
    /// How many byte-distinct [`EvalReport`]s the runs produced — `1` means
    /// perfectly reproducible; anything higher is variance made visible.
    pub distinct_reports: usize,
}

impl VarianceReport {
    /// Aggregate variance over a set of runs of the same suite. Pure.
    pub fn from_reports(reports: &[EvalReport]) -> Self {
        let pass_rates: Vec<f64> = reports.iter().map(|r| r.pass_rate).collect();
        let runs = reports.len();
        let mean = if runs == 0 {
            0.0
        } else {
            pass_rates.iter().sum::<f64>() / runs as f64
        };
        let mut distinct: Vec<String> = reports
            .iter()
            .map(|r| serde_json::to_string(r).unwrap_or_default())
            .collect();
        distinct.sort();
        distinct.dedup();
        Self {
            runs,
            pass_rates: pass_rates.clone(),
            mean_pass_rate: mean,
            min_pass_rate: pass_rates.iter().copied().fold(f64::INFINITY, f64::min),
            max_pass_rate: pass_rates.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            distinct_reports: distinct.len(),
        }
    }
}

/// Run `suite` `n` times through [`run_suite_scored`], returning every report
/// (feed them to [`VarianceReport::from_reports`]). Sequential, like the
/// single-run path — no state carries between runs.
pub async fn run_suite_repeated(
    n: usize,
    suite: &EvalSuite,
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
    scorer: &Scorer,
) -> Result<Vec<EvalReport>> {
    let mut reports = Vec::with_capacity(n);
    for _ in 0..n {
        reports.push(run_suite_scored(suite, def, gateway, registry, scorer).await?);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::CaseResult;
    use apex_common::Usage;

    fn case(id: &str, passed: bool) -> CaseResult {
        CaseResult {
            id: id.to_string(),
            passed,
            detail: if passed { "ok" } else { "wrong answer" }.to_string(),
            usage: Usage::new(1, 1, 0.0),
        }
    }

    fn report(cases: Vec<CaseResult>) -> EvalReport {
        EvalReport::from_cases("s", cases)
    }

    fn baseline_all_pass() -> Baseline {
        Baseline {
            suite: "s".to_string(),
            min_pass_rate: 1.0,
            cases: [("a".to_string(), true), ("b".to_string(), true)].into(),
        }
    }

    #[test]
    fn a_matching_report_passes_the_gate() {
        let result = check(
            &report(vec![case("a", true), case("b", true)]),
            &baseline_all_pass(),
        );
        assert!(result.passed, "{result:#?}");
        assert!(result.violations.is_empty());
        assert!(result.notes.is_empty());
    }

    #[test]
    fn a_dropped_pass_rate_fails_the_gate() {
        let result = check(
            &report(vec![case("a", true), case("b", false)]),
            &baseline_all_pass(),
        );
        assert!(!result.passed);
        assert!(
            result
                .violations
                .iter()
                .any(|v| v.contains("below the baseline threshold"))
        );
        assert!(
            result
                .violations
                .iter()
                .any(|v| v.contains("`b` regressed"))
        );
    }

    #[test]
    fn a_per_case_regression_fails_even_when_the_rate_threshold_is_met() {
        // Threshold 0.5 is met (1 of 2 passes) but case `a` flipped from pass
        // to fail while `b` flipped the other way — the gate must not let a
        // regression hide behind an unchanged aggregate.
        let baseline = Baseline {
            suite: "s".to_string(),
            min_pass_rate: 0.5,
            cases: [("a".to_string(), true), ("b".to_string(), false)].into(),
        };
        let result = check(&report(vec![case("a", false), case("b", true)]), &baseline);
        assert!(!result.passed);
        assert!(
            result
                .violations
                .iter()
                .any(|v| v.contains("`a` regressed"))
        );
        assert!(result.notes.iter().any(|n| n.contains("`b` improved")));
    }

    #[test]
    fn a_vanished_case_fails_and_a_new_case_is_noted() {
        let result = check(
            &report(vec![case("a", true), case("c", true)]),
            &baseline_all_pass(),
        );
        assert!(!result.passed);
        assert!(
            result
                .violations
                .iter()
                .any(|v| v.contains("`b` is in the baseline but missing"))
        );
        assert!(result.notes.iter().any(|n| n.contains("`c` is new")));
    }

    #[test]
    fn a_wrong_suite_baseline_is_rejected() {
        let mut baseline = baseline_all_pass();
        baseline.suite = "other".to_string();
        let result = check(&report(vec![case("a", true), case("b", true)]), &baseline);
        assert!(!result.passed);
        assert!(result.violations[0].contains("suite `s`"));
    }

    #[test]
    fn baselines_round_trip_through_json_and_snapshot_from_reports() {
        let report = report(vec![case("a", true), case("b", false)]);
        let baseline = Baseline::from_report(&report, 0.5);
        assert!(baseline.cases["a"]);
        assert!(!baseline.cases["b"]);
        let back = Baseline::from_json(&baseline.to_json()).unwrap();
        assert_eq!(back, baseline);
        // A snapshotted baseline always gates its own report clean.
        assert!(check(&report, &baseline).passed);
    }

    #[test]
    fn malformed_or_out_of_range_baselines_fail_closed() {
        assert!(Baseline::from_json("not json").is_err());
        assert!(Baseline::from_json(r#"{"suite":"s","min_pass_rate":1.5,"cases":{}}"#).is_err());
    }

    #[test]
    fn variance_over_identical_reports_is_zero() {
        let r = report(vec![case("a", true)]);
        let v = VarianceReport::from_reports(&[r.clone(), r.clone(), r]);
        assert_eq!(v.runs, 3);
        assert_eq!(
            v.distinct_reports, 1,
            "identical runs → one distinct report"
        );
        assert_eq!(v.mean_pass_rate, 1.0);
        assert_eq!(v.min_pass_rate, v.max_pass_rate);
    }

    #[test]
    fn variance_surfaces_a_flaky_run() {
        let good = report(vec![case("a", true)]);
        let bad = report(vec![case("a", false)]);
        let v = VarianceReport::from_reports(&[good.clone(), bad, good]);
        assert_eq!(v.distinct_reports, 2);
        assert_eq!(v.min_pass_rate, 0.0);
        assert_eq!(v.max_pass_rate, 1.0);
        assert!((v.mean_pass_rate - 2.0 / 3.0).abs() < 1e-9);
    }
}
