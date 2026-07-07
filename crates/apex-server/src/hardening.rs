//! Cross-cutting API conventions ([API overview](../../docs/09-api/overview.md)):
//! cursor **pagination** (§6), **idempotency keys** (§9), and **request-id**
//! propagation (§14). These harden the `/v1` surface so list endpoints page
//! consistently, client retries are safe, and every response is traceable.

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, header},
    middleware::Next,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
    inserted_at: Instant,
}

/// Insertion-ordered map + a FIFO eviction queue, so both TTL expiry and the max-entry
/// bound can cheaply find "the oldest entries" without scanning the whole map.
#[derive(Default)]
struct IdempotencyInner {
    entries: HashMap<String, IdempotencyEntry>,
    order: VecDeque<String>,
}

/// Caches responses to mutating requests by `Idempotency-Key`, so a client retry
/// returns the original result instead of acting twice. In-memory + tenant-scoped,
/// bounded two ways (SEC-205): entries older than `ttl` expire (checked lazily, on
/// each `get`/`put`, rather than a background sweeper — no extra task, no clock read
/// outside these two call sites), and the map never exceeds `max_entries` — a client
/// minting unique keys faster than the TTL elapses evicts the oldest tracked key
/// (FIFO) rather than growing without bound.
pub(crate) struct IdempotencyStore {
    inner: Mutex<IdempotencyInner>,
    ttl: Duration,
    max_entries: usize,
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
    /// A store retaining entries for `ttl`, capped at `max_entries`.
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(IdempotencyInner::default()),
            ttl,
            max_entries,
        }
    }

    /// Drop entries at the front of `order` (the oldest) that have expired, or — once
    /// at `max_entries` — the single oldest entry regardless of expiry, to admit one
    /// more. Removing a key already absent from `entries` (a stale duplicate left in
    /// `order` by an overwritten `put`) is a harmless no-op.
    fn evict(inner: &mut IdempotencyInner, ttl: Duration, max_entries: usize, make_room: bool) {
        while let Some(front) = inner.order.front() {
            let expired = inner
                .entries
                .get(front)
                .is_none_or(|e| e.inserted_at.elapsed() > ttl);
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

    /// Remember `body` as the response for `(tenant, key)`.
    pub(crate) fn put(&self, tenant: &str, key: &str, body: Value) {
        let mut inner = self.inner.lock().expect("idempotency mutex poisoned");
        Self::evict(&mut inner, self.ttl, self.max_entries, true);
        let scoped_key = scoped(tenant, key);
        inner.entries.insert(
            scoped_key.clone(),
            IdempotencyEntry {
                body,
                inserted_at: Instant::now(),
            },
        );
        inner.order.push_back(scoped_key);
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
}
