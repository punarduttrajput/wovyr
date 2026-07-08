//! Per-resource directories under `~/.apex`, centralizing path literals that
//! were previously duplicated verbatim across `apex-server` and `apex-cli`
//! (and, in `apex-server`'s case, across its own route modules).

use apex_common::Result;
use std::path::PathBuf;

/// `~/.apex/secrets` — the secret vault's durable store directory.
pub fn secrets_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("secrets"))
}

/// `~/.apex/memory` — the memory engine's file-store directory.
pub fn memory_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("memory"))
}

/// `~/.apex/kms` — the KMS root key + tenant-key catalog directory.
pub fn kms_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("kms"))
}

/// `~/.apex/plugins` — the plugin trust store + installed catalog.
pub fn plugins_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("plugins"))
}

/// `~/.apex/plugins/staging` — content-addressed staged plugin artifacts.
pub fn staging_dir() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("staging"))
}

/// `~/.apex/marketplace` — the local marketplace registry (`registry.json`,
/// and — server-only — the operator curation `policy.json`).
pub fn marketplace_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("marketplace"))
}

/// `~/.apex/workflows` — durable workflow executions/timers/schedules,
/// shared between the CLI's local runner and the server's engine.
pub fn workflows_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("workflows"))
}

/// `~/.apex/workflows/definitions` — server-persisted workflow definitions,
/// so a durable timer/schedule can re-resolve a `Definition` by name with no
/// live HTTP caller around to re-supply it.
pub fn definitions_dir() -> Result<PathBuf> {
    Ok(workflows_dir()?.join("definitions"))
}

/// `~/.apex/webhooks` — the webhook subscription store (server-only; the CLI
/// has no webhooks surface).
pub fn webhooks_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("webhooks"))
}

/// `~/.apex/tenancy` — the multi-tenancy catalog (orgs/projects/members),
/// server-only.
pub fn tenancy_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("tenancy"))
}

/// `~/.apex/audit` — the tamper-evident audit log, server-only.
pub fn audit_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("audit"))
}

/// `~/.apex/auth` — the server's API-key store.
pub fn auth_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("auth"))
}

/// `~/.apex/server` — server-local state never shared with or read by the
/// CLI (the idempotency cache, the daily quota accumulator).
pub fn server_state_dir() -> Result<PathBuf> {
    Ok(crate::root::apex_dir()?.join("server"))
}
