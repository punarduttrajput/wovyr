//! `apex auth` commands: mint API keys for `APEX_AUTH_MODE=apikey`
//! ([RM-GA-P1 SEC-101](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)).
//!
//! Operates directly on the same `~/.apex/auth/api_keys.json` store the server reads
//! from — no server process required, matching how the `kms`/`memory`/`plugin`
//! commands work locally.

use crate::config;

/// `apex auth create-key <principal>` — mint a fresh API key that authenticates as
/// `principal`, printing the raw key once (only its SHA-256 hash is persisted).
pub fn create_key_cmd(principal: &str) -> apex_common::Result<()> {
    let dir = config::config_dir()?.join("auth");
    let store = apex_server::FileApiKeyStore::new(dir)
        .map_err(|e| apex_common::Error::config(format!("could not open the key store: {e}")))?;
    let raw = store
        .create_key(principal)
        .map_err(|e| apex_common::Error::config(format!("could not mint a key: {e}")))?;
    println!("minted a new API key for `{principal}` (shown once — store it securely):");
    println!("{raw}");
    println!(
        "\nUse it as `Authorization: Bearer {raw}` against a server running with \
         APEX_AUTH_MODE=apikey."
    );
    Ok(())
}
