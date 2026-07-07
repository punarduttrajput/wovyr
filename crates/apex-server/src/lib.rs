//! Single-node HTTP server for the Apex Platform API.
//!
//! v0.1 exposes the minimum needed to satisfy the
//! [roadmap](../../docs/18-roadmap/v0.1.md) exit criterion *"a documented agent
//! runs … against a single-node server"*: a health check and a synchronous agent
//! **run** endpoint, following the [Agents API](../../docs/09-api/agents.md) run
//! response shape and the [error envelope](../../docs/09-api/overview.md#8-error-model).
//!
//! Agents can be **persisted** (`POST/GET /api/v1/agents`, `GET/DELETE
//! /api/v1/agents/{id}`) and run by id (`POST /api/v1/agents/{id}/run`), or run inline
//! via `POST /api/v1/agents:run` (manifest in the body). The store is in-memory for
//! now (durable file/db backing is a later slice). The server also hosts the workflow
//! visibility, multi-tenancy, and webhook routes, and applies the shared `/v1`
//! conventions in [`hardening`]: cursor **pagination** (§6) on list endpoints,
//! **idempotency keys** (§9) on runs, and a **request-id** on every response (§14).

mod audit;
mod auth;
mod hardening;
mod kms;
pub use auth::{ApiKeyStore, AuthMode, FileApiKeyStore, InMemoryApiKeyStore};
mod marketplace;
mod memory;
mod plugins;
mod rate_limit;
mod secrets;
mod tenancy;
mod tools;
mod webhooks;
mod workflow_runner;

use apex_agent::{AgentDefinition, NullSink, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_common::Error;
use apex_events::{
    BackoffPolicy, EncryptedFileWebhookStore, FileWebhookStore, InMemoryWebhookStore, WebhookStore,
};
use apex_provider::{CostEvent, CostObserver, Gateway};
use apex_telemetry::Metrics;
use apex_tenancy::{FileTenancyStore, InMemoryTenancyStore, TenancyStore};
use apex_tools::ToolRegistry;
use apex_workflow::{
    CheckpointStore, Clock, Definition, DefinitionResolver, Engine, EventLog, ExecutionFilter,
    FileScheduleStore, FileStore, FileTimerStore, InMemoryScheduleStore, InMemoryStore,
    InMemoryTimerStore, ScheduleDispatcher, ScheduleStore, SystemClock, TimerDispatcher,
    TimerStore, WorkflowState,
};
use axum::{
    BoxError, Json, Router,
    error_handling::HandleErrorLayer,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{
        Html, IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tower::ServiceBuilder;

/// One persisted agent record (RM-GA-P2 DUR-404): a `BTreeMap` keyed by a `(tenant,
/// id)` tuple can't round-trip through `serde_json` (object keys must be strings), so
/// the on-disk shape is a flat list instead — the same convention every other
/// `Vec<Record>` <-> `BTreeMap<Key, Record>` file store in the workspace uses.
#[derive(Clone, Serialize, Deserialize)]
struct AgentRecord {
    tenant: String,
    id: String,
    manifest: String,
}

/// Registry of stored agent manifests, keyed by `(tenant, agent id)` so a tenant only
/// ever sees and mutates its own agents (the `metadata.name` is the id, unique
/// *within* a tenant — two tenants may reuse a name without colliding). Manifests are
/// validated on create. Durable when opened with a `path` (RM-GA-P2 DUR-404): every
/// mutation re-persists the whole catalog via `atomic_write`, and a fresh instance
/// loads it back — otherwise (`path: None`, what tests use for a guaranteed-empty,
/// non-leaking store) it behaves exactly as the original in-memory-only version did.
#[derive(Default)]
struct AgentStore {
    inner: RwLock<BTreeMap<(String, String), String>>,
    path: Option<PathBuf>,
}

impl AgentStore {
    /// Open a store, loading any existing catalog from `path` (best-effort: a missing
    /// or corrupt file starts empty rather than failing server startup). `path: None`
    /// is a purely in-memory store — what every test that cares about a clean,
    /// non-leaking agent list uses via [`AppState::with_agents`].
    fn new(path: Option<PathBuf>) -> Self {
        let inner = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Vec<AgentRecord>>(&bytes).ok())
            .map(|records| {
                records
                    .into_iter()
                    .map(|r| ((r.tenant, r.id), r.manifest))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            inner: RwLock::new(inner),
            path,
        }
    }

    /// Persist the current catalog (best-effort — logged, not propagated, since the
    /// in-memory mutation this follows has already succeeded either way).
    fn persist(&self, map: &BTreeMap<(String, String), String>) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(error = %e, "failed to create agent store directory");
            return;
        }
        let records: Vec<AgentRecord> = map
            .iter()
            .map(|((tenant, id), manifest)| AgentRecord {
                tenant: tenant.clone(),
                id: id.clone(),
                manifest: manifest.clone(),
            })
            .collect();
        match serde_json::to_vec_pretty(&records) {
            Ok(bytes) => {
                if let Err(e) = apex_common::fs::atomic_write(path, bytes) {
                    tracing::error!(error = %e, "failed to persist agent store");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to encode agent store"),
        }
    }

    /// Validate and store a manifest under `tenant`, returning the agent id.
    fn create(&self, tenant: &str, manifest: String) -> Result<String, ApiError> {
        let def = AgentDefinition::from_yaml(&manifest).map_err(|e| {
            ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
        })?;
        let id = def.metadata.name.clone();
        let mut inner = self.inner.write().expect("agent store poisoned");
        inner.insert((tenant.to_string(), id.clone()), manifest);
        self.persist(&inner);
        Ok(id)
    }

    /// The stored manifest for `id` within `tenant`, if any.
    fn manifest(&self, tenant: &str, id: &str) -> Option<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .get(&(tenant.to_string(), id.to_string()))
            .cloned()
    }

    /// The parsed definition for `id` within `tenant` (manifests are validated on create).
    fn definition(&self, tenant: &str, id: &str) -> Option<AgentDefinition> {
        self.manifest(tenant, id)
            .and_then(|m| AgentDefinition::from_yaml(&m).ok())
    }

    /// All stored agent ids in `tenant`, sorted.
    fn list(&self, tenant: &str) -> Vec<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .iter()
            .filter(|((t, _), _)| t == tenant)
            .map(|((_, id), _)| id.clone())
            .collect()
    }

    /// Remove `id` within `tenant`; returns whether it existed.
    fn delete(&self, tenant: &str, id: &str) -> bool {
        let mut inner = self.inner.write().expect("agent store poisoned");
        let existed = inner
            .remove(&(tenant.to_string(), id.to_string()))
            .is_some();
        if existed {
            self.persist(&inner);
        }
        existed
    }
}

/// Shared server state: the LLM gateway, tool registry, metrics, a run counter, and a
/// read-only workflow engine over the durable store (for visibility endpoints).
pub struct AppState {
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
    metrics: Metrics,
    agents: Arc<AgentStore>,
    run_counter: AtomicU64,
    /// Read-only engine over the durable workflow store — drives the `GET
    /// /api/v1/workflows*` visibility endpoints (G4). Its executor is never invoked.
    workflows: Engine,
    /// Workflow execution id → owning tenant, stamped at submit so the workflow routes
    /// enforce per-tenant isolation without the (tenant-agnostic) engine. An execution
    /// with no recorded owner belongs to the anonymous `default` space (back-compat).
    /// Durable when opened with a path (RM-GA-P2 DUR-404) — without this, a restart
    /// dropped every execution's tenant binding, so the owning tenant got 404s while
    /// the anonymous `default` space could see all of them (a tenant-isolation
    /// regression on restart, per `workflow_visible`).
    workflow_owners: RwLock<BTreeMap<String, String>>,
    /// Where `workflow_owners` persists (`None` = in-memory only, what tests use).
    workflow_owners_path: Option<PathBuf>,
    /// Durable registry for wall-clock timers (G1) — the same store attached to
    /// `workflows` via `Engine::with_timer_store` and polled by the background
    /// dispatcher loop `serve()` spawns (RM-GA-P2 EXE-601). Exposed here (rather than
    /// only living inside the `Engine`) so `serve()` can build a `TimerDispatcher`
    /// without reaching into the engine's private fields.
    pub(crate) timers: Arc<dyn TimerStore>,
    /// Durable recurring-schedule registry (G2), shared with the CLI's `apex
    /// workflows schedule create` — the background dispatcher loop `serve()` spawns
    /// (RM-GA-P2 EXE-601) is what lets a CLI-created schedule fire without an
    /// operator ever running `apex workflows tick`.
    pub(crate) schedules: Arc<dyn ScheduleStore>,
    /// Tenancy catalog backing the org/project/membership/quota routes (G: tenancy).
    pub(crate) tenancy: Arc<dyn TenancyStore>,
    /// Per-project run-path quota usage (concurrent runs + daily LLM spend). An `Arc`
    /// (not embedded directly) so it can be shared into the workflow engine's
    /// [`ServerExecutor`](workflow_runner::ServerExecutor), which is constructed before
    /// this struct exists.
    pub(crate) quota: Arc<tenancy::QuotaTracker>,
    /// Webhook subscriptions; events emitted on mutations are delivered to matches.
    pub(crate) webhooks: Arc<dyn WebhookStore>,
    /// Sends signed webhook payloads (swappable for tests).
    pub(crate) webhook_sender: Arc<dyn webhooks::WebhookSender>,
    /// Retry/backoff policy for webhook delivery.
    pub(crate) webhook_policy: BackoffPolicy,
    /// Monotonic counter for emitted event ids.
    pub(crate) event_counter: AtomicU64,
    /// Caches responses by `Idempotency-Key` so client retries of mutations are safe.
    pub(crate) idempotency: hardening::IdempotencyStore,
    /// Memory engine backing the memory-explorer routes (embeds via the gateway).
    pub(crate) memory: apex_memory::MemoryEngine,
    /// The memory store the engine writes to, kept alongside for namespace/record
    /// enumeration (the engine does not expose its store).
    pub(crate) memory_store: Arc<dyn apex_memory::MemoryStore>,
    /// Secret vault backing the `/api/v1/secrets` routes (tenant-scoped).
    pub(crate) secrets: apex_secrets::Vault,
    /// The platform KMS ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)) —
    /// the same instance that backs `secrets` and `memory`'s encrypting stores, also
    /// exposed directly for the `/api/v1/kms/*` tenant-key-management routes.
    pub(crate) kms: Arc<dyn apex_kms::Kms>,
    /// Tamper-evident audit log; security-sensitive routes append to it.
    pub(crate) audit: apex_audit::AuditLog,
    /// API-key store backing `APEX_AUTH_MODE=apikey` ([SEC-101]).
    pub(crate) api_keys: Arc<dyn auth::ApiKeyStore>,
    /// Whether the anonymous default-tenant identity is granted a role set at all
    /// ([SEC-102]), resolved once at construction — see
    /// [`auth::resolve_anonymous_allowed`].
    pub(crate) anonymous_allowed: bool,
    /// Which credential scheme [`auth::authenticate`] verifies ([SEC-101]), resolved
    /// once at construction from `APEX_AUTH_MODE` — see [`auth::AuthMode::from_env`].
    pub(crate) auth_mode: auth::AuthMode,
    /// Cross-cutting HTTP resource limits (timeout/body/concurrency, [SEC-201]),
    /// resolved once at construction — see [`HttpLimits::from_env`].
    pub(crate) http_limits: HttpLimits,
    /// Per-key rate limiter for most routes ([SEC-203]).
    pub(crate) rate_limiter_standard: Arc<rate_limit::RateLimiter>,
    /// A tighter per-key rate limiter for expensive/sensitive routes — the direct
    /// agent-run endpoints, KMS, and secrets ([SEC-203]).
    pub(crate) rate_limiter_sensitive: Arc<rate_limit::RateLimiter>,
    /// `APEX_CORS_ALLOWED_ORIGINS` (comma-separated) — an explicit cross-origin
    /// allow-list ([SEC-204]). Empty (the default) means no `CorsLayer` at all: the
    /// browser's own same-origin policy already blocks cross-origin reads with no
    /// CORS headers present, so "no config" correctly means same-origin-only.
    pub(crate) cors_allowed_origins: Vec<String>,
}

impl AppState {
    /// Build state from the environment (provider chosen by `OPENAI_API_KEY`, or
    /// `APEX_PROVIDER=mistralrs` to run every agent/workflow on this node against a
    /// real local model — see [`default_gateway`]).
    pub async fn from_env() -> Self {
        // Export metrics to OTLP when built with `--features otlp` and the endpoint is
        // set; otherwise an in-process-only registry (see `Metrics::with_otlp_export`).
        let metrics = Metrics::with_otlp_export("apex");
        // Cost events from the gateway become LLM token/cost/savings metrics.
        let gateway = Arc::new(default_gateway().await.with_cost_observer(Arc::new(
            MetricsCostObserver {
                metrics: metrics.clone(),
            },
        )));
        // One shared KMS for both encrypting-store consumers below — same root key +
        // tenant-key catalog, so a tenant's secrets and memories are sealed under
        // (independently generated, but co-located) keys rooted in the same trust anchor.
        let kms = default_kms();
        let secrets = default_secrets_vault(kms.clone());
        let mut registry = ToolRegistry::with_builtins();
        // shell is NOT a default builtin for a hosted server (SEC-301) — arbitrary
        // command execution as the server's own user, available to every agent run,
        // is exactly the risk this ticket closes. An operator opts in explicitly.
        if std::env::var("APEX_ENABLE_SHELL_TOOL")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            registry = registry.with_shell();
        }
        // image_generate needs a real, billed API key, so it's only registered when one
        // is configured — same signal default_gateway() uses to pick a real vs. mock
        // provider.
        if std::env::var_os("OPENAI_API_KEY").is_some() {
            registry.register(std::sync::Arc::new(apex_tools::ImageGenTool::new()));
        }
        // Register enabled plugin tools from the durable catalog into the run registry,
        // routed through a secret-aware runtime (when built with `plugin-wasi`), so agent
        // and workflow runs can invoke them with their tenant-scoped secrets injected.
        // Done before the registry is shared with the workflow engine below.
        plugins::register_enabled_tools(&mut registry, &secrets);
        let (memory, memory_store) = memory::default_engine(kms.clone());
        let agents = Arc::new(AgentStore::new(
            workflows_dir().map(|d| d.join("agents.json")),
        ));
        let tenancy_store = default_tenancy_store();
        let quota = Arc::new(tenancy::QuotaTracker::new(
            server_state_dir().map(|d| d.join("quota.json")),
        ));
        // Durable G1/G2 registries, shared with the CLI's `apex workflows tick`/
        // `schedule create` — attached to the engine (timers) and polled by the
        // background dispatcher loops `serve()` spawns (RM-GA-P2 EXE-601).
        let timers = default_timer_store();
        let schedules = default_schedule_store();
        // Thread gateway + registry + the agent store + tenancy/quota into the workflow
        // engine so the ServerExecutor can actually drive function/ai/agent activities
        // when the submit route runs a workflow (an `agent` activity looks up a stored
        // agent by id here, and enforces the same project quota a direct run would).
        let workflows = default_workflows_engine(
            gateway.clone(),
            registry.clone(),
            agents.clone(),
            tenancy_store.clone(),
            quota.clone(),
            timers.clone(),
        );
        let workflow_owners_path = workflows_dir().map(|d| d.join("owners.json"));
        let workflow_owners = load_owners(workflow_owners_path.as_deref());
        Self {
            gateway,
            registry,
            metrics,
            agents,
            run_counter: AtomicU64::new(1),
            workflows,
            workflow_owners: RwLock::new(workflow_owners),
            workflow_owners_path,
            timers,
            schedules,
            tenancy: tenancy_store,
            quota,
            webhooks: default_webhook_store(kms.clone()),
            webhook_sender: Arc::new(webhooks::ReqwestSender::default()),
            webhook_policy: BackoffPolicy::default(),
            event_counter: AtomicU64::new(1),
            idempotency: hardening::IdempotencyStore::new_with_path(
                Duration::from_secs(env_u64("APEX_IDEMPOTENCY_TTL_SECS", 24 * 60 * 60)),
                env_u64("APEX_IDEMPOTENCY_MAX_ENTRIES", 10_000) as usize,
                server_state_dir().map(|d| d.join("idempotency.json")),
            ),
            memory,
            memory_store,
            secrets,
            kms,
            audit: default_audit_log(),
            api_keys: auth::default_api_key_store(),
            anonymous_allowed: auth::resolve_anonymous_allowed(),
            auth_mode: auth::AuthMode::from_env(),
            http_limits: HttpLimits::from_env(),
            rate_limiter_standard: Arc::new(rate_limit::RateLimiter::new(
                env_u64("APEX_RATE_LIMIT_STANDARD_PER_MIN", 300) as u32,
                env_u64("APEX_RATE_LIMIT_STANDARD_PER_MIN", 300) as u32,
            )),
            rate_limiter_sensitive: Arc::new(rate_limit::RateLimiter::new(
                env_u64("APEX_RATE_LIMIT_SENSITIVE_PER_MIN", 30) as u32,
                env_u64("APEX_RATE_LIMIT_SENSITIVE_PER_MIN", 30) as u32,
            )),
            cors_allowed_origins: std::env::var("APEX_CORS_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }

    /// Override the webhook subscription store (tests inject an in-memory store).
    pub fn with_webhooks(mut self, store: Arc<dyn WebhookStore>) -> Self {
        self.webhooks = store;
        self
    }

    /// Override the webhook sender (tests inject a recording sender).
    #[cfg(test)]
    pub(crate) fn with_webhook_sender(mut self, sender: Arc<dyn webhooks::WebhookSender>) -> Self {
        self.webhook_sender = sender;
        self
    }

    /// Override the webhook retry/backoff policy (tests use a zero-delay policy).
    #[cfg(test)]
    pub(crate) fn with_webhook_policy(mut self, policy: BackoffPolicy) -> Self {
        self.webhook_policy = policy;
        self
    }

    /// Override the read-only workflow engine (used by tests to inject a seeded
    /// in-memory store).
    pub fn with_workflows(mut self, engine: Engine) -> Self {
        self.workflows = engine;
        self
    }

    /// Override the tenancy store (used by tests to inject a seeded in-memory store).
    pub fn with_tenancy(mut self, store: Arc<dyn TenancyStore>) -> Self {
        self.tenancy = store;
        self
    }

    /// Override the tool registry (tests exercise `APEX_ENABLE_SHELL_TOOL`'s effect
    /// this way instead of mutating the process-global env var, which every other
    /// test in this crate's default shell-disabled behavior depends on).
    #[cfg(test)]
    pub(crate) fn with_registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Override the memory engine + store (tests inject an in-memory store so they don't
    /// touch the shared `~/.apex/memory`).
    #[cfg(test)]
    pub(crate) fn with_memory(
        mut self,
        engine: apex_memory::MemoryEngine,
        store: Arc<dyn apex_memory::MemoryStore>,
    ) -> Self {
        self.memory = engine;
        self.memory_store = store;
        self
    }

    /// Override the secret vault (tests inject an in-memory store).
    #[cfg(test)]
    pub(crate) fn with_secrets(mut self, vault: apex_secrets::Vault) -> Self {
        self.secrets = vault;
        self
    }

    /// Override the audit log (tests inject an in-memory log).
    #[cfg(test)]
    pub(crate) fn with_audit(mut self, audit: apex_audit::AuditLog) -> Self {
        self.audit = audit;
        self
    }

    /// Override the platform KMS (tests inject a fresh in-memory-backed instance so
    /// they don't touch the shared `~/.apex/kms`, and so tenant-key state starts clean).
    #[cfg(test)]
    pub(crate) fn with_kms(mut self, kms: Arc<dyn apex_kms::Kms>) -> Self {
        self.kms = kms;
        self
    }

    /// Override the API-key store — tests (this crate's own, an `authz_matrix`
    /// integration suite per SEC-105, or an embedder's) seed principals without
    /// touching `~/.apex/auth`.
    pub fn with_api_keys(mut self, store: Arc<dyn auth::ApiKeyStore>) -> Self {
        self.api_keys = store;
        self
    }

    /// Override whether the anonymous default-tenant identity is granted a role set
    /// (SEC-102), without mutating the process-global `APEX_ALLOW_ANONYMOUS` (which
    /// every other test in this crate's default-anonymous-in-`cfg(test)` behavior
    /// depends on, and would otherwise race against).
    pub fn with_anonymous_allowed(mut self, allowed: bool) -> Self {
        self.anonymous_allowed = allowed;
        self
    }

    /// Override the credential scheme `auth::authenticate` verifies (SEC-101), without
    /// mutating the process-global `APEX_AUTH_MODE` (which every other test in this
    /// crate's default `disabled-loopback` behavior depends on).
    pub fn with_auth_mode(mut self, mode: auth::AuthMode) -> Self {
        self.auth_mode = mode;
        self
    }

    /// Override the HTTP resource limits (SEC-201) — timeout/body/concurrency —
    /// without mutating the process-global `APEX_HTTP_*` env vars (which every other
    /// test in this crate's default-limits behavior depends on).
    #[cfg(test)]
    pub(crate) fn with_http_limits(mut self, limits: HttpLimits) -> Self {
        self.http_limits = limits;
        self
    }

    /// Override the standard-tier rate limiter (SEC-203) with a tighter one so tests
    /// can drive it to its limit quickly, without waiting on the production defaults.
    #[cfg(test)]
    pub(crate) fn with_rate_limiter_standard(mut self, limiter: rate_limit::RateLimiter) -> Self {
        self.rate_limiter_standard = Arc::new(limiter);
        self
    }

    /// Override the CORS allow-list (SEC-204) without mutating the process-global
    /// `APEX_CORS_ALLOWED_ORIGINS`.
    #[cfg(test)]
    pub(crate) fn with_cors_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.cors_allowed_origins = origins;
        self
    }

    /// Override the agent store (RM-GA-P2 DUR-404) — tests that assert an exact,
    /// non-accumulating agent list use a fresh in-memory store (`AgentStore::new(None)`)
    /// so they don't observe agents persisted by a prior test run or process, the same
    /// isolation `with_tenancy`/`with_secrets`/etc. give their own durable resource.
    #[cfg(test)]
    pub(crate) fn with_agents(mut self, agents: Arc<AgentStore>) -> Self {
        self.agents = agents;
        self
    }

    /// Override the timer store (RM-GA-P2 EXE-601) — tests that exercise the
    /// background dispatcher against an isolated engine (`with_workflows`) must
    /// point `spawn_dispatch_loops`'s `state.timers` at the *same* store the
    /// isolated engine's `wait` activities actually schedule into, or the
    /// dispatcher polls a store nothing was ever written to.
    #[cfg(test)]
    pub(crate) fn with_timers(mut self, timers: Arc<dyn TimerStore>) -> Self {
        self.timers = timers;
        self
    }

    /// Record the owning tenant of a workflow execution (called at submit), persisting
    /// the index (RM-GA-P2 DUR-404) so the binding survives a restart.
    fn record_workflow_owner(&self, execution_id: &str, tenant: &str) {
        let mut owners = self
            .workflow_owners
            .write()
            .expect("workflow owners poisoned");
        owners.insert(execution_id.to_string(), tenant.to_string());
        if let Some(path) = &self.workflow_owners_path {
            if let Some(parent) = path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                tracing::error!(error = %e, "failed to create workflow owners directory");
                return;
            }
            match serde_json::to_vec_pretty(&*owners) {
                Ok(bytes) => {
                    if let Err(e) = apex_common::fs::atomic_write(path, bytes) {
                        tracing::error!(error = %e, "failed to persist workflow owners");
                    }
                }
                Err(e) => tracing::error!(error = %e, "failed to encode workflow owners"),
            }
        }
    }

    /// Whether `tenant` may see/act on workflow execution `execution_id`. An execution
    /// with a recorded owner is visible only to that tenant; one with no recorded owner
    /// belongs to the anonymous `default` space (back-compat for pre-existing or
    /// CLI-created executions).
    fn workflow_visible(&self, execution_id: &str, tenant: &str) -> bool {
        match self
            .workflow_owners
            .read()
            .expect("workflow owners poisoned")
            .get(execution_id)
        {
            Some(owner) => owner == tenant,
            None => tenant == tenancy::DEFAULT_TENANT,
        }
    }
}

/// The server's chat/embeddings backend. `Gateway::from_env()` (OpenAI if
/// `OPENAI_API_KEY` is set, else the deterministic mock) unless the operator opts into
/// a real local model with `APEX_PROVIDER=mistralrs` — in which case every agent and
/// workflow run on this node goes through it, no per-run choice. Requires this crate's
/// `mistralrs` cargo feature; without it (or if the model fails to load — e.g. no
/// network for the first-run GGUF download), falls back to `Gateway::from_env()` with a
/// loud warning rather than failing server startup outright.
async fn default_gateway() -> Gateway {
    let wants_mistralrs = std::env::var("APEX_PROVIDER")
        .map(|v| v.eq_ignore_ascii_case("mistralrs"))
        .unwrap_or(false);
    if !wants_mistralrs {
        return Gateway::from_env();
    }
    #[cfg(feature = "mistralrs")]
    {
        match apex_provider::MistralRsProvider::from_env().await {
            Ok(provider) => return Gateway::new(Box::new(provider)),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "APEX_PROVIDER=mistralrs requested but the model failed to load; \
                     falling back to Gateway::from_env()"
                );
            }
        }
    }
    #[cfg(not(feature = "mistralrs"))]
    {
        tracing::warn!(
            "APEX_PROVIDER=mistralrs requested but this apex-server build lacks the \
             mistralrs feature; falling back to Gateway::from_env()"
        );
    }
    Gateway::from_env()
}

/// A durable [`FileTenancyStore`] at `~/.apex/tenancy` (shared with the CLI), falling
/// back to an in-memory store if that directory is unavailable.
fn default_tenancy_store() -> Arc<dyn TenancyStore> {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("tenancy"));
    if let Some(dir) = dir
        && let Ok(store) = FileTenancyStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryTenancyStore::new())
}

/// The platform KMS ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)):
/// sources a root key from `APEX_KMS_ROOT_KEY` (hex) or, failing that,
/// generates-and-persists one at `~/.apex/kms/root.key` (shared with the CLI,
/// so either process can decrypt the other's sealed data), backing tenant
/// keys with a `FileKmsStore` in the same directory. Falls back to a fully
/// ephemeral in-process key if neither is available — anything sealed under
/// it will not survive a restart, so this is logged loudly rather than
/// silently accepted like the other `~/.apex/*` in-memory fallbacks.
fn default_kms() -> Arc<dyn apex_kms::Kms> {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("kms"));
    let root_key = apex_kms::root::from_env("APEX_KMS_ROOT_KEY")
        .ok()
        .or_else(|| {
            dir.as_ref()
                .and_then(|d| apex_kms::root::from_file(d.join("root.key")).ok())
        });
    match (root_key, dir) {
        (Some(key), Some(dir)) => {
            let store: Arc<dyn apex_kms::KmsStore> = match apex_kms::FileKmsStore::new(dir) {
                Ok(s) => Arc::new(s),
                Err(_) => Arc::new(apex_kms::InMemoryKmsStore::new()),
            };
            Arc::new(apex_kms::LocalKms::new(key, store))
        }
        _ => {
            tracing::warn!(
                "no persistent KMS root key available (set APEX_KMS_ROOT_KEY or ensure HOME is set); \
                 using an ephemeral in-process key — anything sealed under it will not survive a restart"
            );
            let key = apex_kms::generate_key().expect("secure RNG available");
            Arc::new(apex_kms::LocalKms::new(
                key,
                Arc::new(apex_kms::InMemoryKmsStore::new()),
            ))
        }
    }
}

/// A secret [`Vault`](apex_secrets::Vault) over a durable store at
/// `~/.apex/secrets` (shared with the CLI). Seals values through `kms` before
/// they reach disk (a distinct `secrets.enc.json`, never mixed with the
/// plaintext `secrets.json`) when `APEX_SECRETS_ENCRYPT_AT_REST` is set —
/// **opt-in**, unlike the always-on memory encryption below: switching the
/// default here would abandon any secrets already sitting in the plaintext
/// file rather than transparently coexisting with them.
fn default_secrets_vault(kms: Arc<dyn apex_kms::Kms>) -> apex_secrets::Vault {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("secrets"));
    let encrypt_at_rest = std::env::var("APEX_SECRETS_ENCRYPT_AT_REST").is_ok();
    let store: Arc<dyn apex_secrets::SecretStore> = match dir {
        Some(d) if encrypt_at_rest => match apex_secrets::EncryptedFileSecretStore::new(d, kms) {
            Ok(s) => Arc::new(s),
            Err(_) => Arc::new(apex_secrets::InMemorySecretStore::new()),
        },
        Some(d) => match apex_secrets::FileSecretStore::new(d) {
            Ok(s) => Arc::new(s),
            Err(_) => Arc::new(apex_secrets::InMemorySecretStore::new()),
        },
        None => Arc::new(apex_secrets::InMemorySecretStore::new()),
    };
    apex_secrets::Vault::new(store)
}

/// A tamper-evident [`AuditLog`](apex_audit::AuditLog) over a durable [`FileAuditSink`]
/// at `~/.apex/audit`, falling back to an in-memory log. Opened with cross-process
/// locking (RM-GA-P2 DUR-403) over that same directory — the CLI can append to the
/// identical `audit.jsonl` (e.g. via `apex plugin` commands, once wired), so a
/// second writer must extend the chain, not fork it.
fn default_audit_log() -> apex_audit::AuditLog {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("audit"));
    let sink = dir
        .clone()
        .and_then(|dir| apex_audit::FileAuditSink::new(dir).ok());
    match (sink, dir) {
        (Some(s), Some(dir)) => apex_audit::AuditLog::open_with_lock(Box::new(s), dir)
            .unwrap_or_else(|_| apex_audit::AuditLog::in_memory()),
        _ => apex_audit::AuditLog::in_memory(),
    }
}

/// A durable webhook store at `~/.apex/webhooks`, falling back to in-memory.
/// Seals a subscription's signing `secret` through `kms` before it reaches
/// disk (a distinct `webhooks.enc.json`, never mixed with the plaintext
/// `webhooks.json`) when `APEX_WEBHOOKS_ENCRYPT_AT_REST` is set — **opt-in**,
/// like the secret vault's equivalent switch: flipping it makes any
/// already-plaintext subscriptions invisible via this store rather than
/// transparently migrating them.
fn default_webhook_store(kms: Arc<dyn apex_kms::Kms>) -> Arc<dyn WebhookStore> {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| {
            std::path::PathBuf::from(home)
                .join(".apex")
                .join("webhooks")
        });
    let encrypt_at_rest = std::env::var("APEX_WEBHOOKS_ENCRYPT_AT_REST").is_ok();
    if let Some(dir) = dir {
        if encrypt_at_rest {
            if let Ok(store) = EncryptedFileWebhookStore::new(&dir, kms) {
                return Arc::new(store);
            }
        } else if let Ok(store) = FileWebhookStore::new(&dir) {
            return Arc::new(store);
        }
    }
    Arc::new(InMemoryWebhookStore::new())
}

/// An [`Engine`] over the durable workflow store at `~/.apex/workflows` (the same
/// directory the CLI writes to), falling back to an empty in-memory store if that
/// directory is unavailable.  The executor is a [`ServerExecutor`] that handles
/// `function`/`ai`/`agent`/`human` activities so the write-path submit route can
/// actually drive workflow runs — including enforcing the submitting project's quota
/// on `agent` activities, the same gate a direct `agents:run` call goes through.  The
/// read paths (`list`/`status`/`history`) are unaffected. Attaches `timers` (RM-GA-P2
/// EXE-601) so a `wait: {timer: ...}}` activity can actually register a durable
/// deadline — before this the server's engine had no timer store at all, so any such
/// activity failed immediately with "no timer store" rather than suspending.
fn default_workflows_engine(
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
    agents: Arc<AgentStore>,
    tenancy: Arc<dyn TenancyStore>,
    quota: Arc<tenancy::QuotaTracker>,
    timers: Arc<dyn TimerStore>,
) -> Engine {
    let executor = Arc::new(workflow_runner::ServerExecutor::new(
        gateway, registry, agents, tenancy, quota,
    ));
    if let Some(dir) = workflows_dir()
        && let Ok(store) = FileStore::new(dir)
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        return Engine::new(events, checkpoints, executor).with_timer_store(timers);
    }
    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    Engine::new(events, checkpoints, executor).with_timer_store(timers)
}

/// A durable [`TimerStore`] at `~/.apex/workflows` (shared with the CLI's `apex
/// workflows tick`), falling back to an in-memory store if that directory is
/// unavailable (RM-GA-P2 EXE-601).
fn default_timer_store() -> Arc<dyn TimerStore> {
    if let Some(dir) = workflows_dir()
        && let Ok(store) = FileTimerStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryTimerStore::new())
}

/// A durable [`ScheduleStore`] at `~/.apex/workflows` (shared with the CLI's `apex
/// workflows schedule create`), falling back to an in-memory store if that directory
/// is unavailable (RM-GA-P2 EXE-601).
fn default_schedule_store() -> Arc<dyn ScheduleStore> {
    if let Some(dir) = workflows_dir()
        && let Ok(store) = FileScheduleStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryScheduleStore::new())
}

/// `~/.apex/workflows/definitions` — where `POST /api/v1/workflows` persists the
/// submitted manifest by workflow name (RM-GA-P2 EXE-601), so the background
/// timer/schedule dispatchers can resolve a `Definition` for an execution that
/// suspends long after the original HTTP request is gone (no caller left to
/// re-supply the manifest). Server-local only — the CLI has no equivalent concept
/// (it always drives one definition file at a time), so this doesn't need
/// DUR-403's cross-process lock.
fn definitions_dir() -> Option<PathBuf> {
    workflows_dir().map(|d| d.join("definitions"))
}

/// Sanitize a workflow name into a safe filename stem (mirrors
/// `apex_workflow::FileStore`'s execution-id sanitization).
fn sanitize_workflow_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Persist `manifest` as the latest known definition for `workflow_name`
/// (RM-GA-P2 EXE-601), best-effort: a failure here doesn't fail the submission
/// itself, it only means a *later* timer/schedule fire for this execution won't
/// find a resolvable definition (a clear, fail-closed error at that point — not a
/// silent misbehavior).
fn save_definition(workflow_name: &str, manifest: &str) {
    let Some(dir) = definitions_dir() else { return };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::error!(error = %e, "failed to create definitions directory");
        return;
    }
    let path = dir.join(format!("{}.yaml", sanitize_workflow_name(workflow_name)));
    if let Err(e) = apex_common::fs::atomic_write(&path, manifest) {
        tracing::error!(error = %e, "failed to persist workflow definition");
    }
}

/// Build a [`DefinitionResolver`] over the persisted-by-name definitions
/// (RM-GA-P2 EXE-601): looks up the *latest* manifest submitted under a workflow
/// name. If that content has since drifted from what a specific still-suspended
/// execution was pinned to at submission time, `Engine::resume`'s G7 pin check
/// rejects it fail-closed (a clear "definition drifted" error, not a silent
/// wrong-DAG replay) — the same guarantee G7 already gives every other resume path.
fn definition_resolver() -> DefinitionResolver {
    Arc::new(move |name: &str| {
        let dir = definitions_dir()?;
        let path = dir.join(format!("{}.yaml", sanitize_workflow_name(name)));
        let yaml = std::fs::read_to_string(path).ok()?;
        Definition::from_yaml(&yaml).ok()
    })
}

/// Spawn the background dispatcher loops that fire due wall-clock timers (G1) and
/// start due schedules (G2) without an operator ever running `apex workflows tick`
/// (RM-GA-P2 EXE-601), polling every `interval` (`serve()` reads
/// `APEX_DISPATCH_INTERVAL_SECS`, default 5s; tests pass a short interval directly
/// rather than racing on the process-global env var). Returns the task handles so
/// the caller can abort them when the server itself stops serving — the loops are
/// not meant to outlive the process's HTTP server.
fn spawn_dispatch_loops(
    state: &Arc<AppState>,
    interval: Duration,
) -> Vec<tokio::task::JoinHandle<()>> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let resolver = definition_resolver();

    let timer_dispatcher = TimerDispatcher::new(
        state.workflows.clone(),
        state.timers.clone(),
        clock.clone(),
        resolver.clone(),
    );
    let timers_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = timer_dispatcher.poll().await {
                tracing::error!(error = %e, "timer dispatcher poll failed");
            }
            tokio::time::sleep(interval).await;
        }
    });

    let schedule_dispatcher = ScheduleDispatcher::new(
        state.workflows.clone(),
        state.schedules.clone(),
        clock,
        resolver,
    );
    let schedules_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = schedule_dispatcher.poll().await {
                tracing::error!(error = %e, "schedule dispatcher poll failed");
            }
            tokio::time::sleep(interval).await;
        }
    });

    vec![timers_handle, schedules_handle]
}

/// Re-drive every execution left in a non-terminal state via the idempotent
/// `resume` (RM-GA-P2 EXE-602) — the crash-recovery entry point: `submit_handler`
/// drives an execution on a fire-and-forget `tokio::spawn`, so a server killed
/// mid-drive strands it wherever the last checkpoint landed, and nothing else ever
/// re-scans the store to pick it back up. Bounded concurrency guards against a
/// thundering herd if the store holds many in-flight executions at once.
async fn resume_in_flight_executions(state: &Arc<AppState>) {
    const MAX_CONCURRENT_RESUMES: usize = 8;

    let summaries = match state.workflows.list(&ExecutionFilter::default()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to list executions for startup resume");
            return;
        }
    };
    let pending: Vec<_> = summaries
        .into_iter()
        .filter(|s| !s.status.is_terminal())
        .collect();
    if pending.is_empty() {
        return;
    }
    tracing::info!(
        count = pending.len(),
        "resuming in-flight executions from startup"
    );

    let resolver = definition_resolver();
    futures::stream::iter(pending)
        .for_each_concurrent(MAX_CONCURRENT_RESUMES, |summary| {
            let engine = state.workflows.clone();
            let resolver = resolver.clone();
            async move {
                let Some(def) = (resolver)(&summary.workflow_name) else {
                    tracing::warn!(
                        execution_id = %summary.execution_id,
                        workflow = %summary.workflow_name,
                        "cannot resume at startup: no resolvable definition"
                    );
                    return;
                };
                if let Err(e) = engine.resume(&def, &summary.execution_id).await {
                    tracing::error!(
                        execution_id = %summary.execution_id,
                        error = %e,
                        "startup resume failed"
                    );
                }
            }
        })
        .await;
}

/// `~/.apex/workflows` — shared with the CLI. Also where the agent store
/// (`agents.json`) and the workflow-owners index (`owners.json`) persist
/// (RM-GA-P2 DUR-404): both are execution-adjacent state, so they live beside the
/// workflow checkpoints they describe rather than in a separate directory.
fn workflows_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".apex").join("workflows"))
}

/// `~/.apex/server` — durable state that is server-process-local, never shared with
/// or read by the CLI (RM-GA-P2 DUR-404: the idempotency cache and the daily quota
/// accumulator). Kept in its own directory rather than `workflows_dir()` precisely
/// *because* it isn't shared — mixing the two would blur that boundary.
fn server_state_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".apex").join("server"))
}

/// Load the persisted workflow-owners index from `path` (best-effort: a missing or
/// corrupt file starts empty rather than failing server startup).
fn load_owners(path: Option<&std::path::Path>) -> BTreeMap<String, String> {
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Translates gateway [`CostEvent`]s into Prometheus metrics
/// ([metrics §6](../../docs/14-observability/metrics.md)).
struct MetricsCostObserver {
    metrics: Metrics,
}

impl CostObserver for MetricsCostObserver {
    fn on_cost(&self, event: CostEvent) {
        self.metrics.counter_add(
            "apex_llm_tokens_total",
            &[("model", &event.model), ("type", "prompt")],
            event.prompt_tokens as f64,
        );
        self.metrics.counter_add(
            "apex_llm_tokens_total",
            &[("model", &event.model), ("type", "completion")],
            event.completion_tokens as f64,
        );
        self.metrics.counter_add(
            "apex_llm_cost_usd_total",
            &[("model", &event.model)],
            event.cost_usd,
        );
        if event.cache.is_some() {
            self.metrics.counter_add(
                "apex_cache_savings_usd_total",
                &[("subsystem", "llm")],
                event.estimated_savings_usd,
            );
        }
    }
}

/// Cross-cutting HTTP resource limits (SEC-201), resolved **once** at
/// [`AppState`] construction — not re-read from the environment on every request, so
/// a test can override them per-`AppState` (`with_http_limits`) without racing the
/// process-global env vars other tests implicitly depend on the defaults of.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HttpLimits {
    /// `APEX_HTTP_TIMEOUT_SECS` — the default per-request timeout, applied to every
    /// route except the direct agent-run endpoints.
    pub(crate) timeout_secs: u64,
    /// `APEX_HTTP_RUN_TIMEOUT_SECS` — a longer timeout for the direct agent-run
    /// endpoints (`agents:run`/`agents:stream`/`agents/{id}/run`), which call an LLM
    /// provider synchronously and can legitimately run far longer than the rest of
    /// the API. A route-scoped override rather than stretching the global timeout.
    pub(crate) run_timeout_secs: u64,
    /// `APEX_HTTP_MAX_BODY_BYTES` — the request body size cap; default 1 MiB.
    pub(crate) max_body_bytes: usize,
    /// `APEX_HTTP_MAX_CONCURRENCY` — the server-wide in-flight request cap; load past
    /// it is shed (`503`) rather than queued unboundedly.
    pub(crate) max_concurrency: usize,
}

impl HttpLimits {
    fn from_env() -> Self {
        Self {
            timeout_secs: env_u64("APEX_HTTP_TIMEOUT_SECS", 30),
            run_timeout_secs: env_u64("APEX_HTTP_RUN_TIMEOUT_SECS", 300),
            max_body_bytes: env_u64("APEX_HTTP_MAX_BODY_BYTES", 1024 * 1024) as usize,
            max_concurrency: env_u64("APEX_HTTP_MAX_CONCURRENCY", 512) as usize,
        }
    }
}

fn env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A `CorsLayer` allowing exactly `origins` (SEC-204), or `None` if the list is empty
/// — same-origin-only, the default posture, needs no layer at all: with no CORS
/// headers present a browser's own same-origin policy already blocks cross-origin
/// reads. Never combines a wildcard origin with credentialed requests (an explicit
/// origin list, never `Any`, alongside `allow_credentials(true)`).
fn cors_layer(origins: &[String]) -> Option<tower_http::cors::CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let allowed: Vec<header::HeaderValue> = origins
        .iter()
        .filter_map(|o| header::HeaderValue::from_str(o).ok())
        .collect();
    Some(
        tower_http::cors::CorsLayer::new()
            .allow_origin(allowed)
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
            ])
            .allow_headers([
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                header::IF_MATCH,
                header::HeaderName::from_static("x-apex-tenant"),
                header::HeaderName::from_static("x-apex-principal"),
                header::HeaderName::from_static("x-apex-project"),
                header::HeaderName::from_static("idempotency-key"),
            ])
            .expose_headers([
                header::HeaderName::from_static("x-request-id"),
                header::ETAG,
            ])
            .allow_credentials(true),
    )
}

/// Convert a boxed error from the timeout/load-shed layers into the standard error
/// envelope (SEC-201): a request that ran past its timeout is `408`; a request that
/// arrived while the server was already at `APEX_HTTP_MAX_CONCURRENCY` is `503` (the
/// caller should retry, not treat it as a permanent failure). Every layer these two
/// wrap is itself infallible, so no other error should reach here.
async fn handle_overload_or_timeout(err: BoxError) -> Response {
    if err.is::<tower::timeout::error::Elapsed>() {
        ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "the request exceeded the server's timeout",
        )
        .into_response()
    } else if err.is::<tower::load_shed::error::Overloaded>() {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "the server is at its concurrency limit; retry shortly",
        )
        .into_response()
    } else {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            err.to_string(),
        )
        .into_response()
    }
}

/// Build the application router over the given state.
///
/// Every route is gated by [`auth::authenticate`] (SEC-101) except the public
/// `/healthz` and `/metrics` — mounted on a separate, unauthenticated sub-router so a
/// load balancer's health probe never needs a credential. Cross-cutting resource
/// limits (SEC-201) wrap the whole router: a body-size cap, then a concurrency cap
/// with load-shedding (`503` past the limit, rather than an unboundedly growing
/// pending-request queue). The direct agent-run endpoints get a longer, dedicated
/// timeout than the rest of the API. Per-key rate limiting ([`rate_limit`], SEC-203)
/// runs *after* auth (so it keys off the verified principal, not a spoofable raw
/// header) with two tiers: a tighter one for the direct agent-run endpoints plus
/// KMS/secrets, a looser one for everything else.
pub fn router(state: Arc<AppState>) -> Router {
    let limits = state.http_limits;
    let cors = cors_layer(&state.cors_allowed_origins);
    let auth = || axum::middleware::from_fn_with_state(state.clone(), auth::authenticate);
    let rate_limit_sensitive = || {
        let limiter = state.rate_limiter_sensitive.clone();
        axum::middleware::from_fn(move |headers: HeaderMap, req: Request, next: Next| {
            rate_limit::enforce(limiter.clone(), headers, req, next)
        })
    };
    let rate_limit_standard = || {
        let limiter = state.rate_limiter_standard.clone();
        axum::middleware::from_fn(move |headers: HeaderMap, req: Request, next: Next| {
            rate_limit::enforce(limiter.clone(), headers, req, next)
        })
    };

    // The direct agent-run endpoints call an LLM provider synchronously and can
    // legitimately run far longer than the rest of the API. Expensive, so a tighter
    // rate-limit tier too (SEC-203), shared with KMS/secrets below.
    let run_routes = Router::new()
        .route("/api/v1/agents:run", post(run_handler))
        .route("/api/v1/agents:stream", post(run_stream_handler))
        .route("/api/v1/agents/{id}/run", post(run_stored_handler))
        // Auth first, so the rate limiter keys off the *verified* principal
        // (auth overwrites `X-Apex-Principal`) — not a spoofable raw header.
        .layer(rate_limit_sensitive())
        .layer(auth())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.run_timeout_secs)),
        );

    // KMS + secrets: the default (not the run-routes') timeout, but the same tighter
    // rate-limit tier as the agent-run endpoints (SEC-203) — sensitive, not merely
    // slow.
    let sensitive_routes = Router::new()
        .merge(secrets::routes())
        .merge(kms::routes())
        .layer(rate_limit_sensitive())
        .layer(auth())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.timeout_secs)),
        );

    let other_protected = Router::new()
        // Agent persistence: register agents once, then run/inspect them by id.
        .route(
            "/api/v1/agents",
            post(create_agent_handler).get(list_agents_handler),
        )
        .route(
            "/api/v1/agents/{id}",
            get(get_agent_handler).delete(delete_agent_handler),
        )
        // Workflow visibility (G4): list/inspect executions + a minimal read-only UI.
        .route("/api/v1/workflows", get(list_workflows_handler))
        .route("/api/v1/workflows/{id}", get(get_workflow_handler))
        .route("/workflows", get(workflows_ui_handler))
        // Workflow builder write-path: validate, submit, signal, approve, cancel.
        .merge(workflow_runner::routes())
        // Multi-tenancy: organizations, projects, memberships, quotas (RBAC-gated).
        .merge(tenancy::routes())
        // Webhooks: register/list/delete subscriptions (RBAC-gated).
        .merge(webhooks::routes())
        // Memory explorer: namespaces, records, hybrid query, put.
        .merge(memory::routes())
        // Plugins: list the installed catalog, enable/disable.
        .merge(plugins::routes())
        // Marketplace: publish, discover, download, rate, verify, install.
        .merge(marketplace::routes())
        // Audit trail: read the tenant's tamper-evident security records.
        .merge(audit::routes())
        // Tool discovery: list registered tools (built-ins + enabled plugin tools).
        .merge(tools::routes())
        .layer(rate_limit_standard())
        .layer(auth())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.timeout_secs)),
        );

    let public = Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_secs(limits.timeout_secs)),
        );

    let app = public
        .merge(run_routes)
        .merge(sensitive_routes)
        .merge(other_protected)
        .with_state(state)
        // Stamp every response (incl. errors) with a request id (API overview §14).
        .layer(axum::middleware::from_fn(hardening::request_id))
        .layer(DefaultBodyLimit::max(limits.max_body_bytes))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .load_shed()
                .concurrency_limit(limits.max_concurrency),
        );

    // Outermost: a CORS preflight (`OPTIONS`) must never reach auth (which would
    // otherwise reject it for lacking a credential) — tower_http's CorsLayer answers
    // preflight requests directly.
    match cors {
        Some(cors) => app.layer(cors),
        None => app,
    }
}

/// Metrics endpoint ([metrics §2](../../docs/14-observability/metrics.md)). Serves
/// **OpenMetrics** (with trace exemplars) when the scraper accepts it, else classic
/// Prometheus text.
async fn metrics_handler(headers: HeaderMap, State(state): State<Arc<AppState>>) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("application/openmetrics-text") {
        (
            [(
                header::CONTENT_TYPE,
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )],
            state.metrics.render_openmetrics(),
        )
            .into_response()
    } else {
        (
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            state.metrics.render_prometheus(),
        )
            .into_response()
    }
}

/// Install rustls' `ring` `CryptoProvider` as the process default, idempotently. Both
/// `ring` (via `reqwest`'s rustls-tls elsewhere in this dependency graph) and
/// `aws-lc-rs` can end up link-able in the same binary; rustls then refuses to guess
/// and panics on the first TLS connection unless one is installed explicitly first.
fn install_default_crypto_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// TLS material paths from `APEX_TLS_CERT`/`APEX_TLS_KEY` (PEM files), if both are
/// configured ([SEC-202](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)).
fn tls_cert_key_paths() -> Option<(String, String)> {
    let cert = std::env::var("APEX_TLS_CERT").ok()?;
    let key = std::env::var("APEX_TLS_KEY").ok()?;
    Some((cert, key))
}

/// Refuse a non-loopback bind that is neither TLS-terminated by this process
/// (`has_tls`) nor explicitly declared as terminated upstream by a reverse proxy
/// (`APEX_TLS_TERMINATED_UPSTREAM`) — cleartext HTTP (carrying credentials post-SEC-101
/// and secret responses) must never be the network-facing default (SEC-202). A
/// loopback bind is always allowed TLS-free, matching local/dev ergonomics elsewhere
/// in this hardening pass (SEC-102).
fn check_insecure_bind(has_tls: bool, addr: SocketAddr) -> apex_common::Result<()> {
    let terminated_upstream = std::env::var_os("APEX_TLS_TERMINATED_UPSTREAM").is_some();
    if !has_tls && !terminated_upstream && !addr.ip().is_loopback() {
        return Err(apex_common::Error::config(format!(
            "refusing to bind {addr} without TLS: set APEX_TLS_CERT and APEX_TLS_KEY \
             (PEM files), or APEX_TLS_TERMINATED_UPSTREAM=1 if a reverse proxy already \
             terminates TLS in front of this process"
        )));
    }
    Ok(())
}

/// Bind to `addr` and serve until the process is stopped. Serves HTTPS via an
/// in-process rustls acceptor when `APEX_TLS_CERT`/`APEX_TLS_KEY` are set (SEC-202);
/// otherwise refuses to bind a non-loopback address unless
/// `APEX_TLS_TERMINATED_UPSTREAM` declares TLS is handled by a reverse proxy.
pub async fn serve(addr: SocketAddr) -> apex_common::Result<()> {
    auth::refuse_anonymous_on_non_loopback(addr)?;
    let tls = tls_cert_key_paths();
    check_insecure_bind(tls.is_some(), addr)?;

    let state = Arc::new(AppState::from_env().await);
    // Crash recovery (RM-GA-P2 EXE-602): a `tokio::spawn`'d `resume` that never got
    // to run (the prior process died mid-drive) would otherwise strand its
    // execution in a non-terminal state forever — nothing else re-scans the store.
    resume_in_flight_executions(&state).await;
    // Background dispatcher loops (RM-GA-P2 EXE-601): without these, G1 durable
    // timers and G2 schedules only ever fired when an operator ran the CLI's
    // `apex workflows tick` on the same host — a `wait: {timer: {after: "30d"}}`
    // workflow submitted over HTTP would simply never resume. Aborted below
    // whenever this function's serving future returns, so they don't outlive the
    // HTTP server itself.
    let dispatch_interval = Duration::from_secs(env_u64("APEX_DISPATCH_INTERVAL_SECS", 5));
    let dispatch_handles = spawn_dispatch_loops(&state, dispatch_interval);
    let app = router(state);

    let result = match tls {
        Some((cert, key)) => {
            install_default_crypto_provider();
            match axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key).await {
                Ok(config) => {
                    tracing::info!(%addr, "apex server listening (https)");
                    // `with_connect_info` so `rate_limit`'s client-IP fallback
                    // (SEC-203) sees the real peer address for callers with no
                    // verified principal.
                    axum_server::bind_rustls(addr, config)
                        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                        .await
                        .map_err(|e| Error::Runtime(format!("server error: {e}")))
                }
                Err(e) => Err(Error::config(format!("failed to load TLS cert/key: {e}"))),
            }
        }
        None => match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "apex server listening (http)");
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .map_err(|e| Error::Runtime(format!("server error: {e}")))
            }
            Err(e) => Err(Error::Io(e)),
        },
    };

    for handle in dispatch_handles {
        handle.abort();
    }
    result
}

/// Liveness probe.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Body for `POST /api/v1/agents:run`.
#[derive(Debug, Deserialize)]
struct RunRequest {
    /// The agent manifest (YAML), supplied inline in v0.1.
    manifest: String,
    /// Run input (e.g. `{"message": "..."}`).
    #[serde(default)]
    input: Value,
    /// Override the model/tool iteration cap (default: [`apex_agent::RunOptions`]'s).
    #[serde(default)]
    max_steps: Option<usize>,
}

/// Run an agent, recording RED golden-signal metrics for the route. Instrumented so
/// the request runs under a trace whose id becomes the latency exemplar.
#[tracing::instrument(name = "api.agents_run", skip_all)]
async fn run_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Json<Value>, ApiError> {
    let start = Instant::now();
    let tenant = tenancy::run_tenant(&headers);

    // Idempotency (overview §9): replay the original response for a repeated key.
    let idem_key = hardening::idempotency_key(&headers);
    if let Some(key) = &idem_key
        && let Some(cached) = state.idempotency.get(&tenant, key)
    {
        return Ok(Json(cached));
    }

    let result = run_inner(&state, tenant.clone(), tenancy::run_project(&headers), req).await;

    // Cache successful responses so a client retry with the same key is safe.
    if let (Some(key), Ok(Json(body))) = (&idem_key, &result) {
        state.idempotency.put(&tenant, key, body.clone());
    }

    let status = match &result {
        Ok(_) => 200u16,
        Err(e) => e.status.as_u16(),
    };
    state.metrics.counter_inc(
        "apex_api_requests_total",
        &[("route", "agents_run"), ("status", &status.to_string())],
    );
    state.metrics.histogram_observe(
        "apex_api_request_duration_seconds",
        &[("route", "agents_run")],
        start.elapsed().as_secs_f64(),
    );
    result
}

/// Parse the inline manifest then run it ([Agents API §5](../../docs/09-api/agents.md)).
async fn run_inner(
    state: &Arc<AppState>,
    tenant: String,
    project: Option<String>,
    req: RunRequest,
) -> Result<Json<Value>, ApiError> {
    let def = AgentDefinition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    run_definition(
        state,
        def,
        req.input,
        &tenant,
        project.as_deref(),
        req.max_steps,
    )
    .await
}

/// A [`RunEventSink`] that forwards each run event to an SSE channel as a JSON frame.
struct ChannelSink {
    tx: futures::channel::mpsc::UnboundedSender<Event>,
}

impl RunEventSink for ChannelSink {
    fn emit(&mut self, event: RunEvent<'_>) {
        let data = match event {
            RunEvent::Start { model, provider } => {
                json!({ "type": "start", "model": model, "provider": provider })
            }
            RunEvent::MemoryRetrieved { source, score } => {
                json!({ "type": "memory", "source": source, "score": score })
            }
            RunEvent::Delta { text } => json!({ "type": "delta", "text": text }),
            RunEvent::ToolCall { name, arguments } => {
                json!({ "type": "tool_call", "name": name, "arguments": arguments })
            }
            RunEvent::ToolResult { name, ok } => {
                json!({ "type": "tool_result", "name": name, "ok": ok })
            }
            RunEvent::Done { usage } => json!({
                "type": "done",
                "usage": { "total_tokens": usage.total_tokens, "cost_usd": usage.cost_usd }
            }),
        };
        let _ = self
            .tx
            .unbounded_send(Event::default().data(data.to_string()));
    }
}

/// `POST /api/v1/agents:stream` — run an inline-manifest agent, streaming its events
/// (start / delta / tool_call / tool_result / done, then a final `result`) as SSE.
async fn run_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Response {
    let def = match AgentDefinition::from_yaml(&req.manifest) {
        Ok(d) => d,
        Err(e) => {
            return ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
                .into_response();
        }
    };
    let input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };

    // Quota gate before streaming begins (429 if exceeded); the permit rides along in
    // the run task and releases when it ends.
    let project = tenancy::run_project(&headers);
    let tenant = tenancy::run_tenant(&headers);
    let permit = match tenancy::admit_run(&state.tenancy, &state.quota, project.as_deref()) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let mut opts = RunOptions::new(input).with_tenant(tenant).with_hosted(true);
    // An explicit per-run override wins; otherwise fall back to the agent's own default.
    if let Some(n) = req.max_steps.or(def.spec.max_steps) {
        opts = opts.with_max_steps(n);
    }

    let (tx, rx) = futures::channel::mpsc::unbounded::<Event>();
    tokio::spawn(async move {
        let _permit = permit;
        let mut sink = ChannelSink { tx: tx.clone() };
        let frame = match run_agent(&def, &state.gateway, &state.registry, opts, &mut sink).await {
            Ok(out) => {
                tenancy::record_run_cost(&state.quota, project.as_deref(), out.usage.cost_usd);
                Event::default().event("result").data(
                    json!({ "status": "succeeded", "output": { "message": out.text }, "steps": out.steps })
                        .to_string(),
                )
            }
            Err(e) => Event::default().event("error").data(e.to_string()),
        };
        let _ = tx.unbounded_send(frame);
        // `tx` (and the sink's clone) drop here, closing the SSE stream.
    });

    Sse::new(rx.map(Ok::<Event, Infallible>)).into_response()
}

/// Run a (parsed) agent definition with `input`, returning the run-response shape.
/// Shared by the inline-manifest and stored-agent run endpoints. Enforces the in-scope
/// `project`'s quota (concurrent runs + daily LLM spend) when one is set.
async fn run_definition(
    state: &Arc<AppState>,
    def: AgentDefinition,
    input: Value,
    tenant: &str,
    project: Option<&str>,
    max_steps: Option<usize>,
) -> Result<Json<Value>, ApiError> {
    let input = if input.is_null() { json!({}) } else { input };

    // Quota gate: hold a concurrency slot for the duration of the run (released on
    // drop), then record the run's cost against the project's daily budget.
    let _permit = tenancy::admit_run(&state.tenancy, &state.quota, project)?;
    let mut opts = RunOptions::new(input).with_tenant(tenant).with_hosted(true);
    // An explicit per-run override wins; otherwise fall back to the agent's own default.
    if let Some(n) = max_steps.or(def.spec.max_steps) {
        opts = opts.with_max_steps(n);
    }
    let out = match run_agent(&def, &state.gateway, &state.registry, opts, &mut NullSink).await {
        Ok(out) => out,
        Err(e) => {
            webhooks::emit(
                state,
                "agent.run.failed",
                tenant,
                json!({ "error": e.to_string() }),
            );
            return Err(ApiError::from(e));
        }
    };
    tenancy::record_run_cost(&state.quota, project, out.usage.cost_usd);

    let run_id = format!("run_{}", state.run_counter.fetch_add(1, Ordering::SeqCst));
    webhooks::emit(
        state,
        "agent.run.completed",
        tenant,
        json!({ "run_id": run_id, "total_tokens": out.usage.total_tokens }),
    );
    Ok(Json(json!({
        "run_id": run_id,
        "status": "succeeded",
        "output": { "message": out.text },
        "steps": out.steps,
        "usage": {
            "total_tokens": out.usage.total_tokens,
            "cost_usd": out.usage.cost_usd,
        }
    })))
}

/// Body for `POST /api/v1/agents` — register an agent from its YAML manifest.
#[derive(Debug, Deserialize)]
struct CreateAgentRequest {
    /// The agent manifest (YAML).
    manifest: String,
}

/// Body for `POST /api/v1/agents/{id}/run` — run a stored agent.
#[derive(Debug, Default, Deserialize)]
struct RunStoredRequest {
    /// Run input (e.g. `{"message": "..."}`).
    #[serde(default)]
    input: Value,
    /// Override the model/tool iteration cap (default: [`apex_agent::RunOptions`]'s).
    #[serde(default)]
    max_steps: Option<usize>,
}

/// `POST /api/v1/agents` — register an agent; returns its id.
async fn create_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:write")?;
    let id = state.agents.create(&tenant, req.manifest)?;
    Ok(Json(json!({ "id": id, "status": "created" })))
}

/// `GET /api/v1/agents` — list the caller's tenant's agent ids (cursor-paginated,
/// overview §6).
async fn list_agents_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<hardening::PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:read")?;
    let items: Vec<Value> = state
        .agents
        .list(&tenant)
        .into_iter()
        .map(Value::String)
        .collect();
    Ok(Json(hardening::paginate(items, &page.page())))
}

/// `GET /api/v1/agents/{id}` — fetch a stored agent's manifest (within the caller's tenant).
async fn get_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:read")?;
    match state.agents.manifest(&tenant, &id) {
        Some(manifest) => Ok(Json(json!({ "id": id, "manifest": manifest }))),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        )),
    }
}

/// `DELETE /api/v1/agents/{id}` — remove a stored agent (within the caller's tenant).
async fn delete_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:write")?;
    if state.agents.delete(&tenant, &id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        ))
    }
}

/// `POST /api/v1/agents/{id}/run` — run a stored agent by id.
#[tracing::instrument(name = "api.agents_run_stored", skip_all)]
async fn run_stored_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<RunStoredRequest>,
) -> Result<Json<Value>, ApiError> {
    let start = Instant::now();
    let project = tenancy::run_project(&headers);
    // Authorize the run in the caller's tenant, then resolve the agent *within* that
    // tenant — a caller can only run its own tenant's stored agents.
    let result = match tenancy::tenant_authorize(&state, &headers, "agents:run") {
        Ok(tenant) => match state.agents.definition(&tenant, &id) {
            Some(def) => {
                run_definition(
                    &state,
                    def,
                    req.input,
                    &tenant,
                    project.as_deref(),
                    req.max_steps,
                )
                .await
            }
            None => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("agent `{id}` not found"),
            )),
        },
        Err(e) => Err(e),
    };

    let status = match &result {
        Ok(_) => 200u16,
        Err(e) => e.status.as_u16(),
    };
    state.metrics.counter_inc(
        "apex_api_requests_total",
        &[
            ("route", "agents_run_stored"),
            ("status", &status.to_string()),
        ],
    );
    state.metrics.histogram_observe(
        "apex_api_request_duration_seconds",
        &[("route", "agents_run_stored")],
        start.elapsed().as_secs_f64(),
    );
    result
}

/// Query params for `GET /api/v1/workflows`: filters plus cursor pagination.
#[derive(Debug, Deserialize)]
struct WorkflowListQuery {
    /// Filter to a workflow name.
    workflow: Option<String>,
    /// Filter to a status (e.g. `running`, `completed`, `failed`).
    status: Option<String>,
    /// `limit` + `cursor` (overview §6).
    #[serde(flatten)]
    page: hardening::PageQuery,
}

/// `GET /api/v1/workflows` — list executions, optionally filtered (G4 visibility),
/// cursor-paginated (overview §6).
async fn list_workflows_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkflowListQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    let status = match query
        .status
        .as_deref()
        .map(parse_workflow_status)
        .transpose()
    {
        Ok(status) => status,
        Err(msg) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation_failed",
                msg,
            ));
        }
    };
    // Fetch the full filtered set; pagination slices it (the cursor is the offset).
    let filter = ExecutionFilter {
        workflow_name: query.workflow,
        status,
        limit: None,
    };
    let executions = state.workflows.list(&filter).await?;
    let items: Vec<Value> = executions
        .into_iter()
        // Tenant isolation: only show executions this tenant owns.
        .filter(|e| state.workflow_visible(&e.execution_id, &tenant))
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Ok(Json(hardening::paginate(items, &query.page.page())))
}

/// `GET /api/v1/workflows/{id}` — an execution's status plus its event timeline (G4).
async fn get_workflow_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    // Hide cross-tenant executions behind the same 404 as a missing one.
    let missing = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("execution `{id}` not found"),
        )
    };
    if !state.workflow_visible(&id, &tenant) {
        return Err(missing());
    }
    let summary = state.workflows.status(&id).await?.ok_or_else(missing)?;
    let events = state.workflows.history(&id).await?;
    Ok(Json(json!({ "execution": summary, "events": events })))
}

/// Reject access to a workflow execution the caller's tenant does not own, hiding its
/// existence behind the same `404` a missing execution returns (used by the write-path
/// signal/approve/cancel routes).
pub(crate) fn require_workflow_visible(
    state: &AppState,
    id: &str,
    tenant: &str,
) -> Result<(), ApiError> {
    if state.workflow_visible(id, tenant) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("execution `{id}` not found"),
        ))
    }
}

/// `GET /workflows` — a minimal, read-only HTML UI over the visibility API (G4).
async fn workflows_ui_handler() -> Html<&'static str> {
    Html(WORKFLOWS_UI)
}

/// Parse a workflow status name (case-insensitive) for the list filter.
fn parse_workflow_status(s: &str) -> Result<WorkflowState, String> {
    match s.to_ascii_lowercase().as_str() {
        "created" => Ok(WorkflowState::Created),
        "validated" => Ok(WorkflowState::Validated),
        "scheduled" => Ok(WorkflowState::Scheduled),
        "running" => Ok(WorkflowState::Running),
        "waiting" => Ok(WorkflowState::Waiting),
        "resumed" => Ok(WorkflowState::Resumed),
        "compensating" => Ok(WorkflowState::Compensating),
        "completed" => Ok(WorkflowState::Completed),
        "failed" => Ok(WorkflowState::Failed),
        "cancelled" | "canceled" => Ok(WorkflowState::Cancelled),
        other => Err(format!("unknown status `{other}`")),
    }
}

/// A self-contained read-only UI that calls the visibility JSON API.
const WORKFLOWS_UI: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Apex Workflows</title>
<style>
  body { font: 14px system-ui, sans-serif; margin: 0; padding: 1.5rem; color: #1a1a1a; }
  h1 { font-size: 1.2rem; } h2 { font-size: 1rem; margin-top: 1.2rem; }
  .row { display: flex; gap: 1.5rem; align-items: flex-start; }
  .col { flex: 1; min-width: 0; }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: left; padding: .4rem .6rem; border-bottom: 1px solid #e3e3e3; }
  tbody tr { cursor: pointer; } tbody tr:hover { background: #f3f6ff; }
  .pill { padding: .1rem .5rem; border-radius: 1rem; font-size: .8rem; }
  .Running, .Waiting { background: #fff4d6; } .Completed { background: #d8f5dd; }
  .Failed, .Cancelled { background: #fbdcdc; }
  pre { background: #f6f8fa; padding: .8rem; border-radius: 6px; overflow-x: auto; }
  .muted { color: #888; } button { font: inherit; }
</style>
</head>
<body>
  <h1>Apex Workflows</h1>
  <div class="muted">Read-only visibility over <code>/api/v1/workflows</code>.
    <button onclick="load()">Refresh</button></div>
  <div class="row" style="margin-top:1rem">
    <div class="col">
      <h2>Executions</h2>
      <table><thead><tr><th>id</th><th>workflow</th><th>status</th></tr></thead>
      <tbody id="list"><tr><td colspan="3" class="muted">loading…</td></tr></tbody></table>
    </div>
    <div class="col"><h2>Detail</h2><div id="detail" class="muted">Select an execution.</div></div>
  </div>
<script>
async function load() {
  const tbody = document.getElementById('list');
  try {
    const res = await fetch('/api/v1/workflows');
    const data = await res.json();
    const rows = (data.data || []);
    if (!rows.length) { tbody.innerHTML = '<tr><td colspan="3" class="muted">no executions</td></tr>'; return; }
    tbody.innerHTML = '';
    for (const e of rows) {
      const tr = document.createElement('tr');
      tr.onclick = () => detail(e.execution_id);
      tr.innerHTML = `<td>${e.execution_id}</td><td>${e.workflow_name}</td>`
        + `<td><span class="pill ${e.status}">${e.status}</span></td>`;
      tbody.appendChild(tr);
    }
  } catch (err) { tbody.innerHTML = `<tr><td colspan="3">error: ${err}</td></tr>`; }
}
async function detail(id) {
  const el = document.getElementById('detail');
  el.textContent = 'loading…';
  try {
    const res = await fetch('/api/v1/workflows/' + encodeURIComponent(id));
    if (!res.ok) { el.textContent = 'not found'; return; }
    const d = await res.json();
    const acts = Object.entries(d.execution.activities || {})
      .map(([k, v]) => `<li>${k}: <span class="pill ${v}">${v}</span></li>`).join('');
    const events = (d.events || []).map((ev, i) => `${String(i + 1).padStart(3)}. ${JSON.stringify(ev)}`).join('\n');
    el.innerHTML = `<div><b>${d.execution.execution_id}</b> — ${d.execution.workflow_name} `
      + `v${d.execution.workflow_version} — <span class="pill ${d.execution.status}">${d.execution.status}</span></div>`
      + `<ul>${acts}</ul><h2>Timeline</h2><pre>${events.replace(/</g, '&lt;')}</pre>`;
  } catch (err) { el.textContent = 'error: ' + err; }
}
load();
</script>
</body>
</html>"#;

/// An API error rendered as the standard envelope.
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Config(m) | Error::Invalid(m) => {
                ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", m)
            }
            Error::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, "not_found", m),
            Error::Forbidden(m) => ApiError::new(StatusCode::FORBIDDEN, "forbidden", m),
            Error::Conflict(m) => ApiError::new(StatusCode::CONFLICT, "conflict", m),
            Error::QuotaExceeded(m) => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, "quota_exceeded", m)
            }
            Error::Provider(m) => ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", m),
            other => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                other.to_string(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let kind = match self.status.as_u16() {
            400..=499 => "client_error",
            _ => "server_error",
        };
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "type": kind,
                "status": self.status.as_u16(),
            }
        });
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    async fn test_app() -> Router {
        router(Arc::new(AppState::from_env().await))
    }

    #[tokio::test]
    async fn healthz_ok() {
        let app = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Build a router whose workflow engine is seeded with one completed execution.
    async fn workflow_app() -> Router {
        use apex_workflow::{ClosureExecutor, Definition};
        let store = InMemoryStore::new();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!("ok")) });
        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let def = Definition::from_yaml(
            "metadata:\n  name: demo\nspec:\n  activities:\n    - {id: a, type: function}\n",
        )
        .unwrap();
        engine.run(&def, "demo-1", json!({})).await.unwrap();
        router(Arc::new(AppState::from_env().await.with_workflows(engine)))
    }

    #[tokio::test]
    async fn lists_and_inspects_workflow_executions() {
        let app = workflow_app().await;

        // List returns the seeded execution.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["data"][0]["execution_id"], "demo-1");
        assert_eq!(v["data"][0]["status"], "Completed");
        assert_eq!(v["has_more"], false);

        // Status filter that excludes it yields an empty list.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows?status=running")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["data"].as_array().unwrap().len(), 0);

        // Detail returns the summary plus a non-empty event timeline.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows/demo-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["execution"]["activities"]["a"], "Completed");
        assert!(v["events"].as_array().unwrap().len() >= 4);

        // Unknown execution → 404.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows/missing")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_returns_agent_output() {
        let app = test_app().await;
        let body = json!({
            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
            "input": { "message": "ping" }
        })
        .to_string();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert!(v["run_id"].as_str().unwrap().starts_with("run_"));
        assert!(v["output"]["message"].as_str().unwrap().contains("ping"));
    }

    /// A caller-supplied `max_steps` reaches [`apex_agent::RunOptions`] rather than
    /// always falling back to the crate default. `max_steps: 0` can't complete even one
    /// model turn, so it must fail instead of silently running with the default budget.
    #[tokio::test]
    async fn max_steps_override_is_honored() {
        let state = Arc::new(AppState::from_env().await);
        let manifest = "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n";

        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" }, "max_steps": 0 }),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("0 steps"),
            "{body}"
        );

        // Omitting the field keeps the existing default-budget behavior.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
    }

    /// An agent's own `spec.max_steps` (settable from the Agent Studio UI) is used as
    /// the run's default budget, but a caller-supplied request-level override still
    /// takes precedence over it — checked via the stored-agent run path too, since that
    /// resolves the definition server-side rather than trusting a request field.
    #[tokio::test]
    async fn agent_level_max_steps_is_a_default_not_a_floor() {
        let state = Arc::new(AppState::from_env().await);
        let manifest =
            "metadata:\n  name: zero-budget\nspec:\n  instructions: Be friendly.\n  max_steps: 0\n";

        // No request-level override → the agent's own (unworkable) budget applies and fails.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" } }),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("0 steps"),
            "{body}"
        );

        // A request-level override wins over the agent's own default.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents:run",
            json!({ "manifest": manifest, "input": { "message": "ping" }, "max_steps": 5 }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        // Same precedence via the stored-agent path (POST /api/v1/agents/{id}/run), which
        // resolves the definition from the store rather than the inline request body.
        let (_, created) = req(
            &state,
            "POST",
            "/api/v1/agents",
            json!({ "manifest": manifest }),
        )
        .await;
        let id = created["id"].as_str().unwrap();
        let (st, body) = req(
            &state,
            "POST",
            &format!("/api/v1/agents/{id}/run"),
            json!({ "input": { "message": "ping" } }),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    }

    #[tokio::test]
    async fn metrics_endpoint_reflects_a_run() {
        // Share one state across two routers so the run's metrics are visible.
        let state = Arc::new(AppState::from_env().await);
        let body = json!({
            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
            "input": { "message": "ping" }
        })
        .to_string();

        let run = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(run.status(), StatusCode::OK);

        let metrics = router(state)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metrics.status(), StatusCode::OK);
        let bytes = to_bytes(metrics.into_body(), 64 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(text.contains("apex_api_requests_total"), "metrics:\n{text}");
        assert!(text.contains(r#"route="agents_run""#));
        assert!(text.contains("apex_api_request_duration_seconds_count"));
        // The mock provider reports a cost, so an LLM cost metric is present.
        assert!(text.contains("apex_llm_cost_usd_total"), "metrics:\n{text}");
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_openmetrics_when_accepted() {
        let state = Arc::new(AppState::from_env().await);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("accept", "application/openmetrics-text")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            ct.contains("application/openmetrics-text"),
            "content-type was {ct}"
        );
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            text.trim_end().ends_with("# EOF"),
            "OpenMetrics body:\n{text}"
        );
    }

    #[tokio::test]
    async fn invalid_manifest_is_400() {
        let app = test_app().await;
        let body = json!({ "manifest": "kind: Workflow\nmetadata:\n  name: x\nspec:\n  instructions: hi\n" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "client_error");
    }

    /// POST/GET/DELETE a JSON request against a shared state, returning (status, body).
    async fn req(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, v)
    }

    /// Issue an agent request as `principal` acting in `tenant` (with project header),
    /// returning (status, body).
    async fn tenant_req(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        tenant: &str,
        principal: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let resp = raw(
            state,
            method,
            uri,
            &[("x-apex-tenant", tenant), ("x-apex-principal", principal)],
            body,
        )
        .await;
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Cross-tenant isolation: a stored agent created in one tenant is invisible and
    /// inaccessible to a principal of another tenant, and the `X-Apex-Tenant` header
    /// cannot be spoofed by a principal lacking a membership in the claimed tenant.
    /// (v0.3 exit criterion: zero cross-tenant leakage — agents surface.)
    #[tokio::test]
    async fn agents_are_isolated_per_tenant() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        // Two tenants, each with an org-admin and (in acme) a read-only viewer.
        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let member = |user: &str, role, org: &str| Membership {
            user: user.to_string(),
            role,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy
            .add_membership(member("alice", Role::OrgAdmin, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(member("carol", Role::Viewer, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(member("bob", Role::OrgAdmin, &org_b.id))
            .unwrap();
        let state = Arc::new(AppState::from_env().await.with_tenancy(tenancy));

        let manifest = "metadata:\n  name: secret-agent\nspec:\n  instructions: Be terse.\n";

        // Alice creates an agent in tenant `acme`.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents",
            "acme",
            "alice",
            json!({ "manifest": manifest }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["id"], "secret-agent");

        // Bob (tenant `beta`) sees an empty list and cannot read/run it by id (404).
        let (st, list) =
            tenant_req(&state, "GET", "/api/v1/agents", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["data"], json!([]), "beta must not see acme's agents");
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents/secret-agent",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents/secret-agent/run",
            "beta",
            "bob",
            json!({ "input": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // Header spoofing: Bob claims tenant `acme` but holds no membership there → 403,
        // for both read and the more dangerous delete.
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents/secret-agent",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "spoofed tenant must be denied");
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            "/api/v1/agents/secret-agent",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);

        // A viewer in acme may read but not create or run (scope granularity).
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents/secret-agent",
            "acme",
            "carol",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "viewer may read");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/agents/secret-agent/run",
            "acme",
            "carol",
            json!({ "input": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer may not run");

        // Alice (real member of acme) retains full access; the agent survived every
        // cross-tenant attempt above.
        let (st, list) = tenant_req(
            &state,
            "GET",
            "/api/v1/agents",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["data"], json!(["secret-agent"]));
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            "/api/v1/agents/secret-agent",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
    }

    /// Cross-tenant isolation for the workflow surface: an execution submitted in one
    /// tenant is invisible and inaccessible (list/get/signal/cancel) to another tenant,
    /// and a spoofed `X-Apex-Tenant` is rejected. (v0.3 exit criterion: zero
    /// cross-tenant leakage — workflows surface.)
    #[tokio::test]
    async fn workflows_are_isolated_per_tenant() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let member = |user: &str, org: &str| Membership {
            user: user.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(member("alice", &org_a.id)).unwrap();
        tenancy.add_membership(member("bob", &org_b.id)).unwrap();
        let state = Arc::new(AppState::from_env().await.with_tenancy(tenancy));

        let manifest = "metadata:\n  name: iso-wf\nspec:\n  activities:\n    - id: echo-step\n      type: function\n      name: echo\n      inputs:\n        message: hi\n";
        let exec_id = "wf-iso-acme-1";

        // Alice submits a workflow in tenant `acme`.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/workflows",
            "acme",
            "alice",
            json!({ "manifest": manifest, "execution_id": exec_id }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["execution_id"], exec_id);

        // Bob (tenant `beta`) cannot see it in his list, nor read/signal/cancel it (404).
        let (st, list) = tenant_req(
            &state,
            "GET",
            "/api/v1/workflows",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let beta_ids: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["execution_id"].as_str())
            .collect();
        assert!(
            !beta_ids.contains(&exec_id),
            "beta must not see acme's execution: {beta_ids:?}"
        );
        let (st, _) = tenant_req(
            &state,
            "GET",
            &format!("/api/v1/workflows/{exec_id}"),
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = tenant_req(
            &state,
            "POST",
            &format!("/api/v1/workflows/{exec_id}/signal"),
            "beta",
            "bob",
            json!({ "manifest": manifest, "event": "go" }),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "signal must not cross tenants");
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            &format!("/api/v1/workflows/{exec_id}"),
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "cancel must not cross tenants");

        // Header spoofing: Bob claims `acme` but holds no membership there → 403.
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/workflows",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "spoofed tenant must be denied");

        // Alice (real member of acme) sees and can act on her execution.
        let (st, alice_list) = tenant_req(
            &state,
            "GET",
            "/api/v1/workflows",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let acme_ids: Vec<&str> = alice_list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["execution_id"].as_str())
            .collect();
        assert!(
            acme_ids.contains(&exec_id),
            "acme must see its own execution: {acme_ids:?}"
        );
        let (st, _) = tenant_req(
            &state,
            "GET",
            &format!("/api/v1/workflows/{exec_id}"),
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "owner may read its execution");
    }

    /// Cross-tenant isolation for the memory surface: records stored by one tenant are
    /// invisible to another via namespaces, record listing, and hybrid query — even when
    /// both tenants use the same logical namespace — and `X-Apex-Tenant` cannot be
    /// spoofed. (v0.3 exit criterion: zero cross-tenant leakage — memory surface.)
    #[tokio::test]
    async fn memory_is_isolated_per_tenant() {
        use apex_memory::{InMemoryStore, MemoryEngine};
        use apex_provider::Gateway;
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let member = |user: &str, org: &str| Membership {
            user: user.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(member("alice", &org_a.id)).unwrap();
        tenancy.add_membership(member("bob", &org_b.id)).unwrap();

        let store: Arc<dyn apex_memory::MemoryStore> = Arc::new(InMemoryStore::new());
        let engine = MemoryEngine::new(Gateway::from_env(), store.clone());
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_memory(engine, store),
        );

        // Both tenants write into the *same* logical namespace "notes".
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory/records",
            "acme",
            "alice",
            json!({ "namespace": "notes", "content": "acme launch codes" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory/records",
            "beta",
            "bob",
            json!({ "namespace": "notes", "content": "beta picnic plan" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // Bob lists records in "notes" — sees only his own, never acme's.
        let (st, list) = tenant_req(
            &state,
            "GET",
            "/api/v1/memory/records?namespace=notes",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let contents: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect();
        assert_eq!(
            contents,
            vec!["beta picnic plan"],
            "beta sees only its own record"
        );

        // Bob's hybrid query cannot surface acme's record.
        let (st, q) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory:query",
            "beta",
            "bob",
            json!({ "text": "launch codes" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let found: Vec<&str> = q["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect();
        assert!(
            !found.iter().any(|c| c.contains("acme")),
            "beta query must not surface acme's record: {found:?}"
        );

        // Header spoofing: bob claims acme but isn't a member → 403.
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/memory/namespaces",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);

        // Alice sees her own record and namespace, and not beta's.
        let (st, ns) = tenant_req(
            &state,
            "GET",
            "/api/v1/memory/namespaces",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(ns["total"], 1, "acme sees only its own record count");
        let (st, q) = tenant_req(
            &state,
            "POST",
            "/api/v1/memory:query",
            "acme",
            "alice",
            json!({ "text": "launch codes" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let found: Vec<&str> = q["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["content"].as_str())
            .collect();
        assert!(
            found.iter().any(|c| c.contains("acme")),
            "acme owner must find its own record: {found:?}"
        );
    }

    /// The secret vault is tenant-scoped, RBAC-gated, and never returns a value over the
    /// API (values leave the vault only via the resolution/injection path).
    #[tokio::test]
    async fn secrets_are_isolated_masked_and_rbac_gated() {
        use apex_secrets::{InMemorySecretStore, Vault};
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let m = |user: &str, role, org: &str| Membership {
            user: user.to_string(),
            role,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy
            .add_membership(m("alice", Role::OrgAdmin, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(m("carol", Role::Viewer, &org_a.id))
            .unwrap();
        tenancy
            .add_membership(m("bob", Role::OrgAdmin, &org_b.id))
            .unwrap();
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_secrets(Vault::new(Arc::new(InMemorySecretStore::new()))),
        );

        // Alice creates a secret in acme — the response carries metadata, never the value.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets",
            "acme",
            "alice",
            json!({ "name": "api-key", "value": "s3cr3t-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["reference"], "secret://acme/api-key");
        assert_eq!(body["version"], 1);
        assert!(
            !body.to_string().contains("s3cr3t"),
            "the value must never appear in a response: {body}"
        );

        // A viewer in acme may read but not write (RBAC).
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/secrets",
            "acme",
            "carol",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "viewer may list");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets",
            "acme",
            "carol",
            json!({ "name": "x", "value": "y" }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer may not create");

        // Bob (beta) cannot see or read acme's secret, and cannot spoof the tenant.
        let (st, list) =
            tenant_req(&state, "GET", "/api/v1/secrets", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["total"], 0, "beta has no secrets of its own");
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/secrets/api-key",
            "beta",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = tenant_req(
            &state,
            "GET",
            "/api/v1/secrets/api-key",
            "acme",
            "bob",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "spoofed tenant denied");

        // Owner can rotate (version bumps) and the value is still never returned.
        let (st, rotated) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets/api-key/rotate",
            "acme",
            "alice",
            json!({ "value": "rotated-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rotated["version"], 2);
        assert!(!rotated.to_string().contains("rotated-value"));

        // Owner can delete.
        let (st, _) = tenant_req(
            &state,
            "DELETE",
            "/api/v1/secrets/api-key",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
    }

    /// Secret-management actions are written to the tamper-evident audit log (by
    /// reference, never value) and readable via the tenant-scoped audit route.
    #[tokio::test]
    async fn secret_mutations_are_audited() {
        use apex_audit::AuditLog;
        use apex_secrets::{InMemorySecretStore, Vault};
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let m = |u: &str, org: &str| Membership {
            user: u.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(m("alice", &org_a.id)).unwrap();
        tenancy.add_membership(m("bob", &org_b.id)).unwrap();
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_secrets(Vault::new(Arc::new(InMemorySecretStore::new())))
                .with_audit(AuditLog::in_memory()),
        );

        // Alice creates then rotates a secret.
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets",
            "acme",
            "alice",
            json!({ "name": "api-key", "value": "s3cr3t-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/secrets/api-key/rotate",
            "acme",
            "alice",
            json!({ "value": "rotated-value" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // The audit trail records both actions, by reference and without the value.
        let (st, audit) =
            tenant_req(&state, "GET", "/api/v1/audit", "acme", "alice", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let entries = audit["entries"].as_array().unwrap();
        let actions: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["event"]["action"].as_str())
            .collect();
        assert!(actions.contains(&"secret.create"), "actions: {actions:?}");
        assert!(actions.contains(&"secret.rotate"), "actions: {actions:?}");
        assert!(
            !audit.to_string().contains("s3cr3t") && !audit.to_string().contains("rotated-value"),
            "audit must not leak secret values: {audit}"
        );
        // Actor + resource are recorded (by reference).
        assert_eq!(entries[0]["event"]["actor"]["principal"], "alice");
        assert_eq!(
            entries[0]["event"]["resource"]["id"],
            "secret://acme/api-key"
        );

        // Tenant-scoped: a beta principal sees none of acme's audit records.
        let (st, beta) =
            tenant_req(&state, "GET", "/api/v1/audit", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(beta["total"], 0);
    }

    /// A fresh in-memory-backed KMS for tests, isolated from the shared `~/.apex/kms`.
    #[cfg(test)]
    fn test_kms() -> Arc<dyn apex_kms::Kms> {
        Arc::new(apex_kms::LocalKms::new(
            apex_kms::generate_key().unwrap(),
            Arc::new(apex_kms::InMemoryKmsStore::new()),
        ))
    }

    #[tokio::test]
    async fn kms_rotate_is_routine_but_destroy_needs_a_higher_tier() {
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let m = |user: &str, role| Membership {
            user: user.to_string(),
            role,
            scope: MemberScope::Organization(org.id.clone()),
        };
        tenancy.add_membership(m("vic", Role::Viewer)).unwrap();
        tenancy.add_membership(m("edna", Role::Editor)).unwrap();
        tenancy.add_membership(m("alice", Role::OrgAdmin)).unwrap();
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_kms(test_kms()),
        );

        // A viewer can do neither.
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "vic",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer must not rotate");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "vic",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "viewer must not destroy");

        // An editor may rotate (routine, `kms:write`) but not destroy (`kms:admin`).
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "edna",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["version"], 2); // provisions v1 on first use, then rolls to v2
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "edna",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "editor must not destroy");

        // An org admin may destroy — irreversibly: acme's key is crypto-shredded, so
        // even the org admin's own (RBAC-permitted) rotate now fails closed.
        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "destroyed");
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "a destroyed tenant key must fail closed even for an org admin"
        );
    }

    #[tokio::test]
    async fn kms_tenant_key_mutations_are_audited() {
        use apex_audit::AuditLog;
        use apex_tenancy::{MemberScope, Membership, Organization, Role};

        let tenancy = Arc::new(InMemoryTenancyStore::new());
        let org_a = tenancy
            .create_org(Organization::new("acme", "Acme"))
            .unwrap();
        let org_b = tenancy
            .create_org(Organization::new("beta", "Beta"))
            .unwrap();
        let m = |u: &str, org: &str| Membership {
            user: u.to_string(),
            role: Role::OrgAdmin,
            scope: MemberScope::Organization(org.to_string()),
        };
        tenancy.add_membership(m("alice", &org_a.id)).unwrap();
        tenancy.add_membership(m("bob", &org_b.id)).unwrap();
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(tenancy)
                .with_kms(test_kms())
                .with_audit(AuditLog::in_memory()),
        );

        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "acme",
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, audit) =
            tenant_req(&state, "GET", "/api/v1/audit", "acme", "alice", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let entries = audit["entries"].as_array().unwrap();
        let actions: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["event"]["action"].as_str())
            .collect();
        assert!(
            actions.contains(&"kms.tenant_key.rotate"),
            "actions: {actions:?}"
        );
        assert!(
            actions.contains(&"kms.tenant_key.destroy"),
            "actions: {actions:?}"
        );
        assert_eq!(entries[0]["event"]["actor"]["principal"], "alice");
        assert_eq!(entries[0]["event"]["resource"]["id"], "acme");

        // Tenant-scoped: beta sees none of acme's kms audit records.
        let (st, beta) =
            tenant_req(&state, "GET", "/api/v1/audit", "beta", "bob", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(beta["total"], 0);
    }

    /// **SEC-102**: the anonymous default-tenant bypass (`tenant_authorize` skipping
    /// its RBAC check for a request with no `X-Apex-Principal` against the default
    /// tenant) is no longer unconditional — it requires `AppState.anonymous_allowed`
    /// (`APEX_ALLOW_ANONYMOUS=1` in production, refused by [`crate::serve`] on any
    /// non-loopback bind; enabled here only via the explicit override). With it
    /// enabled, an anonymous caller can still crypto-shred the default tenant's key
    /// material through the public KMS route — the documented, explicit dev/local
    /// escape hatch, not an accidental gap (see
    /// [compliance-mapping.md §7](../../docs/13-security/compliance-mapping.md#7-residual-risk-and-gaps)).
    /// With it disabled (the production default), the same request is `403`.
    #[tokio::test]
    async fn anonymous_default_tenant_bypass_is_gated_by_the_allow_anonymous_flag() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_kms(test_kms())
                .with_anonymous_allowed(true),
        );

        // No `X-Apex-Principal` header, default tenant — the anonymous escape hatch,
        // not a configured role. Rotate first so the default tenant has key material
        // to destroy (a never-provisioned tenant would 404, testing the wrong thing).
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, body) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/destroy",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "explicit dev/local escape hatch");
        assert_eq!(body["status"], "destroyed");

        // Once shredded, even the *same* anonymous caller is fail-closed again —
        // the bypass grants the action once, it doesn't disable fail-closed
        // behavior afterward.
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }

    /// **SEC-102, production default**: with the anonymous escape hatch disabled (no
    /// `APEX_ALLOW_ANONYMOUS`), a credential-less request to a protected route is
    /// rejected at the perimeter by [`auth::authenticate`] (SEC-101) before
    /// `tenant_authorize`'s own (now equally fail-closed) RBAC check ever runs.
    #[tokio::test]
    async fn anonymous_default_tenant_caller_is_denied_when_the_flag_is_off() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_kms(test_kms())
                .with_anonymous_allowed(false),
        );
        let (st, _) = tenant_req(
            &state,
            "POST",
            "/api/v1/kms/tenant-key/rotate",
            "default",
            "",
            Value::Null,
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn tools_endpoint_lists_builtins_with_descriptions() {
        let state = Arc::new(AppState::from_env().await);
        let (st, body) = req(&state, "GET", "/api/v1/tools", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let tools = body["tools"].as_array().unwrap();
        // The safe-by-default built-ins are always registered.
        let ids: Vec<&str> = tools.iter().filter_map(|t| t["id"].as_str()).collect();
        for id in ["echo", "fs_read", "http_get"] {
            assert!(ids.contains(&id), "missing built-in tool `{id}`: {ids:?}");
        }
        // shell is NOT registered by default in a hosted server (SEC-301).
        assert!(!ids.contains(&"shell"), "shell must be opt-in: {ids:?}");
        // Each entry carries a non-empty description (what the UI shows).
        let fs_read = tools.iter().find(|t| t["id"] == "fs_read").unwrap();
        assert!(!fs_read["description"].as_str().unwrap_or("").is_empty());
    }

    /// `APEX_ENABLE_SHELL_TOOL=1` is the explicit operator opt-in (SEC-301) that
    /// re-enables `shell` in the server's own registry. Exercised via `with_registry`
    /// (what `from_env()` would build with the flag set) rather than actually
    /// mutating the process-global env var, which every other test in this crate's
    /// default shell-disabled behavior depends on.
    #[tokio::test]
    async fn shell_tool_opt_in_env_var_re_enables_it() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_registry(ToolRegistry::with_privileged_builtins()),
        );
        let (st, body) = req(&state, "GET", "/api/v1/tools", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        let ids: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["id"].as_str())
            .collect();
        assert!(ids.contains(&"shell"));
    }

    /// RM-GA-P2 DUR-404 acceptance: create an agent as tenant T, then open a *fresh*
    /// `AgentStore` instance against the same directory (the same "simulated restart"
    /// stand-in the crash-recovery tests elsewhere in this workspace use, since a real
    /// process restart isn't practical inside a unit test) — T's agent must still be
    /// visible, and it alone: the anonymous `default` tenant must not see it.
    #[test]
    fn agent_store_survives_a_restart_and_stays_tenant_scoped() {
        let dir =
            std::env::temp_dir().join(format!("apex_server_agent_restart_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("agents.json");

        {
            let store = AgentStore::new(Some(path.clone()));
            store
                .create(
                    "acme",
                    "metadata:\n  name: restart-test\nspec:\n  instructions: hi\n".to_string(),
                )
                .unwrap();
        }

        // A fresh instance — no in-memory state carried over — reopened against the
        // same path, the same shape a server restart takes.
        let reopened = AgentStore::new(Some(path));
        assert_eq!(reopened.list("acme"), vec!["restart-test".to_string()]);
        assert!(
            reopened.list("default").is_empty(),
            "the anonymous default tenant must not see acme's agent after a restart"
        );
        assert!(reopened.manifest("acme", "restart-test").is_some());
        assert!(reopened.manifest("default", "restart-test").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_persistence_lifecycle() {
        // A fresh in-memory agent store (DUR-404 persists the real default to disk,
        // which would accumulate agents from every prior test run and break this
        // test's exact-list assertions below).
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_agents(Arc::new(AgentStore::new(None))),
        );
        let manifest = "metadata:\n  name: persisted\nspec:\n  instructions: Be friendly.\n";

        // Create → returns the agent id.
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents",
            json!({ "manifest": manifest }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["id"], "persisted");

        // List includes it (paginated envelope); Get returns its manifest.
        let (_, list) = req(&state, "GET", "/api/v1/agents", Value::Null).await;
        assert_eq!(list["data"], json!(["persisted"]));
        assert_eq!(list["total_estimate"], 1);
        let (st, got) = req(&state, "GET", "/api/v1/agents/persisted", Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert!(got["manifest"].as_str().unwrap().contains("persisted"));

        // Run by id → succeeds and reflects the mock output.
        let (st, run) = req(
            &state,
            "POST",
            "/api/v1/agents/persisted/run",
            json!({ "input": { "message": "hi there" } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(run["status"], "succeeded");
        assert!(
            run["output"]["message"]
                .as_str()
                .unwrap()
                .contains("hi there")
        );

        // Delete → 204; subsequent get + run-by-id are 404.
        let (st, _) = req(&state, "DELETE", "/api/v1/agents/persisted", Value::Null).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = req(&state, "GET", "/api/v1/agents/persisted", Value::Null).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = req(
            &state,
            "POST",
            "/api/v1/agents/persisted/run",
            json!({ "input": {} }),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn streaming_endpoint_emits_sse_event_frames() {
        let state = Arc::new(AppState::from_env().await);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:stream")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "manifest": "metadata:\n  name: hello\nspec:\n  instructions: Be friendly.\n",
                            "input": { "message": "stream me" }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type was {ct}");

        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        // The run streamed start, multiple delta frames, a done, and a final result.
        assert!(body.contains(r#""type":"start""#), "body:\n{body}");
        assert!(
            body.matches(r#""type":"delta""#).count() > 1,
            "expected multiple delta frames:\n{body}"
        );
        assert!(body.contains(r#""type":"done""#), "body:\n{body}");
        assert!(body.contains("event: result"), "body:\n{body}");
        assert!(
            body.contains("stream me"),
            "delta text should reach the client:\n{body}"
        );
    }

    #[tokio::test]
    async fn create_agent_rejects_invalid_manifest() {
        let state = Arc::new(AppState::from_env().await);
        let (st, body) = req(
            &state,
            "POST",
            "/api/v1/agents",
            json!({ "manifest": "kind: Workflow\nbroken: true\n" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "client_error");
    }

    // --- /v1 hardening: request id, idempotency, pagination ----------------------

    /// Issue a request with arbitrary headers, returning the response (for header asserts).
    async fn raw(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Value,
    ) -> axum::http::Response<axum::body::Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        builder = builder.header("content-type", "application/json");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        router(state.clone())
            .oneshot(
                builder
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn request_id_is_generated_and_honored() {
        let state = Arc::new(AppState::from_env().await);
        // Generated when absent.
        let resp = raw(&state, "GET", "/healthz", &[], Value::Null).await;
        assert!(resp.headers().get("x-request-id").is_some());
        // Honored when supplied (client correlation).
        let resp = raw(
            &state,
            "GET",
            "/healthz",
            &[("x-request-id", "req-abc")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.headers()["x-request-id"], "req-abc");
    }

    #[tokio::test]
    async fn error_envelope_carries_request_id() {
        let state = Arc::new(AppState::from_env().await);
        let resp = raw(&state, "GET", "/api/v1/agents/missing", &[], Value::Null).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            v["error"]["request_id"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "error body should carry a request_id: {v}"
        );
    }

    #[tokio::test]
    async fn run_is_idempotent_per_key() {
        let state = Arc::new(AppState::from_env().await);
        let body = json!({
            "manifest": "metadata:\n  name: idem\nspec:\n  instructions: Hi.\n",
            "input": { "message": "hi" }
        });
        let run = |key: &'static str| {
            let state = state.clone();
            let body = body.clone();
            async move {
                let resp = raw(
                    &state,
                    "POST",
                    "/api/v1/agents:run",
                    &[("idempotency-key", key)],
                    body,
                )
                .await;
                let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                v["run_id"].as_str().unwrap().to_string()
            }
        };
        // Same key → same run_id (replayed); a different key → a fresh run.
        let first = run("k1").await;
        assert_eq!(run("k1").await, first);
        assert_ne!(run("k2").await, first);
    }

    #[tokio::test]
    async fn agent_list_is_cursor_paginated() {
        // A fresh in-memory agent store — see agent_persistence_lifecycle's comment.
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_agents(Arc::new(AgentStore::new(None))),
        );
        for name in ["alpha", "bravo", "charlie"] {
            let m = format!("metadata:\n  name: {name}\nspec:\n  instructions: hi\n");
            req(&state, "POST", "/api/v1/agents", json!({ "manifest": m })).await;
        }
        // First page of 2 → has_more + a cursor.
        let (_, p1) = req(&state, "GET", "/api/v1/agents?limit=2", Value::Null).await;
        assert_eq!(p1["data"].as_array().unwrap().len(), 2);
        assert_eq!(p1["has_more"], true);
        assert_eq!(p1["total_estimate"], 3);
        let cursor = p1["next_cursor"].as_str().unwrap();
        // Following the cursor returns the remainder.
        let (_, p2) = req(
            &state,
            "GET",
            &format!("/api/v1/agents?limit=2&cursor={cursor}"),
            Value::Null,
        )
        .await;
        assert_eq!(p2["data"].as_array().unwrap().len(), 1);
        assert_eq!(p2["has_more"], false);
        assert_eq!(p2["next_cursor"], Value::Null);
    }

    // --- RM-GA-P1 SEC-201: timeout / body-size / concurrency ------------------------

    fn default_limits() -> HttpLimits {
        HttpLimits::from_env()
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_with_413() {
        let state = Arc::new(AppState::from_env().await.with_http_limits(HttpLimits {
            max_body_bytes: 10,
            ..default_limits()
        }));
        let body = json!({
            "manifest": "metadata:\n  name: too-big\nspec:\n  instructions: hi\n",
            "input": { "message": "hi" }
        });
        let resp = raw(&state, "POST", "/api/v1/agents:run", &[], body).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Direct proof of the layer stack `router()` wires for the default/run timeouts
    /// (SEC-201): a request that outruns its timeout gets the standard `408` envelope,
    /// not a raw/unhandled error — built standalone (rather than against a real, fast
    /// mock-provider route) so the test is deterministic regardless of provider speed.
    #[tokio::test]
    async fn timeout_layer_converts_elapsed_into_408() {
        async fn slow() -> &'static str {
            tokio::time::sleep(Duration::from_millis(50)).await;
            "too slow"
        }
        let app: Router = Router::new().route("/slow", get(slow)).layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                .timeout(Duration::from_millis(5)),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "request_timeout");
    }

    /// SEC-201: "the run permit is released when the client-facing timeout fires."
    /// Proves the general mechanism `RunPermit`'s `Drop` impl relies on — tower's
    /// `Timeout` drops the inner future outright on expiry, so any RAII guard held
    /// across the timed-out `.await` is released — without depending on the real
    /// (near-instant, hard-to-artificially-slow) mock provider actually timing out.
    #[tokio::test]
    async fn timeout_drops_the_inner_future_releasing_raii_guards_held_across_it() {
        use std::sync::atomic::AtomicUsize;

        struct Permit(Arc<AtomicUsize>);
        impl Drop for Permit {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let held = Arc::new(AtomicUsize::new(0));
        let counter = held.clone();
        let app: Router = Router::new()
            .route(
                "/slow",
                get(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let _permit = Permit(counter.clone());
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        "done"
                    }
                }),
            )
            .layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(handle_overload_or_timeout))
                    .timeout(Duration::from_millis(10)),
            );

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);

        // The dropped future's Drop glue runs synchronously as part of the timeout
        // race resolving; a short grace sleep just avoids asserting mid-poll.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            held.load(Ordering::SeqCst),
            0,
            "the RAII guard held across the timed-out .await must be dropped"
        );
    }

    /// SEC-201: "load past the concurrency cap sheds cleanly" — a request arriving
    /// while the server is already at `APEX_HTTP_MAX_CONCURRENCY` is refused
    /// (`Overloaded`, which `handle_overload_or_timeout` maps to `503`) rather than
    /// queued, and the limit isn't a one-shot: once the in-flight call completes, the
    /// next one is admitted again. Exercises the exact `tower::ServiceBuilder` stack
    /// `router()` wires (`load_shed().concurrency_limit(n)`) directly over a bare
    /// `tower::service_fn`, rather than through a full `axum::Router` — axum's Router
    /// always reports itself ready regardless of inner backpressure (by design, so a
    /// single route's slowness can't stall routing to other routes), which would hide
    /// the very backpressure this test needs to observe.
    #[tokio::test]
    async fn concurrency_limit_sheds_load_past_the_cap_then_recovers() {
        use tower::Service;
        use tower::service_fn;

        let notify = Arc::new(tokio::sync::Notify::new());
        let held = notify.clone();
        let inner = service_fn(move |_req: ()| {
            let held = held.clone();
            async move {
                held.notified().await;
                Ok::<_, Infallible>("done")
            }
        });
        let svc = ServiceBuilder::new()
            .load_shed()
            .concurrency_limit(1)
            .service(inner);

        // The first call occupies the single concurrency slot, blocked on the notify
        // (standing in for a slow real request).
        let mut svc1 = svc.clone();
        let first = tokio::spawn(async move { svc1.ready().await.unwrap().call(()).await });
        // Let the first call actually get admitted and start waiting.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // A second, concurrent call is shed — an error, not queued.
        let mut svc2 = svc.clone();
        let result2 = svc2.ready().await.unwrap().call(()).await;
        assert!(result2.is_err(), "expected the second call to be shed");

        // Releasing the first call frees the slot for the next caller.
        notify.notify_one();
        let result1 = first.await.unwrap();
        assert!(result1.is_ok());
    }

    // --- RM-GA-P1 SEC-202: TLS termination or refuse insecure non-loopback bind -----

    #[test]
    fn insecure_bind_is_refused_only_without_tls_and_without_upstream_termination_on_non_loopback()
    {
        let non_loopback: SocketAddr = "0.0.0.0:8443".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:8443".parse().unwrap();

        // Loopback is always fine, TLS or not — local/dev ergonomics (SEC-102 parity).
        assert!(check_insecure_bind(false, loopback).is_ok());
        assert!(check_insecure_bind(true, loopback).is_ok());

        // Non-loopback with no TLS and no upstream-termination declaration → refused.
        assert!(check_insecure_bind(false, non_loopback).is_err());
        // This process terminating TLS itself → fine.
        assert!(check_insecure_bind(true, non_loopback).is_ok());
    }

    /// Real end-to-end proof the TLS path actually serves HTTPS (not just compiles):
    /// a self-signed cert (minted fresh via `rcgen`, never touching disk) is loaded
    /// through the exact same `RustlsConfig::from_pem_file` + `axum_server::bind_rustls`
    /// call `serve()` makes, and a real TLS client round-trips `/healthz` over it.
    #[tokio::test]
    async fn tls_config_serves_https_end_to_end() {
        // Mint a self-signed cert for `127.0.0.1` and write it to a temp dir — same
        // shape an operator's real PEM files would take (`from_pem_file` reads paths).
        install_default_crypto_provider();
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let dir = std::env::temp_dir().join(format!("apex-tls-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();

        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .unwrap();

        // Bind an OS-assigned port, then hand it to axum-server's TLS acceptor.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(Arc::new(AppState::from_env().await));
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, config)
                .unwrap()
                .serve(app.into_make_service())
                .await
        });

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // self-signed — the point under test
            .build()
            .unwrap();
        let resp = client
            .get(format!("https://{addr}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- RM-GA-P1 SEC-203: per-principal rate limiting -------------------------------

    /// A caller past their bucket's budget gets `429` with `Retry-After` — and it's
    /// genuinely per-principal: a different caller has an untouched bucket.
    #[tokio::test]
    async fn standard_tier_rate_limit_returns_429_with_retry_after_then_isolates_by_principal() {
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_rate_limiter_standard(rate_limit::RateLimiter::new(2, 2)),
        );

        for _ in 0..2 {
            let resp = raw(
                &state,
                "GET",
                "/api/v1/tools",
                &[("x-apex-principal", "alice")],
                Value::Null,
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
        let resp = raw(
            &state,
            "GET",
            "/api/v1/tools",
            &[("x-apex-principal", "alice")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().get("retry-after").is_some());

        // bob has his own bucket — untouched by alice being rate-limited.
        let resp = raw(
            &state,
            "GET",
            "/api/v1/tools",
            &[("x-apex-principal", "bob")],
            Value::Null,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Rate limiting must run *after* auth: the bucket key is the verified principal
    /// (which auth overwrites `X-Apex-Principal` with), not a client-supplied header —
    /// so presenting a fresh, spoofed principal on every request cannot evade the
    /// limiter as long as the underlying credential is the same real caller.
    #[tokio::test]
    async fn rate_limit_keys_off_the_verified_principal_not_a_spoofed_header() {
        use axum::body::Body;

        let keys = InMemoryApiKeyStore::new();
        keys.insert("alice-key", "alice");
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_api_keys(Arc::new(keys))
                .with_auth_mode(AuthMode::ApiKey)
                .with_rate_limiter_standard(rate_limit::RateLimiter::new(1, 1)),
        );

        let call = |spoofed_principal: &'static str| {
            let state = state.clone();
            async move {
                router(state)
                    .oneshot(
                        Request::builder()
                            .method("GET")
                            .uri("/api/v1/tools")
                            .header("authorization", "Bearer alice-key")
                            .header("x-apex-principal", spoofed_principal)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        assert_eq!(call("attacker-1").await.status(), StatusCode::OK);
        // A different spoofed header, same real credential — still rate-limited,
        // since the bucket key came from the verified `alice`, not these headers.
        assert_eq!(
            call("attacker-2").await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    // --- RM-GA-P1 SEC-204: CORS allow-list -------------------------------------------

    /// A configured origin's preflight is answered with that exact origin allowed; an
    /// unlisted origin's preflight doesn't 4xx (tower_http just omits the allow
    /// header), which is what tells the browser not to expose the response to script.
    #[tokio::test]
    async fn configured_origin_passes_preflight_but_unlisted_origin_gets_no_allow_header() {
        use axum::body::Body;

        let state = Arc::new(
            AppState::from_env()
                .await
                .with_cors_allowed_origins(vec!["https://dashboard.example.com".to_string()]),
        );

        let preflight = |origin: &'static str| {
            let state = state.clone();
            async move {
                router(state)
                    .oneshot(
                        Request::builder()
                            .method("OPTIONS")
                            .uri("/api/v1/tools")
                            .header("origin", origin)
                            .header("access-control-request-method", "GET")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        let resp = preflight("https://dashboard.example.com").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("access-control-allow-origin").unwrap(),
            "https://dashboard.example.com"
        );

        let resp = preflight("https://evil.example.com").await;
        assert!(resp.headers().get("access-control-allow-origin").is_none());
    }

    /// Default posture (`APEX_CORS_ALLOWED_ORIGINS` unset/empty): no `CorsLayer` at
    /// all, so no CORS headers appear on any response — same-origin only.
    #[tokio::test]
    async fn no_configured_origins_means_no_cors_headers_at_all() {
        let state = Arc::new(AppState::from_env().await.with_cors_allowed_origins(vec![]));
        let resp = raw(
            &state,
            "GET",
            "/healthz",
            &[("origin", "https://dashboard.example.com")],
            Value::Null,
        )
        .await;
        assert!(resp.headers().get("access-control-allow-origin").is_none());
    }
}
