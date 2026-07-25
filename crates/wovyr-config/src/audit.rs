//! Sourcing the **audit MAC key** (SEC-403), shared by `wovyr-server` (and any
//! future CLI audit surface) so both agree on the key that authenticates the
//! tamper-evident audit chain — reusing `wovyr-kms`'s root-key sourcing
//! (`from_env`/`from_file`) exactly as `wovyr_config::kms` does.
//!
//! The audit chain's per-entry hash is a keyed HMAC and its head anchor is
//! keyed too, so an actor who can rewrite `audit.jsonl` cannot forge the chain
//! or the anchor without this key. The key is held *outside* the log file:
//! `WOVYR_AUDIT_MAC_KEY` (hex, preferred — forces the operator to source it from
//! escrow before startup) or a generate-once `~/.wovyr/audit/audit.key`.
//!
//! **Fail-closed on missing key material (SEC-403, the SEC-405 stance).** When
//! *neither* `WOVYR_AUDIT_MAC_KEY` is set *nor* a persistent, writable key file
//! is available, this returns an [`Error::Config`] rather than silently running
//! an *unkeyed* (consistency-only, not tamper-resistant) chain. The escape
//! hatch for a genuine throwaway/test run is `WOVYR_AUDIT_ALLOW_UNKEYED=1` — a
//! deliberate operator choice, mirroring `WOVYR_KMS_ALLOW_EPHEMERAL` /
//! `WOVYR_ALLOW_ANONYMOUS`.
//!
//! Deliberately distinct from the KMS root key: rotating one must not silently
//! invalidate the other, so the audit key has its own env var and its own file.

use std::path::PathBuf;
use wovyr_common::{Error, Result};
use wovyr_kms::KeyBytes;

/// Whether the operator has explicitly opted into running the audit log
/// **unkeyed** (no tamper resistance) via `WOVYR_AUDIT_ALLOW_UNKEYED=1`.
fn unkeyed_allowed() -> bool {
    std::env::var("WOVYR_AUDIT_ALLOW_UNKEYED")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Resolve the audit MAC key (SEC-403): `WOVYR_AUDIT_MAC_KEY` (hex) or a
/// generate-once `~/.wovyr/audit/audit.key`.
///
/// Returns `Ok(Some(key))` for a keyed (tamper-resistant) log, `Ok(None)` when
/// the operator has explicitly opted into an unkeyed log
/// (`WOVYR_AUDIT_ALLOW_UNKEYED=1`), and [`Error::Config`] when no key material is
/// available and the opt-out is off — never a silent unkeyed fallback.
pub fn build_audit_key() -> Result<Option<KeyBytes>> {
    let dir = crate::paths::audit_dir().ok();
    let env_key = wovyr_kms::root::from_env("WOVYR_AUDIT_MAC_KEY").ok();
    build_audit_key_inner(dir, env_key, unkeyed_allowed())
}

/// The pure core of [`build_audit_key`], parameterized on the resolved audit
/// directory, an already-resolved env-sourced key, and the unkeyed opt-in — so
/// the fail-closed decision is testable without mutating the process-global
/// `WOVYR_AUDIT_MAC_KEY`/`HOME` environment (which concurrent tests share).
fn build_audit_key_inner(
    dir: Option<PathBuf>,
    env_key: Option<KeyBytes>,
    allow_unkeyed: bool,
) -> Result<Option<KeyBytes>> {
    // Durable key: an operator-supplied env var (preferred — forces sourcing from
    // escrow before startup), else a generate-once file persisted under `dir`.
    let key = env_key.or_else(|| {
        dir.as_ref()
            .and_then(|d| wovyr_kms::root::from_file(d.join("audit.key")).ok())
    });

    if let Some(key) = key {
        return Ok(Some(key));
    }

    if allow_unkeyed {
        tracing::warn!(
            "WOVYR_AUDIT_ALLOW_UNKEYED=1: the audit log runs with an UNKEYED hash chain — \
             it detects accidental corruption and interior edits by an actor without write \
             access, but is NOT tamper-resistant against an actor who can rewrite \
             audit.jsonl (they can recompute the public chain). For throwaway/test use only \
             (SEC-403)."
        );
        return Ok(None);
    }

    Err(Error::config(
        "no durable audit MAC key available: set WOVYR_AUDIT_MAC_KEY (hex-encoded 32 bytes) \
         or ensure ~/.wovyr/audit is writable so a key can be persisted. Refusing to start \
         with an unkeyed audit chain that an actor with write access could silently forge. \
         Set WOVYR_AUDIT_ALLOW_UNKEYED=1 only for a throwaway/test run (SEC-403).",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_key_material_fails_closed() {
        // No env key, no directory, no opt-in → a clear config error, never a
        // silent unkeyed chain (SEC-403's fail-closed posture).
        match build_audit_key_inner(None, None, false) {
            Err(Error::Config(msg)) => assert!(
                msg.contains("WOVYR_AUDIT_MAC_KEY"),
                "the error must name the fix: {msg}"
            ),
            other => panic!("expected a config error, got {other:?}"),
        }
    }

    #[test]
    fn unkeyed_opt_in_yields_no_key() {
        // The explicit escape hatch runs unkeyed (Ok(None)), not an error.
        assert_eq!(
            build_audit_key_inner(None, None, true).unwrap(),
            None,
            "WOVYR_AUDIT_ALLOW_UNKEYED=1 should run unkeyed, not fail"
        );
    }

    #[test]
    fn env_key_yields_a_keyed_log() {
        let key = wovyr_kms::generate_key().unwrap();
        assert_eq!(
            build_audit_key_inner(None, Some(key), false).unwrap(),
            Some(key),
            "an env-supplied key should be used verbatim"
        );
    }

    #[test]
    fn a_persistent_directory_yields_a_generated_key() {
        // A writable directory with no env key → a generate-once persisted key
        // (the normal single-host path: no error, no unkeyed fallback).
        let dir = std::env::temp_dir().join(format!(
            "wovyr_config_audit_test_{}_persist",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = build_audit_key_inner(Some(dir.clone()), None, false)
            .unwrap()
            .expect("a writable dir yields a key");
        assert!(dir.join("audit.key").exists(), "key must be persisted");
        // A second construction reuses the persisted key (stable across restarts).
        let second = build_audit_key_inner(Some(dir.clone()), None, false)
            .unwrap()
            .unwrap();
        assert_eq!(first, second, "the persisted key must be stable");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
