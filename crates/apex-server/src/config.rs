//! Config/env-var factories: backend construction (gateway, tenancy, KMS,
//! secrets, audit, webhooks, workflow engine, timers/schedules), durable
//! definition persistence, background dispatch loops, and cross-cutting HTTP
//! resource limits (RM-GA-P4 HLTH-904 — split out of `lib.rs`).

use crate::state::AppState;
use crate::{tenancy, workflow_runner};
use apex_events::{
    EncryptedFileWebhookStore, FileWebhookStore, InMemoryWebhookStore, WebhookStore,
};
use apex_provider::{CostEvent, CostObserver, Gateway};
use apex_tenancy::{FileTenancyStore, InMemoryTenancyStore, TenancyStore};
use apex_tools::ToolRegistry;
use apex_workflow::{
    Clock, Definition, DefinitionResolver, Engine, EventLog, ExecutionFilter, FileScheduleStore,
    FileStore, FileTimerStore, InMemoryScheduleStore, InMemoryStore, InMemoryTimerStore,
    ScheduleDispatcher, ScheduleStore, SystemClock, TimerDispatcher, TimerStore,
};
use axum::BoxError;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The server's chat/embeddings backend. `Gateway::from_env()` (OpenAI if
/// `OPENAI_API_KEY` is set, else the deterministic mock) unless the operator opts into
/// a real local model with `APEX_PROVIDER=mistralrs` — in which case every agent and
/// workflow run on this node goes through it, no per-run choice. Requires this crate's
/// `mistralrs` cargo feature; without it (or if the model fails to load — e.g. no
/// network for the first-run GGUF download), falls back to `Gateway::from_env()` with a
/// loud warning rather than failing server startup outright.
pub(crate) async fn default_gateway() -> Gateway {
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
pub(crate) fn default_tenancy_store() -> Arc<dyn TenancyStore> {
    if let Ok(dir) = apex_config::paths::tenancy_dir()
        && let Ok(store) = FileTenancyStore::new(dir)
    {
        return Arc::new(store);
    }
    Arc::new(InMemoryTenancyStore::new())
}

/// The platform KMS ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)),
/// shared with the CLI via `apex-config` (RM-GA-P4 HLTH-903) so both processes
/// agree on the root key + tenant-key catalog instead of each maintaining its
/// own copy of this construction logic.
pub(crate) fn default_kms() -> Arc<dyn apex_kms::Kms> {
    apex_config::kms::build_kms()
}

/// A secret [`Vault`](apex_secrets::Vault) over a durable store at
/// `~/.apex/secrets` (shared with the CLI via `apex-config`, RM-GA-P4
/// HLTH-903). **Encrypted-at-rest by default (RM-AIM-P1 SEC-101):** values are
/// sealed through `kms` into a distinct `secrets.enc.json`, and a legacy
/// plaintext `secrets.json` is auto-migrated (re-sealed, then retired) on first
/// construction so nothing is abandoned by the filename switch.
/// `APEX_SECRETS_PLAINTEXT=1` is the explicit opt-out.
pub(crate) fn default_secrets_vault(kms: Arc<dyn apex_kms::Kms>) -> apex_secrets::Vault {
    apex_config::secrets::build_secrets_vault(kms)
}

/// A tamper-evident [`AuditLog`](apex_audit::AuditLog) over a durable [`FileAuditSink`]
/// at `~/.apex/audit`, falling back to an in-memory log. Opened with cross-process
/// locking (RM-GA-P2 DUR-403) over that same directory — the CLI can append to the
/// identical `audit.jsonl` (e.g. via `apex plugin` commands, once wired), so a
/// second writer must extend the chain, not fork it.
pub(crate) fn default_audit_log() -> apex_audit::AuditLog {
    let dir = apex_config::paths::audit_dir().ok();
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
pub(crate) fn default_webhook_store(kms: Arc<dyn apex_kms::Kms>) -> Arc<dyn WebhookStore> {
    let dir = apex_config::paths::webhooks_dir().ok();
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
/// directory is unavailable. The executor is the shared
/// [`apex_runtime::PlatformActivityExecutor`] (RM-GA-P4 HLTH-901), parameterized by
/// [`workflow_runner::StoredAgentResolver`], so the write-path submit route can
/// actually drive workflow runs — including enforcing the submitting project's quota
/// on `agent` activities, the same gate a direct `agents:run` call goes through.  The
/// read paths (`list`/`status`/`history`) are unaffected. Attaches `timers` (RM-GA-P2
/// EXE-601) so a `wait: {timer: ...}}` activity can actually register a durable
/// deadline — before this the server's engine had no timer store at all, so any such
/// activity failed immediately with "no timer store" rather than suspending.
pub(crate) fn default_workflows_engine(
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
    agents: Arc<crate::state::AgentStore>,
    tenancy: Arc<dyn TenancyStore>,
    quota: Arc<tenancy::QuotaTracker>,
    timers: Arc<dyn TimerStore>,
    metrics_state: crate::hardening::MetricsState,
) -> Engine {
    let executor = Arc::new(workflow_runner::server_executor(
        gateway,
        registry,
        agents,
        tenancy,
        quota,
        metrics_state.metrics,
        metrics_state.tenant_labels,
    ));
    if let Some(dir) = workflows_dir()
        && let Ok(store) = FileStore::new(dir)
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn apex_workflow::CheckpointStore> = Arc::new(store);
        return Engine::new(events, checkpoints, executor).with_timer_store(timers);
    }
    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn apex_workflow::CheckpointStore> = Arc::new(store);
    Engine::new(events, checkpoints, executor).with_timer_store(timers)
}

/// A durable [`TimerStore`] at `~/.apex/workflows` (shared with the CLI's `apex
/// workflows tick`), falling back to an in-memory store if that directory is
/// unavailable (RM-GA-P2 EXE-601).
pub(crate) fn default_timer_store() -> Arc<dyn TimerStore> {
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
pub(crate) fn default_schedule_store() -> Arc<dyn ScheduleStore> {
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
pub(crate) fn save_definition(workflow_name: &str, manifest: &str) {
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
pub(crate) fn definition_resolver() -> DefinitionResolver {
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
pub(crate) fn spawn_dispatch_loops(
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
pub(crate) async fn resume_in_flight_executions(state: &Arc<AppState>) {
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
pub(crate) fn workflows_dir() -> Option<PathBuf> {
    apex_config::paths::workflows_dir().ok()
}

/// `~/.apex/server` — durable state that is server-process-local, never shared with
/// or read by the CLI (RM-GA-P2 DUR-404: the idempotency cache and the daily quota
/// accumulator). Kept in its own directory rather than `workflows_dir()` precisely
/// *because* it isn't shared — mixing the two would blur that boundary.
pub(crate) fn server_state_dir() -> Option<PathBuf> {
    apex_config::paths::server_state_dir().ok()
}

/// Load the persisted workflow-owners index from `path` (best-effort: a missing or
/// corrupt file starts empty rather than failing server startup).
pub(crate) fn load_owners(
    path: Option<&std::path::Path>,
) -> std::collections::BTreeMap<String, String> {
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Translates gateway [`CostEvent`]s into Prometheus metrics
/// ([metrics §6](../../docs/14-observability/metrics.md)).
pub(crate) struct MetricsCostObserver {
    pub(crate) metrics: apex_telemetry::Metrics,
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
    pub(crate) fn from_env() -> Self {
        Self {
            timeout_secs: env_u64("APEX_HTTP_TIMEOUT_SECS", 30),
            run_timeout_secs: env_u64("APEX_HTTP_RUN_TIMEOUT_SECS", 300),
            max_body_bytes: env_u64("APEX_HTTP_MAX_BODY_BYTES", 1024 * 1024) as usize,
            max_concurrency: env_u64("APEX_HTTP_MAX_CONCURRENCY", 512) as usize,
        }
    }
}

pub(crate) fn env_u64(var: &str, default: u64) -> u64 {
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
pub(crate) fn cors_layer(origins: &[String]) -> Option<tower_http::cors::CorsLayer> {
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
pub(crate) async fn handle_overload_or_timeout(err: BoxError) -> Response {
    if err.is::<tower::timeout::error::Elapsed>() {
        crate::ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "the request exceeded the server's timeout",
        )
        .into_response()
    } else if err.is::<tower::load_shed::error::Overloaded>() {
        crate::ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "the server is at its concurrency limit; retry shortly",
        )
        .into_response()
    } else {
        crate::ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            err.to_string(),
        )
        .into_response()
    }
}
