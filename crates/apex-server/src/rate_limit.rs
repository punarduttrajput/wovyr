//! Per-key rate limiting ([RM-GA-P1 SEC-203](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
//! a token-bucket keyed by the caller's verified principal (falling back to client
//! IP for anonymous callers), so one noisy caller can't starve another. Two
//! independent limiters back the two tiers `router()` wires: `standard` for most
//! routes, `sensitive` (a tighter budget) for the direct agent-run endpoints, KMS,
//! and secrets.
//!
//! **Distributed enforcement (RM-AIM-P2 SRV-201):** the bucket state lives either
//! in-process (the dependency-free default — correct for the single-node
//! appliance) or, behind the `redis` cargo feature with
//! `APEX_RATE_LIMIT_REDIS_URL` set, in a shared Redis so a fleet of N nodes
//! enforces **one** combined budget per key instead of N independent ones. The
//! shared path runs the token-bucket refill/take atomically as a Lua script
//! (`EVAL` — read-modify-write under Redis' single-threaded execution, so two
//! nodes never double-spend one token), and **degrades to the in-process bucket**
//! — never to unlimited — when Redis is unreachable or slow (a rate limiter is a
//! protection, so its failure mode is per-node limiting, not no limiting; same
//! advisory posture as the gateway's shared circuit breaker, one notch stricter).

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

/// The in-process bucket store: today's single-node backend, and the degrade
/// path the Redis backend falls back to per call.
struct LocalBuckets {
    buckets: Mutex<HashMap<String, Bucket>>,
    checks_since_sweep: AtomicU64,
}

/// Sweep stale (fully-rested) buckets every this many `check` calls, bounding
/// memory growth from a long-lived process seeing many distinct keys over time —
/// a fully-rested bucket carries no history worth keeping; it's recreated
/// identically on the next request from that key. (The Redis backend gets the
/// same hygiene from a per-key `PEXPIRE` at full-refill time instead.)
const SWEEP_INTERVAL: u64 = 256;

impl LocalBuckets {
    fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            checks_since_sweep: AtomicU64::new(0),
        }
    }

    fn check(&self, capacity: f64, refill_per_sec: f64, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");

        if self.checks_since_sweep.fetch_add(1, Ordering::Relaxed) >= SWEEP_INTERVAL {
            self.checks_since_sweep.store(0, Ordering::Relaxed);
            buckets.retain(|_, b| b.tokens < capacity);
        }

        let bucket = buckets.entry(key.to_string()).or_insert_with(|| Bucket {
            tokens: capacity,
            last_refill: now,
        });
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let deficit = 1.0 - bucket.tokens;
            Err(Duration::from_secs_f64(deficit / refill_per_sec))
        }
    }
}

/// A token bucket per key, refilled continuously at `refill_per_sec` up to
/// `capacity`. Configured in requests-per-minute (`per_minute`) since that's how
/// operators think about rate limits; converted to a per-second refill rate
/// internally for smooth (not bursty-per-tick) admission.
pub(crate) struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    local: LocalBuckets,
    #[cfg(feature = "redis")]
    shared: Option<redis_shared::SharedBuckets>,
}

impl RateLimiter {
    /// A limiter admitting up to `capacity` requests per key, refilling at
    /// `per_minute` requests/minute. In-process state (single-node).
    pub(crate) fn new(capacity: u32, per_minute: u32) -> Self {
        Self {
            capacity: capacity as f64,
            refill_per_sec: per_minute as f64 / 60.0,
            local: LocalBuckets::new(),
            #[cfg(feature = "redis")]
            shared: None,
        }
    }

    /// Back this limiter's buckets with a shared Redis (SRV-201), namespaced under
    /// `prefix` (one prefix per tier, so the two tiers never share a bucket). The
    /// connection is established lazily on first use and re-dialed after an error;
    /// while Redis is unreachable, every check degrades to the in-process bucket.
    #[cfg(feature = "redis")]
    pub(crate) fn with_redis(mut self, client: redis::Client, prefix: impl Into<String>) -> Self {
        self.shared = Some(redis_shared::SharedBuckets::new(client, prefix.into()));
        self
    }

    /// Build a tier's limiter from the environment: Redis-shared when the server
    /// is compiled with the `redis` feature and `APEX_RATE_LIMIT_REDIS_URL` is
    /// set, else in-process. Setting the variable on a binary built *without* the
    /// feature logs a loud warning rather than silently running per-node limits.
    pub(crate) fn from_env(tier: &str, capacity: u32, per_minute: u32) -> Self {
        let limiter = Self::new(capacity, per_minute);
        let Ok(url) = std::env::var("APEX_RATE_LIMIT_REDIS_URL") else {
            return limiter;
        };
        #[cfg(feature = "redis")]
        {
            match redis::Client::open(url.as_str()) {
                Ok(client) => {
                    tracing::info!(tier, "rate limiting: shared Redis buckets enabled");
                    return limiter.with_redis(client, format!("apex:rl:{tier}"));
                }
                Err(e) => {
                    tracing::error!(
                        tier,
                        error = %e,
                        "APEX_RATE_LIMIT_REDIS_URL is invalid; falling back to per-node rate limiting"
                    );
                }
            }
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = &url;
            tracing::error!(
                tier,
                "APEX_RATE_LIMIT_REDIS_URL is set but this binary was built without the \
                 `redis` feature — rate limits are per-node, not fleet-wide"
            );
        }
        limiter
    }

    /// Admit one request under `key`. `Ok(())` within budget; `Err(retry_after)` (a
    /// `Duration` estimate until a token is available) if `key`'s bucket is
    /// exhausted.
    pub(crate) async fn check(&self, key: &str) -> Result<(), Duration> {
        #[cfg(feature = "redis")]
        if let Some(shared) = &self.shared {
            match shared.check(self.capacity, self.refill_per_sec, key).await {
                Ok(decision) => return decision,
                Err(e) => {
                    // Degrade to per-node limiting — never to unlimited.
                    tracing::warn!(
                        error = %e,
                        "shared rate-limit store unavailable; degrading to per-node limiting"
                    );
                }
            }
        }
        self.local.check(self.capacity, self.refill_per_sec, key)
    }
}

/// The Redis-shared bucket store (SRV-201), behind the `redis` cargo feature.
#[cfg(feature = "redis")]
mod redis_shared {
    use std::time::Duration;

    /// The atomic refill-then-take, executed inside Redis so concurrent nodes
    /// serialize on one bucket. State is a hash `{t: tokens, ts: last_refill_ms}`;
    /// `ts` never moves backwards, so a node with a slower clock can't rewind the
    /// bucket into granting double refill. Returns `[allowed, tokens-as-string]`
    /// (stringified because Lua→Redis integer replies truncate fractions).
    const TAKE_SCRIPT: &str = r#"
        local capacity = tonumber(ARGV[1])
        local refill_per_ms = tonumber(ARGV[2])
        local now = tonumber(ARGV[3])
        local ttl_ms = tonumber(ARGV[4])
        local state = redis.call('HMGET', KEYS[1], 't', 'ts')
        local tokens = tonumber(state[1])
        local ts = tonumber(state[2])
        if tokens == nil or ts == nil then
            tokens = capacity
            ts = now
        end
        if now > ts then
            tokens = math.min(capacity, tokens + (now - ts) * refill_per_ms)
            ts = now
        end
        local allowed = 0
        if tokens >= 1 then
            tokens = tokens - 1
            allowed = 1
        end
        redis.call('HSET', KEYS[1], 't', tostring(tokens), 'ts', tostring(ts))
        redis.call('PEXPIRE', KEYS[1], ttl_ms)
        return {allowed, tostring(tokens)}
    "#;

    /// Cap on how long the shared path may take before the caller degrades to the
    /// local bucket — a rate-limit check sits on every request, so a slow Redis
    /// must not become a global request stall.
    const REDIS_BUDGET: Duration = Duration::from_secs(1);

    pub(super) struct SharedBuckets {
        client: redis::Client,
        /// Lazily-dialed multiplexed connection; cleared on command failure so the
        /// next check re-dials instead of erroring forever on a dead socket.
        conn: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
        prefix: String,
        script: redis::Script,
    }

    impl SharedBuckets {
        pub(super) fn new(client: redis::Client, prefix: String) -> Self {
            Self {
                client,
                conn: tokio::sync::Mutex::new(None),
                prefix,
                script: redis::Script::new(TAKE_SCRIPT),
            }
        }

        async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, String> {
            let mut slot = self.conn.lock().await;
            if let Some(conn) = slot.as_ref() {
                return Ok(conn.clone());
            }
            let conn = self
                .client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| format!("redis connect: {e}"))?;
            *slot = Some(conn.clone());
            Ok(conn)
        }

        /// One shared-bucket admission. Outer `Err(reason)` = the store is
        /// unavailable (caller degrades); inner result is the admission decision.
        pub(super) async fn check(
            &self,
            capacity: f64,
            refill_per_sec: f64,
            key: &str,
        ) -> Result<Result<(), Duration>, String> {
            let attempt = async {
                let mut conn = self.connection().await?;
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| format!("clock before epoch: {e}"))?
                    .as_millis() as u64;
                // Key lifetime: worst-case full-refill time plus slack — after
                // that, the bucket is indistinguishable from a fresh one.
                let ttl_ms = ((capacity / refill_per_sec) * 1000.0) as u64 + 60_000;
                let (allowed, tokens): (i64, String) = self
                    .script
                    .key(format!("{}:{key}", self.prefix))
                    .arg(capacity)
                    .arg(refill_per_sec / 1000.0)
                    .arg(now_ms)
                    .arg(ttl_ms)
                    .invoke_async(&mut conn)
                    .await
                    .map_err(|e| format!("redis eval: {e}"))?;
                let tokens: f64 = tokens
                    .parse()
                    .map_err(|e| format!("unparseable token count `{tokens}`: {e}"))?;
                if allowed == 1 {
                    Ok(Ok(()))
                } else {
                    let deficit = (1.0 - tokens).max(0.0);
                    Ok(Err(Duration::from_secs_f64(deficit / refill_per_sec)))
                }
            };
            match tokio::time::timeout(REDIS_BUDGET, attempt).await {
                Ok(Ok(decision)) => Ok(decision),
                Ok(Err(reason)) => {
                    // Drop the cached connection: the next check re-dials.
                    *self.conn.lock().await = None;
                    Err(reason)
                }
                Err(_) => {
                    *self.conn.lock().await = None;
                    Err(format!("redis timed out after {REDIS_BUDGET:?}"))
                }
            }
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
    match limiter.check(&key).await {
        Ok(()) => next.run(req).await,
        Err(retry_after) => rate_limited_response(retry_after),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn admits_up_to_capacity_then_rejects_with_a_retry_after() {
        let limiter = RateLimiter::new(2, 60); // 2/min ⇒ slow refill within the test
        assert!(limiter.check("alice").await.is_ok());
        assert!(limiter.check("alice").await.is_ok());
        let err = limiter.check("alice").await.unwrap_err();
        assert!(err > Duration::from_secs(0));
    }

    #[tokio::test]
    async fn keys_are_independent() {
        let limiter = RateLimiter::new(1, 60);
        assert!(limiter.check("alice").await.is_ok());
        assert!(limiter.check("alice").await.is_err());
        // A different key has its own, untouched bucket.
        assert!(limiter.check("bob").await.is_ok());
    }

    #[tokio::test]
    async fn refills_over_time() {
        let limiter = RateLimiter::new(1, 6000); // 100/sec — refills fast enough to test
        assert!(limiter.check("alice").await.is_ok());
        assert!(limiter.check("alice").await.is_err());
        std::thread::sleep(Duration::from_millis(20));
        assert!(limiter.check("alice").await.is_ok());
    }

    #[tokio::test]
    async fn sweep_does_not_drop_a_still_exhausted_bucket() {
        let limiter = RateLimiter::new(1, 1); // 1/min — refills far too slowly to matter
        assert!(limiter.check("alice").await.is_ok());
        // Drive enough calls (against other keys) to trigger a sweep pass.
        for i in 0..(SWEEP_INTERVAL + 1) {
            let _ = limiter.check(&format!("filler-{i}")).await;
        }
        // alice's bucket was exhausted (not fully rested), so the sweep must have
        // kept it — still rejected, not silently reset.
        assert!(limiter.check("alice").await.is_err());
    }

    /// SRV-201's degrade contract, testable offline: a Redis-configured limiter
    /// whose Redis is unreachable falls back to the in-process bucket — per-node
    /// limiting, never unlimited.
    #[cfg(feature = "redis")]
    #[tokio::test]
    async fn unreachable_redis_degrades_to_local_limiting_not_unlimited() {
        // Port 9 (discard) on loopback: connection refused, immediately.
        let client = redis::Client::open("redis://127.0.0.1:9").unwrap();
        let limiter = RateLimiter::new(1, 60).with_redis(client, "apex:rl:test-degrade");
        assert!(limiter.check("alice").await.is_ok(), "degraded first token");
        assert!(
            limiter.check("alice").await.is_err(),
            "the local fallback bucket still enforces the budget"
        );
    }
}

/// Live integration tests for Redis-shared rate limiting (RM-AIM-P2 SRV-201) —
/// capability-gated like `apex-provider`'s `redis_breaker` test: they read
/// `APEX_REDIS_URL` and return early (logging a `skipping:` line CI's
/// service-container job fails on) when unset or unreachable, so the suite still
/// passes offline. Inline rather than under `tests/` because [`RateLimiter`] is
/// deliberately `pub(crate)` — same reasoning as `lib.rs`'s inline suite.
///
/// ```bash
/// APEX_REDIS_URL=redis://127.0.0.1:6379 \
///   cargo test -p apex-server --features redis --lib rate_limit -- --nocapture
/// ```
#[cfg(all(test, feature = "redis"))]
mod redis_tests {
    use super::RateLimiter;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique key prefix per run so repeated runs (and parallel tests) don't collide.
    fn prefix(name: &str) -> String {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("apex:it:rl:{name}:{nonce}")
    }

    /// Open a Redis client, or `None` (logging a skip) when unconfigured/unreachable.
    async fn client() -> Option<redis::Client> {
        let url = match std::env::var("APEX_REDIS_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("skipping: APEX_REDIS_URL not set");
                return None;
            }
        };
        let client = match redis::Client::open(url.as_str()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping: invalid APEX_REDIS_URL {url}: {e}");
                return None;
            }
        };
        // Probe reachability so an offline machine skips instead of degrading
        // every check to the local bucket (which would silently turn the
        // combined-budget test into a 2× test).
        match client.get_multiplexed_async_connection().await {
            Ok(_) => Some(client),
            Err(e) => {
                eprintln!("skipping: redis unreachable at {url}: {e}");
                None
            }
        }
    }

    /// The ticket's acceptance criterion: two limiter instances over one shared
    /// store enforce a **combined** budget, not 2×.
    #[tokio::test]
    async fn two_limiter_instances_enforce_one_combined_budget() {
        let Some(client) = client().await else { return };
        let p = prefix("combined");

        // Two limiters over the same Redis prefix = two server nodes in a fleet.
        // Capacity 4 with a 1/min refill: too slow to add a token mid-test.
        let node_a = RateLimiter::new(4, 1).with_redis(client.clone(), p.clone());
        let node_b = RateLimiter::new(4, 1).with_redis(client.clone(), p.clone());

        // Alternating across nodes, exactly `capacity` requests are admitted…
        assert!(node_a.check("alice").await.is_ok(), "1/4 via node A");
        assert!(node_b.check("alice").await.is_ok(), "2/4 via node B");
        assert!(node_a.check("alice").await.is_ok(), "3/4 via node A");
        assert!(node_b.check("alice").await.is_ok(), "4/4 via node B");

        // …and the 5th is rejected on *both* nodes: one shared budget, not 2×.
        let retry = node_a
            .check("alice")
            .await
            .expect_err("node A must see the exhausted shared bucket");
        assert!(retry.as_secs_f64() > 0.0, "a Retry-After estimate is given");
        assert!(
            node_b.check("alice").await.is_err(),
            "node B must see the exhausted shared bucket too"
        );
    }

    #[tokio::test]
    async fn shared_buckets_are_still_per_key_and_per_tier() {
        let Some(client) = client().await else { return };

        let limiter_one = RateLimiter::new(1, 1).with_redis(client.clone(), prefix("tier1"));
        let limiter_two = RateLimiter::new(1, 1).with_redis(client.clone(), prefix("tier2"));

        assert!(limiter_one.check("alice").await.is_ok());
        assert!(
            limiter_one.check("alice").await.is_err(),
            "alice exhausted her tier-1 budget"
        );
        // A different key has its own shared bucket…
        assert!(limiter_one.check("bob").await.is_ok());
        // …and the same key under a different tier prefix is untouched.
        assert!(limiter_two.check("alice").await.is_ok());
    }

    #[tokio::test]
    async fn shared_bucket_refills_over_time() {
        let Some(client) = client().await else { return };
        // 6000/min = 100/sec: refills fast enough to observe within the test.
        let limiter = RateLimiter::new(1, 6000).with_redis(client, prefix("refill"));

        assert!(limiter.check("alice").await.is_ok());
        assert!(limiter.check("alice").await.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            limiter.check("alice").await.is_ok(),
            "the shared bucket refills continuously"
        );
    }
}
