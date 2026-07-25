//! Local credential storage for the `wovyr` CLI.
//!
//! Persists the server URL and an access token under `~/.wovyr/credentials.json`
//! so commands can authenticate against a server
//! ([CLI configuration](../../docs/11-cli/configuration.md)). v0.1 stores a token
//! supplied on the command line; interactive/OAuth login flows come later.
//!
//! Per the [coding standards](../../docs/19-implementation-guide/coding-standards.md),
//! the token is never logged and the file is created with owner-only permissions
//! where the platform supports it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use wovyr_common::{Error, Result};

/// Stored authentication state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// Target server base URL.
    pub server: String,
    /// Access token (treated as a secret — never logged).
    pub token: String,
}

impl Credentials {
    /// The token with all but the last four characters masked, for display.
    pub fn masked_token(&self) -> String {
        let n = self.token.chars().count();
        if n <= 4 {
            "****".to_string()
        } else {
            let tail: String = self.token.chars().skip(n - 4).collect();
            format!("****{tail}")
        }
    }
}

/// The `~/.wovyr` configuration directory (RM-GA-P4 HLTH-903: shared with
/// `wovyr-server` via `wovyr-config` instead of each binary resolving
/// `HOME`/`USERPROFILE` independently).
pub fn config_dir() -> Result<PathBuf> {
    wovyr_config::wovyr_dir()
}

/// The path to the credentials file.
pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

/// The platform KMS ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)),
/// over the same `~/.wovyr/kms` directory the server uses — shared via
/// `wovyr-config` (RM-GA-P4 HLTH-903) so either process can decrypt the
/// other's sealed secrets/memories, instead of each maintaining its own copy
/// of this construction logic.
///
/// Fail-closed on missing durable key material (RM-AR-P1 SEC-405): if neither
/// `WOVYR_KMS_ROOT_KEY` nor a writable `~/.wovyr/kms` is available, this exits
/// with a clear message rather than minting an ephemeral key that would lose
/// sealed data on the next run. (In practice the CLI always has a home
/// directory, so this only trips on a genuinely broken environment;
/// `WOVYR_KMS_ALLOW_EPHEMERAL=1` is the throwaway/test opt-out.)
pub fn kms() -> Arc<dyn wovyr_kms::Kms> {
    wovyr_config::kms::build_kms().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    })
}

/// Persist credentials, creating the config directory if needed.
pub fn save_credentials(creds: &Credentials) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    // Cross-process lock (RM-GA-P2 DUR-403): `atomic_write`'s temp file has a
    // fixed name, so two concurrent `login`/`logout` invocations racing on it
    // could otherwise interleave into that shared temp file before either
    // renames.
    let _lock = wovyr_common::fs::FileLock::acquire(&dir)?;
    let path = credentials_path()?;
    let json = serde_json::to_string_pretty(creds)?;
    wovyr_common::fs::atomic_write(&path, json)?;
    restrict_permissions(&path);
    Ok(())
}

/// Load credentials, returning `None` if the user is not logged in.
pub fn load_credentials() -> Result<Option<Credentials>> {
    let path = credentials_path()?;
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let creds = serde_json::from_str(&contents)?;
            Ok(Some(creds))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Delete stored credentials. Returns whether a file was removed.
pub fn delete_credentials() -> Result<bool> {
    let path = credentials_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Restrict the credentials file to owner-only access where supported.
/// Best-effort: failure to tighten permissions should not break login.
fn restrict_permissions(path: &std::path::Path) {
    let _ = wovyr_common::fs::restrict_to_owner(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_token() {
        let c = Credentials {
            server: "https://x".into(),
            token: "abcdef1234".into(),
        };
        assert_eq!(c.masked_token(), "****1234");
    }

    #[test]
    fn masks_short_token() {
        let c = Credentials {
            server: "s".into(),
            token: "ab".into(),
        };
        assert_eq!(c.masked_token(), "****");
    }
}
