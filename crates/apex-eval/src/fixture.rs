//! [`EvalSuite`]: a YAML-defined set of [`Fixture`]s, each pairing an input
//! with an expected-answer check. Mirrors
//! [`apex_agent::AgentDefinition::from_yaml`]'s validate-on-load shape so a
//! malformed suite fails closed at load time, not silently mid-run.
//!
//! [`Expectation`] is a validated "one-of" struct rather than a Rust enum:
//! `serde_yaml` 0.9 (the version this workspace pins, itself
//! `+deprecated` upstream) cannot deserialize an externally-tagged enum from a
//! YAML map — it demands a `!Tag` syntax instead, which no other YAML-DSL
//! struct in this codebase uses (`AgentDefinition` and the workflow
//! `Definition` both avoid enums in their wire schema for the same reason). A
//! one-of struct with exactly one field set, validated at load time, sidesteps
//! the limitation entirely and is the idiom this codebase already follows.

use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// What a case's actual answer is checked against — exactly one field must be
/// set (checked by [`EvalSuite::from_yaml`]'s validation, not by [`score`]).
/// Deliberately just string matching — no regex dependency — to keep this
/// spike's surface minimal.
///
/// [`score`]: crate::score::score
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Expectation {
    /// The answer must contain this substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// The answer must contain every one of these substrings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains_all: Option<Vec<String>>,
    /// The answer must equal this string exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
}

impl Expectation {
    pub fn contains(needle: impl Into<String>) -> Self {
        Self {
            contains: Some(needle.into()),
            ..Self::default()
        }
    }

    pub fn contains_all(needles: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            contains_all: Some(needles.into_iter().map(Into::into).collect()),
            ..Self::default()
        }
    }

    pub fn equals(expected: impl Into<String>) -> Self {
        Self {
            equals: Some(expected.into()),
            ..Self::default()
        }
    }

    /// How many of the one-of fields are set — must be exactly 1 on a valid
    /// [`Expectation`].
    fn set_count(&self) -> usize {
        [
            self.contains.is_some(),
            self.contains_all.is_some(),
            self.equals.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }
}

/// One evaluation case: an input and the check its answer must pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fixture {
    /// Stable case id, unique within its suite.
    pub id: String,
    /// The user-turn text sent to the agent.
    pub input: String,
    /// The check the agent's final answer is scored against.
    pub expect: Expectation,
}

/// A named set of fixtures, loadable from YAML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalSuite {
    pub name: String,
    pub cases: Vec<Fixture>,
}

impl EvalSuite {
    /// Parse a suite from a YAML string, failing closed on anything malformed
    /// (empty name, no cases, a case with an empty id/input, or an `expect`
    /// with zero or more than one check set) rather than silently running an
    /// under-specified case.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let suite: EvalSuite = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid eval suite: {e}")))?;
        suite.validate()?;
        Ok(suite)
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::invalid("suite name must not be empty"));
        }
        if self.cases.is_empty() {
            return Err(Error::invalid("suite must have at least one case"));
        }
        for case in &self.cases {
            if case.id.trim().is_empty() {
                return Err(Error::invalid("every case must have a non-empty id"));
            }
            if case.input.trim().is_empty() {
                return Err(Error::invalid(format!(
                    "case `{}` must have a non-empty input",
                    case.id
                )));
            }
            let set = case.expect.set_count();
            if set != 1 {
                return Err(Error::invalid(format!(
                    "case `{}` must set exactly one of contains/contains_all/equals, found {set}",
                    case.id
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Plain multi-line literals (no `\`-continuation) — Rust's line-continuation
    // strips *all* leading whitespace off the continued line unconditionally,
    // which would flatten this YAML's indentation.
    fn valid_yaml() -> &'static str {
        "
name: greeting-suite
cases:
  - id: greets-by-name
    input: My name is Ada.
    expect:
      contains: Ada
"
    }

    #[test]
    fn parses_a_valid_suite() {
        let suite = EvalSuite::from_yaml(valid_yaml()).unwrap();
        assert_eq!(suite.name, "greeting-suite");
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.cases[0].id, "greets-by-name");
        assert_eq!(suite.cases[0].expect, Expectation::contains("Ada"));
    }

    #[test]
    fn parses_contains_all_and_equals_variants() {
        let yaml = "
name: variants
cases:
  - id: c1
    input: hi
    expect:
      contains_all: [a, b]
  - id: c2
    input: hi
    expect:
      equals: exact
";
        let suite = EvalSuite::from_yaml(yaml).unwrap();
        assert_eq!(suite.cases[0].expect, Expectation::contains_all(["a", "b"]));
        assert_eq!(suite.cases[1].expect, Expectation::equals("exact"));
    }

    #[test]
    fn rejects_malformed_yaml() {
        assert!(EvalSuite::from_yaml("not: [valid, eval, suite").is_err());
    }

    #[test]
    fn rejects_empty_name() {
        let yaml = "name: \"\"\ncases:\n  - id: c1\n    input: hi\n    expect:\n      equals: hi\n";
        assert!(matches!(
            EvalSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn rejects_no_cases() {
        let yaml = "name: empty\ncases: []\n";
        assert!(matches!(
            EvalSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn rejects_case_with_empty_id() {
        let yaml = "name: s\ncases:\n  - id: \"\"\n    input: hi\n    expect:\n      equals: hi\n";
        assert!(matches!(
            EvalSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn rejects_case_with_empty_input() {
        let yaml = "name: s\ncases:\n  - id: c1\n    input: \"\"\n    expect:\n      equals: hi\n";
        assert!(matches!(
            EvalSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn rejects_expect_with_no_checks_set() {
        let yaml = "name: s\ncases:\n  - id: c1\n    input: hi\n    expect: {}\n";
        assert!(matches!(
            EvalSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn rejects_expect_with_more_than_one_check_set() {
        let yaml = "name: s\ncases:\n  - id: c1\n    input: hi\n    expect:\n      contains: a\n      equals: b\n";
        assert!(matches!(
            EvalSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }
}
