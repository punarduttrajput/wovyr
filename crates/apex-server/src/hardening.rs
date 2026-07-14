//! Cross-cutting API conventions ([API overview](../../docs/09-api/overview.md)):
//! cursor **pagination** (§6), **idempotency keys** (§9), and **request-id**
//! propagation (§14). These harden the `/v1` surface so list endpoints page
//! consistently, client retries are safe, and every response is traceable.

use apex_telemetry::Metrics;
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
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::Instrument;

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
///
/// `pub(crate)` (not just an implementation detail of [`paginate`]) so a route whose
/// pagination isn't offset-based — [`crate::audit`]'s SEC-301 seq-cursor — can reuse
/// the exact same opaque wire encoding instead of inventing a second cursor format;
/// the number it wraps means something different per route, but the format looks
/// identical across every `/v1` list endpoint, matching this being documented as
/// opaque rather than route-specific.
pub(crate) fn encode_cursor(offset: usize) -> String {
    offset
        .to_string()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn decode_cursor(cursor: &str) -> Option<usize> {
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
    /// Lines currently in the on-disk log (SRV-305) — always `>= entries.len()`,
    /// since a `put` appends a line unconditionally but a key can only be
    /// overwritten by a *fresh* `put` after its old cached response already
    /// expired (the normal idempotency flow never re-`put`s a live key: a retry
    /// within the TTL hits `get` and short-circuits before `put` runs again).
    /// Reset to exactly `entries.len()` whenever [`IdempotencyStore::put`] compacts.
    log_lines: u64,
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
///
/// **On-disk shape is an append-only JSON-lines log, not a rewritten snapshot**
/// (RM-AIM-P3 SRV-305): the original design re-serialized and `atomic_write`-rewrote
/// *every* live entry on *every* `put`, so one mutating request paid O(entries) disk
/// I/O — real write amplification once the store approached `max_entries`. `put` now
/// appends exactly one line (the same fsync-then-parent-dir-fsync durability
/// `atomic_write` gives a whole-file rewrite, via [`apex_common::fs::sync_parent_dir`]
/// — the standalone primitive this workspace already exposes for append-only logs
/// that don't rename anything, the same one `apex-workflow`'s event log and
/// `apex-audit`'s hash chain use for this exact reason). The log can accumulate more
/// lines than live entries (an expired/evicted key's old line is never retroactively
/// deleted), so [`Self::put`] compacts — a single full rewrite via `atomic_write`,
/// collapsing back to exactly the current live entries — once the log has grown to
/// `max_entries * 2` (never on every call, so the amortized cost per `put` stays O(1)).
pub(crate) struct IdempotencyStore {
    inner: Mutex<IdempotencyInner>,
    ttl: Duration,
    max_entries: usize,
    path: Option<PathBuf>,
    /// Test-observability only (mirrors `FileApiKeyStore::load_count`, SRV-302): counts
    /// real appends vs. full-log compactions, so a test can assert `put` amortizes to
    /// O(1) I/O instead of rewriting the whole log every call. Negligible runtime cost
    /// (one atomic increment per `put` that has a `path`, never per in-memory-only call).
    append_count: std::sync::atomic::AtomicUsize,
    compact_count: std::sync::atomic::AtomicUsize,
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
    /// persisted entries from `path`'s append-only JSON-lines log (best-effort: a
    /// missing file starts empty rather than failing server startup, and a truncated
    /// trailing line — a crash mid-append — is skipped rather than discarding every
    /// entry parsed before it). `path: None` behaves exactly like [`new`](Self::new).
    pub(crate) fn new_with_path(ttl: Duration, max_entries: usize, path: Option<PathBuf>) -> Self {
        let inner = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|contents| {
                let mut inner = IdempotencyInner::default();
                for line in contents.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(r) = serde_json::from_str::<PersistedEntry>(line) else {
                        continue;
                    };
                    inner.log_lines += 1;
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
            append_count: std::sync::atomic::AtomicUsize::new(0),
            compact_count: std::sync::atomic::AtomicUsize::new(0),
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
    /// (RM-GA-P2 DUR-404) if opened with a path — appending one log line in the
    /// common case, or compacting (SRV-305) once the log has grown past
    /// `max_entries * 2`.
    pub(crate) fn put(&self, tenant: &str, key: &str, body: Value) {
        enum Persist {
            Append(PersistedEntry),
            Compact(Vec<PersistedEntry>),
        }

        let persist = {
            let mut inner = self.inner.lock().expect("idempotency mutex poisoned");
            Self::evict(&mut inner, self.ttl, self.max_entries, true);
            let scoped_key = scoped(tenant, key);
            let inserted_at_ms = now_ms();
            inner.entries.insert(
                scoped_key.clone(),
                IdempotencyEntry {
                    body: body.clone(),
                    inserted_at_ms,
                },
            );
            inner.order.push_back(scoped_key.clone());
            inner.log_lines += 1;

            self.path.as_ref().map(|_| {
                // Never on every call — that would reintroduce the O(entries) cost
                // this design exists to avoid — only once the log has grown to
                // roughly twice the live entry count.
                if inner.log_lines >= (self.max_entries as u64).saturating_mul(2) {
                    let records: Vec<PersistedEntry> = inner
                        .order
                        .iter()
                        .filter_map(|k| {
                            inner.entries.get(k).map(|e| PersistedEntry {
                                key: k.clone(),
                                body: e.body.clone(),
                                inserted_at_ms: e.inserted_at_ms,
                            })
                        })
                        .collect();
                    inner.log_lines = records.len() as u64;
                    Persist::Compact(records)
                } else {
                    Persist::Append(PersistedEntry {
                        key: scoped_key,
                        body,
                        inserted_at_ms,
                    })
                }
            })
        };

        let (Some(path), Some(persist)) = (&self.path, persist) else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(error = %e, "failed to create idempotency store directory");
            return;
        }
        match persist {
            Persist::Append(record) => {
                self.append_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Err(e) = Self::append_line(path, &record) {
                    tracing::error!(error = %e, "failed to append to idempotency store");
                }
            }
            Persist::Compact(records) => {
                self.compact_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Err(e) = Self::write_snapshot(path, &records) {
                    tracing::error!(error = %e, "failed to compact idempotency store");
                }
            }
        }
    }

    /// Append one JSON-encoded line to the log, `fsync`ing the file then its parent
    /// directory — the same durability an `atomic_write` gives a whole-file rewrite,
    /// without paying for one.
    fn append_line(path: &PathBuf, record: &PersistedEntry) -> std::io::Result<()> {
        use std::io::Write;
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(line.as_bytes())?;
        file.sync_data()?;
        drop(file);
        apex_common::fs::sync_parent_dir(path)
    }

    /// Rewrite the log to contain exactly `records`, one per line — collapsing every
    /// prior append (including stale/superseded ones) back down to the live set.
    fn write_snapshot(path: &PathBuf, records: &[PersistedEntry]) -> std::io::Result<()> {
        let mut out = String::new();
        for r in records {
            out.push_str(&serde_json::to_string(r)?);
            out.push('\n');
        }
        apex_common::fs::atomic_write(path, out)
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
///
/// **Correlation into logs, traces, and audit (RM-GA-P4 OBS-802):** the id used to
/// exist only in this function's local scope — never reachable by a handler, a log
/// line, or an audit call site, even though `AuditEvent.request_id` and every
/// downstream `HeaderMap`-taking handler could have used it. Two fixes, both here:
/// (1) the resolved id is written back onto the *request's* `x-request-id` header
/// (not just the response's) before `next.run`, so any handler already extracting
/// `headers: HeaderMap` can read it back via [`request_id_of`] — including the
/// `kms.rs`/`secrets.rs`/OBS-804 audit call sites, with zero new extractor plumbing.
/// (2) `next.run` is wrapped in an `http.request` span carrying the id as a field,
/// so it appears on every log line and OTLP trace produced while handling this
/// request (and on the two existing `#[tracing::instrument]`-annotated handler
/// spans, which nest as children) — simpler than annotating every handler with its
/// own `fields(request_id = Empty)` + a manual `.record()` call.
pub(crate) async fn request_id(mut request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("req_{}", REQUEST_COUNTER.fetch_add(1, Ordering::SeqCst)));

    if let Ok(value) = id.parse() {
        request.headers_mut().insert("x-request-id", value);
    }

    let span = tracing::info_span!("http.request", request_id = %id);
    let response = next.run(request).instrument(span).await;
    stamp(response, &id).await
}

/// Read the request id a request carries — the client-supplied one, or the one
/// [`request_id`] generated and wrote back onto the request headers, so it's always
/// present downstream. For a handler wanting to correlate an audit entry (or a
/// manual log line) with the id already on the eventual response.
pub(crate) fn request_id_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
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

// --- bounded per-tenant/per-project metric labels (RM-AIM-P2 OBS-201) ------------

/// Distinct tenant/project label values a [`TenantLabelCap`] keeps their real name
/// before folding overflow into `"other"`. Generous enough that a real deployment's
/// whole tenant/project roster fits without collapsing, while still bounding what a
/// caller flooding `X-Apex-Tenant`/`X-Apex-Project` with arbitrary values — this
/// layer runs *before* auth verifies those headers, see [`track_metrics`] — can do to
/// the metrics registry's series count.
const MAX_TENANT_LABELS: usize = 200;

/// Bounds tenant/project metric-label cardinality: the first [`MAX_TENANT_LABELS`]
/// distinct identifiers seen through a given instance keep their real name as a label
/// value; anything after that folds into `"other"` — the same fallback shape
/// [`route_label`] uses for a path outside [`ROUTE_LABELS`], just data-driven instead
/// of a fixed table, since tenants/projects aren't known ahead of time. One instance
/// lives on [`AppState`](crate::AppState) and is shared between the RED-metric
/// middleware ([`track_metrics`]) and the LLM usage-metric call sites
/// (`record_llm_usage_metrics`) so both agree on which tenants/projects are "known" —
/// a value could otherwise read as exact in one metric and `"other"` in the other
/// depending purely on which happened to observe it first.
#[derive(Clone, Default)]
pub(crate) struct TenantLabelCap {
    seen: Arc<Mutex<HashSet<String>>>,
}

impl TenantLabelCap {
    /// The bounded label for `value` (a tenant or project id). An empty string — e.g.
    /// an unset `X-Apex-Project` — is never bucketed; it renders as `"none"` so a
    /// caller can tell "not scoped to a project" apart from a real identifier that
    /// overflowed the cap.
    pub(crate) fn label(&self, value: &str) -> String {
        if value.is_empty() {
            return "none".to_string();
        }
        let mut seen = self.seen.lock().expect("tenant label cap mutex poisoned");
        if seen.contains(value) {
            return value.to_string();
        }
        if seen.len() < MAX_TENANT_LABELS {
            seen.insert(value.to_string());
            value.to_string()
        } else {
            "other".to_string()
        }
    }
}

/// The coarse `2xx`/`3xx`/`4xx`/`5xx` status class used by the per-tenant request
/// aggregate below — bounding a second dimension so `route × tenant` never has to be
/// multiplied out; only `status_class × tenant` is.
fn status_class(status: u16) -> &'static str {
    match status / 100 {
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

/// Per-tenant/per-project LLM cost + token visibility (RM-AIM-P2 OBS-201). The
/// existing `apex_llm_tokens_total`/`apex_llm_cost_usd_total`
/// ([`crate::config::MetricsCostObserver`]) are labeled by `model` only — they come
/// from a [`CostObserver`](apex_provider::CostObserver) attached once to the shared
/// `Gateway`, with no per-request context to label by. Rather than thread tenant
/// context down through the gateway's cost-event pipeline, this is called directly at
/// every site that already resolves a run's usage against a project's quota
/// (`agents.rs`'s three call sites, `workflow_runner.rs`'s `StoredAgentResolver::record`)
/// — the natural point where `tenant`/`project` and the run's [`apex_common::Usage`]
/// are already both in scope. Labels are bounded via the shared [`TenantLabelCap`], so
/// this and [`track_metrics`]'s per-tenant aggregate agree on which tenants/projects
/// are "known".
pub(crate) fn record_llm_usage_metrics(
    metrics: &Metrics,
    tenant_labels: &TenantLabelCap,
    tenant: &str,
    project: Option<&str>,
    cost_usd: f64,
    tokens: u64,
) {
    let tenant = tenant_labels.label(tenant);
    let project = tenant_labels.label(project.unwrap_or(""));
    metrics.counter_add(
        "apex_llm_cost_usd_by_tenant_total",
        &[("tenant", &tenant), ("project", &project)],
        cost_usd,
    );
    metrics.counter_add(
        "apex_llm_tokens_by_tenant_total",
        &[("tenant", &tenant), ("project", &project)],
        tokens as f64,
    );
}

// --- RED metrics for every route (RM-GA-P4 OBS-801) ------------------------------

/// One route's method + path template + the bounded label its RED metrics use.
/// `template` reuses this API's own axum route syntax (`{id}`) verbatim, so it can
/// be copied straight out of each router module's `routes()` fn.
struct RouteLabel {
    method: Method,
    template: &'static str,
    label: &'static str,
}

/// Does `path` match `template`, treating any `{...}` template segment as a
/// wildcard? Segment-by-segment rather than a single `Exact`/`Prefix` string (as
/// `PathPattern` above uses for deprecations) because most routes here have a path
/// parameter followed by more literal segments (`/api/v1/projects/{id}/quota`),
/// which a prefix match alone can't express.
fn path_matches_template(path: &str, template: &str) -> bool {
    let mut p = path.trim_matches('/').split('/');
    let mut t = template.trim_matches('/').split('/');
    loop {
        match (p.next(), t.next()) {
            (Some(ps), Some(ts)) => {
                let is_param = ts.starts_with('{') && ts.ends_with('}');
                if !is_param && ps != ts {
                    return false;
                }
            }
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// The bounded `route` label for `(method, path)` — `"unmatched"` for anything not
/// in the table (a 404 on a genuinely unknown path, or a route added to a router
/// module without a matching entry here). Hand-maintained for the same reason
/// `DEPRECATIONS` is: this middleware runs via `Router::layer` on the merged app,
/// before axum's `MatchedPath` resolves — see [`track_metrics`].
fn route_label(method: &Method, path: &str) -> &'static str {
    ROUTE_LABELS
        .iter()
        .find(|r| r.method == *method && path_matches_template(path, r.template))
        .map(|r| r.label)
        .unwrap_or("unmatched")
}

/// Every route this server actually mounts (`lib.rs::router` and each route
/// module's `routes()`), labeled for RED metrics. Keep in sync with the router —
/// `route_labels_cover_every_mounted_route` (in `lib.rs`'s test module, which can
/// see the real `router()`) fails if a mounted route has no entry here.
const ROUTE_LABELS: &[RouteLabel] = &[
    RouteLabel {
        method: Method::GET,
        template: "/healthz",
        label: "healthz",
    },
    RouteLabel {
        method: Method::GET,
        template: "/metrics",
        label: "metrics",
    },
    // agents:run / :stream / stored run (lib.rs `run_routes`)
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/agents:run",
        label: "agents_run",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/agents:stream",
        label: "agents_stream",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/agents/{id}/run",
        label: "agents_run_stored",
    },
    // secrets.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/secrets",
        label: "secrets_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/secrets",
        label: "secrets_create",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/secrets/{name}",
        label: "secrets_get",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/secrets/{name}",
        label: "secrets_delete",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/secrets/{name}/rotate",
        label: "secrets_rotate",
    },
    // kms.rs
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/kms/tenant-key/rotate",
        label: "kms_rotate",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/kms/tenant-key/destroy",
        label: "kms_destroy",
    },
    // agent persistence + workflow visibility (lib.rs `other_protected`)
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/agents",
        label: "agents_create",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/agents",
        label: "agents_list",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/agents/{id}",
        label: "agents_get",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/agents/{id}",
        label: "agents_delete",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/agents/runs/{run_id}",
        label: "agents_run_status",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/workflows",
        label: "workflows_list",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/workflows/{id}",
        label: "workflows_get",
    },
    RouteLabel {
        method: Method::GET,
        template: "/workflows",
        label: "workflows_ui",
    },
    // workflow_runner.rs
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/workflows/validate",
        label: "workflows_validate",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/workflows",
        label: "workflows_submit",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/workflows/{id}/signal",
        label: "workflows_signal",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/workflows/{id}/approve",
        label: "workflows_approve",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/workflows/{id}",
        label: "workflows_cancel",
    },
    // tenancy.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/organizations",
        label: "organizations_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/organizations",
        label: "organizations_create",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/projects",
        label: "projects_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/projects",
        label: "projects_create",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/projects/{id}",
        label: "projects_get",
    },
    RouteLabel {
        method: Method::PATCH,
        template: "/api/v1/projects/{id}",
        label: "projects_update",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/projects/{id}",
        label: "projects_delete",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/projects/{id}/members",
        label: "project_members_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/projects/{id}/members",
        label: "project_members_add",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/projects/{id}/members/{uid}",
        label: "project_members_remove",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/projects/{id}/quota",
        label: "project_quota_get",
    },
    RouteLabel {
        method: Method::PATCH,
        template: "/api/v1/projects/{id}/quota",
        label: "project_quota_update",
    },
    // webhooks.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/webhooks",
        label: "webhooks_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/webhooks",
        label: "webhooks_create",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/webhooks/{id}",
        label: "webhooks_delete",
    },
    // memory.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/memory/namespaces",
        label: "memory_namespaces",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/memory/records",
        label: "memory_records_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/memory/records",
        label: "memory_records_create",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/memory:query",
        label: "memory_query",
    },
    // plugins.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/plugins",
        label: "plugins_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/plugins:install",
        label: "plugins_install",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/plugins:enable",
        label: "plugins_enable",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/plugins:disable",
        label: "plugins_disable",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/plugins:upgrade",
        label: "plugins_upgrade",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/plugins:rollback",
        label: "plugins_rollback",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/plugins:trust",
        label: "plugins_trust",
    },
    RouteLabel {
        method: Method::DELETE,
        template: "/api/v1/plugins/{id}",
        label: "plugins_uninstall",
    },
    // marketplace.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/marketplace/listings",
        label: "marketplace_search",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace:publish",
        label: "marketplace_publish",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/marketplace/listings/{id}",
        label: "marketplace_get",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/marketplace/listings/{id}/download",
        label: "marketplace_download",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/marketplace/listings/{id}/attestation",
        label: "marketplace_attestation",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/reviews",
        label: "marketplace_review",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/verify",
        label: "marketplace_verify",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/request-review",
        label: "marketplace_request_review",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/approve",
        label: "marketplace_approve",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/reject",
        label: "marketplace_reject",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/install",
        label: "marketplace_install",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/report",
        label: "marketplace_report",
    },
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/marketplace/listings/{id}/reports",
        label: "marketplace_reports_list",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/reports/{report_id}/resolve",
        label: "marketplace_report_resolve",
    },
    RouteLabel {
        method: Method::POST,
        template: "/api/v1/marketplace/listings/{id}/reports/{report_id}/dismiss",
        label: "marketplace_report_dismiss",
    },
    // audit.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/audit",
        label: "audit_list",
    },
    // tools.rs
    RouteLabel {
        method: Method::GET,
        template: "/api/v1/tools",
        label: "tools_list",
    },
];

/// [`track_metrics`]'s middleware state: the registry plus the shared
/// [`TenantLabelCap`] (RM-AIM-P2 OBS-201) so the per-tenant aggregate it records
/// agrees with the LLM usage-metric call sites on which tenants are "known".
#[derive(Clone)]
pub(crate) struct MetricsState {
    pub(crate) metrics: Metrics,
    pub(crate) tenant_labels: TenantLabelCap,
}

/// RED metrics for every route, in one middleware layer. Previously only
/// `agents:run`/`agents/{id}/run` recorded `apex_api_requests_total`/
/// `apex_api_request_duration_seconds` — via two hand-rolled, near-duplicate
/// call sites inside `agents.rs`'s own handlers — leaving every other route group
/// (workflows, memory, marketplace, tenancy, secrets, plugins, webhooks, audit,
/// tools, KMS) with no request metrics at all.
///
/// Applied at the same outer, whole-app layer as [`request_id`]/
/// [`deprecation_headers`] rather than a per-router `route_layer` using axum's
/// `MatchedPath` — deliberately, so it also counts requests a handler never sees at
/// all (an auth `401`, a rate-limit `429`, an idempotency replay), which is exactly
/// the error-rate visibility RED metrics exist for. `MatchedPath` isn't resolved at
/// this position (the same constraint documented on [`PathPattern`]), so the route
/// label comes from the hand-maintained [`ROUTE_LABELS`] table instead.
///
/// **Per-tenant visibility (RM-AIM-P2 OBS-201):** `apex_api_requests_total`/
/// `_duration_seconds` stay labeled `route`/`method`/`status` only — adding a
/// per-tenant dimension there would multiply an already route×method×status series
/// count by the tenant count. Instead, a **separate**, deliberately low-cardinality
/// aggregate — `apex_api_requests_by_tenant_total{tenant, status_class}` — answers
/// the actual problem ("a noisy tenant is invisible"): traffic volume and coarse
/// error rate per tenant, bounded to `tenant(≤`[`MAX_TENANT_LABELS`]`+1) ×
/// status_class(5)` series regardless of route count. Tenant is read straight off
/// `X-Apex-Tenant` — unverified at this outer layer, same caveat as the tenant
/// rate-limit tier and the idempotency cache's tenant scoping.
pub(crate) async fn track_metrics(
    State(MetricsState {
        metrics,
        tenant_labels,
    }): State<MetricsState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let tenant = tenant_labels.label(&crate::tenancy::run_tenant(request.headers()));
    let start = Instant::now();
    let response = next.run(request).await;
    let label = route_label(&method, &path);
    let status_code = response.status().as_u16();
    let status = status_code.to_string();
    metrics.counter_inc(
        "apex_api_requests_total",
        &[
            ("route", label),
            ("method", method.as_str()),
            ("status", &status),
        ],
    );
    metrics.histogram_observe(
        "apex_api_request_duration_seconds",
        &[("route", label), ("method", method.as_str())],
        start.elapsed().as_secs_f64(),
    );
    metrics.counter_inc(
        "apex_api_requests_by_tenant_total",
        &[
            ("tenant", &tenant),
            ("status_class", status_class(status_code)),
        ],
    );
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

    // --- RM-AIM-P3 SRV-305: durable persistence is append-only, not a per-`put`
    // whole-file rewrite ---------------------------------------------------------------

    /// The literal SRV-305 acceptance criterion: a mutating request (`put`) doesn't
    /// rewrite the entire cache file each time. With `max_entries` large enough that
    /// no compaction triggers across this run, every one of N `put`s must append
    /// (O(1) I/O) rather than rewrite a growing O(N) snapshot.
    #[test]
    fn put_appends_instead_of_rewriting_the_whole_log_each_time() {
        let dir =
            std::env::temp_dir().join(format!("apex-idempotency-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("idempotency.jsonl");

        let store =
            IdempotencyStore::new_with_path(Duration::from_secs(3600), 1_000, Some(path.clone()));
        for i in 0..30 {
            store.put("acme", &format!("k{i}"), json!({ "n": i }));
        }

        assert_eq!(
            store
                .append_count
                .load(std::sync::atomic::Ordering::Relaxed),
            30,
            "every put should append a single line, not rewrite the log"
        );
        assert_eq!(
            store
                .compact_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no compaction should have triggered yet"
        );

        // The log round-trips: reopening a fresh store from the same path recovers
        // every entry, proving the append-only format is faithfully readable.
        let reopened =
            IdempotencyStore::new_with_path(Duration::from_secs(3600), 1_000, Some(path.clone()));
        for i in 0..30 {
            assert_eq!(
                reopened.get("acme", &format!("k{i}")),
                Some(json!({ "n": i }))
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Once the log has grown to roughly twice the live entry count, `put` compacts
    /// it back down to exactly the live entries in one rewrite — bounding disk usage
    /// without paying the O(entries) cost on every call.
    #[test]
    fn put_compacts_the_log_once_it_grows_past_the_threshold() {
        let dir =
            std::env::temp_dir().join(format!("apex-idempotency-compact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("idempotency.jsonl");

        // A small max_entries (4) means the compaction threshold (max_entries * 2 = 8)
        // is reached quickly; a short TTL keeps eviction from also shrinking the log.
        let store =
            IdempotencyStore::new_with_path(Duration::from_secs(3600), 4, Some(path.clone()));
        for i in 0..8 {
            store.put("acme", &format!("k{i}"), json!(i));
        }
        assert_eq!(
            store
                .compact_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the 8th put should have compacted the log"
        );

        // The on-disk log now holds only the live (post-eviction) entries, not every
        // line ever appended.
        let contents = std::fs::read_to_string(&path).unwrap();
        let live_lines = contents.lines().filter(|l| !l.trim().is_empty()).count();
        let inner = store.inner.lock().unwrap();
        assert_eq!(live_lines, inner.entries.len());

        let _ = std::fs::remove_dir_all(&dir);
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

    // --- RM-GA-P4 OBS-801: RED metrics for every route ---------------------------

    #[test]
    fn path_matches_template_handles_literal_and_param_segments() {
        assert!(path_matches_template("/healthz", "/healthz"));
        assert!(path_matches_template(
            "/api/v1/projects/p1/quota",
            "/api/v1/projects/{id}/quota"
        ));
        // A param segment is a wildcard, but surrounding literals must still match.
        assert!(!path_matches_template(
            "/api/v1/projects/p1/members",
            "/api/v1/projects/{id}/quota"
        ));
        // Different segment counts never match, even with a shared prefix.
        assert!(!path_matches_template(
            "/api/v1/projects/p1/quota/extra",
            "/api/v1/projects/{id}/quota"
        ));
        assert!(!path_matches_template(
            "/api/v1/projects",
            "/api/v1/projects/{id}"
        ));
    }

    #[test]
    fn route_label_resolves_known_routes_and_falls_back_for_unknown_ones() {
        assert_eq!(route_label(&Method::GET, "/healthz"), "healthz");
        assert_eq!(
            route_label(&Method::POST, "/api/v1/agents:run"),
            "agents_run"
        );
        assert_eq!(
            route_label(&Method::POST, "/api/v1/agents/abc123/run"),
            "agents_run_stored"
        );
        assert_eq!(
            route_label(&Method::PATCH, "/api/v1/projects/p1/quota"),
            "project_quota_update"
        );
        assert_eq!(
            route_label(
                &Method::POST,
                "/api/v1/marketplace/listings/l1/reports/r1/resolve"
            ),
            "marketplace_report_resolve"
        );
        // Right path, wrong method: falls back rather than mismatching to a
        // same-path different-method entry.
        assert_eq!(
            route_label(&Method::DELETE, "/api/v1/agents:run"),
            "unmatched"
        );
        assert_eq!(
            route_label(&Method::GET, "/api/v1/nonexistent"),
            "unmatched"
        );
    }

    #[tokio::test]
    async fn track_metrics_records_a_normal_response_and_falls_back_for_an_unknown_path() {
        use axum::{Router, body::Body, http::StatusCode as AxumStatusCode, routing::get};
        use tower::ServiceExt;

        let metrics = Metrics::new();
        let state = MetricsState {
            metrics: metrics.clone(),
            tenant_labels: TenantLabelCap::default(),
        };
        let build = || {
            Router::new()
                .route("/healthz", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    track_metrics,
                ))
        };

        let ok = build()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), AxumStatusCode::OK);

        let out = metrics.render_prometheus();
        assert!(
            out.contains(r#"apex_api_requests_total{method="GET",route="healthz",status="200"} 1"#),
            "got:\n{out}"
        );
        assert!(out.contains(
            "apex_api_request_duration_seconds_count{method=\"GET\",route=\"healthz\"} 1"
        ));
        // The new low-cardinality per-tenant aggregate (OBS-201): no `X-Apex-Tenant`
        // header on this request, so it's labeled the default tenant.
        assert!(
            out.contains(
                r#"apex_api_requests_by_tenant_total{status_class="2xx",tenant="default"} 1"#
            ),
            "got:\n{out}"
        );

        // A path not in `ROUTE_LABELS` (here, unregistered on this throwaway router
        // too, so axum itself 404s it) still gets counted — under "unmatched" rather
        // than being silently dropped, so a genuine mismatch between this table and
        // the real router is visible in `/metrics` instead of invisible.
        let notfound = build()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(notfound.status(), AxumStatusCode::NOT_FOUND);
        let out = metrics.render_prometheus();
        assert!(
            out.contains(
                r#"apex_api_requests_total{method="GET",route="unmatched",status="404"} 1"#
            ),
            "got:\n{out}"
        );
        // A distinct status class is a distinct series from the earlier 2xx request,
        // even though the tenant label is the same "default".
        assert!(
            out.contains(
                r#"apex_api_requests_by_tenant_total{status_class="4xx",tenant="default"} 1"#
            ),
            "got:\n{out}"
        );
    }

    // --- RM-AIM-P2 OBS-201: bounded per-tenant/per-project metric labels ----------

    #[tokio::test]
    async fn track_metrics_labels_the_per_tenant_aggregate_from_the_request_header() {
        use axum::{Router, body::Body, routing::get};
        use tower::ServiceExt;

        let metrics = Metrics::new();
        let state = MetricsState {
            metrics: metrics.clone(),
            tenant_labels: TenantLabelCap::default(),
        };
        let router = Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(state, track_metrics));

        router
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .header("x-apex-tenant", "acme")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let out = metrics.render_prometheus();
        assert!(
            out.contains(
                r#"apex_api_requests_by_tenant_total{status_class="2xx",tenant="acme"} 1"#
            ),
            "got:\n{out}"
        );
        // The default-tenant series from an earlier request in this test binary must
        // not exist here — this router has its own fresh `Metrics`.
        assert!(!out.contains(r#"tenant="default""#), "got:\n{out}");
    }

    #[test]
    fn tenant_label_cap_bounds_cardinality_by_folding_overflow_into_other() {
        let cap = TenantLabelCap::default();
        // The first MAX_TENANT_LABELS distinct values keep their real name.
        for i in 0..MAX_TENANT_LABELS {
            let tenant = format!("tenant-{i}");
            assert_eq!(cap.label(&tenant), tenant);
        }
        // Every one of those is still resolved by its real name — capping affects only
        // *new* values, not ones already tracked.
        assert_eq!(cap.label("tenant-0"), "tenant-0");
        // The cap+1'th distinct value folds into "other" instead of growing the set
        // further, bounding cardinality regardless of how many more distinct values
        // a caller throws at it.
        assert_eq!(cap.label("tenant-overflow-1"), "other");
        assert_eq!(cap.label("tenant-overflow-2"), "other");
        // An empty value (no header set) is never bucketed as "other" — it has its own
        // stable "none" label so it's distinguishable from a real overflowed tenant.
        assert_eq!(cap.label(""), "none");
    }

    #[test]
    fn record_llm_usage_metrics_labels_cost_and_tokens_by_tenant_and_project() {
        let metrics = Metrics::new();
        let cap = TenantLabelCap::default();
        record_llm_usage_metrics(&metrics, &cap, "acme", Some("prj-1"), 0.5, 1200);
        // A second call with no project (e.g. an unscoped direct agent run) is labeled
        // "none" for project rather than colliding with a real project id.
        record_llm_usage_metrics(&metrics, &cap, "acme", None, 0.25, 300);

        let out = metrics.render_prometheus();
        assert!(
            out.contains(r#"apex_llm_cost_usd_by_tenant_total{project="prj-1",tenant="acme"} 0.5"#),
            "got:\n{out}"
        );
        assert!(
            out.contains(r#"apex_llm_tokens_by_tenant_total{project="prj-1",tenant="acme"} 1200"#),
            "got:\n{out}"
        );
        assert!(
            out.contains(r#"apex_llm_cost_usd_by_tenant_total{project="none",tenant="acme"} 0.25"#),
            "got:\n{out}"
        );
    }

    #[test]
    fn record_llm_usage_metrics_shares_the_cap_with_track_metrics_bounding() {
        // The same `TenantLabelCap` instance is what `AppState` shares between the RED
        // middleware and this call site — proving that sharing actually keeps them in
        // agreement: filling the cap via one, then asking the other for a fresh value,
        // must yield the same "other" fallback rather than each independently deciding
        // a value is "known".
        let cap = TenantLabelCap::default();
        for i in 0..MAX_TENANT_LABELS {
            cap.label(&format!("tenant-{i}"));
        }
        let metrics = Metrics::new();
        record_llm_usage_metrics(&metrics, &cap, "brand-new-tenant", None, 1.0, 10);
        let out = metrics.render_prometheus();
        assert!(
            out.contains(r#"apex_llm_cost_usd_by_tenant_total{project="none",tenant="other"} 1"#),
            "got:\n{out}"
        );
    }

    // --- RM-GA-P4 OBS-802: request id reaches a handler's headers -----------------

    #[test]
    fn request_id_of_reads_the_header_when_present() {
        let mut headers = HeaderMap::new();
        assert_eq!(request_id_of(&headers), None);
        headers.insert("x-request-id", "req-xyz".parse().unwrap());
        assert_eq!(request_id_of(&headers), Some("req-xyz".to_string()));
    }

    #[tokio::test]
    async fn request_id_middleware_writes_the_id_back_onto_the_request_for_handlers() {
        use axum::{Router, body::Body, routing::get};
        use tower::ServiceExt;

        // A handler that echoes back whatever `request_id_of` sees — proving the id
        // is readable from *inside* the handler, not just on the eventual response
        // (which `hardening::request_id`'s own pre-existing behavior already covered).
        async fn echo_request_id(headers: HeaderMap) -> String {
            request_id_of(&headers).unwrap_or_default()
        }

        let app = Router::new()
            .route("/echo", get(echo_request_id))
            .layer(axum::middleware::from_fn(request_id));

        // Client-supplied id is forwarded to the handler unchanged.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/echo")
                    .header("x-request-id", "req-supplied")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(bytes, "req-supplied".as_bytes());

        // A server-generated id (client sent none) is just as visible to the handler.
        let resp = app
            .oneshot(Request::builder().uri("/echo").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let seen = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            seen.starts_with("req_"),
            "handler should see the generated id, got: {seen}"
        );
    }
}
