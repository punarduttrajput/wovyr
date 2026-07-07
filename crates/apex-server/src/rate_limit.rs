//! Per-key rate limiting ([RM-GA-P1 SEC-203](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
//! a dependency-free token-bucket keyed by the caller's verified principal (falling
//! back to client IP for anonymous callers), so one noisy caller can't starve
//! another. Two independent limiters back the two tiers `router()` wires:
//! `standard` for most routes, `sensitive` (a tighter budget) for the direct
//! agent-run endpoints, KMS, and secrets.

use crate::ApiError;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// A token bucket per key, refilled continuously at `refill_per_sec` up to
/// `capacity`. Configured in requests-per-minute (`per_minute`) since that's how
/// operators think about rate limits; converted to a per-second refill rate
/// internally for smooth (not bursty-per-tick) admission.
pub(crate) struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
    checks_since_sweep: AtomicU64,
}

/// Sweep stale (fully-rested) buckets every this many `check` calls, bounding
/// memory growth from a long-lived process seeing many distinct keys over time —
/// a fully-rested bucket carries no history worth keeping; it's recreated
/// identically on the next request from that key.
const SWEEP_INTERVAL: u64 = 256;

impl RateLimiter {
    /// A limiter admitting up to `capacity` requests per key, refilling at
    /// `per_minute` requests/minute.
    pub(crate) fn new(capacity: u32, per_minute: u32) -> Self {
        Self {
            capacity: capacity as f64,
            refill_per_sec: per_minute as f64 / 60.0,
            buckets: Mutex::new(HashMap::new()),
            checks_since_sweep: AtomicU64::new(0),
        }
    }

    /// Admit one request under `key`. `Ok(())` within budget; `Err(retry_after)` (a
    /// `Duration` estimate until a token is available) if `key`'s bucket is
    /// exhausted.
    pub(crate) fn check(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");

        if self.checks_since_sweep.fetch_add(1, Ordering::Relaxed) >= SWEEP_INTERVAL {
            self.checks_since_sweep.store(0, Ordering::Relaxed);
            buckets.retain(|_, b| b.tokens < self.capacity);
        }

        let bucket = buckets.entry(key.to_string()).or_insert_with(|| Bucket {
            tokens: self.capacity,
            last_refill: now,
        });
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let deficit = 1.0 - bucket.tokens;
            Err(Duration::from_secs_f64(deficit / self.refill_per_sec))
        }
    }
}

/// The rate-limit key for a request: the verified principal set by
/// [`crate::auth::authenticate`] (`X-Apex-Principal`, overwritten there — never a raw
/// client-supplied value) if non-empty; otherwise the client's `X-Forwarded-For` (the
/// first, closest-to-client hop — trust this only behind a proxy that itself sets it,
/// same caveat as any `X-Forwarded-For` use) or, failing that, the real peer address
/// from `ConnectInfo` (present when serving via
/// `into_make_service_with_connect_info::<SocketAddr>()`); `"anonymous"` if none of
/// the above are available (e.g. a test harness with no real connection).
fn rate_limit_key(headers: &HeaderMap, req: &Request) -> String {
    if let Some(principal) = headers
        .get("x-apex-principal")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
    {
        return format!("principal:{principal}");
    }
    if let Some(fwd) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return format!("ip:{fwd}");
    }
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return format!("ip:{}", addr.ip());
    }
    "anonymous".to_string()
}

fn rate_limited_response(retry_after: Duration) -> Response {
    let mut resp = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "rate limit exceeded",
    )
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
        resp.headers_mut().insert("retry-after", v);
    }
    resp
}

/// The middleware `router()` mounts per tier (via `axum::middleware::from_fn` over a
/// closure capturing `limiter` — not `from_fn_with_state`, so the limiter lives
/// wherever the caller already keeps it, here `AppState`, rather than needing its own
/// place in axum's state-extraction machinery): check the caller's bucket, admit or
/// `429` with `Retry-After`.
pub(crate) async fn enforce(
    limiter: Arc<RateLimiter>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let key = rate_limit_key(&headers, &req);
    match limiter.check(&key) {
        Ok(()) => next.run(req).await,
        Err(retry_after) => rate_limited_response(retry_after),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_capacity_then_rejects_with_a_retry_after() {
        let limiter = RateLimiter::new(2, 60); // 2/min ⇒ slow refill within the test
        assert!(limiter.check("alice").is_ok());
        assert!(limiter.check("alice").is_ok());
        let err = limiter.check("alice").unwrap_err();
        assert!(err > Duration::from_secs(0));
    }

    #[test]
    fn keys_are_independent() {
        let limiter = RateLimiter::new(1, 60);
        assert!(limiter.check("alice").is_ok());
        assert!(limiter.check("alice").is_err());
        // A different key has its own, untouched bucket.
        assert!(limiter.check("bob").is_ok());
    }

    #[test]
    fn refills_over_time() {
        let limiter = RateLimiter::new(1, 6000); // 100/sec — refills fast enough to test
        assert!(limiter.check("alice").is_ok());
        assert!(limiter.check("alice").is_err());
        std::thread::sleep(Duration::from_millis(20));
        assert!(limiter.check("alice").is_ok());
    }

    #[test]
    fn sweep_does_not_drop_a_still_exhausted_bucket() {
        let limiter = RateLimiter::new(1, 1); // 1/min — refills far too slowly to matter
        assert!(limiter.check("alice").is_ok());
        // Drive enough calls (against other keys) to trigger a sweep pass.
        for i in 0..(SWEEP_INTERVAL + 1) {
            let _ = limiter.check(&format!("filler-{i}"));
        }
        // alice's bucket was exhausted (not fully rested), so the sweep must have
        // kept it — still rejected, not silently reset.
        assert!(limiter.check("alice").is_err());
    }
}
