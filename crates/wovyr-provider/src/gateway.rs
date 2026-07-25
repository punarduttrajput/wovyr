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
//! an exact check first) — optional **request hedging** (race a slow candidate
//! against its successors), and emits [`CostEvent`]s. A single-provider gateway
//! (`new`/`from_env`) is just a candidate list of length one.

use crate::anthropic::AnthropicProvider;
use crate::embeddings::{EmbeddingRequest, EmbeddingResponse};
use crate::image::{ImageGenRequest, ImageGenResponse};
use crate::mock::MockProvider;
use crate::openai::OpenAiProvider;
use crate::provider::AIProvider;
use crate::resilience::{
    BreakerConfig, CacheConfig, CacheMode, CircuitBreaker, CostEvent, CostObserver, ExactCache,
    HedgeConfig, InMemorySemanticCache, Jitter, LocalCircuitBreaker, RandomJitter, RetryConfig,
    SemanticCacheStore,
};
use crate::types::{ChatRequest, ChatResponse, Role};
use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wovyr_common::{Error, Result};

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
    cache: Mutex<ExactCache>,
    semantic_cache: Box<dyn SemanticCacheStore>,
    /// Optional dedicated embedding provider (RM-AR-P1 AIC-301), resolved
    /// independently of the chat provider chain: a deployment whose chat
    /// provider can't embed (e.g. Anthropic) can still serve memory/RAG by
    /// attaching an embedding-capable provider here. `None` = embeddings use
    /// the primary provider, as before.
    embedding_provider: Option<Box<dyn AIProvider>>,
    hedge_cfg: HedgeConfig,
    cost: Option<Arc<dyn CostObserver>>,
    start: Instant,
    /// Jitter source for retry backoff (RM-AIM-P2 PRV-205); defaults to
    /// [`RandomJitter`], overridable via [`with_jitter`](Self::with_jitter) for
    /// deterministic tests.
    jitter: Arc<dyn Jitter>,
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
            cache: Mutex::new(ExactCache::new(CacheConfig::default().max_entries)),
            semantic_cache: Box::new(InMemorySemanticCache::new()),
            embedding_provider: None,
            hedge_cfg: HedgeConfig::default(),
            cost: None,
            start: Instant::now(),
            jitter: Arc::new(RandomJitter),
        }
    }

    /// Build a gateway from the environment.
    ///
    /// Uses the OpenAI-compatible provider when `OPENAI_API_KEY` is set (the
    /// original behavior, kept first for back-compat), then the native
    /// [`AnthropicProvider`] when `ANTHROPIC_API_KEY` is set (RM-AIM-P2
    /// PRV-201); otherwise falls back to the offline [`MockProvider`] so local
    /// runs work with no setup.
    pub fn from_env() -> Self {
        if let Ok(p) = OpenAiProvider::from_env() {
            tracing::info!("llm gateway: using openai-compatible provider");
            return Self::new(Box::new(p));
        }
        if let Ok(p) = AnthropicProvider::from_env() {
            tracing::info!("llm gateway: using anthropic provider");
            return Self::new(Box::new(p));
        }
        tracing::info!("llm gateway: no API key set, using mock provider");
        Self::new(Box::new(MockProvider::new()))
    }

    /// Override the retry policy.
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Override the retry-backoff jitter source (RM-AIM-P2 PRV-205). Tests use
    /// this to make backoff delays deterministic; production leaves the default
    /// [`RandomJitter`].
    pub fn with_jitter(mut self, jitter: impl Jitter + 'static) -> Self {
        self.jitter = Arc::new(jitter);
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
                    format!("wovyr:breaker:{}", p.name()),
                )) as Box<dyn CircuitBreaker>
            })
            .collect();
        Ok(self)
    }

    /// Override the cache configuration. Rebuilds the exact cache with the
    /// configured entry cap (RM-AR-P1 AIC-302).
    pub fn with_cache(mut self, cache_cfg: CacheConfig) -> Self {
        self.cache_cfg = cache_cfg;
        self.cache = Mutex::new(ExactCache::new(cache_cfg.max_entries));
        self
    }

    /// Enable request hedging ([resilience §7](../../docs/05-llm-gateway/resilience.md)):
    /// race slow candidates against their successors to cut tail latency.
    pub fn with_hedging(mut self, hedge_cfg: HedgeConfig) -> Self {
        self.hedge_cfg = hedge_cfg;
        self
    }

    /// Override the semantic-cache backend (default in-process). Pass a distributed
    /// store to share semantic cache entries across a fleet of gateways.
    pub fn with_semantic_store(mut self, store: Box<dyn SemanticCacheStore>) -> Self {
        self.semantic_cache = store;
        self
    }

    /// Use a **Qdrant-backed** distributed semantic cache ([caching §4](../../docs/05-llm-gateway/caching.md))
    /// so the whole fleet shares cached responses. Requires the `qdrant` cargo feature.
    #[cfg(feature = "qdrant")]
    pub fn with_qdrant_semantic_cache(self, url: &str, collection: &str) -> Self {
        self.with_semantic_store(Box::new(crate::semantic_qdrant::QdrantSemanticCache::new(
            url, collection,
        )))
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

    /// Attach a dedicated embedding provider (RM-AR-P1 AIC-301), used by
    /// [`embed`](Self::embed) in place of the primary provider — so a
    /// deployment whose chat provider can't embed (e.g. Anthropic) can still
    /// serve memory/RAG by resolving embeddings through a separate,
    /// embedding-capable provider.
    pub fn with_embedding_provider(mut self, provider: Box<dyn AIProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// The provider [`embed`](Self::embed) will route to: the dedicated
    /// embedding provider if one is attached, else the primary.
    fn embed_provider(&self) -> Option<&dyn AIProvider> {
        self.embedding_provider
            .as_deref()
            .or_else(|| self.providers.first().map(|p| p.as_ref()))
    }

    /// Whether this gateway can produce embeddings (RM-AR-P1 AIC-301) — true iff
    /// the provider `embed` would route to supports embeddings. Lets a
    /// memory/RAG consumer fail loud at construction on an embedding-less
    /// deployment (e.g. Anthropic-only) instead of erroring per-call mid-run.
    pub fn supports_embeddings(&self) -> bool {
        self.embed_provider()
            .is_some_and(|p| p.supports_embeddings())
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
            "anthropic" => match selector.class.as_str() {
                "frontier" => "claude-opus-4-8",
                "balanced" => "claude-sonnet-5",
                _ => "claude-haiku-4-5",
            }
            .to_string(),
            // Mock and unknown providers echo a descriptive synthetic id.
            other => format!("{other}-{}-{}", selector.capability, selector.class),
        }
    }

    /// Resolve the embedding model to use — keyed off the provider `embed`
    /// actually routes to (the dedicated embedding provider if attached, else
    /// the primary), so a distinct embedder picks its own model id rather than
    /// the chat provider's.
    pub fn resolve_embedding_model(&self, pinned: Option<&str>) -> String {
        if let Some(model) = pinned {
            return model.to_string();
        }
        let name = self.embed_provider().map(|p| p.name()).unwrap_or("none");
        match name {
            "openai" => "text-embedding-3-small".to_string(),
            other => format!("{other}-embeddings"),
        }
    }

    /// Milliseconds since this gateway was created (monotonic clock for breakers).
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Execute a chat completion with caching, retry, failover, and circuit breaking.
    #[tracing::instrument(
        name = "gateway.chat",
        skip_all,
        fields(provider = self.provider_name(), model = %request.model)
    )]
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
        //     on a miss. Embedding failures degrade gracefully to a live call. The
        //     resolved embedding-model id rides along on every lookup/store so an
        //     entry is only ever compared against vectors from the same model
        //     (RM-AIM-P2 RAG-203).
        let embedding_model = self.resolve_embedding_model(None);
        let query_vec = if self.cache_cfg.mode == CacheMode::Semantic {
            self.embed_canonical(&request, &embedding_model).await
        } else {
            None
        };
        if let Some(qv) = &query_vec {
            match self
                .semantic_cache
                .lookup(
                    &param_key(&request),
                    &embedding_model,
                    qv,
                    self.cache_cfg.similarity_threshold,
                    self.cache_cfg.ttl_ms,
                    self.now_ms(),
                )
                .await
            {
                Ok(Some(hit)) => {
                    self.emit_cost_hit(&hit, "semantic");
                    return Ok(zero_cost(hit));
                }
                Ok(None) => {}
                // A cache backend error must never fail the request — serve live.
                Err(e) => tracing::warn!("semantic cache lookup failed, serving live: {e}"),
            }
        }

        // 2. Resilient dispatch — racing (hedged) or sequential failover.
        let response = if self.hedge_cfg.enabled {
            self.dispatch_hedged(&request).await
        } else {
            self.dispatch_sequential(&request).await
        }?;

        // 3. On a live result: store on the miss and meter the cost.
        if self.cache_cfg.mode != CacheMode::Off {
            self.cache_store(&request, &response);
        }
        if let Some(qv) = &query_vec
            && let Err(e) = self
                .semantic_cache
                .store(
                    &param_key(&request),
                    &embedding_model,
                    qv,
                    &response,
                    self.now_ms(),
                )
                .await
        {
            tracing::warn!("semantic cache store failed: {e}");
        }
        self.emit_cost_live(&response);
        Ok(response)
    }

    /// Stream a chat completion. Establishes the stream against the candidate list
    /// with failover + circuit breaking (like [`chat`](Self::chat)), then emits a
    /// cost event when the stream completes. The response cache and per-call retry do
    /// not apply to streams (the standard streaming tradeoff).
    pub async fn chat_stream(&self, request: ChatRequest) -> Result<crate::provider::ChatStream> {
        use futures::StreamExt;

        let mut last_err: Option<Error> = None;
        let mut hops = 0usize;
        for i in 0..self.providers.len() {
            if hops > self.max_failovers {
                break;
            }
            if !self.breakers[i].allow(self.now_ms()).await {
                continue;
            }
            hops += 1;
            match self.providers[i].chat_stream(request.clone()).await {
                Ok(stream) => {
                    self.breakers[i].on_success().await;
                    let cost = self.cost.clone();
                    let provider = self.provider_name().to_string();
                    // Meter the cost when the terminal `Done` event passes through.
                    let metered = stream.map(move |event| {
                        if let (Ok(crate::provider::ChatStreamEvent::Done(resp)), Some(obs)) =
                            (&event, &cost)
                        {
                            obs.on_cost(CostEvent {
                                provider: provider.clone(),
                                model: resp.model.clone(),
                                prompt_tokens: resp.usage.prompt_tokens,
                                completion_tokens: resp.usage.completion_tokens,
                                cost_usd: resp.usage.cost_usd,
                                cache: None,
                                estimated_savings_usd: 0.0,
                            });
                        }
                        event
                    });
                    return Ok(Box::pin(metered));
                }
                Err(err) => {
                    if !is_transient(&err) {
                        return Err(err);
                    }
                    self.breakers[i].on_failure(self.now_ms()).await;
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::provider("no healthy providers available")))
    }

    /// Sequential failover: try each healthy candidate in order, moving on only when
    /// one returns a transient error.
    async fn dispatch_sequential(&self, request: &ChatRequest) -> Result<ChatResponse> {
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

            match self.try_provider(i, request).await {
                Ok(response) => return Ok(response),
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

    /// Hedged dispatch: launch the primary, and each time `delay_ms` elapses with no
    /// answer (or a candidate fails transiently) launch the next healthy candidate,
    /// racing up to `max_parallel` in flight. Returns the first success; a permanent
    /// error short-circuits. Losing calls are cancelled when this future returns.
    async fn dispatch_hedged(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let delay = Duration::from_millis(self.hedge_cfg.delay_ms);
        let mut inflight = FuturesUnordered::new();
        let mut next = 0usize; // next provider index to consider
        let mut launched = 0usize; // distinct candidates dispatched so far
        let mut last_err: Option<Error> = None;

        loop {
            // Fill up to the concurrency / failover budget, skipping open breakers.
            while inflight.len() < self.hedge_cfg.max_parallel
                && launched <= self.max_failovers
                && next < self.providers.len()
            {
                let i = next;
                next += 1;
                if !self.breakers[i].allow(self.now_ms()).await {
                    continue;
                }
                launched += 1;
                inflight.push(self.try_provider(i, request));
                break; // launch one, then wait (the hedge delay staggers the rest)
            }

            if inflight.is_empty() {
                return Err(
                    last_err.unwrap_or_else(|| Error::provider("no healthy providers available"))
                );
            }

            // More candidates we could still hedge to after the delay?
            let can_hedge = inflight.len() < self.hedge_cfg.max_parallel
                && launched <= self.max_failovers
                && next < self.providers.len();

            tokio::select! {
                // Bias toward completed responses over launching another hedge.
                biased;
                result = inflight.next() => match result {
                    Some(Ok(response)) => return Ok(response),
                    Some(Err((err, transient))) => {
                        if !transient {
                            return Err(err);
                        }
                        // Failed fast — loop to launch the next candidate immediately.
                        last_err = Some(err);
                    }
                    None => unreachable!("inflight is non-empty"),
                },
                _ = tokio::time::sleep(delay), if can_hedge => {
                    // Delay elapsed with no answer → loop to launch a hedge.
                }
            }
        }
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
                        // A server-specified Retry-After overrides our own backoff
                        // entirely (RM-AIM-P2 PRV-205 / resilience §4); otherwise
                        // fall back to jittered exponential backoff.
                        let delay = retry_after(&err).unwrap_or_else(|| {
                            self.retry.backoff_with_jitter(attempt, &*self.jitter)
                        });
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err((err, true));
                }
            }
        }
    }

    /// Embed one or more texts via the dedicated embedding provider if one is
    /// attached ([`with_embedding_provider`](Self::with_embedding_provider)),
    /// else the primary provider.
    pub async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let provider = self
            .embed_provider()
            .ok_or_else(|| Error::provider("no providers configured"))?;
        provider.embed(request).await
    }

    /// Generate one or more images via the primary provider.
    ///
    /// Deliberately as simple as [`embed`](Self::embed) — no retry/failover/
    /// cache/cost-metering pipeline — since [`CostEvent`] is token-shaped and
    /// doesn't fit an image call, and `embed` (the other non-chat capability)
    /// sets the precedent of a plain pass-through rather than forcing image
    /// generation through machinery built for token-priced chat completions.
    pub async fn generate_image(&self, request: ImageGenRequest) -> Result<ImageGenResponse> {
        let provider = self
            .providers
            .first()
            .ok_or_else(|| Error::provider("no providers configured"))?;
        provider.generate_image(request).await
    }

    // --- cache helpers -----------------------------------------------------

    fn cache_lookup(&self, request: &ChatRequest) -> Option<ChatResponse> {
        let key = cache_key(request);
        let now = self.now_ms();
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        cache.get(&key, now, self.cache_cfg.ttl_ms)
    }

    fn cache_store(&self, request: &ChatRequest, response: &ChatResponse) {
        let key = cache_key(request);
        let now = self.now_ms();
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        cache.insert(key, response.clone(), now, self.cache_cfg.ttl_ms);
    }

    // --- semantic cache helpers --------------------------------------------

    /// Embed the canonical request (its user turns) with `embedding_model` for
    /// semantic lookup/store. Returns `None` when there is nothing to embed or
    /// embedding fails.
    async fn embed_canonical(
        &self,
        request: &ChatRequest,
        embedding_model: &str,
    ) -> Option<Vec<f32>> {
        let text = canonical_request(request);
        if text.is_empty() {
            return None;
        }
        match self
            .embed(EmbeddingRequest::new(embedding_model, vec![text]))
            .await
        {
            Ok(resp) => resp.vectors.into_iter().next(),
            Err(e) => {
                tracing::warn!("semantic cache: embedding failed, serving live: {e}");
                None
            }
        }
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
/// ([caching §4](../../docs/05-llm-gateway/caching.md)). Deliberately user turns
/// only — the *similarity* signal is what the user asked; the system prompt and
/// tool set are compatibility constraints, enforced exactly via [`param_key`]
/// rather than diluted into the embedding (RM-AIM-P2 RAG-203).
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
/// with the same model, temperature, output-shaping constraints, **system
/// prompt, and tool set** ([caching §4](../../docs/05-llm-gateway/caching.md)).
/// A response produced under a different `tool_choice`/`response_format`
/// (PRV-202) has a different shape, and one produced under a different system
/// prompt or tool set answers a different *context* (RM-AIM-P2 RAG-203 — the
/// same user text with a different system prompt used to wrongly hit). The
/// system/tools text is embedded verbatim rather than hashed: exact,
/// dependency-free, and stable across processes/builds — which a
/// `DefaultHasher` digest is not guaranteed to be, and the Qdrant backend
/// shares these keys across a fleet.
fn param_key(request: &ChatRequest) -> String {
    let system = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(|m| m.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    let tools = serde_json::to_string(&request.tools).unwrap_or_default();
    format!(
        "{}|{:?}|{:?}|{:?}|sys:{system}|tools:{tools}",
        request.model, request.temperature, request.tool_choice, request.response_format
    )
}

/// Whether an error is a transient provider failure (retry + failover) versus a
/// permanent client error ([resilience §8](../../docs/05-llm-gateway/resilience.md)).
fn is_transient(err: &Error) -> bool {
    matches!(err, Error::Provider { .. })
}

/// A server-specified retry delay carried on the error (RM-AIM-P2 PRV-205 — e.g.
/// parsed from a `Retry-After` header on an HTTP 429/503), if any.
fn retry_after(err: &Error) -> Option<Duration> {
    match err {
        Error::Provider {
            retry_after_ms: Some(ms),
            ..
        } => Some(Duration::from_millis(*ms)),
        _ => None,
    }
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
        "{}|{:?}|{:?}|{:?}|{:?}|{messages}|{tools}",
        request.model,
        request.temperature,
        request.max_tokens,
        request.tool_choice,
        request.response_format
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resilience::CostEvent;
    use crate::types::Message;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wovyr_common::Usage;

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
    fn anthropic_selector_resolves_by_class() {
        let gw = Gateway::new(Box::new(AnthropicProvider::new("http://localhost", "k")));
        let sel = |class: &str| ModelSelector {
            capability: "chat".to_string(),
            class: class.to_string(),
        };
        assert_eq!(gw.resolve_model(None, &sel("fast")), "claude-haiku-4-5");
        assert_eq!(gw.resolve_model(None, &sel("balanced")), "claude-sonnet-5");
        assert_eq!(gw.resolve_model(None, &sel("frontier")), "claude-opus-4-8");
        assert_eq!(gw.resolve_model(Some("pinned"), &sel("fast")), "pinned");
    }

    #[test]
    fn resolves_embedding_model() {
        let gw = Gateway::new(Box::new(MockProvider::new()));
        assert_eq!(gw.resolve_embedding_model(None), "mock-embeddings");
        assert_eq!(gw.resolve_embedding_model(Some("custom")), "custom");
    }

    // --- RM-AR-P1 AIC-301: embedding-capability detection --------------------

    #[test]
    fn supports_embeddings_reflects_the_effective_provider() {
        // Mock embeds (deterministically).
        assert!(Gateway::new(Box::new(MockProvider::new())).supports_embeddings());

        // An Anthropic-class chat provider does not — the broken Anthropic-only
        // deployment the fail-loud check exists for (no network call: the
        // capability is a pure trait method).
        let anthropic = Gateway::new(Box::new(AnthropicProvider::new(
            "https://example.invalid",
            "test-key",
        )));
        assert!(!anthropic.supports_embeddings());
        assert_eq!(
            anthropic.resolve_embedding_model(None),
            "anthropic-embeddings"
        );
    }

    #[test]
    fn a_dedicated_embedding_provider_makes_a_chat_only_gateway_embed() {
        // Anthropic for chat, a distinct embedding-capable provider attached:
        // embeddings resolve through the latter, keyed to its own model id.
        let gw = Gateway::new(Box::new(AnthropicProvider::new(
            "https://example.invalid",
            "test-key",
        )))
        .with_embedding_provider(Box::new(MockProvider::new()));
        assert!(gw.supports_embeddings());
        assert_eq!(gw.resolve_embedding_model(None), "mock-embeddings");
        // The chat provider is still Anthropic.
        assert_eq!(gw.provider_name(), "anthropic");
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

    /// RM-AIM-P2 PRV-205 acceptance: retry backoff is jittered, not raw
    /// exponential. A huge retry config would sleep seconds if the exponential
    /// cap were honored directly; with an injected [`crate::resilience::FixedJitter`]
    /// the gateway sleeps for exactly the fixed value instead.
    #[tokio::test(start_paused = true)]
    async fn retry_backoff_uses_the_injected_jitter_source() {
        let gw = Gateway::with_providers(vec![Box::new(FlakyProvider::new("p", 1, false))])
            .with_retry(RetryConfig {
                max_attempts: 2,
                base_delay_ms: 10_000,
                max_delay_ms: 10_000,
            })
            .with_jitter(crate::resilience::FixedJitter(37));

        let start = tokio::time::Instant::now();
        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from p"));
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(37)
        );
    }

    /// A provider whose one failure carries a `Retry-After` hint (RM-AIM-P2 PRV-205).
    struct RetryAfterOnceProvider {
        retry_after_ms: u64,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AIProvider for RetryAfterOnceProvider {
        fn name(&self) -> &str {
            "p"
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(Error::provider_with_retry_after(
                    "429 rate limited",
                    self.retry_after_ms,
                ));
            }
            Ok(ChatResponse {
                message: Message::assistant("ok"),
                model: request.model,
                usage: Usage::new(3, 2, 0.01),
                finish_reason: "stop".to_string(),
            })
        }
    }

    /// RM-AIM-P2 PRV-205 acceptance: a server-specified `Retry-After` overrides
    /// the gateway's own backoff entirely. The retry config's exponential cap is
    /// huge (10s); if it were honored the request would take seconds — instead
    /// the gateway sleeps for exactly the hinted 50ms.
    #[tokio::test(start_paused = true)]
    async fn retry_after_hint_overrides_backoff() {
        let provider = RetryAfterOnceProvider {
            retry_after_ms: 50,
            calls: AtomicUsize::new(0),
        };
        let gw = Gateway::with_providers(vec![Box::new(provider)]).with_retry(RetryConfig {
            max_attempts: 2,
            base_delay_ms: 10_000,
            max_delay_ms: 10_000,
        });

        let start = tokio::time::Instant::now();
        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok"));
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(50)
        );
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
    async fn chat_stream_emits_chunks_and_meters_cost() {
        use crate::provider::ChatStreamEvent;
        use futures::StreamExt;

        struct Collector(Mutex<Vec<CostEvent>>);
        impl CostObserver for Collector {
            fn on_cost(&self, event: CostEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = Gateway::new(Box::new(MockProvider::new())).with_cost_observer(collector.clone());

        let mut stream = gw.chat_stream(req()).await.unwrap();
        let mut deltas = 0usize;
        let mut text = String::new();
        let mut done: Option<ChatResponse> = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                ChatStreamEvent::Delta(t) => {
                    deltas += 1;
                    text.push_str(&t);
                }
                ChatStreamEvent::Done(r) => done = Some(r),
                ChatStreamEvent::ToolCallDelta { .. } | ChatStreamEvent::ReasoningDelta(_) => {}
            }
        }
        let response = done.expect("a Done event ends the stream");
        // The mock chunks its reply, so we see real multi-delta streaming...
        assert!(deltas > 1, "expected multiple deltas, got {deltas}");
        // ...and reassembling the deltas reproduces the final content exactly.
        assert_eq!(text, response.message.content.unwrap());
        // Cost is metered once, when the stream completes.
        let events = collector.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].cost_usd > 0.0);
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

    /// A constrained request must never be served another request's cached
    /// response: `tool_choice`/`response_format` are part of the exact cache
    /// key (PRV-202).
    #[tokio::test]
    async fn exact_cache_does_not_cross_output_constraints() {
        let gw = Gateway::with_providers(vec![Box::new(FlakyProvider::new("p", 0, false))])
            .with_cache(CacheConfig {
                mode: CacheMode::Exact,
                ttl_ms: 60_000,
                ..CacheConfig::default()
            });

        let first = gw.chat(req()).await.unwrap();
        assert!(first.usage.cost_usd > 0.0, "live call has cost");
        let hit = gw.chat(req()).await.unwrap();
        assert_eq!(hit.usage.cost_usd, 0.0, "identical request is a cache hit");

        // Same messages, but a forced tool: must go live, not serve the hit.
        let forced = req().with_tool_choice(crate::types::ToolChoice::Tool("echo".to_string()));
        let third = gw.chat(forced).await.unwrap();
        assert!(
            third.usage.cost_usd > 0.0,
            "constrained request must not reuse the cache"
        );

        // Same messages, but a schema constraint: also live.
        let shaped = req().with_response_format(crate::types::ResponseFormat::JsonObject);
        let fourth = gw.chat(shaped).await.unwrap();
        assert!(
            fourth.usage.cost_usd > 0.0,
            "shaped request must not reuse the cache"
        );
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
                ..CacheConfig::default()
            })
            .with_cost_observer(observer)
    }

    /// Same user turn and system prompt but a different `max_tokens`: the exact
    /// key differs (a miss) while the canonical text and param key match → a
    /// semantic hit serving the first response. (This test used to vary the
    /// *system prompt* instead — exactly the wrong-context hit RAG-203 closed;
    /// `max_tokens` is the remaining exact-key field that is deliberately not
    /// part of param compatibility.)
    #[tokio::test]
    async fn semantic_cache_hits_on_meaning_match_after_exact_miss() {
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(0.9, collector.clone());

        let mut a = ChatRequest::new("m", vec![Message::system("you are A"), Message::user("hi")]);
        a.max_tokens = Some(100);
        let mut b = ChatRequest::new("m", vec![Message::system("you are A"), Message::user("hi")]);
        b.max_tokens = Some(200);

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

    /// RAG-203 acceptance (context half): the same user text under a different
    /// system prompt must NOT hit — it answers a different context.
    #[tokio::test]
    async fn semantic_cache_does_not_cross_system_prompts() {
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(0.9, collector.clone());

        let a = ChatRequest::new(
            "m",
            vec![Message::system("answer in French"), Message::user("hi")],
        );
        let b = ChatRequest::new(
            "m",
            vec![Message::system("answer in German"), Message::user("hi")],
        );

        gw.chat(a).await.unwrap();
        let second = gw.chat(b).await.unwrap();
        assert!(
            second.usage.cost_usd > 0.0,
            "a different system prompt must be a live call, not a cache hit"
        );
    }

    /// Same user text but a different advertised tool set: not compatible.
    #[tokio::test]
    async fn semantic_cache_does_not_cross_tool_specs() {
        use crate::types::ToolSpec;

        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(0.9, collector.clone());

        let tool = |name: &str| ToolSpec {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
            strict: false,
        };
        let mut a = ChatRequest::new("m", vec![Message::user("hi")]);
        a.tools = vec![tool("search")];
        let mut b = ChatRequest::new("m", vec![Message::user("hi")]);
        b.tools = vec![tool("calculator")];

        gw.chat(a).await.unwrap();
        let second = gw.chat(b).await.unwrap();
        assert!(
            second.usage.cost_usd > 0.0,
            "a different tool set must be a live call, not a cache hit"
        );
    }

    /// RAG-203 acceptance (embedding-model half), end to end through the
    /// gateway: two gateways sharing one semantic store but resolving
    /// different embedding models never serve each other's entries — even
    /// though the mock embedder produces the identical vector for the
    /// identical canonical text.
    #[tokio::test]
    async fn semantic_cache_is_not_shared_across_embedding_models() {
        use crate::resilience::SemanticCacheStore;

        /// Delegates to [`MockProvider`] under a different provider name, so
        /// `resolve_embedding_model` yields a different embedding-model id.
        struct RenamedMock(MockProvider);
        #[async_trait]
        impl AIProvider for RenamedMock {
            fn name(&self) -> &str {
                "othermock"
            }
            async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
                self.0.chat(request).await
            }
            async fn embed(
                &self,
                request: crate::EmbeddingRequest,
            ) -> Result<crate::EmbeddingResponse> {
                self.0.embed(request).await
            }
        }

        /// Shares one [`InMemorySemanticCache`] between two gateways.
        struct SharedCache(Arc<InMemorySemanticCache>);
        #[async_trait]
        impl SemanticCacheStore for SharedCache {
            async fn lookup(
                &self,
                param_key: &str,
                embedding_model: &str,
                embedding: &[f32],
                threshold: f32,
                ttl_ms: u64,
                now_ms: u64,
            ) -> Result<Option<ChatResponse>> {
                self.0
                    .lookup(
                        param_key,
                        embedding_model,
                        embedding,
                        threshold,
                        ttl_ms,
                        now_ms,
                    )
                    .await
            }
            async fn store(
                &self,
                param_key: &str,
                embedding_model: &str,
                embedding: &[f32],
                response: &ChatResponse,
                now_ms: u64,
            ) -> Result<()> {
                self.0
                    .store(param_key, embedding_model, embedding, response, now_ms)
                    .await
            }
        }

        let shared = Arc::new(InMemorySemanticCache::new());
        let cache_cfg = CacheConfig {
            mode: CacheMode::Semantic,
            ttl_ms: 60_000,
            similarity_threshold: 0.9,
            ..CacheConfig::default()
        };
        let gw_a = Gateway::with_providers(vec![Box::new(MockProvider::new())])
            .with_cache(cache_cfg)
            .with_semantic_store(Box::new(SharedCache(shared.clone())));
        let gw_b = Gateway::with_providers(vec![Box::new(RenamedMock(MockProvider::new()))])
            .with_cache(cache_cfg)
            .with_semantic_store(Box::new(SharedCache(shared)));
        assert_ne!(
            gw_a.resolve_embedding_model(None),
            gw_b.resolve_embedding_model(None),
            "test premise: the two gateways resolve different embedding models"
        );

        // Same model id + params on the wire, so the param keys agree; only
        // the embedding-model stamp differs.
        gw_a.chat(ChatRequest::new("m", vec![Message::user("hi")]))
            .await
            .unwrap();
        let cross = gw_b
            .chat(ChatRequest::new("m", vec![Message::user("hi")]))
            .await
            .unwrap();
        assert!(
            cross.usage.cost_usd > 0.0,
            "an entry embedded by another model must not be served"
        );
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
    /// identical-meaning, fully param-compatible request. (Uses `max_tokens` to
    /// dodge the exact cache — a differing system prompt would make the miss
    /// about param compatibility instead of the threshold since RAG-203.)
    #[tokio::test]
    async fn semantic_cache_threshold_gates_hits() {
        let collector = Arc::new(Collector(Mutex::new(Vec::new())));
        let gw = semantic_gw(1.01, collector.clone());

        let mut a = ChatRequest::new("m", vec![Message::system("x"), Message::user("hi")]);
        a.max_tokens = Some(100);
        let mut b = ChatRequest::new("m", vec![Message::system("x"), Message::user("hi")]);
        b.max_tokens = Some(200);

        gw.chat(a).await.unwrap();
        let second = gw.chat(b).await.unwrap();
        assert!(
            second.usage.cost_usd > 0.0,
            "similarity below threshold → live call"
        );
    }

    /// A provider that records its call count, waits `delay_ms`, then succeeds (or
    /// returns a transient error). With `tokio`'s paused clock the waits are virtual,
    /// so the races below are deterministic.
    struct SlowProvider {
        name: &'static str,
        delay_ms: u64,
        fail: bool,
        calls: Arc<AtomicUsize>,
    }

    impl SlowProvider {
        fn new(name: &'static str, delay_ms: u64, fail: bool) -> (Box<Self>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Box::new(Self {
                    name,
                    delay_ms,
                    fail,
                    calls: calls.clone(),
                }),
                calls,
            )
        }
    }

    #[async_trait]
    impl AIProvider for SlowProvider {
        fn name(&self) -> &str {
            self.name
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            if self.fail {
                return Err(Error::provider("503 unavailable"));
            }
            Ok(ChatResponse {
                message: Message::assistant(format!("ok from {}", self.name)),
                model: request.model,
                usage: Usage::new(3, 2, 0.01),
                finish_reason: "stop".to_string(),
            })
        }
    }

    fn hedge_on(delay_ms: u64) -> HedgeConfig {
        HedgeConfig {
            enabled: true,
            delay_ms,
            max_parallel: 2,
        }
    }

    /// A slow primary is hedged after the delay; the faster secondary wins, and both
    /// were dispatched.
    #[tokio::test(start_paused = true)]
    async fn hedging_races_and_returns_faster_secondary() {
        let (primary, pc) = SlowProvider::new("primary", 1_000, false);
        let (secondary, sc) = SlowProvider::new("secondary", 10, false);
        let gw = Gateway::with_providers(vec![primary, secondary]).with_hedging(hedge_on(100));

        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from secondary"));
        assert_eq!(pc.load(Ordering::SeqCst), 1, "primary was dispatched");
        assert_eq!(sc.load(Ordering::SeqCst), 1, "secondary was hedged in");
    }

    /// A fast primary answers before the hedge delay, so no hedge is launched.
    #[tokio::test(start_paused = true)]
    async fn hedging_skips_when_primary_is_fast() {
        let (primary, pc) = SlowProvider::new("primary", 10, false);
        let (secondary, sc) = SlowProvider::new("secondary", 1_000, false);
        let gw = Gateway::with_providers(vec![primary, secondary]).with_hedging(hedge_on(100));

        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from primary"));
        assert_eq!(pc.load(Ordering::SeqCst), 1);
        assert_eq!(sc.load(Ordering::SeqCst), 0, "secondary never launched");
    }

    /// With hedging off, dispatch waits out the slow primary (no secondary call).
    #[tokio::test(start_paused = true)]
    async fn hedging_disabled_uses_primary_only() {
        let (primary, pc) = SlowProvider::new("primary", 1_000, false);
        let (secondary, sc) = SlowProvider::new("secondary", 10, false);
        let gw = Gateway::with_providers(vec![primary, secondary]); // hedging default off

        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from primary"));
        assert_eq!(pc.load(Ordering::SeqCst), 1);
        assert_eq!(
            sc.load(Ordering::SeqCst),
            0,
            "no hedge → secondary untouched"
        );
    }

    /// Hedging composes with failover: a transient primary failure launches the next
    /// candidate immediately (without waiting the full hedge delay).
    #[tokio::test(start_paused = true)]
    async fn hedging_fails_over_on_transient_error() {
        let (primary, _pc) = SlowProvider::new("primary", 10, true);
        let (secondary, sc) = SlowProvider::new("secondary", 10, false);
        let gw = Gateway::with_providers(vec![primary, secondary])
            .with_hedging(hedge_on(10_000)) // long delay: only a failure triggers failover
            .with_retry(fast_retry());

        let resp = gw.chat(req()).await.unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("ok from secondary"));
        assert!(
            sc.load(Ordering::SeqCst) >= 1,
            "secondary served the request"
        );
    }
}
