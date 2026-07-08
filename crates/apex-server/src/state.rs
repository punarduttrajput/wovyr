//! Server state: persisted-agent storage, async-run tracking, and the shared
//! [`AppState`] every route handler operates on (RM-GA-P4 HLTH-904 — split out
//! of `lib.rs`, which had grown into a god module mixing state, config
//! factories, and route handlers in one file).

use crate::{auth, hardening, plugins, rate_limit, tenancy, webhooks};
use apex_agent::AgentDefinition;
use apex_events::{BackoffPolicy, WebhookStore};
use apex_provider::Gateway;
use apex_telemetry::Metrics;
use apex_tenancy::TenancyStore;
use apex_tools::ToolRegistry;
use apex_workflow::{Engine, ScheduleStore, TimerStore};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

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
pub(crate) struct AgentStore {
    inner: RwLock<BTreeMap<(String, String), String>>,
    path: Option<PathBuf>,
}

impl AgentStore {
    /// Open a store, loading any existing catalog from `path` (best-effort: a missing
    /// or corrupt file starts empty rather than failing server startup). `path: None`
    /// is a purely in-memory store — what every test that cares about a clean,
    /// non-leaking agent list uses via [`crate::AppState::with_agents`].
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
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
    pub(crate) fn create(&self, tenant: &str, manifest: String) -> Result<String, crate::ApiError> {
        let def = AgentDefinition::from_yaml(&manifest).map_err(|e| {
            crate::ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
        })?;
        let id = def.metadata.name.clone();
        let mut inner = self.inner.write().expect("agent store poisoned");
        inner.insert((tenant.to_string(), id.clone()), manifest);
        self.persist(&inner);
        Ok(id)
    }

    /// The stored manifest for `id` within `tenant`, if any.
    pub(crate) fn manifest(&self, tenant: &str, id: &str) -> Option<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .get(&(tenant.to_string(), id.to_string()))
            .cloned()
    }

    /// The parsed definition for `id` within `tenant` (manifests are validated on create).
    pub(crate) fn definition(&self, tenant: &str, id: &str) -> Option<AgentDefinition> {
        self.manifest(tenant, id)
            .and_then(|m| AgentDefinition::from_yaml(&m).ok())
    }

    /// All stored agent ids in `tenant`, sorted.
    pub(crate) fn list(&self, tenant: &str) -> Vec<String> {
        self.inner
            .read()
            .expect("agent store poisoned")
            .iter()
            .filter(|((t, _), _)| t == tenant)
            .map(|((_, id), _)| id.clone())
            .collect()
    }

    /// Remove `id` within `tenant`; returns whether it existed.
    pub(crate) fn delete(&self, tenant: &str, id: &str) -> bool {
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

/// The current disposition of an asynchronously-submitted agent run (RM-GA-P2
/// EXE-604).
#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum AsyncRunStatus {
    Running,
    Succeeded {
        output: Value,
        steps: usize,
        usage: Value,
    },
    Failed {
        error: String,
    },
}

struct RunRecord {
    tenant: String,
    status: AsyncRunStatus,
    inserted_at: Instant,
}

/// Tracks runs submitted via `POST /api/v1/agents:run` with `Prefer: respond-async`
/// (RM-GA-P2 EXE-604), so `GET /api/v1/agents/runs/{id}` has something to poll.
/// In-memory only, unlike `AgentStore`/`workflow_owners`/etc. (DUR-404's durable
/// pieces): an agent run has no checkpoint to resume from, so a run truly in flight
/// when the process dies is gone regardless of whether its *record* survives —
/// persisting just the record would misleadingly suggest otherwise. Bounded the
/// same way `hardening::IdempotencyStore` is (SEC-205's discipline): entries expire
/// after `ttl` and the map is capped at `max_entries`, so a long-lived server
/// doesn't accumulate one entry per run forever.
pub(crate) struct RunStore {
    inner: Mutex<RunStoreInner>,
    ttl: Duration,
    max_entries: usize,
}

#[derive(Default)]
struct RunStoreInner {
    entries: HashMap<String, RunRecord>,
    order: VecDeque<String>,
}

impl RunStore {
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(RunStoreInner::default()),
            ttl,
            max_entries,
        }
    }

    /// Mirrors `IdempotencyStore::evict`: drop expired entries, or — once at
    /// capacity — the single oldest entry regardless of expiry, to admit one more.
    fn evict(inner: &mut RunStoreInner, ttl: Duration, max_entries: usize, make_room: bool) {
        while let Some(front) = inner.order.front() {
            let expired = inner
                .entries
                .get(front)
                .is_none_or(|r| r.inserted_at.elapsed() > ttl);
            let over_capacity = make_room && inner.entries.len() >= max_entries;
            if !expired && !over_capacity {
                break;
            }
            let key = inner.order.pop_front().expect("front just checked Some");
            inner.entries.remove(&key);
        }
    }

    /// Record a newly-submitted run as `Running`.
    pub(crate) fn insert_running(&self, run_id: String, tenant: String) {
        let mut inner = self.inner.lock().expect("run store poisoned");
        Self::evict(&mut inner, self.ttl, self.max_entries, true);
        inner.entries.insert(
            run_id.clone(),
            RunRecord {
                tenant,
                status: AsyncRunStatus::Running,
                inserted_at: Instant::now(),
            },
        );
        inner.order.push_back(run_id);
    }

    /// Move a run to its terminal status. A no-op if the run was already evicted
    /// (TTL/capacity) before it finished — the poller will see a 404, which is
    /// honest: this server instance no longer has anything to report.
    pub(crate) fn finish(&self, run_id: &str, status: AsyncRunStatus) {
        let mut inner = self.inner.lock().expect("run store poisoned");
        if let Some(record) = inner.entries.get_mut(run_id) {
            record.status = status;
        }
    }

    /// The run's tenant + current status, if it's still tracked.
    pub(crate) fn get(&self, run_id: &str) -> Option<(String, AsyncRunStatus)> {
        let inner = self.inner.lock().expect("run store poisoned");
        inner
            .entries
            .get(run_id)
            .map(|r| (r.tenant.clone(), r.status.clone()))
    }
}

/// Shared server state: the LLM gateway, tool registry, metrics, a run counter, and a
/// read-only workflow engine over the durable store (for visibility endpoints).
pub struct AppState {
    pub(crate) gateway: Arc<Gateway>,
    pub(crate) registry: ToolRegistry,
    pub(crate) metrics: Metrics,
    pub(crate) agents: Arc<AgentStore>,
    pub(crate) run_counter: AtomicU64,
    /// Read-only engine over the durable workflow store — drives the `GET
    /// /api/v1/workflows*` visibility endpoints (G4). Its executor is never invoked.
    pub(crate) workflows: Engine,
    /// Workflow execution id → owning tenant, stamped at submit so the workflow routes
    /// enforce per-tenant isolation without the (tenant-agnostic) engine. An execution
    /// with no recorded owner belongs to the anonymous `default` space (back-compat).
    /// Durable when opened with a path (RM-GA-P2 DUR-404) — without this, a restart
    /// dropped every execution's tenant binding, so the owning tenant got 404s while
    /// the anonymous `default` space could see all of them (a tenant-isolation
    /// regression on restart, per `workflow_visible`).
    pub(crate) workflow_owners: RwLock<BTreeMap<String, String>>,
    /// Where `workflow_owners` persists (`None` = in-memory only, what tests use).
    pub(crate) workflow_owners_path: Option<PathBuf>,
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
    /// [`StoredAgentResolver`](crate::workflow_runner::StoredAgentResolver), which is
    /// constructed before this struct exists.
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
    /// Tracks runs submitted via `agents:run` with `Prefer: respond-async`
    /// (RM-GA-P2 EXE-604) so `GET /api/v1/agents/runs/{id}` has something to poll.
    pub(crate) runs: Arc<RunStore>,
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
    /// resolved once at construction — see [`crate::config::HttpLimits::from_env`].
    pub(crate) http_limits: crate::config::HttpLimits,
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
    /// real local model — see [`crate::config::default_gateway`]).
    pub async fn from_env() -> Self {
        // Export metrics to OTLP when built with `--features otlp` and the endpoint is
        // set; otherwise an in-process-only registry (see `Metrics::with_otlp_export`).
        let metrics = Metrics::with_otlp_export("apex");
        // Cost events from the gateway become LLM token/cost/savings metrics.
        let gateway = Arc::new(crate::config::default_gateway().await.with_cost_observer(
            Arc::new(crate::config::MetricsCostObserver {
                metrics: metrics.clone(),
            }),
        ));
        // One shared KMS for both encrypting-store consumers below — same root key +
        // tenant-key catalog, so a tenant's secrets and memories are sealed under
        // (independently generated, but co-located) keys rooted in the same trust anchor.
        let kms = crate::config::default_kms();
        let secrets = crate::config::default_secrets_vault(kms.clone());
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
            registry.register(std::sync::Arc::new(apex_tools::ImageGenTool::new(
                gateway.clone(),
            )));
        }
        // Register enabled plugin tools from the durable catalog into the run registry,
        // routed through a secret-aware runtime (when built with `plugin-wasi`), so agent
        // and workflow runs can invoke them with their tenant-scoped secrets injected.
        // Done before the registry is shared with the workflow engine below.
        plugins::register_enabled_tools(&mut registry, &secrets);
        let (memory, memory_store) = crate::memory::default_engine(kms.clone());
        let agents = Arc::new(AgentStore::new(
            crate::config::workflows_dir().map(|d| d.join("agents.json")),
        ));
        let tenancy_store = crate::config::default_tenancy_store();
        let quota = Arc::new(tenancy::QuotaTracker::new(
            crate::config::server_state_dir().map(|d| d.join("quota.json")),
        ));
        // Durable G1/G2 registries, shared with the CLI's `apex workflows tick`/
        // `schedule create` — attached to the engine (timers) and polled by the
        // background dispatcher loops `serve()` spawns (RM-GA-P2 EXE-601).
        let timers = crate::config::default_timer_store();
        let schedules = crate::config::default_schedule_store();
        // Thread gateway + registry + the agent store + tenancy/quota into the workflow
        // engine so the shared executor can actually drive function/ai/agent activities
        // when the submit route runs a workflow (an `agent` activity looks up a stored
        // agent by id here, and enforces the same project quota a direct run would).
        let workflows = crate::config::default_workflows_engine(
            gateway.clone(),
            registry.clone(),
            agents.clone(),
            tenancy_store.clone(),
            quota.clone(),
            timers.clone(),
        );
        let workflow_owners_path = crate::config::workflows_dir().map(|d| d.join("owners.json"));
        let workflow_owners = crate::config::load_owners(workflow_owners_path.as_deref());
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
            webhooks: crate::config::default_webhook_store(kms.clone()),
            webhook_sender: Arc::new(webhooks::ReqwestSender::default()),
            webhook_policy: BackoffPolicy::default(),
            event_counter: AtomicU64::new(1),
            idempotency: hardening::IdempotencyStore::new_with_path(
                Duration::from_secs(crate::config::env_u64(
                    "APEX_IDEMPOTENCY_TTL_SECS",
                    24 * 60 * 60,
                )),
                crate::config::env_u64("APEX_IDEMPOTENCY_MAX_ENTRIES", 10_000) as usize,
                crate::config::server_state_dir().map(|d| d.join("idempotency.json")),
            ),
            runs: Arc::new(RunStore::new(
                Duration::from_secs(crate::config::env_u64("APEX_ASYNC_RUN_TTL_SECS", 60 * 60)),
                crate::config::env_u64("APEX_ASYNC_RUN_MAX_ENTRIES", 10_000) as usize,
            )),
            memory,
            memory_store,
            secrets,
            kms,
            audit: crate::config::default_audit_log(),
            api_keys: auth::default_api_key_store(),
            anonymous_allowed: auth::resolve_anonymous_allowed(),
            auth_mode: auth::AuthMode::from_env(),
            http_limits: crate::config::HttpLimits::from_env(),
            rate_limiter_standard: Arc::new(rate_limit::RateLimiter::new(
                crate::config::env_u64("APEX_RATE_LIMIT_STANDARD_PER_MIN", 300) as u32,
                crate::config::env_u64("APEX_RATE_LIMIT_STANDARD_PER_MIN", 300) as u32,
            )),
            rate_limiter_sensitive: Arc::new(rate_limit::RateLimiter::new(
                crate::config::env_u64("APEX_RATE_LIMIT_SENSITIVE_PER_MIN", 30) as u32,
                crate::config::env_u64("APEX_RATE_LIMIT_SENSITIVE_PER_MIN", 30) as u32,
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
    pub(crate) fn with_http_limits(mut self, limits: crate::config::HttpLimits) -> Self {
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
    pub(crate) fn record_workflow_owner(&self, execution_id: &str, tenant: &str) {
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
    pub(crate) fn workflow_visible(&self, execution_id: &str, tenant: &str) -> bool {
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
