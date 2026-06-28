//! Performance test: LLM Gateway overhead.
//!
//! Asserts the documented NFR — non-cached gateway overhead **< 8 ms p95**
//! ([performance §3](../../../docs/15-testing/performance-tests.md#3-targets-examples),
//! [gateway NFRs](../../../docs/05-llm-gateway/overview.md)). A near-no-op provider
//! isolates the gateway's own dispatch cost (cache miss + breaker + cost path) from
//! model latency, per the methodology's "fake provider with no latency" (§4).
//!
//! In-process dispatch is microsecond-scale, so the 8 ms target has large headroom;
//! the test still measures p50/p95/p99 and prints them as a baseline.

use apex_common::{Result, Usage};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message};
use async_trait::async_trait;
use std::time::Instant;

/// A provider that returns instantly with no real work.
struct NoopProvider;

#[async_trait]
impl AIProvider for NoopProvider {
    fn name(&self) -> &str {
        "noop"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        Ok(ChatResponse {
            message: Message::assistant("ok"),
            model: request.model,
            usage: Usage::new(1, 1, 0.0),
            finish_reason: "stop".to_string(),
        })
    }
}

/// The `pct`-quantile of `samples` (in microseconds).
fn pct(samples: &[u128], q: f64) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = (((sorted.len() as f64) * q) as usize).min(sorted.len() - 1);
    sorted[idx]
}

#[tokio::test]
async fn gateway_overhead_p95_under_8ms() {
    const WARMUP: usize = 100;
    const SAMPLES: usize = 2_000;
    const NFR_P95_US: u128 = 8_000; // 8 ms

    let gw = Gateway::new(Box::new(NoopProvider));
    let request = ChatRequest::new("m", vec![Message::user("hello there")]);

    for _ in 0..WARMUP {
        gw.chat(request.clone()).await.unwrap();
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        gw.chat(request.clone()).await.unwrap();
        samples.push(start.elapsed().as_micros());
    }

    let (p50, p95, p99) = (
        pct(&samples, 0.50),
        pct(&samples, 0.95),
        pct(&samples, 0.99),
    );
    eprintln!("gateway overhead: p50={p50}us p95={p95}us p99={p99}us (NFR p95 < {NFR_P95_US}us)");

    assert!(
        p95 < NFR_P95_US,
        "gateway overhead p95 {p95}us exceeds the {NFR_P95_US}us NFR"
    );
}
