//! A public, reusable conformance suite (PRD-005 RM-GUI-P3 EMB-704):
//! must-allow / must-block / must-redact vectors over the trust layer's
//! **documented default posture** — deny-by-default sensitive inputs,
//! destructive actions, deception shapes, unapproved media origins, and the
//! no-policy hosted floor.
//!
//! This exists so a **deployer**, not just this workspace's own CI, can
//! verify their own configuration actually enforces what PRD-005 claims:
//!
//! ```
//! use apex_ui_guard::{UiPolicy, PolicyRules};
//! use apex_ui_guard::conformance::conformance_report;
//!
//! let policy = UiPolicy { name: "prod".into(), version: 1, rules: PolicyRules::default() };
//! let report = conformance_report(&policy);
//! assert!(report.all_passed(), "{report}");
//! ```
//!
//! Every vector's frame JSON is inlined as source text (not built via helper
//! functions) so it's copy-pasteable, diffable, and legible on its own — a
//! deployer reading a failure should be able to see exactly what was sent
//! without chasing constructors.

use crate::{PolicyRules, UiPolicy, Verdict, evaluate, hosted_floor, rules};
use apex_ui::UiFrame;
use std::fmt;

/// What a vector's evaluation must produce to pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expectation {
    /// The frame must render unmodified.
    Allow,
    /// The frame must render, but transformed (a redaction fired).
    Redact,
    /// The frame must never render; `rule` names which one fired.
    Block(&'static str),
}

/// One conformance vector: a frame, and what evaluating it must produce.
pub struct Vector {
    /// Short, stable name — identifies the vector in a failure report.
    pub name: &'static str,
    /// The frame's JSON source, verbatim.
    pub frame_json: &'static str,
    pub expected: Expectation,
}

/// One vector's outcome against a specific policy (or the hosted floor).
pub struct VectorResult {
    pub name: &'static str,
    pub expected: Expectation,
    pub actual: Result<Expectation, String>,
}

impl VectorResult {
    pub fn passed(&self) -> bool {
        matches!(&self.actual, Ok(actual) if *actual == self.expected)
    }
}

/// The full conformance run: default-policy vectors judged against the
/// caller's policy, plus the policy-independent hosted-floor vectors.
pub struct ConformanceReport {
    pub policy_ref: String,
    pub results: Vec<VectorResult>,
}

impl ConformanceReport {
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(VectorResult::passed)
    }

    pub fn failures(&self) -> Vec<&VectorResult> {
        self.results.iter().filter(|r| !r.passed()).collect()
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "conformance report for policy `{}`:", self.policy_ref)?;
        for result in &self.results {
            let status = if result.passed() { "PASS" } else { "FAIL" };
            writeln!(
                f,
                "  [{status}] {} — expected {:?}, got {:?}",
                result.name, result.expected, result.actual
            )?;
        }
        Ok(())
    }
}

/// All rule ids [`evaluate`]/[`hosted_floor`] can produce — used to map a
/// verdict's owned `rule: String` back to the `&'static str` constant it came
/// from, so [`Expectation::Block`] never has to leak memory to stay `'static`.
const KNOWN_RULE_IDS: &[&str] = &[
    rules::MAX_NODES,
    rules::MAX_DEPTH,
    rules::MEDIA_ORIGIN,
    rules::SENSITIVE_INPUT,
    rules::DESTRUCTIVE_ACTION,
    rules::INTENT_MISMATCH,
    rules::REDACT_TEXT,
    rules::HOSTED_FLOOR,
];

fn block_expectation(rule: &str) -> Expectation {
    match KNOWN_RULE_IDS.iter().find(|&&id| id == rule) {
        Some(&id) => Expectation::Block(id),
        None => Expectation::Block("unknown_rule"),
    }
}

/// Evaluate `vector.frame_json` against `policy` and classify the outcome as
/// an [`Expectation`] (parse/evaluate failures surface as `Err`, never
/// silently treated as a pass).
fn evaluate_vector(policy: &UiPolicy, vector: &Vector) -> Result<Expectation, String> {
    let value: serde_json::Value = serde_json::from_str(vector.frame_json)
        .map_err(|e| format!("vector `{}` is not valid JSON: {e}", vector.name))?;
    let frame = UiFrame::from_value(&value)
        .map_err(|e| format!("vector `{}` failed protocol validation: {e}", vector.name))?;
    Ok(match evaluate(policy, &frame) {
        Verdict::Allow => Expectation::Allow,
        Verdict::Redact { .. } => Expectation::Redact,
        Verdict::Block { rule, .. } => block_expectation(&rule),
    })
}

fn evaluate_hosted_floor_vector(vector: &Vector) -> Result<Expectation, String> {
    let value: serde_json::Value = serde_json::from_str(vector.frame_json)
        .map_err(|e| format!("vector `{}` is not valid JSON: {e}", vector.name))?;
    let frame = UiFrame::from_value(&value)
        .map_err(|e| format!("vector `{}` failed protocol validation: {e}", vector.name))?;
    Ok(match hosted_floor(&frame) {
        Verdict::Allow => Expectation::Allow,
        Verdict::Redact { .. } => Expectation::Redact,
        Verdict::Block { rule, .. } => block_expectation(&rule),
    })
}

/// Run every default-policy vector against `policy`, plus the
/// policy-independent hosted-floor vectors, and return the combined report.
pub fn conformance_report(policy: &UiPolicy) -> ConformanceReport {
    let mut results: Vec<VectorResult> = default_policy_vectors()
        .into_iter()
        .map(|v| {
            let actual = evaluate_vector(policy, &v);
            VectorResult {
                name: v.name,
                expected: v.expected,
                actual,
            }
        })
        .collect();
    results.extend(hosted_floor_vectors().into_iter().map(|v| {
        let actual = evaluate_hosted_floor_vector(&v);
        VectorResult {
            name: v.name,
            expected: v.expected,
            actual,
        }
    }));
    ConformanceReport {
        policy_ref: policy.reference(),
        results,
    }
}

/// The must-allow / must-block / must-redact vectors judged against a
/// **default-rules** policy (`PolicyRules::default()`) — the deny-by-default
/// posture PRD-005 documents. A deployer's customized policy may legitimately
/// diverge from some of these (e.g. `allow_destructive_actions: true`) —
/// [`conformance_report`] is meant to be read alongside knowing which of
/// *your own* rules you deliberately loosened, not treated as unconditional.
pub fn default_policy_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "benign_confirmation_is_allowed",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "text", "text": "Reorder 3 boxes?" },
                    { "type": "button", "action": "approve", "label": "Approve", "class": "approve" },
                    { "type": "button", "action": "cancel", "label": "Cancel", "class": "cancel" }
                ]}
            }"#,
            expected: Expectation::Allow,
        },
        Vector {
            name: "credential_named_input_is_blocked",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "text_input", "name": "card_number", "label": "Card number" },
                    { "type": "button", "action": "pay", "label": "Continue", "class": "submit" }
                ]}
            }"#,
            expected: Expectation::Block(rules::SENSITIVE_INPUT),
        },
        Vector {
            name: "credential_lookalike_word_is_not_blocked",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "text_input", "name": "discard_reason", "label": "Reason to discard" },
                    { "type": "button", "action": "submit", "label": "Submit", "class": "submit" }
                ]}
            }"#,
            expected: Expectation::Allow,
        },
        Vector {
            name: "destructive_action_without_opt_in_is_blocked",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "button", "action": "purge", "label": "Delete everything", "class": "destructive" }
                ]}
            }"#,
            expected: Expectation::Block(rules::DESTRUCTIVE_ACTION),
        },
        Vector {
            name: "affirmative_action_wearing_a_cancel_label_is_blocked",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "button", "action": "sneaky", "label": "Cancel", "class": "confirm" }
                ]}
            }"#,
            expected: Expectation::Block(rules::INTENT_MISMATCH),
        },
        Vector {
            name: "destructive_reading_label_under_a_neutral_class_is_blocked",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "button", "action": "cleanup", "label": "Delete account", "class": "neutral" }
                ]}
            }"#,
            expected: Expectation::Block(rules::INTENT_MISMATCH),
        },
        Vector {
            name: "image_with_no_allowed_origins_is_blocked",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "image", "url": "https://cdn.example.com/chart.png", "alt": "chart" }
            }"#,
            expected: Expectation::Block(rules::MEDIA_ORIGIN),
        },
    ]
}

/// The GRD-207 hosted-floor vectors — policy-independent by construction
/// (`hosted_floor` takes no policy at all): with **no** policy configured, an
/// interactive frame is always denied and a display-only one always passes.
pub fn hosted_floor_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "hosted_floor_denies_an_interactive_frame",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "button", "action": "go", "label": "Go", "class": "confirm" }
                ]}
            }"#,
            expected: Expectation::Block(rules::HOSTED_FLOOR),
        },
        Vector {
            name: "hosted_floor_allows_a_display_only_frame",
            frame_json: r#"{
                "schema_version": "1.0.0",
                "root": { "type": "column", "children": [
                    { "type": "badge", "text": "healthy", "tone": "success" },
                    { "type": "text", "text": "All queues nominal." }
                ]}
            }"#,
            expected: Expectation::Allow,
        },
    ]
}

/// The default-rules policy [`conformance_report`]'s own gate test judges
/// against — exported so a caller building a similar CI gate doesn't have to
/// hand-construct the same thing.
pub fn reference_policy() -> UiPolicy {
    UiPolicy {
        name: "conformance-reference".to_string(),
        version: 1,
        rules: PolicyRules::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conformance suite's own gate: a `PolicyRules::default()` policy
    /// must pass every vector. Wired into `cargo test --workspace` (this
    /// workspace's CI gate) — the same claim a deployer's own build of this
    /// crate can verify against their configuration.
    #[test]
    fn reference_policy_passes_the_full_conformance_suite() {
        let report = conformance_report(&reference_policy());
        assert!(report.all_passed(), "{report}");
    }

    #[test]
    fn a_deliberately_loosened_policy_diverges_exactly_where_expected() {
        let mut policy = reference_policy();
        policy.rules.allow_destructive_actions = true;
        let report = conformance_report(&policy);
        let failures = report.failures();
        assert_eq!(
            failures.len(),
            1,
            "only the destructive-action vector should diverge: {report}"
        );
        assert_eq!(
            failures[0].name,
            "destructive_action_without_opt_in_is_blocked"
        );
    }

    #[test]
    fn every_vector_frame_is_itself_protocol_valid() {
        // A vector whose *frame_json* is malformed would silently report as a
        // failure with a confusing message rather than a clean diff — catch
        // that class of authoring bug directly.
        for vector in default_policy_vectors()
            .into_iter()
            .chain(hosted_floor_vectors())
        {
            let value: serde_json::Value = serde_json::from_str(vector.frame_json)
                .unwrap_or_else(|e| panic!("vector `{}` is not valid JSON: {e}", vector.name));
            UiFrame::from_value(&value).unwrap_or_else(|e| {
                panic!("vector `{}` failed protocol validation: {e}", vector.name)
            });
        }
    }
}
