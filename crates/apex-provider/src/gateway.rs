//! The LLM gateway: provider selection, model resolution, and resilience.
//!
//! Agents declare *what kind* of model they need via a [`ModelSelector`]
//! (`capability` + `class`) rather than pinning a vendor model — see the
//! [hello agent](../../docs/16-examples/hello-agent.md) and
//! [routing spec](../../docs/05-llm-gateway/routing.md). The gateway turns that
//! intent into a concrete model on a concrete provider.
//!
//! The gateway holds an **ordered candidate list** of providers and applies the
//! [resilience pipeline](../../docs/05-llm-gateway/resilience.md): retry transient
//! failures against a provider, fail over to the next candidate, and a per-provider
//! [circuit breaker](crate::CircuitBreaker) removes unhealthy upstreams. It also
//! supports a [response cache](../../docs/05-llm-gateway/caching.md) — exact (request
//! hash) and **semantic** (embedding similarity over param-compatible entries, with
//! an exact check first) — and emits [`CostEvent`]s. A single-provider gateway
//! (`new`/`from_env`) is just a candidate list of length one.

use crate::embeddings::{EmbeddingRequest, EmbeddingResponse, cosine_similarity};
use crate::mock::MockProvider;
use crate::openai::OpenAiProvider;
use crate::provider::AIProvider;
use crate::resilience::{
    BreakerConfig, CacheConfig, CacheEntry, CacheMode, CircuitBreaker, CostEvent, CostObserver,
    LocalCircuitBreaker, RetryConfig, SemanticEntry,
};
use crate::types::{ChatRequest, ChatResponse, Role};
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Declarative model requirement: pick a model by capability and class instead
/// of pinning a vendor model id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelSelector {
    /// Required capability, e.g. `chat` or `embeddings`.
    #[serde(default = "default_capability")]
    pub capability: String,
    /// Desired class/tier, e.g. `fast`, `balanced`, `frontier`.
    #[serde(default = "default_class")]
    pub class: String,
}

fn default_capability() -> String {
    "chat".to_string()
}
fn default_class() -> String {
    "fast".to_string()
}

impl Default for ModelSelector {
    fn default() -> Self {
        Self {
            capability: default_capability(),
            class: default_class(),
        }
    }
}

/// Routes chat requests across a resilient candidate list of providers.
pub struct Gateway {
    providers: Vec<Box<dyn AIProvider>>,
    breakers: Vec<Box<dyn CircuitBreaker>>,
    retry: RetryConfig,
    max_failovers: usize,
    cache_cfg: CacheConfig,
    cache: Mutex<HashMap<String, CacheEntry>>,
    semantic_cache: Mutex<Vec<SemanticEntry>>,
    cost: Option<Arc<dyn CostObserver>>,
    start: Instant,
}

impl Gateway {
    /// Construct a single-provider gateway with default resilience settings.
    pub fn new(provider: Box<dyn AIProvider>) -> Self {
        Self::with_providers(vec![provider])
    }

    /// Construct a gateway over an ordered candidate list (primary first).
    pub fn with_providers(providers: Vec<Box<dyn AIProvider>>) -> Self {
        let breakers = providers
            .iter()
            .map(|_| {
                Box::new(LocalCircuitBreaker::new(BreakerConfig::default()))
                    as Box<dyn CircuitBreaker>
            })
            .collect();
        Self {
            providers,
            breakers,
            retry: RetryConfig::default(),
            max_failovers: 2,
            cache_cfg: CacheConfig::default(),
            cache: Mutex::new(HashMap::new()),
            semantic_cache: Mutex::new(Vec::new()),
            cost: None,
            start: Instant::now(),
        }
    }

    /// Build a gateway from the environment.
    ///
    /// Uses the OpenAI-compatible provider when `OPENAI_API_KEY` is set; otherwise
    /// falls back to the offline [`MockProvider`] so local runs work with no setup.
    pub fn from_env() -> Self {
        match OpenAiProvider::from_env() {
            Ok(p) => {
                tracing::info!("llm gateway: using openai-compatible provider");
                Self::new(Box::new(p))
            }
            Err(_) => {
                tracing::info!("llm gateway: OPENAI_API_KEY not set, using mock provider");
                Self::new(Box::new(MockProvider::new()))
            }
        }
    }

    /// Override the retry policy.
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Override the circuit-breaker configuration with in-process breakers
    /// (applied to every provider).
    pub fn with_breaker(mut self, cfg: BreakerConfig) -> Self {
        self.breakers = self
            .providers
            .iter()
            .map(|_| Box::new(LocalCircuitBreaker::new(cfg)) as Box<dyn CircuitBreaker>)
            .collect();
        self
    }

    /// Use **Redis-shared** circuit breakers (one per provider, keyed by provider
    /// name) so a fleet of gateways reacts to a failing provider consistently
    /// ([resilience §6](../../docs/05-llm-gateway/resilience.md)). Requires the
    /// `redis` cargo feature; connects once and shares the multiplexed connection
    /// across all per-provider breakers.
    #[cfg(feature = "redis")]
    pub async fn with_redis_breakers(mut self, url: &str, cfg: BreakerConfig) -> Result<Self> {
        let kv = std::sync::Arc::new(crate::redis_breaker::RedisKv::connect(url).await?)
            as std::sync::Arc<dyn crate::resilience::BreakerKv>;
        self.breakers = self
            .providers
            .iter()
            .map(|p| {
                Box::new(crate::resilience::SharedCircuitBreaker::new(
                    cfg,
                    kv.clone(),
                    format!("apex:breaker:{}", p.name()),
                )) as Box<dyn CircuitBreaker>
            })
            .collect();
        Ok(self)
    }

    /// Override the cache configuration.
    pub fn with_cache(mut self, cache_cfg: CacheConfig) -> Self {
        self.cache_cfg = cache_cfg;
        self
    }

    /// Set the max number of failover hops (default 2).
    pub fn with_max_failovers(mut self, max_failovers: usize) -> Self {
        self.max_failovers = max_failovers;
        self
    }

    /// Register a cost-event observer.
    pub fn with_cost_observer(mut self, observer: Arc<dyn CostObserver>) -> Self {
        self.cost = Some(observer);
        self
    }

    /// Name of the primary provider (for tracing / the run header).
    pub fn provider_name(&self) -> &str {
        self.providers.first().map(|p| p.name()).unwrap_or("none")
    }

    /// Resolve a selector (and optional pinned model) to a concrete model id.
    ///
    /// A pinned model always wins. Otherwise the class maps to a default model id
    /// per provider; unknown providers fall back to a generic `chat` class name.
    pub fn resolve_model(&self, pinned: Option<&str>, selector: &ModelSelector) -> String {
        if let Some(model) = pinned {
            return model.to_string();
        }
        match self.provider_name() {
            "openai" => match selector.class.as_str() {
                "balanced" | "frontier" => "gpt-4o",
                _ => "gpt-4o-mini",
            }
            .to_string(),
            // Mock and unknown providers echo a descriptive synthetic id.
            other => format!("{other}-{}-{}", selector.capability, selector.class),
        }
    }

    /// Resolve the embedding model to use.
    pub fn resolve_embedding_model(&self, pinned: Option<&str>) -> String {
        if let Some(model) = pinned {
            return model.to_string();
        }
        match self.provider_name() {
            "openai" => "text-embedding-3-small".to_string(),
            other => format!("{other}-embeddings"),
        }
    }

    /// Milliseconds since this gateway was created (monotonic clock for breakers).
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Execute a chat completion with caching, retry, failover, and circuit breaking.
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        // 1. Exact lookup — both Exact and Semantic modes check it first because it
        //    is cheaper and stronger ([caching §6](../../docs/05-llm-gateway/caching.md)).
        if self.cache_cfg.mode != CacheMode::Off
            && let Some(hit) = self.cache_lookup(&request)
        {
            self.emit_cost_hit(&hit, "exact");
            return Ok(zero_cost(hit));
        }

        // 1b. Semantic lookup — embed the request once and reuse the vector to store
        //     on a miss. Embedding failures degrade gracefully to a live call.
        let query_vec = if self.cache_cfg.mode == CacheMode::Semantic {
            self.embed_canonical(&request).await
        } else {
            None
        };
        if let Some(qv) = &query_vec
            && let Some(hit) = self.semantic_lookup(&request, qv)
        {
            self.emit_cost_hit(&hit, "semantic");
            return Ok(zero_cost(hit));
        }

        // 2. Resilient dispatch across the candidate list.
        let mut last_err: Option<Error> = None;
        let mut hops = 0usize;

        for i in 0..self.providers.len() {
            if hops > self.max_failovers {
                break;
            }
            if !self.breakers[i].allow(self.now_ms()).await {
                // Provider circuit is open; skip without counting a hop.
                continue;
            }
            hops += 1;

            match self.try_provider(i, &request).await {
                Ok(response) => {
                    // Store on a miss: exact for both modes, plus the semantic index
                    // (reusing the embedding computed above) for Semantic mode.
                    if self.cache_cfg.mode != CacheMode::Off {
                        self.cache_store(&request, &response);
                    }
                    if let Some(qv) = &query_vec {
                        self.semantic_store(&request, qv, &response);
                    }
                    self.emit_cost_live(&response);
                    return Ok(response);
                }
                Err((err, transient)) => {
                    if !transient {
                        // Permanent error (bad request/auth): failover won't help.
                        return Err(err);
                    }
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| Error::provider("no healthy providers available")))
    }

    /// Try one provider with retry. Returns `Err((error, transient))` where
    /// `transient` indicates whether failover to the next provider is warranted.
    async fn try_provider(
        &self,
        index: usize,
        request: &ChatRequest,
    ) -> std::result::Result<ChatResponse, (Error, bool)> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.providers[index].chat(request.clone()).await {
                Ok(response) => {
                    self.breakers[index].on_success().await;
                    return Ok(response);
                }
                Err(err) => {
                    let transient = is_transient(&err);
                    if !transient {
                        // Don't trip the breaker on client errors.
                        return Err((err, false));
                    }
                    self.breakers[index].on_failure(self.now_ms()).await;
                    if attempt < self.retry.max_attempts {
                        tokio::time::sleep(self.retry.backoff(attempt)).await;
                        continue;
                    }
                    return Err((err, true));
                }
            }
        }
    }

    /// Embed one or more texts via the primary provider.
    pub async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let provider = self
            .providers
            .first()
            .ok_or_else(|| Error::provider("no providers configured"))?;
        provider.embed(request).await
    }

    // --- cache helpers -----------------------------------------------------

    fn cache_lookup(&self, request: &ChatRequest) -> Option<ChatResponse> {
        let key = cache_key(request);
        let cache = self.cache.lock().expect("cache mutex poisoned");
        let entry = cache.get(&key)?;
        if self.now_ms().saturating_sub(entry.created_ms) > self.cache_cfg.ttl_ms {
            return None;
        }
        Some(entry.response.clone())
    }

    fn cache_store(&self, request: &ChatRequest, response: &ChatResponse) {
        let key = cache_key(request);
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        cache.insert(
            key,
            CacheEntry {
                response: response.clone(),
                created_ms: self.now_ms(),
            },
        );
    }

    // --- semantic cache helpers --------------------------------------------

    /// Embed the canonical request (its user turns) for semantic lookup/store.
    /// Returns `None` when there is nothing to embed or embedding fails.
    async fn embed_canonical(&self, request: &ChatRequest) -> Option<Vec<f32>> {
        let text = canonical_request(request);
        if text.is_empty() {
            return None;
        }
        let model = self.resolve_embedding_model(None);
        match self.embed(EmbeddingRequest::new(model, vec![text])).await {
            Ok(resp) => resp.vectors.into_iter().next(),
            Err(e) => {
                tracing::warn!("semantic cache: embedding failed, serving live: {e}");
                None
            }
        }
    }

    /// Best param-compatible entry whose embedding similarity clears the threshold.
    fn semantic_lookup(&self, request: &ChatRequest, query: &[f32]) -> Option<ChatResponse> {
        let pk = param_key(request);
        let now = self.now_ms();
        let store = self.semantic_cache.lock().expect("cache mutex poisoned");
        let mut best: Option<(f32, &SemanticEntry)> = None;
        for entry in store.iter() {
            if entry.param_key != pk || now.saturating_sub(entry.created_ms) > self.cache_cfg.ttl_ms
            {
                continue;
            }
            let sim = cosine_similarity(query, &entry.embedding);
            if sim >= self.cache_cfg.similarity_threshold && best.is_none_or(|(b, _)| sim > b) {
                best = Some((sim, entry));
            }
        }
        best.map(|(_, e)| e.response.clone())
    }

    fn semantic_store(&self, request: &ChatRequest, query: &[f32], response: &ChatResponse) {
        let mut store = self.semantic_cache.lock().expect("cache mutex poisoned");
        store.push(SemanticEntry {
            embedding: query.to_vec(),
            param_key: param_key(request),
            response: response.clone(),
            created_ms: self.now_ms(),
        });
    }

    // --- cost events -------------------------------------------------------

    fn emit_cost_live(&self, response: &ChatResponse) {
        if let Some(obs) = &self.cost {
            obs.on_cost(CostEvent {
                provider: self.provider_name().to_string(),
                model: response.model.clone(),
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                cost_usd: response.usage.cost_usd,
                cache: None,
                estimated_savings_usd: 0.0,
            });
        }
    }

    /// Emit a savings event for a cache hit; `kind` is the disposition
    /// (`"exact"` or `"semantic"`).
    fn emit_cost_hit(&self, response: &ChatResponse, kind: &str) {
        if let Some(obs) = &self.cost {
            obs.on_cost(CostEvent {
                provider: self.provider_name().to_string(),
                model: response.model.clone(),
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                cost_usd: 0.0,
                cache: Some(kind.to_string()),
                estimated_savings_usd: response.usage.cost_usd,
            });
        }
    }
}

/// The canonical text for semantic matching: the request's user turns concatenated
/// ([caching §4](../../docs/05-llm-gateway/caching.md)).
fn canonical_request(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| m.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parameter-compatibility key: a semantic entry may only be served for a request
/// with the same model and temperature ([caching §4](../../docs/05-llm-gateway/caching.md)).
fn param_key(request: &ChatRequest) -> String {
    format!("{}|{:?}", request.model, request.temperature)
}

/// Whether an error is a transient provider failure (retry + failover) versus a
/// permanent client error ([resilience §8](../../docs/05-llm-gateway/resilience.md)).
fn is_transient(err: &Error) -> bool {
    matches!(err, Error::Provider(_))
}

/// Return a clone of `response` with cost zeroed (used for cache hits).
fn zero_cost(mut response: ChatResponse) -> ChatResponse {
    response.usage.cost_usd = 0.0;
    response
}

/// Stable exact-cache key over the output-affecting request fields
/// ([caching §3](../../docs/05-llm-gateway/caching.md)).
fn cache_key(request: &ChatRequest) -> String {
    let messages = serde_json::to_string(&request.messages).unwrap_or_default();
    let tools = serde_json::to_string(&request.tools).unwrap_or_default();
    format!(
        "{}|{:?}|{:?}|{messages}|{tools}",
        request.model, request.temperature, request.max_tokens
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resilience::CostEvent;
    use crate::types::Message;
    use apex_common::Usage;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn pinned_model_wins() {
        let gw = Gateway::new(Box::new(MockProvider::new()));
        assert_eq!(
            gw.resolve_model(Some("gpt-5"), &ModelSelector::default()),
            "gpt-5"
        );
    }

    #[test]
    fn mock_selector_resolves_descriptively() {
        let gw = Gateway::new(Box::new(MockProvider::new()));
        assert_eq!(
            gw.resolve_model(None, &ModelSelector::default()),
            "mock-chat-fast"
        );
    }

    #[test]
    fn resolves_embedding_model() {
        let gw = Gateway::new(Box::new(MockProvider::new()));
        assert_eq!(gw.resolve_embedding_model(None), "mock-embeddings");
        assert_eq!(gw.resolve_embedding_model(Some("custom")), "custom");
    }

    /// A provider that fails a configurable number of times, then succeeds.
    struct FlakyProvider {
        name: &'static str,
        fail_n: usize,
        calls: AtomicUsize,
        permanent: bool,
    }

    impl FlakyProvider {
        fn new(name: &'static str, fail_n: usize, permanent: bool) -> Self {
            Self {
                name,
                fail_n,
                calls: AtomicUsize::new(0),
                permanent,
            }
        }
    }

    #[async_trait]
    impl AIProvider for FlakyProvider {
        fn name(&self) -> &str {
            self.name
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                return Err(if self.permanent {
                    Error::invalid("bad request")
                } else {
                    Error::provider("503 unavailable")
                });
            }
            Ok(ChatResponse {
                message: Message::assistant(format!("ok from {}", self.name)),
                model: request.model,
                usage: Usage::new(3, 2, 0.01),
                finish_reason: "stop".to_string(),
            })
        }
    }

    fn req() -> ChatRequest {
        ChatRequest::new("m", vec![Message::user("hi")])
    }

    fn fast_retry() -> RetryConfig {
        RetryConfig {
            max_attempts: 2,
            base_delay_ms: 1,
            max_delay_ms: 1,
        }
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        // Fails once (transient), succeeds on retry — no failover needed.
        let gw = Gateway::with_providers(vec![Box::new(FlakyProvider::new("p", 1, false))])
            .with_retry(fast_retry());
        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from p"));
    }

    #[tokio::test]
    async fn fails_over_to_healthy_provider() {
        // Primary always fails (transient); secondary succeeds.
        let gw = Gateway::with_providers(vec![
            Box::new(FlakyProvider::new("primary", 999, false)),
            Box::new(FlakyProvider::new("secondary", 0, false)),
        ])
        .with_retry(fast_retry());
        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from secondary"));
    }

    #[tokio::test]
    async fn permanent_error_does_not_failover() {
        // Primary returns a client error; gateway must not try the secondary.
        let secondary = Box::new(FlakyProvider::new("secondary", 0, false));
        let gw = Gateway::with_providers(vec![
            Box::new(FlakyProvider::new("primary", 999, true)),
            secondary,
        ])
        .with_retry(fast_retry());
        let err = gw.chat(req()).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[tokio::test]
    async fn exact_cache_returns_zero_cost_and_emits_savings() {
        struct Collector(Mutex<Vec<CostEvent>>);
        impl CostObserver for Collector {
            fn on_cost(&self, event: CostEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let provider = FlakyProvider::new("p", 0, false);

        let gw = Gateway::with_providers(vec![Box::new(provider)])
            .with_cache(CacheConfig {
                mode: CacheMode::Exact,
                ttl_ms: 60_000,
                ..CacheConfig::default()
            })
            .with_cost_observer(collector.clone());

        let first = gw.chat(req()).await.unwrap();
        assert!(first.usage.cost_usd > 0.0, "live call has cost");
        let second = gw.chat(req()).await.unwrap();
        assert_eq!(second.usage.cost_usd, 0.0, "cache hit is free");

        let events = collector.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].cache, None);
        assert_eq!(events[1].cache.as_deref(), Some("exact"));
        assert!(events[1].estimated_savings_usd > 0.0);
    }

    /// Collects cost events for the semantic-cache tests.
    struct Collector(Mutex<Vec<CostEvent>>);
    impl CostObserver for Collector {
        fn on_cost(&self, event: CostEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    fn semantic_gw(threshold: f32, observer: Arc<Collector>) -> Gateway {
        // MockProvider supports embeddings (deterministic, so identical canonical
        // text yields an identical vector → cosine 1.0).
        Gateway::with_providers(vec![Box::new(MockProvider::new())])
            .with_cache(CacheConfig {
                mode: CacheMode::Semantic,
                ttl_ms: 60_000,
                similarity_threshold: threshold,
            })
            .with_cost_observer(observer)
    }

    /// Same user turn but a different system prompt: the exact key differs (a miss)
    /// while the canonical text matches → a semantic hit serving the first response.
    #[tokio::test]
    async fn semantic_cache_hits_on_meaning_match_after_exact_miss() {
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(0.9, collector.clone());

        let a = ChatRequest::new("m", vec![Message::system("you are A"), Message::user("hi")]);
        let b = ChatRequest::new("m", vec![Message::system("you are B"), Message::user("hi")]);

        let first = gw.chat(a).await.unwrap();
        assert!(first.usage.cost_usd > 0.0, "live call has cost");
        let second = gw.chat(b).await.unwrap();
        assert_eq!(second.usage.cost_usd, 0.0, "semantic hit is free");
        assert_eq!(
            second.message.content, first.message.content,
            "the cached response is served"
        );

        let events = collector.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].cache, None, "first is a live call");
        assert_eq!(events[1].cache.as_deref(), Some("semantic"));
        assert!(events[1].estimated_savings_usd > 0.0);
    }

    /// A matching meaning but incompatible params (different temperature) is a miss.
    #[tokio::test]
    async fn semantic_cache_respects_param_compatibility() {
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(0.9, collector.clone());

        let mut a = ChatRequest::new("m", vec![Message::user("same text")]);
        a.temperature = Some(0.0);
        let mut b = ChatRequest::new("m", vec![Message::user("same text")]);
        b.temperature = Some(0.7);

        gw.chat(a).await.unwrap();
        let second = gw.chat(b).await.unwrap();
        assert!(
            second.usage.cost_usd > 0.0,
            "different temperature is not param-compatible → live call"
        );
    }

    /// The threshold gates hits: an unreachable threshold (>1.0) misses even an
    /// identical-meaning request.
    #[tokio::test]
    async fn semantic_cache_threshold_gates_hits() {
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(1.01, collector.clone());

        let a = ChatRequest::new("m", vec![Message::system("x"), Message::user("hi")]);
        let b = ChatRequest::new("m", vec![Message::system("y"), Message::user("hi")]);

        gw.chat(a).await.unwrap();
        let second = gw.chat(b).await.unwrap();
        assert!(
            second.usage.cost_usd > 0.0,
            "similarity below threshold → live call"
        );
    }
}
