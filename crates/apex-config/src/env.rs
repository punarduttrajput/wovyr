//! Typed readers for the handful of `APEX_*` env vars that select a backend
//! and are read **identically by both binaries** — real duplication, not
//! just similar-looking code. Process-tuning knobs with no CLI equivalent
//! (TLS, HTTP limits, rate limits, CORS, auth mode, `APEX_PLATFORM_ADMINS`,
//! the tiered-memory backend vars, `APEX_WEBHOOKS_ENCRYPT_AT_REST`) stay
//! where they are — there's no cross-binary drift risk to fix there.

/// Whether the secret vault should seal values at rest
/// (`APEX_SECRETS_ENCRYPT_AT_REST`) — read identically by `apex-server`'s and
/// `apex-cli`'s vault construction, so both must agree on which of
/// `secrets.json`/`secrets.enc.json` is the live file.
pub fn secrets_encrypt_at_rest() -> bool {
    std::env::var("APEX_SECRETS_ENCRYPT_AT_REST").is_ok()
}

/// The Postgres URL for a shared marketplace registry
/// (`APEX_MARKETPLACE_POSTGRES_URL`) — read identically by `apex-server`'s
/// and `apex-cli`'s `postgres`-feature-gated registry construction.
pub fn marketplace_postgres_url() -> Option<String> {
    std::env::var("APEX_MARKETPLACE_POSTGRES_URL").ok()
}
