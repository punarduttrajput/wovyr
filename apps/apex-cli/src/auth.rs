//! `apex auth` commands: mint API keys for `APEX_AUTH_MODE=apikey`
//! ([RM-GA-P1 SEC-101](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)).
//!
//! Operates directly on the same `~/.apex/auth/api_keys.json` store the server reads
//! from — no server process required, matching how the `kms`/`memory`/`plugin`
//! commands work locally.

use crate::config;
use std::time::Duration;

fn store() -> apex_common::Result<apex_server::FileApiKeyStore> {
    let dir = config::config_dir()?.join("auth");
    apex_server::FileApiKeyStore::new(dir)
        .map_err(|e| apex_common::Error::config(format!("could not open the key store: {e}")))
}

/// `apex auth create-key <principal> [--ttl-days N]` — mint a fresh API key that
/// authenticates as `principal`, printing the raw key + its id once (only its SHA-256
/// hash is persisted). An optional TTL sets an expiry (SRV-104).
pub fn create_key_cmd(principal: &str, ttl_days: Option<u64>) -> apex_common::Result<()> {
    let ttl = ttl_days.map(|d| Duration::from_secs(d * 24 * 60 * 60));
    let (key_id, raw) = store()?
        .create_key(principal, ttl)
        .map_err(|e| apex_common::Error::config(format!("could not mint a key: {e}")))?;
    println!("minted a new API key for `{principal}` (shown once — store it securely):");
    println!("  id:  {key_id}");
    println!("  key: {raw}");
    if let Some(days) = ttl_days {
        println!("  expires in {days} day(s)");
    }
    println!(
        "\nUse it as `Authorization: Bearer {raw}` against a server running with \
         APEX_AUTH_MODE=apikey. Revoke or rotate it later by its id (`{key_id}`)."
    );
    Ok(())
}

/// `apex auth list-keys` — show every key's metadata (never the secret): id, principal,
/// created/expiry, revoked flag, last-used (SRV-104).
pub fn list_keys_cmd() -> apex_common::Result<()> {
    let keys = store()?
        .list_keys()
        .map_err(|e| apex_common::Error::config(format!("could not read the key store: {e}")))?;
    if keys.is_empty() {
        println!("no API keys");
        return Ok(());
    }
    for k in keys {
        let expiry = k
            .expires_at_ms
            .map(|e| format!("expires_at_ms={e}"))
            .unwrap_or_else(|| "no-expiry".to_string());
        let last = k
            .last_used_ms
            .map(|l| format!("last_used_ms={l}"))
            .unwrap_or_else(|| "never-used".to_string());
        println!(
            "{}  principal={}  created_at_ms={}  {}  revoked={}  {}",
            k.key_id, k.principal, k.created_at_ms, expiry, k.revoked, last
        );
    }
    Ok(())
}

/// `apex auth revoke <key-id>` — immediately reject a key at auth (SRV-104).
pub fn revoke_key_cmd(key_id: &str) -> apex_common::Result<()> {
    let found = store()?
        .revoke(key_id)
        .map_err(|e| apex_common::Error::config(format!("could not revoke the key: {e}")))?;
    if found {
        println!("revoked API key `{key_id}`");
        Ok(())
    } else {
        Err(apex_common::Error::NotFound(format!(
            "no API key with id `{key_id}`"
        )))
    }
}

/// `apex auth rotate <key-id> [--grace-hours N]` — mint a replacement key and expire the
/// old one after a grace window (default 24h) so an in-flight client keeps working
/// during the swap (SRV-104).
pub fn rotate_key_cmd(key_id: &str, grace_hours: u64) -> apex_common::Result<()> {
    let grace = Duration::from_secs(grace_hours * 60 * 60);
    let rotated = store()?
        .rotate(key_id, grace)
        .map_err(|e| apex_common::Error::config(format!("could not rotate the key: {e}")))?;
    match rotated {
        Some((new_id, raw)) => {
            println!("rotated `{key_id}` → new key (shown once — store it securely):");
            println!("  id:  {new_id}");
            println!("  key: {raw}");
            println!("\nThe old key `{key_id}` keeps working for {grace_hours}h, then lapses.");
            Ok(())
        }
        None => Err(apex_common::Error::NotFound(format!(
            "no API key with id `{key_id}`"
        ))),
    }
}
