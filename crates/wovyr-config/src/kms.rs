//! The platform KMS construction shared by `wovyr-server` and `wovyr-cli` —
//! previously a byte-for-byte duplicate of the same root-key + tenant-catalog
//! logic in each binary (`wovyr-server/src/lib.rs`'s `default_kms` and
//! `wovyr-cli/src/config.rs`'s `kms`).

use std::path::PathBuf;
use std::sync::Arc;
use wovyr_common::{Error, Result};
use wovyr_kms::{FileKmsStore, InMemoryKmsStore, KeyBytes, Kms, KmsStore, LocalKms};

/// Whether the operator has explicitly opted into an **ephemeral** (in-process,
/// non-durable) KMS root key via `WOVYR_KMS_ALLOW_EPHEMERAL=1` — the documented
/// escape hatch for a genuine test/dev run (SEC-405), mirroring the
/// `WOVYR_ENABLE_SHELL_TOOL`/`WOVYR_ALLOW_ANONYMOUS` opt-in precedent. Off means
/// missing durable key material is a fail-closed startup error, not a silent
/// data-loss trap.
fn ephemeral_allowed() -> bool {
    std::env::var("WOVYR_KMS_ALLOW_EPHEMERAL")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// The platform KMS ([Encryption
/// §5](../../../docs/13-security/encryption.md#5-key-management)): sources a
/// root key from `WOVYR_KMS_ROOT_KEY` (hex) or, failing that,
/// generates-and-persists one at `~/.wovyr/kms/root.key` — shared by
/// `wovyr-server` and `wovyr-cli` so either process can decrypt the other's
/// sealed data — backing tenant keys with a [`FileKmsStore`] in the same
/// directory.
///
/// **Fail-closed on missing durable key material (RM-AR-P1 SEC-405).** When
/// *neither* `WOVYR_KMS_ROOT_KEY` is set *nor* a persistent, writable key file
/// is available (e.g. a container with no persistent volume and no
/// `HOME`/`USERPROFILE`), this returns a clear [`Error::Config`] rather than
/// minting an ephemeral in-process key. The old behavior silently sealed every
/// secret/memory under a key that vanished on the next restart, permanently
/// losing that data with no startup failure. The escape hatch for a genuine
/// throwaway/test run is `WOVYR_KMS_ALLOW_EPHEMERAL=1` — a deliberate operator
/// choice, the `WOVYR_ALLOW_ANONYMOUS` precedent.
pub fn build_kms() -> Result<Arc<dyn Kms>> {
    let dir = crate::paths::kms_dir().ok();
    let env_key = wovyr_kms::root::from_env("WOVYR_KMS_ROOT_KEY").ok();
    build_kms_inner(dir, env_key, ephemeral_allowed())
}

/// The pure core of [`build_kms`], parameterized on the resolved KMS directory,
/// an already-resolved env-sourced root key, and the ephemeral opt-in — so the
/// fail-closed decision is testable without mutating the process-global
/// `HOME`/`WOVYR_KMS_ROOT_KEY` environment (which concurrent tests share).
fn build_kms_inner(
    dir: Option<PathBuf>,
    env_key: Option<KeyBytes>,
    allow_ephemeral: bool,
) -> Result<Arc<dyn Kms>> {
    // Durable root key: an operator-supplied env var (preferred — it forces the
    // key to have been sourced from somewhere durable before startup), else a
    // generate-once file persisted under `dir`.
    let root_key = env_key.or_else(|| {
        dir.as_ref()
            .and_then(|d| wovyr_kms::root::from_file(d.join("root.key")).ok())
    });

    if let Some(key) = root_key {
        // Tenant-key catalog: the durable file store when the directory is
        // usable (the same condition `from_file` needed, so this is the norm),
        // else in-memory — the root key is still durable either way.
        let store: Arc<dyn KmsStore> = match dir.as_ref().and_then(|d| FileKmsStore::new(d).ok()) {
            Some(s) => Arc::new(s),
            None => Arc::new(InMemoryKmsStore::new()),
        };
        return Ok(Arc::new(LocalKms::new(key, store)));
    }

    // No durable key material anywhere. Fail closed unless the operator has
    // explicitly accepted an ephemeral key.
    if allow_ephemeral {
        tracing::warn!(
            "WOVYR_KMS_ALLOW_EPHEMERAL=1: using an ephemeral in-process KMS root key — \
             anything sealed under it (secrets, sensitive memory) will NOT survive a \
             restart. This is for throwaway/test use only (RM-AR-P1 SEC-405)."
        );
        let key = wovyr_kms::generate_key()?;
        return Ok(Arc::new(LocalKms::new(
            key,
            Arc::new(InMemoryKmsStore::new()),
        )));
    }

    Err(Error::config(
        "no durable KMS root key available: set WOVYR_KMS_ROOT_KEY (hex-encoded 32 bytes) \
         or ensure ~/.wovyr/kms is writable so a root key can be persisted. Refusing to \
         start with an ephemeral key that would silently lose every sealed secret and \
         sensitive memory on the next restart. Set WOVYR_KMS_ALLOW_EPHEMERAL=1 only for a \
         throwaway/test run (RM-AR-P1 SEC-405).",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_key_material_fails_closed() {
        // No env key, no directory, no ephemeral opt-in → a clear config error,
        // never a silent ephemeral key (SEC-405's acceptance criterion). Match
        // rather than `unwrap_err` — the Ok type (`Arc<dyn Kms>`) isn't `Debug`.
        match build_kms_inner(None, None, false) {
            Err(Error::Config(msg)) => assert!(
                msg.contains("WOVYR_KMS_ROOT_KEY"),
                "the error must name the fix: {msg}"
            ),
            Err(other) => panic!("expected a config error, got {other:?}"),
            Ok(_) => panic!("expected fail-closed, but a KMS was constructed"),
        }
    }

    #[test]
    fn ephemeral_opt_in_yields_a_key() {
        // The explicit escape hatch still produces a working (ephemeral) KMS.
        let kms = build_kms_inner(None, None, true).expect("ephemeral opt-in should succeed");
        // It's usable: minting a data key for a tenant works.
        kms.generate_data_key("t").expect("ephemeral kms is usable");
    }

    #[test]
    fn a_persistent_key_directory_yields_a_durable_kms() {
        // A writable directory with no env key → a generate-once persisted root
        // key, the normal single-host path (no error, no ephemeral fallback).
        let dir = std::env::temp_dir().join(format!(
            "wovyr_config_kms_test_{}_{}",
            std::process::id(),
            "durable"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let kms = build_kms_inner(Some(dir.clone()), None, false)
            .expect("a writable key dir should yield a durable KMS");
        kms.generate_data_key("t").expect("durable kms is usable");
        // The root key was persisted, so a second construction reuses it.
        assert!(dir.join("root.key").exists(), "root key must be persisted");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
