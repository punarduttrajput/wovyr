//! Sourcing the **root key**
//! ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)):
//! in a real deployment this material lives in a managed KMS/HSM and the
//! platform only ever references it; [`crate::LocalKms`] is a single-host
//! stand-in, so this module's job is just to get 32 bytes from somewhere
//! reasonable for that stand-in — an env var, or a generate-once file —
//! never to *be* the KMS itself.
//!
//! **Root-key escrow is a mandatory production install step (RM-GA-P2 DR-1002).**
//! Every secret and every sensitive memory record in the platform is sealed,
//! directly or transitively, under this one key; if the host that generated it
//! is lost with no escrowed copy, that data is permanently and
//! unrecoverably gone — there is no recovery path, by design (this is the same
//! property that makes `LocalKms::destroy_tenant_key` an irreversible
//! crypto-shred). [`from_env`] (`WOVYR_KMS_ROOT_KEY`, hex-encoded) is the
//! supported production mode precisely because it forces the operator to have
//! sourced the key from somewhere durable (a secrets manager, an HSM export, a
//! sealed escrow document) *before* the platform ever starts. [`from_file`]'s
//! generate-on-first-use behavior is a dev/local convenience only — it logs a
//! loud warning the moment it generates a fresh key, telling the operator to
//! escrow the file it just wrote, because nothing else ever will.

use crate::crypto::{KeyBytes, generate_key};
use std::path::Path;
use wovyr_common::{Error, Result};

/// Read a hex-encoded 32-byte root key from the environment variable `var`.
pub fn from_env(var: &str) -> Result<KeyBytes> {
    let hex_str = std::env::var(var).map_err(|_| Error::config(format!("{var} is not set")))?;
    decode_hex_key(&hex_str, var)
}

/// Load the root key from `path`, generating and persisting a fresh one on
/// first use, restricted to the owning user only (via
/// [`wovyr_common::fs::restrict_to_owner`]). Convenient for local/dev; a
/// production deployment should prefer [`from_env`] (or, later, a real
/// KMS-backed root) —
/// generating a fresh key here logs a loud warning, since this file is now the
/// *only* copy of the key that protects every secret/memory this process ever
/// seals, and nothing escrows it automatically.
pub fn from_file(path: impl AsRef<Path>) -> Result<KeyBytes> {
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| Error::config(format!("create kms root key dir: {e}")))?;

    // Cross-process lock (RM-GA-P2 DUR-403) spanning the whole "check, else
    // generate+write" sequence: two processes racing to create the root key
    // for the first time must not each generate an independent key while
    // only one survives on disk — the loser's in-memory key would silently
    // seal data nothing else can ever decrypt.
    let _flock = wovyr_common::fs::FileLock::acquire(parent)
        .map_err(|e| Error::config(format!("lock kms root key dir: {e}")))?;

    if path.exists() {
        let hex_str = std::fs::read_to_string(path)
            .map_err(|e| Error::config(format!("read root key file: {e}")))?;
        return decode_hex_key(hex_str.trim(), "root key file");
    }
    let key = generate_key()?;
    wovyr_common::fs::atomic_write(path, hex::encode(key))
        .map_err(|e| Error::config(format!("write root key file: {e}")))?;
    restrict_permissions(path)?;
    tracing::warn!(
        path = %path.display(),
        "generated a new KMS root key — this is the ONLY copy of the key that protects \
         every secret and sensitive memory record this process will ever seal; if this \
         host is lost without an escrowed copy of this file, that data is PERMANENTLY \
         UNRECOVERABLE. Escrow it now (a secrets manager, an HSM, a sealed document) and \
         set WOVYR_KMS_ROOT_KEY from the escrowed copy for production use instead of \
         relying on this generate-on-first-use file (RM-GA-P2 DR-1002)."
    );
    Ok(key)
}

fn decode_hex_key(hex_str: &str, source: &str) -> Result<KeyBytes> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| Error::config(format!("{source} is not valid hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::config(format!("{source} must decode to exactly 32 bytes")))
}

fn restrict_permissions(path: &Path) -> Result<()> {
    wovyr_common::fs::restrict_to_owner(path)
        .map_err(|e| Error::config(format!("restrict root key file permissions: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_decodes_a_valid_hex_key() {
        let var = "WOVYR_KMS_TEST_ROOT_KEY_VALID";
        let key = generate_key().unwrap();
        // SAFETY: test-only, single-threaded within this test's scope for this var name.
        unsafe { std::env::set_var(var, hex::encode(key)) };
        assert_eq!(from_env(var).unwrap(), key);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn from_env_missing_is_a_config_error() {
        assert!(matches!(
            from_env("WOVYR_KMS_TEST_ROOT_KEY_DEFINITELY_UNSET").unwrap_err(),
            Error::Config(_)
        ));
    }

    #[test]
    fn from_env_rejects_wrong_length() {
        let var = "WOVYR_KMS_TEST_ROOT_KEY_SHORT";
        unsafe { std::env::set_var(var, "deadbeef") };
        assert!(matches!(from_env(var).unwrap_err(), Error::Config(_)));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn from_file_generates_once_then_persists_across_calls() {
        let dir = std::env::temp_dir().join(format!("wovyr_kms_root_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("root.key");

        let first = from_file(&path).unwrap();
        let second = from_file(&path).unwrap();
        assert_eq!(first, second);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        #[cfg(windows)]
        {
            let user = std::env::var("USERNAME").unwrap();
            let output = std::process::Command::new("icacls")
                .arg(&path)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.contains(&user), "icacls output: {text}");
            assert!(!text.contains("(I)"), "icacls output: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
