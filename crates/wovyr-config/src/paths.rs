//! Per-resource directories under `~/.wovyr`, centralizing path literals that
//! were previously duplicated verbatim across `wovyr-server` and `wovyr-cli`
//! (and, in `wovyr-server`'s case, across its own route modules).

use std::path::PathBuf;
use wovyr_common::Result;

/// `~/.wovyr/secrets` — the secret vault's durable store directory.
pub fn secrets_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("secrets"))
}

/// `~/.wovyr/memory` — the memory engine's file-store directory.
pub fn memory_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("memory"))
}

/// `~/.wovyr/kms` — the KMS root key + tenant-key catalog directory.
pub fn kms_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("kms"))
}

/// `~/.wovyr/plugins` — the plugin trust store + installed catalog.
pub fn plugins_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("plugins"))
}

/// `~/.wovyr/plugins/staging` — content-addressed staged plugin artifacts.
pub fn staging_dir() -> Result<PathBuf> {
    Ok(plugins_dir()?.join("staging"))
}

/// `~/.wovyr/marketplace` — the local marketplace registry (`registry.json`,
/// and — server-only — the operator curation `policy.json`).
pub fn marketplace_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("marketplace"))
}

/// `~/.wovyr/workflows` — durable workflow executions/timers/schedules,
/// shared between the CLI's local runner and the server's engine.
pub fn workflows_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("workflows"))
}

/// `~/.wovyr/workflows/definitions` — server-persisted workflow definitions,
/// so a durable timer/schedule can re-resolve a `Definition` by name with no
/// live HTTP caller around to re-supply it.
pub fn definitions_dir() -> Result<PathBuf> {
    Ok(workflows_dir()?.join("definitions"))
}

/// `~/.wovyr/webhooks` — the webhook subscription store (server-only; the CLI
/// has no webhooks surface).
pub fn webhooks_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("webhooks"))
}

/// `~/.wovyr/tenancy` — the multi-tenancy catalog (orgs/projects/members),
/// server-only.
pub fn tenancy_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("tenancy"))
}

/// `~/.wovyr/audit` — the tamper-evident audit log, server-only.
pub fn audit_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("audit"))
}

/// `~/.wovyr/auth` — the server's API-key store.
pub fn auth_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("auth"))
}

/// `~/.wovyr/ui` — the generative-UI runtime's durable state (pending frames
/// awaiting a human decision, tenant policy documents), server-only.
pub fn ui_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("ui"))
}

/// `~/.wovyr/server` — server-local state never shared with or read by the
/// CLI (the idempotency cache, the daily quota accumulator).
pub fn server_state_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("server"))
}

/// `~/.wovyr/mcp` — the persisted MCP connection catalog (PRD-006, RM-MCX-P1-101),
/// server-only.
pub fn mcp_dir() -> Result<PathBuf> {
    Ok(crate::root::wovyr_dir()?.join("mcp"))
}
