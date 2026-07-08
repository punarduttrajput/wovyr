//! Cross-cutting API conventions ([API overview](../../docs/09-api/overview.md)):
//! cursor **pagination** (§6), **idempotency keys** (§9), and **request-id**
//! propagation (§14). These harden the `/v1` surface so list endpoints page
//! consistently, client retries are safe, and every response is traceable.

use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// --- pagination (overview §6) ----------------------------------------------------

/// Default page size when `limit` is omitted.
pub(crate) const DEFAULT_LIMIT: usize = 25;
/// Maximum page size; larger requests are clamped.
pub(crate) const MAX_LIMIT: usize = 100;

/// `?limit=&cursor=` query parameters for a list endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct PageQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

/// A resolved page window (offset + clamped limit).
pub(crate) struct Page {
    limit: usize,
    offset: usize,
}

impl PageQuery {
    /// Resolve the window, clamping `limit` to `[1, MAX_LIMIT]` and decoding the cursor
    /// (an unparseable cursor starts from the beginning).
    pub(crate) fn page(&self) -> Page {
        Page {
            limit: self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
            offset: self.cursor.as_deref().and_then(decode_cursor).unwrap_or(0),
        }
    }
}

/// Build the standard paginated envelope (`{data, has_more, next_cursor,
/// total_estimate}`) for `items`, returning one page from `page.offset`.
pub(crate) fn paginate(items: Vec<Value>, page: &Page) -> Value {
    let total = items.len();
    let end = (page.offset + page.limit).min(total);
    let start = page.offset.min(total);
    let data = &items[start..end];
    let has_more = end < total;
    json!({
        "data": data,
        "has_more": has_more,
        "next_cursor": if has_more { Value::String(encode_cursor(end)) } else { Value::Null },
        "total_estimate": total,
    })
}

/// Opaque cursor: the offset hex-encoded (clients must not parse it).
fn encode_cursor(offset: usize) -> String {
    offset
        .to_string()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn decode_cursor(cursor: &str) -> Option<usize> {
    if cursor.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..cursor.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cursor[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()?.parse().ok()
}

// --- idempotency (overview §9) ---------------------------------------------------

/// Default idempotency-key retention (SEC-205): long enough to cover a client's
/// realistic retry window, short enough that memory doesn't grow forever.
const DEFAULT_IDEMPOTENCY_TTL_SECS: u64 = 24 * 60 * 60;
/// Default cap on distinct tracked keys (SEC-205) — bounds memory even under a burst
/// of unique keys arriving faster than the TTL sweeps them.
const DEFAULT_IDEMPOTENCY_MAX_ENTRIES: usize = 10_000;

struct IdempotencyEntry {
    body: Value,
    inserted_at_ms: u64,
}

/// The on-disk shape (RM-GA-P2 DUR-404): a flat list in `order`'s sequence, so
/// reloading rebuilds both `entries` and the FIFO `order` queue identically.
#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    key: String,
    body: Value,
    inserted_at_ms: u64,
}

/// Wall-clock milliseconds since the epoch — read only at this boundary (`get`/`put`
/// below), never in core logic. Used instead of `Instant` so an entry's age is
/// meaningful after a process restart (an `Instant` has no fixed epoch and can't be
/// persisted).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Insertion-ordered map + a FIFO eviction queue, so both TTL expiry and the max-entry
/// bound can cheaply find "the oldest entries" without scanning the whole map.
#[derive(Default)]
struct IdempotencyInner {
    entries: HashMap<String, IdempotencyEntry>,
    order: VecDeque<String>,
}

/// Caches responses to mutating requests by `Idempotency-Key`, so a client retry
/// returns the original result instead of acting twice. Tenant-scoped, bounded two
/// ways (SEC-205): entries older than `ttl` expire (checked lazily, on each
/// `get`/`put`, rather than a background sweeper — no extra task, no clock read
/// outside these two call sites), and the map never exceeds `max_entries` — a client
/// minting unique keys faster than the TTL elapses evicts the oldest tracked key
/// (FIFO) rather than growing without bound. Durable when opened with a path
/// (RM-GA-P2 DUR-404, [`new_with_path`](Self::new_with_path)) — otherwise
/// ([`new`](Self::new), what tests use) purely in-memory, exactly as before.
pub(crate) struct IdempotencyStore {
    inner: Mutex<IdempotencyInner>,
    ttl: Duration,
    max_entries: usize,
    path: Option<PathBuf>,
}

impl Default for IdempotencyStore {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(DEFAULT_IDEMPOTENCY_TTL_SECS),
            DEFAULT_IDEMPOTENCY_MAX_ENTRIES,
        )
    }
}

impl IdempotencyStore {
    /// A purely in-memory store retaining entries for `ttl`, capped at `max_entries`.
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self::new_with_path(ttl, max_entries, None)
    }

    /// Open a store retaining entries for `ttl` (capped at `max_entries`), loading any
    /// persisted entries from `path` (best-effort: a missing or corrupt file starts
    /// empty rather than failing server startup). `path: None` behaves exactly like
    /// [`new`](Self::new).
    pub(crate) fn new_with_path(ttl: Duration, max_entries: usize, path: Option<PathBuf>) -> Self {
        let inner = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Vec<PersistedEntry>>(&bytes).ok())
            .map(|records| {
                let mut inner = IdempotencyInner::default();
                for r in records {
                    inner.entries.insert(
                        r.key.clone(),
                        IdempotencyEntry {
                            body: r.body,
                            inserted_at_ms: r.inserted_at_ms,
                        },
                    );
                    inner.order.push_back(r.key);
                }
                inner
            })
            .unwrap_or_default();
        Self {
            inner: Mutex::new(inner),
            ttl,
            max_entries,
            path,
        }
    }

    /// Drop entries at the front of `order` (the oldest) that have expired, or — once
    /// at `max_entries` — the single oldest entry regardless of expiry, to admit one
    /// more. Removing a key already absent from `entries` (a stale duplicate left in
    /// `order` by an overwritten `put`) is a harmless no-op.
    fn evict(inner: &mut IdempotencyInner, ttl: Duration, max_entries: usize, make_room: bool) {
        let now = now_ms();
        while let Some(front) = inner.order.front() {
            let expired = inner
                .entries
                .get(front)
                .is_none_or(|e| now.saturating_sub(e.inserted_at_ms) > ttl.as_millis() as u64);
            let over_capacity = make_room && inner.entries.len() >= max_entries;
            if !expired && !over_capacity {
                break;
            }
            let key = inner.order.pop_front().expect("front just checked Some");
            inner.entries.remove(&key);
        }
    }

    /// The cached response body for `(tenant, key)`, if this key was already handled
    /// and hasn't expired.
    pub(crate) fn get(&self, tenant: &str, key: &str) -> Option<Value> {
        let mut inner = self.inner.lock().expect("idempotency mutex poisoned");
        Self::evict(&mut inner, self.ttl, self.max_entries, false);
        inner
            .entries
            .get(&scoped(tenant, key))
            .map(|e| e.body.clone())
    }

    /// Remember `body` as the response for `(tenant, key)`, persisting the store
    /// (RM-GA-P2 DUR-404) if opened with a path.
    pub(crate) fn put(&self, tenant: &str, key: &str, body: Value) {
        let persisted = {
            let mut inner = self.inner.lock().expect("idempotency mutex poisoned");
            Self::evict(&mut inner, self.ttl, self.max_entries, true);
            let scoped_key = scoped(tenant, key);
            inner.entries.insert(
                scoped_key.clone(),
                IdempotencyEntry {
                    body,
                    inserted_at_ms: now_ms(),
                },
            );
            inner.order.push_back(scoped_key);
            self.path.as_ref().map(|_| {
                inner
                    .order
                    .iter()
                    .filter_map(|k| {
                        inner.entries.get(k).map(|e| PersistedEntry {
                            key: k.clone(),
                            body: e.body.clone(),
                            inserted_at_ms: e.inserted_at_ms,
                        })
                    })
                    .collect::<Vec<_>>()
            })
        };
        if let (Some(path), Some(records)) = (&self.path, persisted) {
            if let Some(parent) = path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                tracing::error!(error = %e, "failed to create idempotency store directory");
                return;
            }
            match serde_json::to_vec_pretty(&records) {
                Ok(bytes) => {
                    if let Err(e) = apex_common::fs::atomic_write(path, bytes) {
                        tracing::error!(error = %e, "failed to persist idempotency store");
                    }
                }
                Err(e) => tracing::error!(error = %e, "failed to encode idempotency store"),
            }
        }
    }
}

fn scoped(tenant: &str, key: &str) -> String {
    format!("{tenant}\u{1f}{key}")
}

/// The `Idempotency-Key` header value, if present.
pub(crate) fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A route is eligible for idempotency replay when it uses a mutating method
/// (`agents:run` already qualified before RM-GA-P4 API-703; this widens the same
/// treatment to every other mutating route) — except two POST routes that only *look*
/// like mutations: `memory:query` is a read (nothing to protect against
/// double-execution), and `agents:stream` returns an SSE body this middleware can't
/// buffer and replay as an opaque JSON value.
fn is_replay_eligible(method: &Method, path: &str) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) && !path.ends_with(":query")
        && !path.ends_with(":stream")
}

/// Extend `Idempotency-Key` replay (overview §9) to every mutating route
/// (RM-GA-P4 API-703), not just `agents:run`: a client retry of an unacknowledged
/// POST/PUT/PATCH/DELETE carrying the same key gets back the original response
/// instead of re-executing the mutation. Keyed by `(tenant, method, path, key)` — the
/// method+path component is what `agents:run`'s original hand-rolled check lacked, so
/// the same key reused (deliberately or by a buggy client) against two different
/// routes can never collide. Tenant is read straight off `X-Apex-Tenant` (this layer
/// sits innermost, right before the handler, so the route's own auth/RBAC has already
/// run by the time a cache hit short-circuits it). Only a successful (2xx) response
/// with a JSON-decodable (or empty, e.g. `204`) body is cached — anything this
/// middleware can't confidently replay is served once and never stored.
pub(crate) async fn idempotency_middleware(
    State(state): State<Arc<crate::AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    if !is_replay_eligible(&method, &path) {
        return next.run(request).await;
    }
    let Some(key) = idempotency_key(request.headers()) else {
        return next.run(request).await;
    };
    let tenant = crate::tenancy::run_tenant(request.headers());
    let scoped_key = format!("{method} {path}\u{1f}{key}");

    if let Some(cached) = state.idempotency.get(&tenant, &scoped_key) {
        return replay_cached(cached);
    }

    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };
    let cacheable_body = if bytes.is_empty() {
        Some(Value::Null)
    } else {
        serde_json::from_slice::<Value>(&bytes).ok()
    };
    if let Some(body_value) = cacheable_body {
        state.idempotency.put(
            &tenant,
            &scoped_key,
            json!({ "status": parts.status.as_u16(), "body": body_value }),
        );
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Reconstruct a cached `{status, body}` entry into the response a fresh call to the
/// same route would have produced.
fn replay_cached(cached: Value) -> Response {
    let status = cached
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::OK);
    match cached.get("body") {
        Some(Value::Null) | None => status.into_response(),
        Some(body) => (status, Json(body.clone())).into_response(),
    }
}

// --- optimistic concurrency (overview §10) ---------------------------------------

/// The `ETag` header value for a resource version (a quoted version number).
pub(crate) fn etag(version: u64) -> String {
    format!("\"{version}\"")
}

/// The version requested by an `If-Match` header, if present and parseable. Tolerates
/// quotes and the weak-validator `W/` prefix (`If-Match: "3"` / `W/"3"` / `3`).
pub(crate) fn if_match(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().trim_start_matches("W/").trim_matches('"'))
        .and_then(|s| s.parse().ok())
}

// --- request id (overview §14) ---------------------------------------------------

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Middleware that gives every response a request id: it honors an incoming
/// `X-Request-Id` (client correlation) or generates `req_<n>`, stamps the response
/// header, and — for JSON **error** envelopes — fills in `error.request_id` (§8/§14).
pub(crate) async fn request_id(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("req_{}", REQUEST_COUNTER.fetch_add(1, Ordering::SeqCst)));

    let response = next.run(request).await;
    stamp(response, &id).await
}

/// Set the `X-Request-Id` header, and inject `request_id` into a JSON error body.
async fn stamp(response: Response, id: &str) -> Response {
    let is_error = response.status().is_client_error() || response.status().is_server_error();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"));

    // Only error JSON needs body rewriting; buffer it (small) to add `request_id`.
    if is_error && is_json {
        let (mut parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap_or_default();
        let new_body = match serde_json::from_slice::<Value>(&bytes) {
            Ok(mut v) => {
                if let Some(err) = v.get_mut("error").and_then(Value::as_object_mut) {
                    err.insert("request_id".into(), json!(id));
                }
                serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec())
            }
            Err(_) => bytes.to_vec(),
        };
        parts.headers.remove(header::CONTENT_LENGTH); // recomputed for the new body
        set_request_id(&mut parts.headers, id);
        return Response::from_parts(parts, Body::from(new_body));
    }

    let mut response = response;
    set_request_id(response.headers_mut(), id);
    response
}

fn set_request_id(headers: &mut HeaderMap, id: &str) {
    if let Ok(value) = id.parse() {
        headers.insert("x-request-id", value);
    }
}

// --- deprecation headers (docs/09-api/deprecation-policy.md §4, RM-GA-P4 API-705) -

/// How a [`Deprecation`] entry matches a request path. `Prefix` covers a
/// path-templated route (e.g. `/api/v1/agents/`, matching `/api/v1/agents/{id}`
/// and any sub-resource under it) without wiring up axum's `MatchedPath` —
/// unavailable here since this middleware is applied via `Router::layer`,
/// which runs *before* route matching resolves it. Simple and sufficient for
/// every route this API actually has; a future deprecation needing finer
/// matching can extend this enum then. Both variants are currently only
/// constructed by tests — `DEPRECATIONS` (below) is empty in production until
/// a real deprecation is announced, which is the intended, dormant-until-needed
/// state, not dead code to delete.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum PathPattern {
    Exact(&'static str),
    Prefix(&'static str),
}

impl PathPattern {
    fn matches(&self, path: &str) -> bool {
        match self {
            PathPattern::Exact(p) => path == *p,
            PathPattern::Prefix(p) => path.starts_with(p),
        }
    }
}

/// One documented deprecation (deprecation-policy.md §4): the route it
/// applies to, when it was announced, and the date after which the old
/// behavior may be removed. `sunset` must be at least 90 days after
/// `deprecated_since` — checked by `deprecation_table_windows_are_valid`
/// below on every test run, not enforced at request time (the table is a
/// fixed `const`, so a too-short window is a review-time logic error, not
/// something to fail closed on per-request).
pub(crate) struct Deprecation {
    pub(crate) method: Method,
    pub(crate) path: PathPattern,
    /// `(year, month, day)` the deprecation was announced. Read only by
    /// `deprecation_table_windows_are_valid` (a test) — never at request
    /// time, since the response only ever needs `sunset`.
    #[allow(dead_code)]
    pub(crate) deprecated_since: (i64, u32, u32),
    /// `(year, month, day)` after which the old behavior may be removed.
    pub(crate) sunset: (i64, u32, u32),
}

/// The live deprecation table. **Empty today** — per
/// `docs/09-api/deprecation-policy.md` §7, nothing in `/api/v1` has been
/// deprecated yet. Add an entry here (and update that doc's "Current State"
/// section) the day a real deprecation is announced; `deprecation_headers`
/// picks it up automatically, with no other code change needed.
pub(crate) const DEPRECATIONS: &[Deprecation] = &[];

/// The `(Deprecation: true, Sunset: <date>)` header values for the first table
/// entry matching `method`/`path`, if any. Pulled out of the middleware below
/// so it's unit-testable against a synthetic table without a live request.
fn deprecation_for(table: &[Deprecation], method: &Method, path: &str) -> Option<String> {
    table
        .iter()
        .find(|d| d.method == *method && d.path.matches(path))
        .map(|d| http_date(d.sunset))
}

/// Emits `Deprecation: true` and `Sunset: <RFC 7231 date>` ([RFC
/// 8594](https://www.rfc-editor.org/rfc/rfc8594)) on any response whose
/// request matches an entry in `table` — the mechanical enforcement
/// `deprecation-policy.md` §4 describes. A no-op today: `DEPRECATIONS` is
/// empty, so this never fires until a real deprecation is added to it.
pub(crate) async fn deprecation_headers(
    State(table): State<&'static [Deprecation]>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let sunset = deprecation_for(table, &method, &path);
    let response = next.run(request).await;
    match sunset {
        Some(sunset_http_date) => stamp_deprecation(response, &sunset_http_date),
        None => response,
    }
}

fn stamp_deprecation(mut response: Response, sunset_http_date: &str) -> Response {
    let headers = response.headers_mut();
    headers.insert("deprecation", HeaderValue::from_static("true"));
    if let Ok(value) = HeaderValue::from_str(sunset_http_date) {
        headers.insert("sunset", value);
    }
    response
}

/// Days since 1970-01-01 for a civil (proleptic Gregorian) date — Howard
/// Hinnant's `days_from_civil` algorithm, the encode-direction mirror of
/// `apex-workflow`'s `cron.rs::civil_from_days` (same no-dependency
/// house style; the two crates don't share code since this is a handful of
/// lines and pulling in a cross-crate dependency for it isn't worth it).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64) + if m > 2 { -3 } else { 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

const DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// An RFC 7231 `IMF-fixdate` for `(y, m, d)` at midnight UTC — the format
/// RFC 8594's `Sunset` header requires (e.g. `"Wed, 07 Oct 2026 00:00:00 GMT"`).
fn http_date((y, m, d): (i64, u32, u32)) -> String {
    let days = days_from_civil(y, m, d);
    // 1970-01-01 (day 0) was a Thursday; `rem_euclid` keeps this correct for
    // any pre-1970 date too, even though every real table entry postdates it.
    let weekday = DAY_NAMES[(days + 4).rem_euclid(7) as usize];
    let month = MONTH_NAMES[(m - 1) as usize];
    format!("{weekday}, {d:02} {month} {y:04} 00:00:00 GMT")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<Value> {
        (0..n).map(|i| json!({ "i": i })).collect()
    }

    #[test]
    fn cursor_round_trips() {
        let c = encode_cursor(50);
        assert_eq!(decode_cursor(&c), Some(50));
        assert_eq!(decode_cursor("not-hex!"), None);
    }

    // --- RM-GA-P4 API-703: idempotency replay eligibility -----------------------------

    #[test]
    fn get_and_head_are_never_replay_eligible() {
        assert!(!is_replay_eligible(&Method::GET, "/api/v1/organizations"));
        assert!(!is_replay_eligible(&Method::HEAD, "/api/v1/organizations"));
    }

    #[test]
    fn mutating_methods_are_replay_eligible_except_query_and_stream_shaped_routes() {
        assert!(is_replay_eligible(&Method::POST, "/api/v1/organizations"));
        assert!(is_replay_eligible(&Method::PUT, "/api/v1/projects/p1"));
        assert!(is_replay_eligible(&Method::PATCH, "/api/v1/projects/p1"));
        assert!(is_replay_eligible(&Method::DELETE, "/api/v1/webhooks/w1"));
        assert!(!is_replay_eligible(&Method::POST, "/api/v1/memory:query"));
        assert!(!is_replay_eligible(&Method::POST, "/api/v1/agents:stream"));
    }

    #[test]
    fn replay_cached_reconstructs_status_and_body() {
        let resp = replay_cached(json!({ "status": 201, "body": {"id": "org-1"} }));
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp = replay_cached(json!({ "status": 204, "body": Value::Null }));
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[test]
    fn limit_clamps_to_bounds() {
        assert_eq!(
            PageQuery {
                limit: None,
                cursor: None
            }
            .page()
            .limit,
            DEFAULT_LIMIT
        );
        assert_eq!(
            PageQuery {
                limit: Some(0),
                cursor: None
            }
            .page()
            .limit,
            1
        );
        assert_eq!(
            PageQuery {
                limit: Some(999),
                cursor: None
            }
            .page()
            .limit,
            MAX_LIMIT
        );
    }

    #[test]
    fn paginate_pages_and_chains_cursor() {
        // First page of 2 from 5 items → has_more, a next_cursor.
        let q = PageQuery {
            limit: Some(2),
            cursor: None,
        };
        let page1 = paginate(items(5), &q.page());
        assert_eq!(page1["data"].as_array().unwrap().len(), 2);
        assert_eq!(page1["has_more"], true);
        assert_eq!(page1["total_estimate"], 5);

        // Follow the cursor to the next page.
        let cursor = page1["next_cursor"].as_str().unwrap().to_string();
        let q2 = PageQuery {
            limit: Some(2),
            cursor: Some(cursor),
        };
        let page2 = paginate(items(5), &q2.page());
        assert_eq!(page2["data"][0]["i"], 2);

        // Last page → no more.
        let q3 = PageQuery {
            limit: Some(10),
            cursor: None,
        };
        let last = paginate(items(5), &q3.page());
        assert_eq!(last["has_more"], false);
        assert_eq!(last["next_cursor"], Value::Null);
    }

    #[test]
    fn if_match_parses_quoted_and_weak_validators() {
        let mut h = HeaderMap::new();
        assert_eq!(if_match(&h), None);
        h.insert(header::IF_MATCH, "\"3\"".parse().unwrap());
        assert_eq!(if_match(&h), Some(3));
        h.insert(header::IF_MATCH, "W/\"7\"".parse().unwrap());
        assert_eq!(if_match(&h), Some(7));
        h.insert(header::IF_MATCH, "5".parse().unwrap());
        assert_eq!(if_match(&h), Some(5));
        assert_eq!(etag(42), "\"42\"");
    }

    #[test]
    fn idempotency_store_is_tenant_scoped() {
        let store = IdempotencyStore::default();
        store.put("acme", "k1", json!({"run":"a"}));
        assert_eq!(store.get("acme", "k1"), Some(json!({"run":"a"})));
        // Same key, different tenant → independent.
        assert_eq!(store.get("other", "k1"), None);
        assert_eq!(store.get("acme", "k2"), None);
    }

    // --- RM-GA-P1 SEC-205: bounded + TTL-evicted --------------------------------------

    #[test]
    fn entries_expire_after_the_configured_ttl() {
        let store = IdempotencyStore::new(Duration::from_millis(10), 100);
        store.put("acme", "k1", json!({"run": "a"}));
        assert_eq!(store.get("acme", "k1"), Some(json!({"run": "a"})));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            store.get("acme", "k1"),
            None,
            "expired entry must not be returned"
        );
    }

    #[test]
    fn total_entries_are_capped_evicting_the_oldest_first() {
        let store = IdempotencyStore::new(Duration::from_secs(3600), 2);
        store.put("acme", "k1", json!(1));
        store.put("acme", "k2", json!(2));
        // A third distinct key exceeds the cap — the oldest (k1) is evicted to admit
        // it, not the just-inserted k2.
        store.put("acme", "k3", json!(3));
        assert_eq!(
            store.get("acme", "k1"),
            None,
            "oldest entry should be evicted"
        );
        assert_eq!(store.get("acme", "k2"), Some(json!(2)));
        assert_eq!(store.get("acme", "k3"), Some(json!(3)));
    }

    /// A soak test with unique keys shows bounded memory (SEC-205 acceptance
    /// criterion): far more distinct keys than `max_entries` are inserted, and the
    /// store never grows past that cap.
    #[test]
    fn soak_with_unique_keys_stays_within_the_entry_cap() {
        let max_entries = 50;
        let store = IdempotencyStore::new(Duration::from_secs(3600), max_entries);
        for i in 0..(max_entries * 20) {
            store.put("acme", &format!("k{i}"), json!(i));
        }
        let inner = store.inner.lock().unwrap();
        assert!(inner.entries.len() <= max_entries);
        assert!(inner.order.len() <= max_entries);
    }

    // --- RM-GA-P4 API-705: deprecation headers ---------------------------------------

    #[test]
    fn days_from_civil_matches_known_reference_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        // A 90-day gap should be exactly 90 days apart, incl. crossing a
        // month/quarter boundary (2026-07-08 -> 2026-10-06).
        assert_eq!(
            days_from_civil(2026, 10, 6) - days_from_civil(2026, 7, 8),
            90
        );
    }

    #[test]
    fn http_date_formats_known_dates_per_rfc_7231() {
        // 1970-01-01 was a Thursday.
        assert_eq!(http_date((1970, 1, 1)), "Thu, 01 Jan 1970 00:00:00 GMT");
        // 2026-10-06 is a Tuesday.
        assert_eq!(http_date((2026, 10, 6)), "Tue, 06 Oct 2026 00:00:00 GMT");
    }

    /// Standing regression guard (API-705's own acceptance criterion): every
    /// real table entry — whenever one exists — must give clients at least
    /// the 90-day window `deprecation-policy.md` §4 promises. Vacuously true
    /// today since `DEPRECATIONS` is empty.
    #[test]
    fn deprecation_table_windows_are_valid() {
        for d in DEPRECATIONS {
            let since = days_from_civil(
                d.deprecated_since.0,
                d.deprecated_since.1,
                d.deprecated_since.2,
            );
            let sunset = days_from_civil(d.sunset.0, d.sunset.1, d.sunset.2);
            assert!(
                sunset - since >= 90,
                "deprecation window for a route is shorter than the 90-day policy minimum"
            );
        }
    }

    #[test]
    fn deprecation_for_matches_exact_and_prefix_but_not_other_routes() {
        const TABLE: &[Deprecation] = &[
            Deprecation {
                method: Method::GET,
                path: PathPattern::Exact("/api/v1/old-thing"),
                deprecated_since: (2026, 1, 1),
                sunset: (2026, 4, 1),
            },
            Deprecation {
                method: Method::DELETE,
                path: PathPattern::Prefix("/api/v1/legacy/"),
                deprecated_since: (2026, 1, 1),
                sunset: (2026, 4, 1),
            },
        ];

        assert!(deprecation_for(TABLE, &Method::GET, "/api/v1/old-thing").is_some());
        // Wrong method on an otherwise-matching exact path: no match.
        assert!(deprecation_for(TABLE, &Method::POST, "/api/v1/old-thing").is_none());
        // Prefix covers any concrete path under it (e.g. a path-templated route).
        assert!(deprecation_for(TABLE, &Method::DELETE, "/api/v1/legacy/123").is_some());
        // An unrelated route never matches.
        assert!(deprecation_for(TABLE, &Method::GET, "/api/v1/unrelated").is_none());
    }

    #[tokio::test]
    async fn deprecated_route_carries_headers_end_to_end() {
        use axum::{Router, body::Body, routing::get};
        use tower::ServiceExt;

        const TABLE: &[Deprecation] = &[Deprecation {
            method: Method::GET,
            path: PathPattern::Exact("/api/v1/example"),
            deprecated_since: (2026, 1, 1),
            sunset: (2026, 4, 1),
        }];

        let app = Router::new()
            .route("/api/v1/example", get(|| async { "ok" }))
            .route("/api/v1/other", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                TABLE,
                deprecation_headers,
            ));

        let deprecated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deprecated.headers().get("deprecation").unwrap(), "true");
        assert_eq!(
            deprecated.headers().get("sunset").unwrap(),
            "Wed, 01 Apr 2026 00:00:00 GMT"
        );

        let unaffected = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/other")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(unaffected.headers().get("deprecation").is_none());
        assert!(unaffected.headers().get("sunset").is_none());
    }
}
