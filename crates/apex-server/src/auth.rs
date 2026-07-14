//! Credential verification (`RM-GA-P1` SEC-101/SEC-102): a middleware layer that
//! verifies a caller's identity *before* any handler runs, closing the hole where
//! `X-Apex-Principal` (or a bearer token) was trusted as an unverified, caller-supplied
//! string ([`tenancy`](crate::tenancy) derives principal/tenant from headers alone).
//!
//! Selected by `APEX_AUTH_MODE` (default `disabled-loopback`):
//! - `jwt` — a bearer JWT, HS256 (`APEX_JWT_HS_SECRET`) or RS256
//!   (`APEX_JWT_RS_PUBLIC_KEY`, PEM), checked for expiry always and issuer/audience
//!   (`APEX_JWT_ISSUER`/`APEX_JWT_AUDIENCE`) when configured. The verified `sub` claim
//!   becomes the principal.
//! - `apikey` — a bearer token, SHA-256 hashed and looked up in an [`ApiKeyStore`].
//! - `disabled-loopback` — no verification at all (today's raw-header behavior);
//!   [`refuse_anonymous_on_non_loopback`] stops this mode's anonymous escape hatch
//!   from ever reaching a non-loopback bind.
//!
//! A verified credential's principal **overwrites** the request's `X-Apex-Principal`
//! header before any handler runs, so the existing `tenancy::context`/`tenant_context`
//! (unchanged) can no longer be spoofed by a raw client-supplied value — every route
//! that authorizes off that header now authorizes off a verified identity instead.

use crate::{ApiError, AppState};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Which credential scheme the server verifies, from `APEX_AUTH_MODE`. Resolved
/// **once**, at [`AppState`](crate::AppState) construction (`AppState.auth_mode`),
/// for the same reason `anonymous_allowed` is: so a test can override it per-`AppState`
/// without racing the process-global env var against every other test in this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Jwt,
    ApiKey,
    DisabledLoopback,
}

impl AuthMode {
    pub(crate) fn from_env() -> Self {
        match std::env::var("APEX_AUTH_MODE").ok().as_deref() {
            Some("jwt") => AuthMode::Jwt,
            Some("apikey") => AuthMode::ApiKey,
            _ => AuthMode::DisabledLoopback,
        }
    }
}

/// Whether the anonymous default-tenant identity (SEC-102) is granted a role set at
/// all, gating both this module's `disabled-loopback` pass-through and
/// [`tenancy::tenant_authorize`](crate::tenancy::tenant_authorize)'s legacy bypass.
/// Resolved **once**, at [`AppState`](crate::AppState) construction time (into
/// `AppState.anonymous_allowed`) rather than re-read from the environment on every
/// request — so tests can override it per-`AppState` (`with_anonymous_allowed`)
/// without racing the process-global env var against every other test in this crate
/// that depends on the default below.
///
/// Explicit opt-in (`APEX_ALLOW_ANONYMOUS=1`) in a real deployment. Defaults to
/// enabled for this crate's own unit tests (which authorize via raw request headers
/// rather than minted credentials, and never exercise the loopback-bind boot check),
/// so the existing suite doesn't need every call site to mint one. Integration tests
/// under `tests/` link a normally-built copy of this crate (no `cfg(test)`) and see
/// the real, secure-by-default behavior — the point of [SEC-105](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md).
pub(crate) fn resolve_anonymous_allowed() -> bool {
    match std::env::var("APEX_ALLOW_ANONYMOUS") {
        Ok(v) => v == "1",
        Err(_) => cfg!(test),
    }
}

/// The raw `APEX_ALLOW_ANONYMOUS` flag with no test-only default — used only at the
/// `serve()` boot boundary, which is never compiled under `cfg(test)` and always
/// reflects real deployment intent.
fn env_allow_anonymous() -> bool {
    std::env::var("APEX_ALLOW_ANONYMOUS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Refuse to bind `addr` when the anonymous escape hatch is enabled on a non-loopback
/// address (SEC-102): `APEX_ALLOW_ANONYMOUS=1` is a dev-only convenience, never a
/// network-reachable default.
pub(crate) fn refuse_anonymous_on_non_loopback(
    addr: std::net::SocketAddr,
) -> apex_common::Result<()> {
    check_anonymous_bind(env_allow_anonymous(), addr)
}

/// The pure decision behind [`refuse_anonymous_on_non_loopback`], factored out so it's
/// unit-testable without mutating the process-global `APEX_ALLOW_ANONYMOUS` (which
/// every other test in this crate's default-anonymous-in-`cfg(test)` behavior depends
/// on, and would otherwise race against).
fn check_anonymous_bind(allowed: bool, addr: std::net::SocketAddr) -> apex_common::Result<()> {
    if allowed && !addr.ip().is_loopback() {
        return Err(apex_common::Error::config(format!(
            "APEX_ALLOW_ANONYMOUS=1 is refused on a non-loopback bind ({addr}); set \
             APEX_AUTH_MODE=jwt or apikey (and unset APEX_ALLOW_ANONYMOUS) for a \
             network-reachable server"
        )));
    }
    Ok(())
}

/// The `Authorization: Bearer <token>` credential, if present and non-empty.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Overwrite (never merge with) the request's `X-Apex-Principal` header with the
/// verified principal, so a downstream handler reading it — unchanged — sees only the
/// verified value regardless of what the client sent.
fn set_verified_principal(req: &mut Request, principal: &str) {
    match HeaderValue::from_str(principal) {
        Ok(value) => {
            req.headers_mut().insert("x-apex-principal", value);
        }
        Err(_) => {
            req.headers_mut().remove("x-apex-principal");
        }
    }
}

fn unauthorized(msg: impl Into<String>) -> Response {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", msg).into_response()
}

/// Verifies the caller's credential before any handler runs (SEC-101). Mounted via
/// [`crate::router`] over every route except the public `/healthz` and `/metrics`.
pub(crate) async fn authenticate(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    match state.auth_mode {
        AuthMode::DisabledLoopback => {
            if state.anonymous_allowed {
                next.run(req).await
            } else {
                unauthorized(
                    "no credential presented; set APEX_AUTH_MODE=jwt|apikey or \
                     APEX_ALLOW_ANONYMOUS=1 for local/dev use",
                )
            }
        }
        AuthMode::Jwt => match bearer_token(req.headers()) {
            Some(token) => match verify_jwt_bearer(&token) {
                Ok(principal) => {
                    set_verified_principal(&mut req, &principal);
                    next.run(req).await
                }
                Err(e) => unauthorized(format!("invalid bearer credential: {e}")),
            },
            None => unauthorized("missing Authorization: Bearer credential"),
        },
        AuthMode::ApiKey => match bearer_token(req.headers()) {
            Some(token) => match state.api_keys.principal_for(&token) {
                Some(principal) => {
                    set_verified_principal(&mut req, &principal);
                    next.run(req).await
                }
                None => unauthorized("unknown API key"),
            },
            None => unauthorized("missing Authorization: Bearer credential"),
        },
    }
}

// --- JWT (HS256 / RS256) ---------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Claims {
    /// The verified principal.
    sub: String,
    /// Expiry (seconds since epoch); required, like the rest of the JWT ecosystem.
    #[allow(dead_code)]
    exp: usize,
}

fn jwt_decoding_key() -> Result<jsonwebtoken::DecodingKey, String> {
    if let Ok(secret) = std::env::var("APEX_JWT_HS_SECRET") {
        return Ok(jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()));
    }
    if let Ok(pem) = std::env::var("APEX_JWT_RS_PUBLIC_KEY") {
        return jsonwebtoken::DecodingKey::from_rsa_pem(pem.as_bytes()).map_err(|e| e.to_string());
    }
    Err(
        "APEX_AUTH_MODE=jwt requires APEX_JWT_HS_SECRET (HS256) or APEX_JWT_RS_PUBLIC_KEY \
         (RS256, PEM)"
            .to_string(),
    )
}

fn jwt_validation() -> jsonwebtoken::Validation {
    let alg = if std::env::var_os("APEX_JWT_RS_PUBLIC_KEY").is_some() {
        jsonwebtoken::Algorithm::RS256
    } else {
        jsonwebtoken::Algorithm::HS256
    };
    let mut validation = jsonwebtoken::Validation::new(alg);
    if let Ok(iss) = std::env::var("APEX_JWT_ISSUER") {
        validation.set_issuer(&[iss]);
    }
    if let Ok(aud) = std::env::var("APEX_JWT_AUDIENCE") {
        validation.set_audience(&[aud]);
    }
    validation
}

/// Verify `token` against the configured scheme, returning the verified principal
/// (the `sub` claim) or a rejection reason (bad signature, expired, wrong
/// issuer/audience, or a misconfigured server).
fn verify_jwt_bearer(token: &str) -> Result<String, String> {
    let key = jwt_decoding_key()?;
    let validation = jwt_validation();
    let data =
        jsonwebtoken::decode::<Claims>(token, &key, &validation).map_err(|e| e.to_string())?;
    Ok(data.claims.sub)
}

// --- API keys ---------------------------------------------------------------------

fn hash_key(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// Wall-clock milliseconds since the Unix epoch — read only at the key-store boundary.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `last_used` is refreshed at most this often per key, so the auth hot path doesn't
/// rewrite the store on every single request (SRV-104).
const LAST_USED_PERSIST_INTERVAL_MS: u64 = 60_000;

/// Stored metadata for one API key (RM-AIM-P1 SRV-104). The raw key is never stored —
/// the map is keyed by its SHA-256 hash — and `key_id` (a stable, non-secret prefix of
/// that hash) is the handle an operator uses to revoke/rotate it.
#[derive(Clone, Serialize, Deserialize)]
pub struct KeyRecord {
    pub key_id: String,
    pub principal: String,
    pub created_at_ms: u64,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub last_used_ms: Option<u64>,
}

impl KeyRecord {
    /// Whether the key is currently usable: not revoked and not past its expiry.
    fn is_live(&self, now: u64) -> bool {
        !self.revoked && self.expires_at_ms.is_none_or(|exp| now < exp)
    }
}

/// A value-free projection of a [`KeyRecord`] for listing (never carries the hash).
#[derive(Clone, Serialize)]
pub struct KeyMetadata {
    pub key_id: String,
    pub principal: String,
    pub created_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub revoked: bool,
    pub last_used_ms: Option<u64>,
}

impl From<&KeyRecord> for KeyMetadata {
    fn from(r: &KeyRecord) -> Self {
        Self {
            key_id: r.key_id.clone(),
            principal: r.principal.clone(),
            created_at_ms: r.created_at_ms,
            expires_at_ms: r.expires_at_ms,
            revoked: r.revoked,
            last_used_ms: r.last_used_ms,
        }
    }
}

/// The stable, non-secret handle for a key: `key_<first 12 hex of its hash>`.
fn key_id_for(hash: &str) -> String {
    format!("key_{}", &hash[..hash.len().min(12)])
}

/// A fresh random raw key (shown once) — 40 alphanumerics.
fn mint_raw() -> String {
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(40)
        .map(char::from)
        .collect()
}

/// Resolves a bearer API key to the principal it authenticates.
pub trait ApiKeyStore: Send + Sync {
    /// The principal `raw_key` authenticates, if it maps to a **live** key (not
    /// revoked, not expired). Refreshes the key's `last_used` (throttled) as a
    /// side effect.
    fn principal_for(&self, raw_key: &str) -> Option<String>;
}

/// Look up `raw_key` in `map`, enforcing revocation + expiry. Returns the principal of
/// a live key and, when its `last_used` is stale enough to persist, `true` so the
/// caller writes the (already-mutated) map back (SRV-104).
fn resolve_live_key(
    map: &mut BTreeMap<String, KeyRecord>,
    raw_key: &str,
) -> (Option<String>, bool) {
    let now = now_ms();
    let Some(record) = map.get_mut(&hash_key(raw_key)) else {
        return (None, false);
    };
    if !record.is_live(now) {
        return (None, false);
    }
    let principal = record.principal.clone();
    let should_persist = record
        .last_used_ms
        .is_none_or(|lu| now.saturating_sub(lu) >= LAST_USED_PERSIST_INTERVAL_MS);
    if should_persist {
        record.last_used_ms = Some(now);
    }
    (Some(principal), should_persist)
}

/// An in-memory key store — the crate's own tests, an `authz_matrix` integration
/// test (SEC-105), or an embedder's own tests, all of which need to mint a
/// credential without touching `~/.apex/auth`.
#[derive(Default)]
pub struct InMemoryApiKeyStore {
    by_hash: RwLock<BTreeMap<String, KeyRecord>>,
}

impl InMemoryApiKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `raw_key` as authenticating `principal` — a live key with no expiry
    /// (test helper / bootstrap).
    pub fn insert(&self, raw_key: &str, principal: impl Into<String>) {
        let hash = hash_key(raw_key);
        let key_id = key_id_for(&hash);
        self.by_hash
            .write()
            .expect("api key store poisoned")
            .insert(
                hash,
                KeyRecord {
                    key_id,
                    principal: principal.into(),
                    created_at_ms: now_ms(),
                    expires_at_ms: None,
                    revoked: false,
                    last_used_ms: None,
                },
            );
    }
}

impl ApiKeyStore for InMemoryApiKeyStore {
    fn principal_for(&self, raw_key: &str) -> Option<String> {
        let mut map = self.by_hash.write().expect("api key store poisoned");
        resolve_live_key(&mut map, raw_key).0
    }
}

/// The in-memory cache [`FileApiKeyStore`] keeps warm (SRV-302): the last-loaded key
/// map, stamped with the file's modification time at load — the invalidation signal.
/// A cheap `stat()` per request (checking `mtime`) replaces a full read + JSON parse
/// on every request; the map itself is only re-read when the file's `mtime` has
/// actually moved (an operator's `create-key`/`revoke`/`rotate`, in this process or
/// another one sharing the same `~/.apex/auth`).
struct CachedKeys {
    mtime: std::time::SystemTime,
    map: BTreeMap<String, KeyRecord>,
}

/// A durable key store at `~/.apex/auth/api_keys.json` ([`KeyRecord`] keyed by hash;
/// the raw key is never stored, mirroring secrets/webhook signing keys elsewhere).
/// Supports the full lifecycle (SRV-104): create (with optional TTL), list, revoke,
/// rotate-with-grace — used by the CLI's `apex auth` subcommands. **Cached in memory**
/// (SRV-302): the hot `principal_for` auth path no longer reads + deserializes the
/// whole file on every request — see [`CachedKeys`] / [`Self::load_cached`].
pub struct FileApiKeyStore {
    path: PathBuf,
    cache: RwLock<Option<CachedKeys>>,
    /// Test-observability only: counts real disk reads via [`Self::load`], so a test
    /// can assert the cache actually avoids repeat reads after warm-up. Negligible
    /// runtime cost (one atomic increment per real load, never per request).
    load_count: std::sync::atomic::AtomicUsize,
}

impl FileApiKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("api_keys.json"),
            cache: RwLock::new(None),
            load_count: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn file_mtime(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
    }

    /// The key map, served from the in-memory cache when the file's `mtime` hasn't
    /// moved since it was last loaded (SRV-302) — the common case on the hot auth
    /// path, where nothing has changed between one request and the next. Falls back
    /// to a real disk read + reparse ([`Self::load`]) on the first call, or whenever
    /// an external change (or the lack of a readable `mtime` at all) is detected.
    fn load_cached(&self) -> BTreeMap<String, KeyRecord> {
        let mtime = self.file_mtime();
        if let Some(mtime) = mtime {
            let cache = self.cache.read().expect("api key cache poisoned");
            if let Some(cached) = cache.as_ref()
                && cached.mtime == mtime
            {
                return cached.map.clone();
            }
        }
        let map = self.load();
        if let Some(mtime) = mtime {
            *self.cache.write().expect("api key cache poisoned") = Some(CachedKeys {
                mtime,
                map: map.clone(),
            });
        }
        map
    }

    /// Load the key map, transparently migrating the pre-SRV-104 `hash → principal`
    /// string format (each entry becomes a live, never-expiring [`KeyRecord`]). A
    /// real disk read — [`Self::load_cached`] is the path that should usually be
    /// called instead; this exists as the cache-miss fallback and for the
    /// mutating operations below, which always need the true current contents.
    fn load(&self) -> BTreeMap<String, KeyRecord> {
        self.load_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Ok(bytes) = std::fs::read(&self.path) else {
            return BTreeMap::new();
        };
        if let Ok(map) = serde_json::from_slice::<BTreeMap<String, KeyRecord>>(&bytes) {
            return map;
        }
        // Fall back to the old `hash -> principal` shape and migrate it in memory.
        serde_json::from_slice::<BTreeMap<String, String>>(&bytes)
            .map(|old| {
                old.into_iter()
                    .map(|(hash, principal)| {
                        let key_id = key_id_for(&hash);
                        (
                            hash,
                            KeyRecord {
                                key_id,
                                principal,
                                created_at_ms: now_ms(),
                                expires_at_ms: None,
                                revoked: false,
                                last_used_ms: None,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Persist `map` and refresh the in-memory cache to match, so a mutation made in
    /// *this* process (create/revoke/rotate) is immediately reflected without an
    /// extra disk round-trip on the next `principal_for` call.
    fn save(&self, map: &BTreeMap<String, KeyRecord>) -> std::io::Result<()> {
        std::fs::write(&self.path, serde_json::to_vec_pretty(map)?)?;
        if let Some(mtime) = self.file_mtime() {
            *self.cache.write().expect("api key cache poisoned") = Some(CachedKeys {
                mtime,
                map: map.clone(),
            });
        }
        Ok(())
    }

    /// Mint a fresh key for `principal` (optionally expiring after `ttl`), persist only
    /// its hash + metadata, and return `(key_id, raw_key)` — the raw key shown once.
    pub fn create_key(
        &self,
        principal: &str,
        ttl: Option<Duration>,
    ) -> std::io::Result<(String, String)> {
        let raw = mint_raw();
        let hash = hash_key(&raw);
        let key_id = key_id_for(&hash);
        let now = now_ms();
        let mut map = self.load();
        map.insert(
            hash,
            KeyRecord {
                key_id: key_id.clone(),
                principal: principal.to_string(),
                created_at_ms: now,
                expires_at_ms: ttl.map(|t| now + t.as_millis() as u64),
                revoked: false,
                last_used_ms: None,
            },
        );
        self.save(&map)?;
        Ok((key_id, raw))
    }

    /// Metadata for every key (value-free — no hashes), sorted by creation time.
    pub fn list_keys(&self) -> std::io::Result<Vec<KeyMetadata>> {
        let mut items: Vec<KeyMetadata> = self.load().values().map(KeyMetadata::from).collect();
        items.sort_by_key(|k| k.created_at_ms);
        Ok(items)
    }

    /// Revoke the key with `key_id` (immediately rejected at auth). Returns whether a
    /// matching key was found.
    pub fn revoke(&self, key_id: &str) -> std::io::Result<bool> {
        let mut map = self.load();
        let mut found = false;
        for record in map.values_mut() {
            if record.key_id == key_id {
                record.revoked = true;
                found = true;
            }
        }
        if found {
            self.save(&map)?;
        }
        Ok(found)
    }

    /// Rotate the key with `key_id`: mint a replacement for the same principal and set
    /// the old key to expire after `grace` (so an in-flight client keeps working during
    /// the window, then the old key is rejected). Returns the new `(key_id, raw_key)`,
    /// or `None` if `key_id` was unknown.
    pub fn rotate(
        &self,
        key_id: &str,
        grace: Duration,
    ) -> std::io::Result<Option<(String, String)>> {
        let now = now_ms();
        let mut map = self.load();
        let Some(principal) = map
            .values()
            .find(|r| r.key_id == key_id)
            .map(|r| r.principal.clone())
        else {
            return Ok(None);
        };
        // Expire the old key after the grace window (unless already sooner).
        let deadline = now + grace.as_millis() as u64;
        for record in map.values_mut() {
            if record.key_id == key_id {
                record.expires_at_ms = Some(match record.expires_at_ms {
                    Some(existing) => existing.min(deadline),
                    None => deadline,
                });
            }
        }
        let raw = mint_raw();
        let hash = hash_key(&raw);
        let new_id = key_id_for(&hash);
        map.insert(
            hash,
            KeyRecord {
                key_id: new_id.clone(),
                principal,
                created_at_ms: now,
                expires_at_ms: None,
                revoked: false,
                last_used_ms: None,
            },
        );
        self.save(&map)?;
        Ok(Some((new_id, raw)))
    }
}

impl ApiKeyStore for FileApiKeyStore {
    fn principal_for(&self, raw_key: &str) -> Option<String> {
        // SRV-302: the cached path — no disk read at all unless the file's `mtime`
        // has moved since it was last loaded.
        let mut map = self.load_cached();
        let (principal, changed) = resolve_live_key(&mut map, raw_key);
        if changed {
            // Best-effort last-used refresh; failure to persist must not fail auth.
            let _ = self.save(&map);
        }
        principal
    }
}

/// The server's API-key store: durable at `~/.apex/auth` (shared with the CLI's own
/// key-minting command), falling back to an empty in-memory store.
pub(crate) fn default_api_key_store() -> Arc<dyn ApiKeyStore> {
    if let Ok(dir) = apex_config::paths::auth_dir()
        && let Ok(store) = FileApiKeyStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryApiKeyStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::json;
    use std::sync::Mutex;

    /// Serializes the tests below: they mutate process-global `APEX_JWT_*` env vars,
    /// which would otherwise race across `cargo test`'s parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn token_with(secret: &str, claims: serde_json::Value) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    fn future_exp() -> usize {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize
            + 3600
    }

    #[test]
    fn jwt_hs256_accepts_a_valid_token_and_extracts_the_principal() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("APEX_JWT_HS_SECRET", "test-secret-1");
            std::env::remove_var("APEX_JWT_RS_PUBLIC_KEY");
            std::env::remove_var("APEX_JWT_ISSUER");
            std::env::remove_var("APEX_JWT_AUDIENCE");
        }
        let token = token_with(
            "test-secret-1",
            json!({ "sub": "alice", "exp": future_exp() }),
        );
        assert_eq!(verify_jwt_bearer(&token).as_deref(), Ok("alice"));
    }

    #[test]
    fn jwt_rejects_wrong_signature() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("APEX_JWT_HS_SECRET", "test-secret-2");
            std::env::remove_var("APEX_JWT_RS_PUBLIC_KEY");
            std::env::remove_var("APEX_JWT_ISSUER");
            std::env::remove_var("APEX_JWT_AUDIENCE");
        }
        let token = token_with(
            "wrong-secret",
            json!({ "sub": "alice", "exp": future_exp() }),
        );
        assert!(verify_jwt_bearer(&token).is_err());
    }

    #[test]
    fn jwt_rejects_expired_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("APEX_JWT_HS_SECRET", "test-secret-3");
            std::env::remove_var("APEX_JWT_RS_PUBLIC_KEY");
            std::env::remove_var("APEX_JWT_ISSUER");
            std::env::remove_var("APEX_JWT_AUDIENCE");
        }
        let token = token_with("test-secret-3", json!({ "sub": "alice", "exp": 1 }));
        let err = verify_jwt_bearer(&token).unwrap_err();
        assert!(err.to_lowercase().contains("expired"), "{err}");
    }

    #[test]
    fn jwt_rejects_wrong_issuer_and_audience() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("APEX_JWT_HS_SECRET", "test-secret-4");
            std::env::remove_var("APEX_JWT_RS_PUBLIC_KEY");
            std::env::set_var("APEX_JWT_ISSUER", "apex-issuer");
            std::env::set_var("APEX_JWT_AUDIENCE", "apex-clients");
        }
        let bad_issuer = token_with(
            "test-secret-4",
            json!({ "sub": "alice", "exp": future_exp(), "iss": "someone-else", "aud": "apex-clients" }),
        );
        assert!(verify_jwt_bearer(&bad_issuer).is_err());

        let bad_audience = token_with(
            "test-secret-4",
            json!({ "sub": "alice", "exp": future_exp(), "iss": "apex-issuer", "aud": "someone-else" }),
        );
        assert!(verify_jwt_bearer(&bad_audience).is_err());

        let good = token_with(
            "test-secret-4",
            json!({ "sub": "alice", "exp": future_exp(), "iss": "apex-issuer", "aud": "apex-clients" }),
        );
        assert_eq!(verify_jwt_bearer(&good).as_deref(), Ok("alice"));
        unsafe {
            std::env::remove_var("APEX_JWT_ISSUER");
            std::env::remove_var("APEX_JWT_AUDIENCE");
        }
    }

    #[test]
    fn api_key_store_round_trips_and_rejects_unknown_keys() {
        let store = InMemoryApiKeyStore::new();
        store.insert("k-alice", "alice");
        assert_eq!(store.principal_for("k-alice").as_deref(), Some("alice"));
        assert_eq!(store.principal_for("k-bob"), None);
    }

    #[test]
    fn file_api_key_store_persists_minted_keys() {
        let dir = std::env::temp_dir().join(format!("apex-auth-test-{}", std::process::id()));
        let store = FileApiKeyStore::new(&dir).unwrap();
        let (_key_id, raw) = store.create_key("alice", None).unwrap();
        assert_eq!(store.principal_for(&raw).as_deref(), Some("alice"));
        assert_eq!(store.principal_for("not-a-key"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-302: after warm-up, repeated `principal_for` lookups are served from the
    /// in-memory cache — no additional real disk reads as long as the file's
    /// `mtime` hasn't moved.
    #[test]
    fn file_api_key_store_avoids_repeat_reads_after_warm_up() {
        let dir = std::env::temp_dir().join(format!("apex-auth-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileApiKeyStore::new(&dir).unwrap();
        let (_id, raw) = store.create_key("alice", None).unwrap();

        let loads_after_create = store.load_count.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            loads_after_create > 0,
            "create_key itself performs at least one real load"
        );

        for _ in 0..5 {
            assert_eq!(store.principal_for(&raw).as_deref(), Some("alice"));
        }
        assert_eq!(
            store.load_count.load(std::sync::atomic::Ordering::Relaxed),
            loads_after_create,
            "repeated lookups after warm-up must not re-read the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-302: a key change made through an independent `FileApiKeyStore` handle on
    /// the same directory (standing in for a separate process, e.g. the CLI's
    /// `apex auth` commands) is still picked up — the cache invalidates on the
    /// file's `mtime`, not just this process's own writes.
    #[test]
    fn file_api_key_store_picks_up_an_external_change() {
        let dir = std::env::temp_dir().join(format!("apex-auth-external-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let store_a = FileApiKeyStore::new(&dir).unwrap();
        let (_id_a, raw_a) = store_a.create_key("alice", None).unwrap();
        assert_eq!(store_a.principal_for(&raw_a).as_deref(), Some("alice"));

        // A distinct mtime is needed for the change to be observable; NTFS/most
        // modern filesystems have far finer resolution than this, but a short
        // sleep keeps the test robust regardless.
        std::thread::sleep(Duration::from_millis(10));
        let store_b = FileApiKeyStore::new(&dir).unwrap();
        let (_id_b, raw_b) = store_b.create_key("bob", None).unwrap();

        // `store_a` never wrote bob's key itself, but must still resolve it.
        assert_eq!(store_a.principal_for(&raw_b).as_deref(), Some("bob"));
        // The original key is unaffected.
        assert_eq!(store_a.principal_for(&raw_a).as_deref(), Some("alice"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-104: an expired key is rejected at auth.
    #[test]
    fn expired_key_is_rejected() {
        let dir = std::env::temp_dir().join(format!("apex-auth-exp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileApiKeyStore::new(&dir).unwrap();
        // A zero-duration TTL is already in the past by the time auth runs.
        let (_id, raw) = store
            .create_key("alice", Some(Duration::from_millis(0)))
            .unwrap();
        // Ensure the clock has advanced at least 1ms past the expiry.
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            store.principal_for(&raw),
            None,
            "an expired key must be rejected"
        );
        // A non-expiring key for the same principal still works.
        let (_id2, raw2) = store.create_key("alice", None).unwrap();
        assert_eq!(store.principal_for(&raw2).as_deref(), Some("alice"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-104: a revoked key is rejected at auth.
    #[test]
    fn revoked_key_is_rejected() {
        let dir = std::env::temp_dir().join(format!("apex-auth-rev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileApiKeyStore::new(&dir).unwrap();
        let (key_id, raw) = store.create_key("alice", None).unwrap();
        assert_eq!(store.principal_for(&raw).as_deref(), Some("alice"));
        assert!(store.revoke(&key_id).unwrap(), "revoke finds the key");
        assert_eq!(
            store.principal_for(&raw),
            None,
            "a revoked key must be rejected"
        );
        // Revoking an unknown key id is reported as not-found.
        assert!(!store.revoke("key_deadbeef").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-104: rotation issues a new working key and expires the old on a grace
    /// schedule — both valid during the window, only the old one lapses after it.
    #[test]
    fn rotation_issues_new_key_and_expires_old_after_grace() {
        let dir = std::env::temp_dir().join(format!("apex-auth-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileApiKeyStore::new(&dir).unwrap();
        let (old_id, old_raw) = store.create_key("svc", None).unwrap();

        // A tiny grace so the test can observe the old key lapse without a long sleep.
        let (new_id, new_raw) = store
            .rotate(&old_id, Duration::from_millis(30))
            .unwrap()
            .expect("rotate finds the key");
        assert_ne!(new_id, old_id, "rotation mints a distinct key");

        // Within the grace window both keys authenticate.
        assert_eq!(store.principal_for(&old_raw).as_deref(), Some("svc"));
        assert_eq!(store.principal_for(&new_raw).as_deref(), Some("svc"));

        // After the grace window the old key lapses; the new key still works.
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(
            store.principal_for(&old_raw),
            None,
            "old key must expire after grace"
        );
        assert_eq!(store.principal_for(&new_raw).as_deref(), Some("svc"));

        // Rotating an unknown key id returns None.
        assert!(
            store
                .rotate("key_deadbeef", Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-104: the pre-lifecycle `hash -> principal` on-disk format is migrated on
    /// load, so existing keys keep authenticating after an upgrade.
    #[test]
    fn legacy_hash_to_principal_format_is_migrated() {
        let dir = std::env::temp_dir().join(format!("apex-auth-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Write the old format directly: { "<sha256(raw)>": "alice" }.
        let raw = "legacy-key-value";
        let legacy: BTreeMap<String, String> =
            [(hash_key(raw), "alice".to_string())].into_iter().collect();
        std::fs::write(
            dir.join("api_keys.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let store = FileApiKeyStore::new(&dir).unwrap();
        assert_eq!(
            store.principal_for(raw).as_deref(),
            Some("alice"),
            "a legacy key must still authenticate after migration"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end (SEC-101 acceptance criteria): with `APEX_AUTH_MODE=apikey`, a
    /// request with no credential, or an unknown one, is `401`; a request bearing a
    /// minted key resolves the correct principal — and a spoofed `X-Apex-Principal`
    /// header is powerless to override it, since the middleware overwrites it before
    /// any handler runs. Uses a real tenancy membership (not `APEX_PLATFORM_ADMINS`,
    /// a process-global env var that would race against this crate's other tests).
    #[tokio::test]
    async fn apikey_mode_end_to_end_over_the_router() {
        use apex_tenancy::{
            InMemoryTenancyStore, MemberScope, Membership, Organization, Role, TenancyStore,
        };
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org = tenancy
            .create_org(Organization::new("default", "Apikey Test Co"))
            .unwrap();
        tenancy
            .add_membership(Membership {
                user: "alice".to_string(),
                role: Role::Viewer,
                scope: MemberScope::Organization(org.id.clone()),
            })
            .unwrap();

        let keys = InMemoryApiKeyStore::new();
        keys.insert("alice-key", "alice");
        let state = Arc::new(
            crate::AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_api_keys(Arc::new(keys))
                .with_auth_mode(AuthMode::ApiKey),
        );

        let call = |auth_header: Option<&'static str>, spoof_principal: bool| {
            let state = state.clone();
            async move {
                let mut builder = Request::builder()
                    .method("GET")
                    .uri("/api/v1/agents")
                    .header("content-type", "application/json");
                if let Some(h) = auth_header {
                    builder = builder.header("authorization", h);
                }
                if spoof_principal {
                    builder = builder.header("x-apex-principal", "alice");
                }
                crate::router(state.clone())
                    .oneshot(builder.body(Body::empty()).unwrap())
                    .await
                    .unwrap()
            }
        };

        // No credential at all → 401.
        assert_eq!(call(None, false).await.status(), StatusCode::UNAUTHORIZED);
        // Spoofing the principal header with no credential is powerless → still 401.
        assert_eq!(call(None, true).await.status(), StatusCode::UNAUTHORIZED);
        // An unknown key → 401.
        assert_eq!(
            call(Some("Bearer not-a-real-key"), false).await.status(),
            StatusCode::UNAUTHORIZED
        );
        // The minted key resolves alice, a real (viewer) member → 200.
        assert_eq!(
            call(Some("Bearer alice-key"), false).await.status(),
            StatusCode::OK
        );
    }

    #[test]
    fn refuses_anonymous_on_non_loopback_bind_only_when_flag_is_set() {
        let non_loopback: std::net::SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let loopback: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(check_anonymous_bind(true, non_loopback).is_err());
        assert!(check_anonymous_bind(true, loopback).is_ok());
        assert!(check_anonymous_bind(false, non_loopback).is_ok());
    }
}
