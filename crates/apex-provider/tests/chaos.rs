//! Chaos tests for the LLM gateway: inject provider faults and assert the
//! documented resilience behaviors hold ([chaos testing](../../../docs/15-testing/chaos-testing.md),
//! [resilience](../../../docs/05-llm-gateway/resilience.md)).
//!
//! Each test states a steady-state hypothesis, injects a fault (provider outage,
//! flapping, permanent error, total outage), and asserts the system degrades
//! gracefully and/or recovers. A [`FaultProvider`] is the application-level fault
//! hook the chaos spec calls for (§6).

use apex_common::{Error, Result, Usage};
use apex_provider::{
    AIProvider, BreakerConfig, CacheConfig, CacheMode, ChatRequest, ChatResponse, Gateway, Message,
    RetryConfig,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Shared, toggleable fault state for a provider.
struct Fault {
    healthy: AtomicBool,
    permanent: bool,
    calls: AtomicUsize,
}

impl Fault {
    fn new(healthy: bool, permanent: bool) -> Arc<Self> {
        Arc::new(Self {
            healthy: AtomicBool::new(healthy),
            permanent,
            calls: AtomicUsize::new(0),
        })
    }
    fn set_healthy(&self, v: bool) {
        self.healthy.store(v, Ordering::SeqCst);
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// A provider whose health can be toggled at runtime to inject outages.
struct FaultProvider {
    name: &'static str,
    fault: Arc<Fault>,
}

/// Build a provider plus a handle to its fault state.
fn provider(
    name: &'static str,
    healthy: bool,
    permanent: bool,
) -> (Box<dyn AIProvider>, Arc<Fault>) {
    let fault = Fault::new(healthy, permanent);
    (
        Box::new(FaultProvider {
            name,
            fault: fault.clone(),
        }),
        fault,
    )
}

#[async_trait]
impl AIProvider for FaultProvider {
    fn name(&self) -> &str {
        self.name
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.fault.calls.fetch_add(1, Ordering::SeqCst);
        if self.fault.healthy.load(Ordering::SeqCst) {
            Ok(ChatResponse {
                message: Message::assistant(format!("ok from {}", self.name)),
                model: request.model,
                usage: Usage::new(3, 2, 0.01),
                finish_reason: "stop".to_string(),
            })
        } else if self.fault.permanent {
            Err(Error::invalid("400 bad request"))
        } else {
            Err(Error::provider("503 service unavailable"))
        }
    }
}

fn req() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user("hi")])
}

/// No retries: makes per-provider call counts deterministic for breaker tests.
fn no_retry() -> RetryConfig {
    RetryConfig {
        max_attempts: 1,
        base_delay_ms: 1,
        max_delay_ms: 1,
    }
}

fn content(resp: &ChatResponse) -> &str {
    resp.message.content.as_deref().unwrap_or("")
}

// Hypothesis: while a healthy provider exists, a provider outage causes no
// user-visible failure — the gateway fails over.
#[tokio::test]
async fn provider_outage_fails_over_with_no_user_visible_failure() {
    let (primary, pf) = provider("primary", false, false);
    let (secondary, _sf) = provider("secondary", true, false);
    let gw = Gateway::with_providers(vec![primary, secondary]).with_retry(no_retry());

    let resp = gw.chat(req()).await.unwrap();
    assert_eq!(content(&resp), "ok from secondary");
    assert!(pf.calls() >= 1, "primary should have been attempted");
}

// Hypothesis: a persistently failing provider trips its breaker open and is then
// skipped — the gateway stops hammering a dead upstream while still serving.
#[tokio::test]
async fn circuit_breaker_stops_hammering_a_dead_provider() {
    let (primary, pf) = provider("primary", false, false);
    let (secondary, _sf) = provider("secondary", true, false);
    let gw = Gateway::with_providers(vec![primary, secondary])
        .with_retry(no_retry())
        .with_breaker(BreakerConfig {
            failure_threshold: 2,
            open_duration_ms: 60_000,
            half_open_max_calls: 1,
        });

    for _ in 0..6 {
        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(content(&resp), "ok from secondary");
    }
    // The primary is probed only until the breaker opens (2 failures), then skipped.
    assert_eq!(
        pf.calls(),
        2,
        "open breaker must stop sending traffic to the dead provider"
    );
}

// Hypothesis: after the cool-down, a recovered provider is brought back into
// service via a half-open trial (self-healing).
#[tokio::test]
async fn circuit_breaker_recovers_after_cooldown() {
    let (primary, pf) = provider("primary", false, false);
    let (secondary, _sf) = provider("secondary", true, false);
    let gw = Gateway::with_providers(vec![primary, secondary])
        .with_retry(no_retry())
        .with_breaker(BreakerConfig {
            failure_threshold: 1,
            open_duration_ms: 50,
            half_open_max_calls: 1,
        });

    // First call trips the primary's breaker; service continues via secondary.
    let r1 = gw.chat(req()).await.unwrap();
    assert_eq!(content(&r1), "ok from secondary");

    // The primary recovers; after the cool-down the half-open trial restores it.
    pf.set_healthy(true);
    tokio::time::sleep(Duration::from_millis(80)).await;
    let r2 = gw.chat(req()).await.unwrap();
    assert_eq!(
        content(&r2),
        "ok from primary",
        "breaker should have recovered"
    );
}

// Hypothesis: a permanent (client) error is surfaced immediately — no failover, no
// wasted secondary call.
#[tokio::test]
async fn permanent_error_surfaces_without_failover() {
    let (primary, _pf) = provider("primary", false, true); // permanent failure
    let (secondary, sf) = provider("secondary", true, false);
    let gw = Gateway::with_providers(vec![primary, secondary]).with_retry(no_retry());

    let err = gw.chat(req()).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "got {err:?}");
    assert_eq!(sf.calls(), 0, "must not fail over on a client error");
}

// Hypothesis: a warm cache shields callers from a provider outage — identical
// requests stay available (degraded: no fresh generation) and free.
#[tokio::test]
async fn cache_shields_against_provider_outage() {
    let (primary, pf) = provider("primary", true, false);
    let gw = Gateway::with_providers(vec![primary])
        .with_retry(no_retry())
        .with_cache(CacheConfig {
            mode: CacheMode::Exact,
            ttl_ms: 60_000,
        });

    let first = gw.chat(req()).await.unwrap();
    assert_eq!(content(&first), "ok from primary");

    // The provider goes down; the identical request is still served from cache.
    pf.set_healthy(false);
    let second = gw.chat(req()).await.unwrap();
    assert_eq!(content(&second), "ok from primary");
    assert_eq!(second.usage.cost_usd, 0.0, "cache hit is free");
    assert_eq!(pf.calls(), 1, "cache hit must not call the dead provider");
}

// Hypothesis: a total outage fails clearly with a transient error (so callers may
// retry) rather than hanging or panicking.
#[tokio::test]
async fn total_outage_returns_a_clear_transient_error() {
    let (p1, _) = provider("p1", false, false);
    let (p2, _) = provider("p2", false, false);
    let gw = Gateway::with_providers(vec![p1, p2])
        .with_retry(no_retry())
        .with_max_failovers(5);

    let err = gw.chat(req()).await.unwrap_err();
    assert!(matches!(err, Error::Provider(_)), "got {err:?}");
}
