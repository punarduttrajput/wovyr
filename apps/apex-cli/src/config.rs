//! Local credential storage for the `apex` CLI.
//!
//! Persists the server URL and an access token under `~/.apex/credentials.json`
//! so commands can authenticate against a server
//! ([CLI configuration](../../docs/11-cli/configuration.md)). v0.1 stores a token
//! supplied on the command line; interactive/OAuth login flows come later.
//!
//! Per the [coding standards](../../docs/19-implementation-guide/coding-standards.md),
//! the token is never logged and the file is created with owner-only permissions
//! where the platform supports it.

use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

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

/// The `~/.apex` configuration directory (RM-GA-P4 HLTH-903: shared with
/// `apex-server` via `apex-config` instead of each binary resolving
/// `HOME`/`USERPROFILE` independently).
pub fn config_dir() -> Result<PathBuf> {
    apex_config::apex_dir()
}

/// The path to the credentials file.
pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

/// The platform KMS ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)),
/// over the same `~/.apex/kms` directory the server uses — shared via
/// `apex-config` (RM-GA-P4 HLTH-903) so either process can decrypt the
/// other's sealed secrets/memories, instead of each maintaining its own copy
/// of this construction logic.
pub fn kms() -> Arc<dyn apex_kms::Kms> {
    apex_config::kms::build_kms()
}

/// Persist credentials, creating the config directory if needed.
pub fn save_credentials(creds: &Credentials) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    // Cross-process lock (RM-GA-P2 DUR-403): `atomic_write`'s temp file has a
    // fixed name, so two concurrent `login`/`logout` invocations racing on it
    // could otherwise interleave into that shared temp file before either
    // renames.
    let _lock = apex_common::fs::FileLock::acquire(&dir)?;
    let path = credentials_path()?;
    let json = serde_json::to_string_pretty(creds)?;
    apex_common::fs::atomic_write(&path, json)?;
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
    let _ = apex_common::fs::restrict_to_owner(path);
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
