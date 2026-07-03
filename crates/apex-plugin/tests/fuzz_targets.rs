//! Property/fuzz tests for the manifest YAML and `.apexpkg` envelope parsers
//! ([security-testing.md §8](../../docs/15-testing/security-testing.md#8-automated-scanning-ci)):
//! arbitrary input must never panic — only ever `Ok` or a clean `Err`. These are
//! the first things touched on an untrusted download, before signature
//! verification even runs, so a panic here would be a denial-of-service any
//! unauthenticated caller could trigger. Complements the hand-written
//! malformed-input unit tests already in `manifest.rs`/`engine.rs` with broad,
//! generated coverage.

use apex_plugin::{Package, PluginManifest};
use proptest::prelude::*;

proptest! {
    /// Arbitrary text fed to the manifest YAML parser must never panic.
    #[test]
    fn manifest_from_yaml_never_panics(input in ".{0,500}") {
        let _ = PluginManifest::from_yaml(&input);
    }

    /// Arbitrary bytes fed to the `.apexpkg` envelope parser must never panic.
    #[test]
    fn apexpkg_from_bytes_never_panics(input in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Package::from_apexpkg(&input);
    }

    /// A structurally-plausible-but-fuzzed manifest (real top-level keys, generated
    /// values) is a more realistic adversarial input than pure noise — must not
    /// panic either.
    #[test]
    fn manifest_from_yaml_with_plausible_keys_never_panics(
        api_version in "[a-zA-Z0-9./:_-]{0,40}",
        kind in "[a-zA-Z]{0,20}",
        name in "[a-zA-Z0-9_-]{0,40}",
        version in "[a-zA-Z0-9.+-]{0,20}",
        publisher in "[a-zA-Z0-9_-]{0,40}",
    ) {
        let yaml = format!(
            "apiVersion: {api_version}\nkind: {kind}\nmetadata:\n  name: {name}\n  version: {version}\n  publisher: {publisher}\n"
        );
        let _ = PluginManifest::from_yaml(&yaml);
    }

    /// A structurally-plausible-but-fuzzed `.apexpkg` JSON envelope must not panic.
    #[test]
    fn apexpkg_envelope_with_plausible_shape_never_panics(
        manifest in ".{0,200}",
        signature in "[0-9a-fA-F]{0,80}",
    ) {
        let envelope = format!(
            r#"{{"manifest": {manifest:?}, "signature": {signature:?}, "artifacts": {{}}}}"#
        );
        let _ = Package::from_apexpkg(envelope.as_bytes());
    }
}
