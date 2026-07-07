//! Sourcing the **root key**
//! ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)):
//! in a real deployment this material lives in a managed KMS/HSM and the
//! platform only ever references it; [`crate::LocalKms`] is a single-host
//! stand-in, so this module's job is just to get 32 bytes from somewhere
//! reasonable for that stand-in — an env var, or a generate-once file —
//! never to *be* the KMS itself.

use crate::crypto::{KeyBytes, generate_key};
use apex_common::{Error, Result};
use std::path::Path;

/// Read a hex-encoded 32-byte root key from the environment variable `var`.
pub fn from_env(var: &str) -> Result<KeyBytes> {
    let hex_str = std::env::var(var).map_err(|_| Error::config(format!("{var} is not set")))?;
    decode_hex_key(&hex_str, var)
}

/// Load the root key from `path`, generating and persisting a fresh one on
/// first use (mode `0600` on Unix). Convenient for local/dev; a production
/// deployment should prefer [`from_env`] (or, later, a real KMS-backed root).
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
    let _flock = apex_common::fs::FileLock::acquire(parent)
        .map_err(|e| Error::config(format!("lock kms root key dir: {e}")))?;

    if path.exists() {
        let hex_str = std::fs::read_to_string(path)
            .map_err(|e| Error::config(format!("read root key file: {e}")))?;
        return decode_hex_key(hex_str.trim(), "root key file");
    }
    let key = generate_key()?;
    apex_common::fs::atomic_write(path, hex::encode(key))
        .map_err(|e| Error::config(format!("write root key file: {e}")))?;
    restrict_permissions(path)?;
    Ok(key)
}

fn decode_hex_key(hex_str: &str, source: &str) -> Result<KeyBytes> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| Error::config(format!("{source} is not valid hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::config(format!("{source} must decode to exactly 32 bytes")))
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| Error::config(format!("stat root key file: {e}")))?
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| Error::config(format!("chmod root key file: {e}")))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_decodes_a_valid_hex_key() {
        let var = "APEX_KMS_TEST_ROOT_KEY_VALID";
        let key = generate_key().unwrap();
        // SAFETY: test-only, single-threaded within this test's scope for this var name.
        unsafe { std::env::set_var(var, hex::encode(key)) };
        assert_eq!(from_env(var).unwrap(), key);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn from_env_missing_is_a_config_error() {
        assert!(matches!(
            from_env("APEX_KMS_TEST_ROOT_KEY_DEFINITELY_UNSET").unwrap_err(),
            Error::Config(_)
        ));
    }

    #[test]
    fn from_env_rejects_wrong_length() {
        let var = "APEX_KMS_TEST_ROOT_KEY_SHORT";
        unsafe { std::env::set_var(var, "deadbeef") };
        assert!(matches!(from_env(var).unwrap_err(), Error::Config(_)));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn from_file_generates_once_then_persists_across_calls() {
        let dir = std::env::temp_dir().join(format!("apex_kms_root_test_{}", std::process::id()));
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
