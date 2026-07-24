//! Gateway resilience primitives: retry/backoff, circuit breaking, response
//! caching, and cost events.
//!
//! Implements the building blocks from the
//! [Resilience spec](../../docs/05-llm-gateway/resilience.md) (retry → failover →
//! circuit break → error map) and the [Caching spec](../../docs/05-llm-gateway/caching.md)
//! (exact cache with cost/savings events on hit). The [`Gateway`](crate::Gateway)
//! wires these together.
//!
//! v0.2 slice: in-process exact cache and per-provider breakers, plus an optional
//! **Redis-shared breaker** so a fleet of gateways reacts to a failing provider
//! consistently ([resilience §6](../../docs/05-llm-gateway/resilience.md)). Semantic
//! caching and hedging are deferred.

use crate::embeddings::cosine_similarity;
use crate::types::ChatResponse;
use apex_common::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Retry policy for a single provider ([resilience §4](../../docs/05-llm-gateway/resilience.md)).
#[derive(Clone, Copy, Debug)]
pub struct RetryConfig {
    /// Max attempts against one provider (including the first).
    pub max_attempts: u32,
    /// Base backoff delay in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum backoff delay in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 200,
            max_delay_ms: 4_000,
        }
    }
}

impl RetryConfig {
    /// The exponential delay before the given 1-based `attempt`, capped at
    /// `max_delay_ms`, before jitter is applied.
    fn exp_cap_ms(&self, attempt: u32) -> u64 {
        let exp = self.base_delay_ms as f64 * 2f64.powi(attempt.saturating_sub(1) as i32);
        exp.min(self.max_delay_ms as f64) as u64
    }

    /// Exponential backoff before the given 1-based `attempt`, capped at
    /// `max_delay_ms`. Deterministic — no jitter; see
    /// [`backoff_with_jitter`](Self::backoff_with_jitter) for the jittered form
    /// the gateway actually sleeps for (RM-AIM-P2 PRV-205).
    pub fn backoff(&self, attempt: u32) -> Duration {
        Duration::from_millis(self.exp_cap_ms(attempt))
    }

    /// "Full jitter" backoff ([resilience §4](../../docs/05-llm-gateway/resilience.md):
    /// `jitter: full`) — a uniformly random delay in `[0, backoff(attempt)]`, so a
    /// fleet of retrying callers doesn't retry in lockstep. The jitter source is
    /// injected so this stays deterministic in tests (see [`FixedJitter`)]; the
    /// gateway defaults to [`RandomJitter`].
    pub fn backoff_with_jitter(&self, attempt: u32, jitter: &dyn Jitter) -> Duration {
        Duration::from_millis(jitter.jitter_ms(self.exp_cap_ms(attempt)))
    }
}

/// Source of jitter for retry backoff ([resilience §4](../../docs/05-llm-gateway/resilience.md)).
///
/// Injectable so retry delay stays deterministic in tests — the default
/// [`RandomJitter`] draws from the process RNG, which "no ambient randomness in
/// core logic" ([coding standards §7](../../docs/19-implementation-guide/coding-standards.md))
/// would otherwise forbid.
pub trait Jitter: Send + Sync {
    /// A pseudo-random delay in `[0, bound_ms]` (`bound_ms == 0` always returns 0).
    fn jitter_ms(&self, bound_ms: u64) -> u64;
}

/// The default jitter source: a uniform draw from the process-wide RNG.
#[derive(Default)]
pub struct RandomJitter;

impl Jitter for RandomJitter {
    fn jitter_ms(&self, bound_ms: u64) -> u64 {
        if bound_ms == 0 {
            return 0;
        }
        rand::Rng::gen_range(&mut rand::thread_rng(), 0..=bound_ms)
    }
}

/// A fixed jitter source for deterministic tests: always returns
/// `bound_ms.min(self.0)`.
pub struct FixedJitter(pub u64);

impl Jitter for FixedJitter {
    fn jitter_ms(&self, bound_ms: u64) -> u64 {
        bound_ms.min(self.0)
    }
}

/// Parse a `Retry-After` header value as milliseconds (RM-AIM-P2 PRV-205).
///
/// Only the delay-seconds form (`Retry-After: 30`) is supported — the form every
/// real LLM provider rate-limit response uses; the HTTP-date form is out of scope
/// and is treated as absent (falls back to the gateway's own jittered backoff).
pub fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| secs.saturating_mul(1000))
}

/// Response cache mode ([caching §2](../../docs/05-llm-gateway/caching.md)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheMode {
    /// No lookup, no store (safe default).
    Off,
    /// Lookup/store on an exact normalized-request hash.
    Exact,
    /// Exact lookup first, then by embedding similarity; store in both on a miss
    /// ([caching §4](../../docs/05-llm-gateway/caching.md)).
    Semantic,
}

/// Cache configuration.
#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    /// Cache mode.
    pub mode: CacheMode,
    /// Entry time-to-live in milliseconds.
    pub ttl_ms: u64,
    /// Minimum cosine similarity for a semantic hit (a higher value trades hit
    /// rate for safety; ignored unless `mode` is `Semantic`).
    pub similarity_threshold: f32,
    /// Hard cap on live exact-cache entries (RM-AR-P1 AIC-302). The exact cache
    /// LRU-evicts down to this bound and TTL-sweeps expired entries on insert, so
    /// memory and per-lookup cost stay bounded in a long-running server rather
    /// than growing without limit. `0` disables exact caching entirely.
    pub max_entries: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mode: CacheMode::Off,
            ttl_ms: 3_600_000,
            similarity_threshold: 0.95,
            max_entries: 10_000,
        }
    }
}

/// Request hedging ([resilience §7](../../docs/05-llm-gateway/resilience.md)).
///
/// When enabled, if a candidate hasn't responded within `delay_ms` the gateway
/// dispatches the same request to the next healthy candidate and returns whichever
/// finishes first, cancelling the losers. Disabled by default — hedging trades extra
/// cost for tail-latency, and is metered as separate attempts.
#[derive(Clone, Copy, Debug)]
pub struct HedgeConfig {
    /// Whether hedging is on.
    pub enabled: bool,
    /// Delay with no response before launching the next hedge.
    pub delay_ms: u64,
    /// Maximum requests in flight at once (including the original).
    pub max_parallel: usize,
}

impl Default for HedgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            delay_ms: 800,
            max_parallel: 2,
        }
    }
}

/// A metered cost (or savings) event ([caching §9](../../docs/05-llm-gateway/caching.md)).
#[derive(Clone, Debug)]
pub struct CostEvent {
    /// Provider that served (or would have served) the request.
    pub provider: String,
    /// Concrete model.
    pub model: String,
    /// Prompt tokens.
    pub prompt_tokens: u32,
    /// Completion tokens.
    pub completion_tokens: u32,
    /// Actual cost charged (0 on a cache hit).
    pub cost_usd: f64,
    /// Cache disposition: `None` (live), or `Some("exact")` on a hit.
    pub cache: Option<String>,
    /// On a cache hit, the cost the live call would have incurred.
    pub estimated_savings_usd: f64,
}

/// Receives cost events emitted by the gateway.
pub trait CostObserver: Send + Sync {
    /// Handle one cost event.
    fn on_cost(&self, event: CostEvent);
}

// ---------------------------------------------------------------------------
// Circuit breaker
// ---------------------------------------------------------------------------

/// Circuit breaker configuration ([resilience §6](../../docs/05-llm-gateway/resilience.md)).
#[derive(Clone, Copy, Debug)]
pub struct BreakerConfig {
    /// Consecutive failures that trip the breaker open.
    pub failure_threshold: u32,
    /// Cool-down before a trial (half-open) request is allowed.
    pub open_duration_ms: u64,
    /// Trial requests allowed while half-open.
    pub half_open_max_calls: u32,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_duration_ms: 15_000,
            half_open_max_calls: 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BreakerStatus {
    Closed,
    Open,
    HalfOpen,
}

/// A per-provider circuit breaker. The gateway consults one per candidate provider:
/// `allow` gates dispatch, and `on_success`/`on_failure` feed the state machine.
///
/// Time is passed in as `now_ms` (a monotonic millisecond clock) so the logic is
/// testable without sleeping. Methods are async so an implementation may consult
/// shared state (e.g. Redis) — see [`SharedCircuitBreaker`]; the default
/// [`LocalCircuitBreaker`] keeps everything in process.
#[async_trait]
pub trait CircuitBreaker: Send + Sync {
    /// Whether a request may be sent to this provider now.
    async fn allow(&self, now_ms: u64) -> bool;
    /// Record a success: close the breaker.
    async fn on_success(&self);
    /// Record a failure at `now_ms`: trip open on threshold (or any half-open fail).
    async fn on_failure(&self, now_ms: u64);
}

/// An in-process per-provider circuit breaker (the default, offline path).
pub struct LocalCircuitBreaker {
    cfg: BreakerConfig,
    inner: Mutex<Inner>,
}

struct Inner {
    status: BreakerStatus,
    consecutive_failures: u32,
    open_until_ms: u64,
    half_open_calls: u32,
}

impl LocalCircuitBreaker {
    /// Construct a closed breaker.
    pub fn new(cfg: BreakerConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(Inner {
                status: BreakerStatus::Closed,
                consecutive_failures: 0,
                open_until_ms: 0,
                half_open_calls: 0,
            }),
        }
    }
}

#[async_trait]
impl CircuitBreaker for LocalCircuitBreaker {
    async fn allow(&self, now_ms: u64) -> bool {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        match s.status {
            BreakerStatus::Closed => true,
            BreakerStatus::Open => {
                if now_ms >= s.open_until_ms {
                    s.status = BreakerStatus::HalfOpen;
                    s.half_open_calls = 1;
                    true
                } else {
                    false
                }
            }
            BreakerStatus::HalfOpen => {
                if s.half_open_calls < self.cfg.half_open_max_calls {
                    s.half_open_calls += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    async fn on_success(&self) {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        s.status = BreakerStatus::Closed;
        s.consecutive_failures = 0;
        s.half_open_calls = 0;
    }

    async fn on_failure(&self, now_ms: u64) {
        let mut s = self.inner.lock().expect("breaker mutex poisoned");
        s.consecutive_failures += 1;
        if s.status == BreakerStatus::HalfOpen
            || s.consecutive_failures >= self.cfg.failure_threshold
        {
            s.status = BreakerStatus::Open;
            s.open_until_ms = now_ms + self.cfg.open_duration_ms;
            s.half_open_calls = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Shared (fleet-wide) circuit breaker over a small key-value primitive
// ---------------------------------------------------------------------------

/// Minimal atomic key-value operations a [`SharedCircuitBreaker`] needs from a
/// shared store (Redis in production; [`InMemoryKv`] for tests/single-process).
///
/// Counters use server-side atomic increment so concurrent gateways don't race;
/// TTLs bound how long failure state and half-open trial budgets live.
#[async_trait]
pub trait BreakerKv: Send + Sync {
    /// Atomically increment the integer at `key`, returning the new value. On
    /// first creation the key is given a `ttl_ms` lifetime.
    async fn incr(&self, key: &str, ttl_ms: u64) -> Result<i64>;
    /// Read an integer value, or `None` if the key is absent/expired.
    async fn get_i64(&self, key: &str) -> Result<Option<i64>>;
    /// Set an integer value with a `ttl_ms` lifetime.
    async fn set_i64(&self, key: &str, value: i64, ttl_ms: u64) -> Result<()>;
    /// Delete the given keys (missing keys are ignored).
    async fn del(&self, keys: &[&str]) -> Result<()>;
}

/// A circuit breaker whose state lives in a shared [`BreakerKv`], so every gateway
/// in a fleet sees the same provider health. The consecutive-failure state machine
/// mirrors [`LocalCircuitBreaker`], expressed with atomic counters:
///
/// - `…:fails` — consecutive failures (atomic `INCR`; cleared on success).
/// - `…:open`  — millisecond timestamp the breaker stays open until.
/// - `…:half`  — half-open trial budget consumed since the cool-down elapsed.
///
/// The breaker is **advisory**: any KV error fails *open* (request allowed, error
/// logged) so a store outage never blackholes traffic.
pub struct SharedCircuitBreaker {
    cfg: BreakerConfig,
    kv: std::sync::Arc<dyn BreakerKv>,
    prefix: String,
}

impl SharedCircuitBreaker {
    /// A breaker keyed under `prefix` (typically `apex:breaker:<provider>`).
    pub fn new(
        cfg: BreakerConfig,
        kv: std::sync::Arc<dyn BreakerKv>,
        prefix: impl Into<String>,
    ) -> Self {
        Self {
            cfg,
            kv,
            prefix: prefix.into(),
        }
    }

    fn k_fails(&self) -> String {
        format!("{}:fails", self.prefix)
    }
    fn k_open(&self) -> String {
        format!("{}:open", self.prefix)
    }
    fn k_half(&self) -> String {
        format!("{}:half", self.prefix)
    }

    /// Failure counters live a few cool-down windows so stale state self-heals.
    fn fails_ttl_ms(&self) -> u64 {
        (self.cfg.open_duration_ms.saturating_mul(4)).max(30_000)
    }
}

#[async_trait]
impl CircuitBreaker for SharedCircuitBreaker {
    async fn allow(&self, now_ms: u64) -> bool {
        match self.kv.get_i64(&self.k_open()).await {
            Ok(None) => true,                                            // closed
            Ok(Some(open_until)) if now_ms < open_until as u64 => false, // cooling down
            Ok(Some(_)) => {
                // Cool-down elapsed → half-open: allow a bounded number of trials.
                match self
                    .kv
                    .incr(&self.k_half(), self.cfg.open_duration_ms)
                    .await
                {
                    Ok(n) => n as u32 <= self.cfg.half_open_max_calls,
                    Err(e) => {
                        tracing::warn!("breaker kv incr failed, allowing: {e}");
                        true
                    }
                }
            }
            Err(e) => {
                tracing::warn!("breaker kv get failed, allowing: {e}");
                true
            }
        }
    }

    async fn on_success(&self) {
        // Close and reset all state.
        if let Err(e) = self
            .kv
            .del(&[&self.k_fails(), &self.k_open(), &self.k_half()])
            .await
        {
            tracing::warn!("breaker kv del failed on success: {e}");
        }
    }

    async fn on_failure(&self, now_ms: u64) {
        let fails = match self.kv.incr(&self.k_fails(), self.fails_ttl_ms()).await {
            Ok(n) => n as u32,
            Err(e) => {
                tracing::warn!("breaker kv incr failed on failure: {e}");
                return;
            }
        };
        // A failure while half-open (cool-down already elapsed) reopens immediately.
        let in_half_open = matches!(
            self.kv.get_i64(&self.k_open()).await,
            Ok(Some(open_until)) if now_ms >= open_until as u64
        );
        if in_half_open || fails >= self.cfg.failure_threshold {
            let open_until = now_ms + self.cfg.open_duration_ms;
            let _ = self
                .kv
                .set_i64(&self.k_open(), open_until as i64, self.fails_ttl_ms())
                .await;
            let _ = self.kv.del(&[&self.k_half()]).await;
        }
    }
}

/// An in-process [`BreakerKv`] backed by a `HashMap` — for tests and single-process
/// multi-gateway setups. TTLs are accepted but not enforced (no expiry sweep).
#[derive(Default)]
pub struct InMemoryKv {
    map: Mutex<HashMap<String, i64>>,
}

impl InMemoryKv {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BreakerKv for InMemoryKv {
    async fn incr(&self, key: &str, _ttl_ms: u64) -> Result<i64> {
        let mut m = self.map.lock().expect("kv mutex poisoned");
        let v = m.entry(key.to_string()).or_insert(0);
        *v += 1;
        Ok(*v)
    }
    async fn get_i64(&self, key: &str) -> Result<Option<i64>> {
        Ok(self
            .map
            .lock()
            .expect("kv mutex poisoned")
            .get(key)
            .copied())
    }
    async fn set_i64(&self, key: &str, value: i64, _ttl_ms: u64) -> Result<()> {
        self.map
            .lock()
            .expect("kv mutex poisoned")
            .insert(key.to_string(), value);
        Ok(())
    }
    async fn del(&self, keys: &[&str]) -> Result<()> {
        let mut m = self.map.lock().expect("kv mutex poisoned");
        for k in keys {
            m.remove(*k);
        }
        Ok(())
    }
}

/// A cached chat response with its insertion time and a monotonic last-access
/// tick used for LRU eviction (RM-AR-P1 AIC-302).
pub(crate) struct CacheEntry {
    pub response: ChatResponse,
    pub created_ms: u64,
    pub last_access: u64,
}

/// A bounded, LRU-evicting exact-match response cache with opportunistic TTL
/// eviction (RM-AR-P1 AIC-302). Not thread-safe on its own — the gateway wraps
/// it in a `Mutex`. Before this bound existed the underlying map only ever
/// inserted and checked TTL on lookup, so expired entries were never reclaimed
/// and the map grew without limit in a long-running server.
pub(crate) struct ExactCache {
    entries: std::collections::HashMap<String, CacheEntry>,
    /// Monotonic access counter; the entry with the smallest `last_access` is the
    /// least-recently-used eviction victim.
    tick: u64,
    /// Hard cap on live entries; `0` disables caching entirely.
    max_entries: usize,
}

impl ExactCache {
    /// A cache bounded to `max_entries` entries.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            tick: 0,
            max_entries,
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    /// Return a live (non-expired) cached response, refreshing its LRU recency.
    /// An expired entry is *removed* on access rather than merely ignored, so a
    /// key that is looked up but never re-inserted still gets reclaimed.
    pub fn get(&mut self, key: &str, now_ms: u64, ttl_ms: u64) -> Option<ChatResponse> {
        match self.entries.get(key) {
            Some(e) if now_ms.saturating_sub(e.created_ms) > ttl_ms => {
                self.entries.remove(key);
                None
            }
            Some(_) => {
                let tick = self.next_tick();
                let entry = self.entries.get_mut(key)?;
                entry.last_access = tick;
                Some(entry.response.clone())
            }
            None => None,
        }
    }

    /// Insert/replace an entry, TTL-sweeping expired entries first and then
    /// LRU-evicting until within `max_entries`.
    pub fn insert(&mut self, key: String, response: ChatResponse, now_ms: u64, ttl_ms: u64) {
        if self.max_entries == 0 {
            return;
        }
        // Opportunistic TTL sweep: drop everything already expired.
        self.entries
            .retain(|_, e| now_ms.saturating_sub(e.created_ms) <= ttl_ms);
        let tick = self.next_tick();
        self.entries.insert(
            key,
            CacheEntry {
                response,
                created_ms: now_ms,
                last_access: tick,
            },
        );
        // Evict the least-recently-used entry until within cap. After a sweep +
        // single insert this runs at most once, so the O(n) min-scan is paid
        // only when the cache is genuinely full.
        while self.entries.len() > self.max_entries {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_access)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    self.entries.remove(&k);
                }
                None => break,
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// A semantic-cache entry: the response plus the request embedding, a
/// param-compatibility key it may be served for, and the id of the embedding
/// model that produced the vector ([caching §4](../../docs/05-llm-gateway/caching.md)).
pub(crate) struct SemanticEntry {
    pub embedding: Vec<f32>,
    pub param_key: String,
    /// Embedding-model id stamped at store time (RM-AIM-P2 RAG-203): vectors
    /// from different embedding models live in different spaces (or different
    /// dimensions entirely — where cosine silently reads 0.0), so a lookup
    /// only ever compares against entries produced by the *same* model.
    pub embedding_model: String,
    pub response: ChatResponse,
    pub created_ms: u64,
}

/// Stores semantic-cache entries (request embedding → cached response) and serves
/// nearest-neighbour lookups. The default [`InMemorySemanticCache`] keeps them in
/// process; a distributed impl (e.g. Qdrant) shares them across a gateway fleet
/// ([caching §4](../../docs/05-llm-gateway/caching.md)).
#[async_trait]
pub trait SemanticCacheStore: Send + Sync {
    /// Best param-compatible response whose embedding similarity to `embedding`
    /// clears `threshold`, within `ttl_ms` of `now_ms`. Only entries stamped
    /// with the same `embedding_model` are considered (RM-AIM-P2 RAG-203 —
    /// vectors from different models must never be compared). `None` is a miss.
    async fn lookup(
        &self,
        param_key: &str,
        embedding_model: &str,
        embedding: &[f32],
        threshold: f32,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ChatResponse>>;
    /// Index `response` under its request embedding, param-compatibility key,
    /// and the id of the embedding model that produced the vector.
    async fn store(
        &self,
        param_key: &str,
        embedding_model: &str,
        embedding: &[f32],
        response: &ChatResponse,
        now_ms: u64,
    ) -> Result<()>;
}

/// Default bound on in-process semantic-cache entries (RM-AR-P1 AIC-302).
const DEFAULT_SEMANTIC_CACHE_MAX_ENTRIES: usize = 10_000;

/// In-process semantic cache: a linear cosine scan over recent entries. Bounded
/// (RM-AR-P1 AIC-302) so both stored-entry count and the per-lookup scan stay
/// capped — before this bound `store` pushed on every miss and never evicted,
/// and `lookup` scanned every entry ever stored.
pub struct InMemorySemanticCache {
    entries: Mutex<Vec<SemanticEntry>>,
    /// Hard cap on stored entries; the oldest are evicted once exceeded. `0`
    /// disables semantic caching entirely.
    max_entries: usize,
}

impl Default for InMemorySemanticCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            max_entries: DEFAULT_SEMANTIC_CACHE_MAX_ENTRIES,
        }
    }
}

impl InMemorySemanticCache {
    /// An empty cache bounded to the default entry count.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty cache bounded to `max_entries` stored entries (RM-AR-P1 AIC-302).
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            max_entries,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().expect("cache mutex poisoned").len()
    }
}

#[async_trait]
impl SemanticCacheStore for InMemorySemanticCache {
    async fn lookup(
        &self,
        param_key: &str,
        embedding_model: &str,
        embedding: &[f32],
        threshold: f32,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ChatResponse>> {
        let entries = self.entries.lock().expect("cache mutex poisoned");
        let mut best: Option<(f32, &SemanticEntry)> = None;
        for entry in entries.iter() {
            // Skip (not evict) entries from a different embedding model: they
            // age out via TTL, and skipping stays correct through a rolling
            // deploy where a fleet briefly mixes models.
            if entry.param_key != param_key
                || entry.embedding_model != embedding_model
                || now_ms.saturating_sub(entry.created_ms) > ttl_ms
            {
                continue;
            }
            let sim = cosine_similarity(embedding, &entry.embedding);
            if sim >= threshold && best.is_none_or(|(b, _)| sim > b) {
                best = Some((sim, entry));
            }
        }
        Ok(best.map(|(_, e)| e.response.clone()))
    }

    async fn store(
        &self,
        param_key: &str,
        embedding_model: &str,
        embedding: &[f32],
        response: &ChatResponse,
        now_ms: u64,
    ) -> Result<()> {
        if self.max_entries == 0 {
            return Ok(());
        }
        let mut entries = self.entries.lock().expect("cache mutex poisoned");
        entries.push(SemanticEntry {
            embedding: embedding.to_vec(),
            param_key: param_key.to_string(),
            embedding_model: embedding_model.to_string(),
            response: response.clone(),
            created_ms: now_ms,
        });
        // Evict the oldest entries (by creation time) until within cap, so the
        // stored-entry count — and hence the linear lookup scan — stays bounded
        // under a flood of misses. After a single push this runs at most once.
        while entries.len() > self.max_entries {
            let oldest = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.created_ms)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    entries.remove(i);
                }
                None => break,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let r = RetryConfig {
            max_attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 500,
        };
        assert_eq!(r.backoff(1), Duration::from_millis(100));
        assert_eq!(r.backoff(2), Duration::from_millis(200));
        assert_eq!(r.backoff(3), Duration::from_millis(400));
        assert_eq!(r.backoff(4), Duration::from_millis(500)); // capped
    }

    #[test]
    fn jittered_backoff_is_deterministic_under_a_fixed_source() {
        let r = RetryConfig {
            max_attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 500,
        };
        // FixedJitter(30) always returns min(bound, 30); attempt 1's cap is 100ms.
        let j = FixedJitter(30);
        assert_eq!(r.backoff_with_jitter(1, &j), Duration::from_millis(30));
        // A fixed jitter larger than the (small) cap saturates at the cap, not
        // the fixed value — jitter never exceeds the un-jittered backoff.
        let tiny = RetryConfig {
            max_attempts: 5,
            base_delay_ms: 1,
            max_delay_ms: 1,
        };
        assert_eq!(tiny.backoff_with_jitter(1, &j), Duration::from_millis(1));
    }

    #[test]
    fn random_jitter_stays_within_bounds() {
        let j = RandomJitter;
        assert_eq!(j.jitter_ms(0), 0);
        for _ in 0..100 {
            assert!(j.jitter_ms(50) <= 50);
        }
    }

    /// RAG-203 acceptance (store half): an entry stored under one embedding
    /// model is never served to a lookup using another — even for an
    /// identical vector and param key.
    #[tokio::test]
    async fn semantic_entry_from_a_different_embedding_model_is_not_served() {
        use crate::types::Message;

        let cache = InMemorySemanticCache::new();
        let response = crate::types::ChatResponse {
            message: Message::assistant("cached"),
            model: "m".to_string(),
            usage: apex_common::Usage::new(1, 1, 0.01),
            finish_reason: "stop".to_string(),
        };
        let vec = [1.0_f32, 0.0];
        cache
            .store("pk", "text-embedding-a", &vec, &response, 1_000)
            .await
            .unwrap();

        let cross = cache
            .lookup("pk", "text-embedding-b", &vec, 0.9, 60_000, 1_500)
            .await
            .unwrap();
        assert!(cross.is_none(), "a different embedding model must not hit");

        let same = cache
            .lookup("pk", "text-embedding-a", &vec, 0.9, 60_000, 1_500)
            .await
            .unwrap();
        assert_eq!(
            same.and_then(|r| r.message.content).as_deref(),
            Some("cached"),
            "the same embedding model still hits"
        );
    }

    /// A minimal cached response for cache-bound tests.
    fn cached_response(text: &str) -> ChatResponse {
        crate::types::ChatResponse {
            message: crate::types::Message::assistant(text),
            model: "m".to_string(),
            usage: apex_common::Usage::new(1, 1, 0.01),
            finish_reason: "stop".to_string(),
        }
    }

    /// AIC-302 acceptance (exact cache): inserting well past the cap keeps the
    /// entry count bounded (LRU eviction), and an expired entry is *evicted* on
    /// insert, not merely ignored on lookup.
    #[test]
    fn exact_cache_stays_bounded_and_evicts_expired() {
        let ttl = 1_000_u64;
        let mut cache = ExactCache::new(4);
        let resp = cached_response("r");

        // Flood well past the cap, all within TTL.
        for i in 0..100 {
            cache.insert(format!("k{i}"), resp.clone(), 10, ttl);
        }
        assert!(
            cache.len() <= 4,
            "entry count stays bounded under a flood, got {}",
            cache.len()
        );

        // A fresh cache: insert one entry, let it expire, then insert another —
        // the expired entry is swept, not accumulated.
        let mut cache = ExactCache::new(100);
        cache.insert("old".to_string(), resp.clone(), 0, ttl);
        assert_eq!(cache.len(), 1);
        // now_ms past the TTL → the next insert sweeps "old" first.
        cache.insert("new".to_string(), resp.clone(), ttl + 1, ttl);
        assert_eq!(cache.len(), 1, "expired entry evicted, not accumulated");
        assert!(
            cache.get("old", ttl + 1, ttl).is_none(),
            "the expired key is gone"
        );
        assert!(
            cache.get("new", ttl + 1, ttl).is_some(),
            "the fresh key survives"
        );
    }

    /// AIC-302: LRU recency governs which entry survives eviction — a
    /// recently-read entry is kept over an older-but-untouched one.
    #[test]
    fn exact_cache_evicts_least_recently_used() {
        let ttl = 10_000_u64;
        let mut cache = ExactCache::new(2);
        let resp = cached_response("r");
        cache.insert("a".to_string(), resp.clone(), 0, ttl);
        cache.insert("b".to_string(), resp.clone(), 0, ttl);
        // Touch "a" so it becomes most-recently-used.
        assert!(cache.get("a", 0, ttl).is_some());
        // Inserting "c" evicts the LRU, which is "b".
        cache.insert("c".to_string(), resp.clone(), 0, ttl);
        assert_eq!(cache.len(), 2);
        assert!(cache.get("a", 0, ttl).is_some(), "recently-used survives");
        assert!(cache.get("b", 0, ttl).is_none(), "LRU victim evicted");
        assert!(cache.get("c", 0, ttl).is_some(), "new entry present");
    }

    /// AIC-302 acceptance (semantic cache): the stored-entry count stays bounded
    /// under a flood of misses (before this bound `store` grew a `Vec` without
    /// limit and `lookup` scanned every entry ever stored).
    #[tokio::test]
    async fn semantic_cache_stays_bounded_under_a_flood_of_misses() {
        let cache = InMemorySemanticCache::with_max_entries(8);
        let resp = cached_response("r");
        for i in 0..200 {
            // Distinct vectors so nothing ever hits — every call is a store.
            let v = [i as f32, 1.0_f32];
            cache
                .store("pk", "model-a", &v, &resp, 1_000 + i as u64)
                .await
                .unwrap();
        }
        assert!(
            cache.len() <= 8,
            "semantic entry count stays bounded, got {}",
            cache.len()
        );
    }

    #[test]
    fn retry_after_header_parses_as_milliseconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), Some(30_000));
    }

    #[test]
    fn missing_or_non_numeric_retry_after_is_none() {
        assert_eq!(
            parse_retry_after_ms(&reqwest::header::HeaderMap::new()),
            None
        );

        let mut headers = reqwest::header::HeaderMap::new();
        // The HTTP-date form is out of scope — treated as absent.
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after_ms(&headers), None);
    }

    #[tokio::test]
    async fn breaker_trips_after_threshold_and_recovers() {
        let cb = LocalCircuitBreaker::new(BreakerConfig {
            failure_threshold: 3,
            open_duration_ms: 1_000,
            half_open_max_calls: 1,
        });
        assert!(cb.allow(0).await);
        cb.on_failure(0).await;
        cb.on_failure(0).await;
        assert!(cb.allow(0).await, "still closed after 2 failures");
        cb.on_failure(0).await; // third → trips open
        assert!(
            !cb.allow(10).await,
            "open: requests blocked during cooldown"
        );

        // After cooldown, one trial is allowed (half-open).
        assert!(cb.allow(1_000).await);
        // Success closes it again.
        cb.on_success().await;
        assert!(cb.allow(2_000).await);
    }

    #[tokio::test]
    async fn half_open_failure_reopens() {
        let cb = LocalCircuitBreaker::new(BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 100,
            half_open_max_calls: 1,
        });
        cb.on_failure(0).await; // trips open
        assert!(!cb.allow(50).await);
        assert!(cb.allow(100).await); // half-open trial
        cb.on_failure(100).await; // trial fails → reopen
        assert!(!cb.allow(150).await);
    }

    /// Two breakers sharing one `InMemoryKv` stand in for two gateway nodes: a trip
    /// driven through one is observed by the other (the point of Redis-shared state).
    #[tokio::test]
    async fn shared_breaker_state_is_visible_across_instances() {
        let cfg = BreakerConfig {
            failure_threshold: 3,
            open_duration_ms: 1_000,
            half_open_max_calls: 1,
        };
        let kv = std::sync::Arc::new(InMemoryKv::new());
        let node_a = SharedCircuitBreaker::new(cfg, kv.clone(), "apex:breaker:openai");
        let node_b = SharedCircuitBreaker::new(cfg, kv.clone(), "apex:breaker:openai");

        // Node A drives the provider to its failure threshold.
        node_a.on_failure(0).await;
        node_a.on_failure(0).await;
        assert!(node_b.allow(0).await, "node B sees closed after 2 failures");
        node_a.on_failure(0).await; // third → trips open fleet-wide

        // Node B (a different instance) immediately sees the open breaker.
        assert!(!node_b.allow(10).await, "node B sees the shared open state");

        // After the cool-down, node B gets the single half-open trial; a success
        // there closes the breaker for the whole fleet, including node A.
        assert!(node_b.allow(1_000).await, "half-open trial on node B");
        assert!(
            !node_a.allow(1_000).await,
            "trial budget is shared: node A is blocked"
        );
        node_b.on_success().await;
        assert!(node_a.allow(2_000).await, "success closes it fleet-wide");
    }

    /// A half-open failure (after cool-down) reopens the shared breaker.
    #[tokio::test]
    async fn shared_breaker_half_open_failure_reopens() {
        let cfg = BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 100,
            half_open_max_calls: 1,
        };
        let kv = std::sync::Arc::new(InMemoryKv::new());
        let cb = SharedCircuitBreaker::new(cfg, kv, "apex:breaker:p");
        cb.on_failure(0).await; // trips open
        assert!(!cb.allow(50).await);
        assert!(cb.allow(100).await); // half-open trial
        cb.on_failure(100).await; // trial fails → reopen
        assert!(!cb.allow(150).await);
    }
}
