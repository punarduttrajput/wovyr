//! Property/fuzz tests for the workflow DSL and cron-expression parsers
//! ([security-testing.md §8](../../docs/15-testing/security-testing.md#8-automated-scanning-ci)):
//! arbitrary input must never panic — only ever `Ok` or a clean `Err`. Complements
//! the hand-written malformed-input unit tests already in `definition.rs`/`cron.rs`
//! with broad, generated coverage.

use apex_workflow::{Cron, Definition};
use proptest::prelude::*;

proptest! {
    /// Arbitrary text fed to the workflow DSL parser must never panic.
    #[test]
    fn definition_from_yaml_never_panics(input in ".{0,500}") {
        let _ = Definition::from_yaml(&input);
    }

    /// A structurally-plausible-but-fuzzed workflow (real top-level keys, generated
    /// values) must not panic either.
    #[test]
    fn definition_from_yaml_with_plausible_keys_never_panics(
        name in "[a-zA-Z0-9_-]{0,40}",
        version in "[a-zA-Z0-9.+-]{0,20}",
        activity_id in "[a-zA-Z0-9_-]{0,20}",
        activity_type in "[a-zA-Z0-9_-]{0,20}",
    ) {
        let yaml = format!(
            "metadata:\n  name: {name}\n  version: {version}\nspec:\n  activities:\n    - {{id: {activity_id}, type: {activity_type}}}\n"
        );
        let _ = Definition::from_yaml(&yaml);
    }

    /// Arbitrary text fed to the cron parser must never panic.
    #[test]
    fn cron_parse_never_panics(input in ".{0,100}") {
        let _ = Cron::parse(&input);
    }

    /// A structurally-plausible-but-fuzzed 5-field cron expression must not panic.
    #[test]
    fn cron_parse_with_plausible_fields_never_panics(
        minute in "[0-9*/,-]{0,10}",
        hour in "[0-9*/,-]{0,10}",
        dom in "[0-9*/,-]{0,10}",
        month in "[0-9*/,A-Za-z-]{0,10}",
        dow in "[0-9*/,A-Za-z-]{0,10}",
    ) {
        let expr = format!("{minute} {hour} {dom} {month} {dow}");
        let _ = Cron::parse(&expr);
    }

    /// A successfully-parsed cron's `next_after` must never panic, for any `after_ms`
    /// — including values near `u64::MAX`, where naive minute/second arithmetic could
    /// overflow.
    #[test]
    fn cron_next_after_never_panics(after_ms in any::<u64>()) {
        let cron = Cron::parse("*/7 * * * *").unwrap();
        let _ = cron.next_after(after_ms);
    }
}
