//! Local support for agent manifest `spec.mcp_servers` resolution (PRD-006,
//! RM-MCX-P2-201) — shared by `agents run --local` (a bare run) and
//! `workflows run --local`'s [`wovyr_runtime::AgentResolver`] impl.
//!
//! Opens the same durable connection store the CLI's (not-yet-shipped) MCP
//! management commands and the server both would use, at `~/.wovyr/mcp` — see
//! `wovyr-config`'s `paths::mcp_dir`.

use wovyr_common::Result;
use wovyr_tools::McpConnectionStore;

/// The durable MCP connection store at `~/.wovyr/mcp`.
pub fn store() -> Result<McpConnectionStore> {
    McpConnectionStore::new(wovyr_config::paths::mcp_dir()?)
}

/// The local secret vault over `~/.wovyr/secrets` (falls back to in-memory if
/// the home directory is unavailable) — the same construction the server and
/// the CLI's plugin-secret-injection path use, so a connection's
/// `secret_ref` resolves against whichever vault a `secrets`/`kms` command
/// last wrote to.
pub fn secrets_vault() -> wovyr_secrets::Vault {
    wovyr_config::secrets::build_secrets_vault(crate::config::kms())
}
