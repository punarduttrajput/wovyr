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
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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

/// Resolves a bearer API key to the principal it authenticates.
pub trait ApiKeyStore: Send + Sync {
    /// The principal `raw_key` authenticates, if it is a known, live key.
    fn principal_for(&self, raw_key: &str) -> Option<String>;
}

/// An in-memory key store — the crate's own tests, an `authz_matrix` integration
/// test (SEC-105), or an embedder's own tests, all of which need to mint a
/// credential without touching `~/.apex/auth`.
#[derive(Default)]
pub struct InMemoryApiKeyStore {
    by_hash: RwLock<BTreeMap<String, String>>,
}

impl InMemoryApiKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `raw_key` as authenticating `principal` (test helper / bootstrap).
    pub fn insert(&self, raw_key: &str, principal: impl Into<String>) {
        self.by_hash
            .write()
            .expect("api key store poisoned")
            .insert(hash_key(raw_key), principal.into());
    }
}

impl ApiKeyStore for InMemoryApiKeyStore {
    fn principal_for(&self, raw_key: &str) -> Option<String> {
        self.by_hash
            .read()
            .expect("api key store poisoned")
            .get(&hash_key(raw_key))
            .cloned()
    }
}

/// A durable key store at `~/.apex/auth/api_keys.json` (hash → principal; the raw key
/// is never stored, mirroring how secrets/webhook signing keys are handled elsewhere
/// in this codebase).
pub struct FileApiKeyStore {
    path: PathBuf,
}

impl FileApiKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("api_keys.json"),
        })
    }

    fn load(&self) -> BTreeMap<String, String> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Mint a fresh random API key for `principal`, persist only its hash, and return
    /// the raw key — shown once, exactly like every other credential-issuance flow.
    /// The CLI's `apex auth create-key` is the operator-facing entry point.
    pub fn create_key(&self, principal: &str) -> std::io::Result<String> {
        use rand::Rng;
        let raw: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(40)
            .map(char::from)
            .collect();
        let mut map = self.load();
        map.insert(hash_key(&raw), principal.to_string());
        std::fs::write(&self.path, serde_json::to_vec_pretty(&map)?)?;
        Ok(raw)
    }
}

impl ApiKeyStore for FileApiKeyStore {
    fn principal_for(&self, raw_key: &str) -> Option<String> {
        self.load().get(&hash_key(raw_key)).cloned()
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
        let raw = store.create_key("alice").unwrap();
        assert_eq!(store.principal_for(&raw).as_deref(), Some("alice"));
        assert_eq!(store.principal_for("not-a-key"), None);
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
