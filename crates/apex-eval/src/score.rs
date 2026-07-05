//! [`score`] — the pure scoring function. No ambient clock or randomness
//! ([coding-standards §7](../../docs/19-implementation-guide/coding-standards.md)):
//! given the same `actual` and `expect`, it always returns the same
//! [`CaseOutcome`], which is what makes [`crate::EvalReport`] reproducible.

use crate::fixture::Expectation;

/// The result of scoring one case's actual answer against its expectation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaseOutcome {
    pub passed: bool,
    /// A human-readable reason, useful when `passed` is `false`.
    pub detail: String,
}

/// Score `actual` against `expect`. Pure and deterministic: no I/O, no clock,
/// no randomness — the same inputs always produce the same [`CaseOutcome`].
///
/// `expect` is assumed valid (exactly one check set) — [`EvalSuite::from_yaml`]
/// enforces that at load time. An unset `expect` (only reachable by
/// hand-constructing one, bypassing validation) always fails.
///
/// [`EvalSuite::from_yaml`]: crate::fixture::EvalSuite::from_yaml
pub fn score(actual: &str, expect: &Expectation) -> CaseOutcome {
    if let Some(needle) = &expect.contains {
        return if actual.contains(needle.as_str()) {
            CaseOutcome {
                passed: true,
                detail: format!("answer contains `{needle}`"),
            }
        } else {
            CaseOutcome {
                passed: false,
                detail: format!("answer does not contain `{needle}`"),
            }
        };
    }
    if let Some(needles) = &expect.contains_all {
        let missing: Vec<&String> = needles
            .iter()
            .filter(|n| !actual.contains(n.as_str()))
            .collect();
        return if missing.is_empty() {
            CaseOutcome {
                passed: true,
                detail: format!("answer contains all of {needles:?}"),
            }
        } else {
            CaseOutcome {
                passed: false,
                detail: format!("answer is missing {missing:?}"),
            }
        };
    }
    if let Some(expected) = &expect.equals {
        return if actual == expected {
            CaseOutcome {
                passed: true,
                detail: "answer matches exactly".to_string(),
            }
        } else {
            CaseOutcome {
                passed: false,
                detail: format!("answer `{actual}` != expected `{expected}`"),
            }
        };
    }
    CaseOutcome {
        passed: false,
        detail: "expectation has no check set".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_passes_and_fails_correctly() {
        assert!(score("hello Ada", &Expectation::contains("Ada")).passed);
        assert!(!score("hello Bob", &Expectation::contains("Ada")).passed);
    }

    #[test]
    fn contains_all_requires_every_needle() {
        let expect = Expectation::contains_all(["a", "b"]);
        assert!(score("has a and b", &expect).passed);
        assert!(!score("has only a", &expect).passed);
    }

    #[test]
    fn equals_requires_an_exact_match() {
        let expect = Expectation::equals("exact");
        assert!(score("exact", &expect).passed);
        assert!(!score("exact ", &expect).passed);
    }

    #[test]
    fn scoring_the_same_input_twice_gives_identical_outcomes() {
        let expect = Expectation::contains("x");
        assert_eq!(score("xyz", &expect), score("xyz", &expect));
    }

    #[test]
    fn an_unset_expectation_never_passes() {
        assert!(!score("anything", &Expectation::default()).passed);
    }
}
