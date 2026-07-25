//! Typed readers for the handful of `WOVYR_*` env vars that select a backend
//! and are read **identically by both binaries** — real duplication, not
//! just similar-looking code. Process-tuning knobs with no CLI equivalent
//! (TLS, HTTP limits, rate limits, CORS, auth mode, `WOVYR_PLATFORM_ADMINS`,
//! the tiered-memory backend vars, `WOVYR_WEBHOOKS_ENCRYPT_AT_REST`) stay
//! where they are — there's no cross-binary drift risk to fix there.

/// Whether the secret vault should seal values at rest — read identically by
/// `wovyr-server`'s and `wovyr-cli`'s vault construction, so both must agree on
/// which of `secrets.json`/`secrets.enc.json` is the live file.
///
/// **Encrypted is the default (RM-AIM-P1 SEC-101).** `WOVYR_SECRETS_PLAINTEXT=1`
/// is the explicit opt-out for a trusted-local setup that genuinely wants the
/// old plaintext `secrets.json`. The pre-SEC-101 opt-*in* var
/// (`WOVYR_SECRETS_ENCRYPT_AT_REST`) is still honored and, being an explicit
/// request *for* encryption, wins over a contradictory plaintext opt-out —
/// fail toward the safer mode.
pub fn secrets_encrypt_at_rest() -> bool {
    if std::env::var("WOVYR_SECRETS_ENCRYPT_AT_REST").is_ok() {
        return true;
    }
    !std::env::var("WOVYR_SECRETS_PLAINTEXT")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// The Postgres URL for a shared marketplace registry
/// (`WOVYR_MARKETPLACE_POSTGRES_URL`) — read identically by `wovyr-server`'s
/// and `wovyr-cli`'s `postgres`-feature-gated registry construction.
pub fn marketplace_postgres_url() -> Option<String> {
    std::env::var("WOVYR_MARKETPLACE_POSTGRES_URL").ok()
}
