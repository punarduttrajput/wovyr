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

/// Resolve the user's home directory across platforms.
fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::config("could not determine home directory (set HOME or USERPROFILE)")
        })
}

/// The `~/.apex` configuration directory.
pub fn config_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".apex"))
}

/// The path to the credentials file.
pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

/// Persist credentials, creating the config directory if needed.
pub fn save_credentials(creds: &Credentials) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = credentials_path()?;
    let json = serde_json::to_string_pretty(creds)?;
    std::fs::write(&path, json)?;
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
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    // Best-effort: failure to tighten permissions should not break login.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

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
