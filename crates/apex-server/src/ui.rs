//! The generative-UI runtime (PRD-005 P1: UIP-104, GRD-202/205/206/207,
//! HIL-301/302/303 — [RM-GUI-P1](../../docs/18-roadmap/v1.2-generative-ui.md)).
//!
//! Three responsibilities, one durable loop:
//!
//! 1. **Present** — a `ui` workflow activity carries a frame in `inputs.frame`.
//!    [`UiActivityExecutor`] parses it (fail-closed protocol validation), stamps
//!    provenance, runs it through the tenant's [`UiPolicy`] (or the GRD-207
//!    hosted floor when none exists), records the verdict in the tamper-evident
//!    audit chain, persists the **validated** frame as a [`PendingFrame`], and
//!    suspends durably (`ActivityError::Interrupted`, the `human`-activity
//!    machinery) — so the render→decide→resume cycle survives a crash.
//! 2. **Pull** — `GET /api/v1/ui/frames[/{frame_id}]` serves the pending,
//!    already-validated frames to a renderer. Only checked frames ever exist
//!    here: there is no API that returns an unchecked frame (GRD-202's
//!    buffering stance, applied to storage).
//! 3. **Decide** — `POST /api/v1/ui/decisions/{frame_id}` validates the typed
//!    decision against the frame it answers (HIL-302, fail-closed **at the API
//!    boundary**), records who decided (HIL-303/306), and resumes the execution
//!    through the same signal path `human` approvals use.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied
//! here (RM-AIM-P3 SRV-306), same as every other route module.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::{ApiError, AppState};
use apex_audit::{ActorType, AuditEvent, AuditLog};
use apex_ui::{UiDecision, UiFrame, validate_decision};
use apex_ui_guard::{UiPolicy, Verdict, evaluate, hosted_floor};
use apex_workflow::{ActivityContext, ActivityError, ActivityExecutor};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// A validated frame awaiting a human decision. Only frames that passed the
/// trust layer are ever stored — `frame` is the exact JSON a renderer shows,
/// redactions already applied.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PendingFrame {
    /// Deterministic handle (`uif-…`), derived from `(execution_id, activity_id)`
    /// so a resume re-presenting the same activity converges on the same id.
    pub frame_id: String,
    /// Owning tenant — decisions and reads are scoped to it.
    pub tenant: String,
    /// The suspended execution a decision resumes.
    pub execution_id: String,
    /// The suspended `ui` activity within it.
    pub activity_id: String,
    /// The validated (possibly redacted) `apex_ui::UiFrame` JSON.
    pub frame: Value,
    /// Content hash of `frame` — the audit chain pairs decisions with it (HIL-306).
    pub frame_hash: String,
    /// Which policy judged the frame: `name@vN`, `hosted-floor`, or `unrestricted`
    /// (GRD-206: a run records the policy version that judged its frames).
    pub policy_ref: String,
    /// Wall-clock at presentation (boundary clock, like every audit timestamp).
    pub created_at_ms: u64,
}

/// Durable registry of pending frames — the same file-store shape as
/// `AgentStore` (DUR-404): a `path: None` store is purely in-memory (tests);
/// with a path, every mutation re-persists via `atomic_write` and a fresh
/// instance loads it back, so a pending decision survives a restart.
pub(crate) struct UiFrameStore {
    inner: RwLock<BTreeMap<String, PendingFrame>>,
    path: Option<PathBuf>,
}

impl UiFrameStore {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        let inner = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Vec<PendingFrame>>(&bytes).ok())
            .map(|frames| {
                frames
                    .into_iter()
                    .map(|f| (f.frame_id.clone(), f))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            inner: RwLock::new(inner),
            path,
        }
    }

    fn persist(&self, map: &BTreeMap<String, PendingFrame>) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(error = %e, "failed to create ui frame store directory");
            return;
        }
        let frames: Vec<&PendingFrame> = map.values().collect();
        match serde_json::to_vec_pretty(&frames) {
            Ok(bytes) => {
                if let Err(e) = apex_common::fs::atomic_write(path, bytes) {
                    tracing::error!(error = %e, "failed to persist ui frame store");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to encode ui frame store"),
        }
    }

    /// Insert or refresh `frame`; returns whether anything **new** landed
    /// (a resume re-presenting an unchanged frame is a no-op, so the audit
    /// trail records one `present` per distinct content, not one per attempt).
    pub(crate) fn upsert(&self, frame: PendingFrame) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let fresh = inner
            .get(&frame.frame_id)
            .is_none_or(|existing| existing.frame_hash != frame.frame_hash);
        if fresh {
            inner.insert(frame.frame_id.clone(), frame);
            self.persist(&inner);
        }
        fresh
    }

    pub(crate) fn get(&self, frame_id: &str) -> Option<PendingFrame> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(frame_id)
            .cloned()
    }

    pub(crate) fn remove(&self, frame_id: &str) -> Option<PendingFrame> {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let removed = inner.remove(frame_id);
        if removed.is_some() {
            self.persist(&inner);
        }
        removed
    }

    pub(crate) fn list(&self, tenant: &str) -> Vec<PendingFrame> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|f| f.tenant == tenant)
            .cloned()
            .collect()
    }
}

/// How a tenant's frames are judged (GRD-206/207).
enum PolicySource {
    /// `APEX_UNRESTRICTED_UI=1` — the documented trusted-first-party escape
    /// hatch (GRD-207): frames pass protocol validation only.
    Unrestricted,
    /// No policy configured: the hosted floor (interactive frames denied).
    Floor,
    /// A real policy document.
    Policy(Box<UiPolicy>),
}

/// The shared generative-UI runtime: pending frames + policy resolution +
/// audit. One instance serves both the workflow executor (present/suspend) and
/// the HTTP routes (pull/decide) — they must share the same [`UiFrameStore`]
/// or a presented frame would be invisible to the decision endpoint.
pub struct UiRuntime {
    frames: Arc<UiFrameStore>,
    /// In-process tenant policies (tests, and a future policy CRUD surface).
    /// Checked before the on-disk policy directory.
    policies: Arc<RwLock<BTreeMap<String, UiPolicy>>>,
    /// `~/.apex/ui/policies/<tenant>.yaml` — reloaded fresh per presentation
    /// (the DUR-403 reload-fresh discipline; policy reads are rare and cheap).
    policies_dir: Option<PathBuf>,
    /// Fallback policy for tenants without their own (`APEX_UI_POLICY=<path>`).
    default_policy: Option<UiPolicy>,
    /// `APEX_UNRESTRICTED_UI=1` at startup.
    unrestricted: bool,
    audit: Arc<AuditLog>,
}

impl UiRuntime {
    /// The production runtime: durable frame store + policy dir under
    /// `~/.apex/ui`, default policy and the unrestricted escape hatch from the
    /// environment. A malformed `APEX_UI_POLICY` file is logged and treated as
    /// absent — the *floor* then applies, which is stricter, not weaker
    /// (fail-closed in the safe direction).
    pub fn from_env(audit: Arc<AuditLog>) -> Self {
        let dir = apex_config::paths::ui_dir().ok();
        let default_policy = std::env::var("APEX_UI_POLICY").ok().and_then(|path| {
            match std::fs::read_to_string(&path) {
                Ok(yaml) => match UiPolicy::from_yaml(&yaml) {
                    Ok(policy) => Some(policy),
                    Err(e) => {
                        tracing::error!(error = %e, path, "APEX_UI_POLICY is malformed; \
                             ignoring it — the hosted floor applies instead");
                        None
                    }
                },
                Err(e) => {
                    tracing::error!(error = %e, path, "APEX_UI_POLICY is unreadable; \
                         ignoring it — the hosted floor applies instead");
                    None
                }
            }
        });
        Self {
            frames: Arc::new(UiFrameStore::new(
                dir.clone().map(|d| d.join("pending_frames.json")),
            )),
            policies: Arc::new(RwLock::new(BTreeMap::new())),
            policies_dir: dir.map(|d| d.join("policies")),
            default_policy,
            unrestricted: std::env::var("APEX_UNRESTRICTED_UI")
                .map(|v| v == "1")
                .unwrap_or(false),
            audit,
        }
    }

    /// An isolated in-memory runtime (tests): no durable store, no policy dir,
    /// no default policy, floor enforced.
    pub fn in_memory(audit: Arc<AuditLog>) -> Self {
        Self {
            frames: Arc::new(UiFrameStore::new(None)),
            policies: Arc::new(RwLock::new(BTreeMap::new())),
            policies_dir: None,
            default_policy: None,
            unrestricted: false,
            audit,
        }
    }

    /// The same runtime (shared frame store and policies) reporting into a
    /// different audit log — used by `AppState::with_audit` so the state's log
    /// and the UI runtime's never diverge in tests.
    #[cfg(test)]
    pub(crate) fn with_audit_log(&self, audit: Arc<AuditLog>) -> Self {
        Self {
            frames: self.frames.clone(),
            policies: self.policies.clone(),
            policies_dir: self.policies_dir.clone(),
            default_policy: self.default_policy.clone(),
            unrestricted: self.unrestricted,
            audit,
        }
    }

    /// Register/replace `tenant`'s policy in-process (tests and, later, a
    /// policy CRUD surface — GRD-206's version-pin discipline is the caller's).
    pub fn set_tenant_policy(&self, tenant: &str, policy: UiPolicy) {
        self.policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tenant.to_string(), policy);
    }

    pub(crate) fn frames(&self) -> &UiFrameStore {
        &self.frames
    }

    /// Resolve how `tenant`'s frames are judged. A malformed on-disk tenant
    /// policy is an **error** (the activity fails; fail-closed), never a
    /// fall-through to a weaker source.
    fn policy_for(&self, tenant: &str) -> Result<PolicySource, apex_common::Error> {
        if self.unrestricted {
            return Ok(PolicySource::Unrestricted);
        }
        if let Some(policy) = self
            .policies
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(tenant)
        {
            return Ok(PolicySource::Policy(Box::new(policy.clone())));
        }
        if let Some(dir) = &self.policies_dir {
            let path = dir.join(format!("{}.yaml", sanitize(tenant)));
            match std::fs::read_to_string(&path) {
                Ok(yaml) => return Ok(PolicySource::Policy(Box::new(UiPolicy::from_yaml(&yaml)?))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(apex_common::Error::Config(format!(
                        "ui policy for tenant `{tenant}` is unreadable: {e}"
                    )));
                }
            }
        }
        Ok(match &self.default_policy {
            Some(policy) => PolicySource::Policy(Box::new(policy.clone())),
            None => PolicySource::Floor,
        })
    }

    fn record_audit(&self, event: AuditEvent) {
        if let Err(e) = self.audit.record(event) {
            tracing::warn!("ui audit record failed: {e}");
        }
    }

    /// The `ui` activity body (HIL-301). See the module docs for the flow.
    async fn execute_ui_activity(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        // A decision already injected (bare id or `event.<id>`, the same two
        // key conventions the `human` dispatch checks) completes the activity.
        if let Some(decision) = ctx
            .variables
            .get(&ctx.id)
            .or_else(|| ctx.variables.get(&format!("event.{}", ctx.id)))
        {
            // The decision endpoint removes the pending record on the happy
            // path; this covers a crash between signal and removal.
            if let Some(execution_id) = execution_id_of(ctx) {
                self.frames.remove(&frame_id_for(&execution_id, &ctx.id));
            }
            return Ok(decision.clone());
        }

        let Some(execution_id) = execution_id_of(ctx) else {
            return Err(ActivityError::Permanent(format!(
                "activity `{}`: no `__execution_id` variable — `ui` activities need the \
                 engine-stamped execution id to key their pending decision",
                ctx.id
            )));
        };
        let tenant = ctx
            .variables
            .get("__tenant")
            .and_then(Value::as_str)
            .unwrap_or(crate::tenancy::DEFAULT_TENANT)
            .to_string();
        let frame_id = frame_id_for(&execution_id, &ctx.id);

        let Some(frame_value) = ctx.inputs.get("frame") else {
            return Err(ActivityError::Permanent(format!(
                "activity `{}`: `ui` activities require `inputs.frame` (a UiFrame document)",
                ctx.id
            )));
        };

        // Protocol validation (UIP-101/106), fail-closed; a reject is audited —
        // a malformed frame in a hosted deployment is a signal worth keeping.
        let mut frame = match UiFrame::from_value(frame_value) {
            Ok(frame) => frame,
            Err(e) => {
                self.record_audit(
                    system_event(&tenant, "ui.frame.reject", &frame_id)
                        .denied(format!("protocol validation failed: {e}")),
                );
                return Err(ActivityError::Permanent(format!(
                    "activity `{}`: invalid ui frame ({e})",
                    ctx.id
                )));
            }
        };

        // Provenance is runtime-stamped (UIP-102), never author-trusted.
        frame.provenance.execution_id = Some(execution_id.clone());
        frame.provenance.activity_id = Some(ctx.id.clone());

        // Trust layer (GRD-202): policy, floor, or the explicit escape hatch.
        let source = self.policy_for(&tenant).map_err(|e| {
            ActivityError::Permanent(format!(
                "activity `{}`: ui policy resolution failed ({e}); failing closed",
                ctx.id
            ))
        })?;
        let (verdict, policy_ref) = match source {
            PolicySource::Unrestricted => (Verdict::Allow, "unrestricted".to_string()),
            PolicySource::Floor => (hosted_floor(&frame), "hosted-floor".to_string()),
            PolicySource::Policy(policy) => {
                let reference = policy.reference();
                (evaluate(&policy, &frame), reference)
            }
        };

        let final_frame = match verdict {
            Verdict::Allow => frame,
            Verdict::Redact { frame, .. } => *frame,
            Verdict::Block { rule, reason } => {
                self.record_audit(
                    system_event(&tenant, "ui.frame.block", &frame_id)
                        .denied(format!("policy `{policy_ref}` rule `{rule}`: {reason}")),
                );
                // The workflow-visible error names the rule, not the reason —
                // the blocklist stance: detail lives in the audit chain, not in
                // an error message a model (or end user) might see echoed back.
                return Err(ActivityError::Permanent(format!(
                    "activity `{}`: ui frame blocked by policy `{policy_ref}` rule `{rule}`",
                    ctx.id
                )));
            }
        };

        let frame_hash = final_frame.content_hash();
        let frame_json = serde_json::to_value(&final_frame).map_err(|e| {
            ActivityError::Permanent(format!(
                "activity `{}`: failed to serialize validated frame ({e})",
                ctx.id
            ))
        })?;

        // A display-only frame (no declared actions) has nothing to decide:
        // present it (audited), complete immediately, and store nothing — a
        // pending record would linger forever with no decision able to clear
        // it. (The protocol already rejects inputs-without-actions, so nothing
        // interactive slips through this arm.)
        if final_frame.actions().is_empty() {
            self.record_audit(system_event(
                &tenant,
                "ui.frame.present",
                &format!("{frame_id}:{frame_hash}"),
            ));
            return Ok(json!({
                "frame_id": frame_id,
                "frame_hash": frame_hash,
                "frame": frame_json,
                "presented": true,
            }));
        }

        let fresh = self.frames.upsert(PendingFrame {
            frame_id: frame_id.clone(),
            tenant: tenant.clone(),
            execution_id,
            activity_id: ctx.id.clone(),
            frame: frame_json,
            frame_hash: frame_hash.clone(),
            policy_ref,
            created_at_ms: crate::audit::now_ms(),
        });
        if fresh {
            // The presented-frame audit record pairs id and content hash —
            // the evidentiary object a decision's record points back at (HIL-306).
            self.record_audit(system_event(
                &tenant,
                "ui.frame.present",
                &format!("{frame_id}:{frame_hash}"),
            ));
        }

        Err(ActivityError::Interrupted(format!(
            "activity `{}` is awaiting a ui decision (frame `{frame_id}`)",
            ctx.id
        )))
    }
}

/// A workflow-engine audit event: the platform acted (presented/blocked a
/// frame), not a request principal.
fn system_event(tenant: &str, action: &str, resource_id: &str) -> AuditEvent {
    AuditEvent::new(
        crate::audit::now_ms(),
        "workflow-engine",
        tenant,
        action,
        "ui_frame",
        resource_id,
    )
    .with_actor_type(ActorType::System)
}

fn execution_id_of(ctx: &ActivityContext) -> Option<String> {
    ctx.variables
        .get("__execution_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `uif-<16 hex>` — deterministic per `(execution, activity)` so a restarted
/// execution re-presenting the same activity converges on the same handle.
pub(crate) fn frame_id_for(execution_id: &str, activity_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(execution_id.as_bytes());
    hasher.update([0x1f]);
    hasher.update(activity_id.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("uif-{hex}")
}

fn sanitize(tenant: &str) -> String {
    tenant
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Decorates the platform executor with the `ui` activity type (HIL-301).
/// Everything else delegates unchanged — the CLI/eval executors don't get this
/// wrapper, so a `ui` activity in a local run is a clear "unsupported activity
/// type" rather than an ungoverned render (P1 is server-side only by design).
pub(crate) struct UiActivityExecutor<E> {
    inner: E,
    ui: Arc<UiRuntime>,
}

impl<E> UiActivityExecutor<E> {
    pub(crate) fn new(inner: E, ui: Arc<UiRuntime>) -> Self {
        Self { inner, ui }
    }
}

#[async_trait]
impl<E: ActivityExecutor> ActivityExecutor for UiActivityExecutor<E> {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        if ctx.activity_type == "ui" {
            self.ui.execute_ui_activity(ctx).await
        } else {
            self.inner.execute(ctx).await
        }
    }
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/ui/frames", get(list_frames_handler))
        .route("/api/v1/ui/frames/{frame_id}", get(get_frame_handler))
        .route("/api/v1/ui/decisions/{frame_id}", post(decide_handler))
}

fn frame_body(frame: &PendingFrame) -> Value {
    json!({
        "frame_id": frame.frame_id,
        "execution_id": frame.execution_id,
        "activity_id": frame.activity_id,
        "frame": frame.frame,
        "frame_hash": frame.frame_hash,
        "policy_ref": frame.policy_ref,
        "created_at_ms": frame.created_at_ms,
    })
}

/// `GET /api/v1/ui/frames` — the caller's tenant's pending (validated) frames,
/// the pull half of UIP-104 for renderers that don't hold an SSE stream.
#[utoipa::path(
    get,
    path = "/api/v1/ui/frames",
    tag = "ui",
    responses((status = 200, description = "Pending validated ui frames awaiting a decision, tenant-scoped.")),
)]
pub(crate) async fn list_frames_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    let data: Vec<Value> = state
        .ui
        .frames()
        .list(&tenant)
        .iter()
        .map(frame_body)
        .collect();
    Ok(Json(json!({ "data": data })))
}

/// `GET /api/v1/ui/frames/{frame_id}` — one pending frame, tenant-scoped. An
/// unknown id and another tenant's frame are the same `404` (no cross-tenant
/// existence oracle).
#[utoipa::path(
    get,
    path = "/api/v1/ui/frames/{frame_id}",
    tag = "ui",
    params(("frame_id" = String, Path, description = "The pending frame id (`uif-…`).")),
    responses(
        (status = 200, description = "The pending validated frame."),
        (status = 404, description = "Unknown frame (or not the caller's tenant's).", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn get_frame_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(frame_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    match state.ui.frames().get(&frame_id) {
        Some(frame) if frame.tenant == tenant => Ok(Json(frame_body(&frame))),
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no pending ui frame `{frame_id}`"),
        )),
    }
}

/// The decision request body (HIL-302/303).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct DecideRequest {
    /// The declared action taken — must be one of the frame's button actions.
    action: String,
    /// Submitted input values, keyed by input name.
    #[serde(default)]
    values: BTreeMap<String, Value>,
}

/// `POST /api/v1/ui/decisions/{frame_id}` — validate the typed decision against
/// the frame it answers and resume the suspended execution (HIL-303). The
/// decision payload delivered to the workflow carries the decision-taker and
/// the frame hash it answered, and the audit record pairs both (HIL-306).
#[utoipa::path(
    post,
    path = "/api/v1/ui/decisions/{frame_id}",
    tag = "ui",
    params(("frame_id" = String, Path, description = "The pending frame id (`uif-…`).")),
    request_body = DecideRequest,
    responses(
        (status = 200, description = "Decision accepted; execution resumed."),
        (status = 400, description = "Decision fails validation against the frame.", body = crate::openapi::ApiErrorBody),
        (status = 404, description = "Unknown frame (or not the caller's tenant's).", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn decide_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(frame_id): Path<String>,
    Json(req): Json<DecideRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "workflows:run")?;
    let Some(pending) = state.ui.frames().get(&frame_id) else {
        return Err(not_found(&frame_id));
    };
    if pending.tenant != tenant {
        // Same 404 as an unknown id — no cross-tenant existence oracle.
        return Err(not_found(&frame_id));
    }

    // The stored frame passed the trust layer at presentation; a decode failure
    // here is state corruption, not caller error.
    let frame = UiFrame::from_value(&pending.frame).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            format!("stored frame `{frame_id}` failed to decode: {e}"),
        )
    })?;
    let decision = UiDecision {
        action: req.action,
        values: req.values,
    };
    // HIL-302: fail-closed at the boundary — an out-of-vocabulary decision is
    // never delivered to the workflow.
    validate_decision(&frame, &decision)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;

    let def =
        crate::workflow_runner::resolve_definition(&state, &pending.execution_id, None).await?;
    let payload = json!({
        "action": decision.action,
        "values": decision.values,
        "decided_by": crate::tenancy::principal(&headers),
        "decided_at_ms": crate::audit::now_ms(),
        "frame_id": pending.frame_id,
        "frame_hash": pending.frame_hash,
    });
    state
        .workflows
        .signal_event(&def, &pending.execution_id, &pending.activity_id, payload)
        .await
        .map_err(ApiError::from)?;
    state.ui.frames().remove(&frame_id);
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "ui.decision.submit",
        "ui_frame",
        &format!("{frame_id}:{}", pending.frame_hash),
    );

    Ok(Json(json!({
        "frame_id": frame_id,
        "execution_id": pending.execution_id,
        "activity_id": pending.activity_id,
        "status": "decided",
    })))
}

fn not_found(frame_id: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("no pending ui frame `{frame_id}`"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AgentStore;
    use apex_audit::{AuditFilter, AuditLog, Outcome};
    use apex_ui_guard::{PolicyRules, UiPolicy};
    use apex_workflow::{
        CheckpointStore, Engine, EventLog, InMemoryStore, InMemoryTimerStore, WorkflowState,
    };
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    fn ensure_admin_env() {
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
    }

    /// A permissive-but-real test policy for the anonymous `default` tenant.
    fn test_policy() -> UiPolicy {
        UiPolicy {
            name: "test".into(),
            version: 1,
            rules: PolicyRules::default(),
        }
    }

    /// An isolated state whose engine drives `ui` activities into `ui`'s frame
    /// store over the given (shareable) workflow `store` — the same
    /// construction `workflow_runner::tests::isolated_state` uses, with the
    /// store handle exposed so a "restart" can build a second engine over it.
    /// The state's audit log is pointed at the same `Arc` the UI runtime
    /// records into, so route-side and executor-side records land in one chain.
    async fn ui_state(
        store: &InMemoryStore,
        ui: &Arc<UiRuntime>,
        audit: &Arc<AuditLog>,
    ) -> Arc<AppState> {
        let base = AppState::from_env().await.with_audit_arc(audit.clone());
        let agents = Arc::new(AgentStore::new(None));
        let executor = Arc::new(crate::workflow_runner::server_executor(
            base.gateway.clone(),
            base.registry.clone(),
            agents.clone(),
            base.tenancy.clone(),
            base.quota.clone(),
            base.metrics.clone(),
            base.tenant_label_cap.clone(),
            ui.clone(),
        ));
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let timers: Arc<dyn apex_workflow::TimerStore> = Arc::new(InMemoryTimerStore::new());
        let engine = Engine::new(events, checkpoints, executor).with_timer_store(timers.clone());
        Arc::new(
            base.with_agents(agents)
                .with_workflows(engine)
                .with_timers(timers)
                .with_ui(ui.clone()),
        )
    }

    /// A fresh in-memory audit log + a UI runtime recording into it, with the
    /// `default` tenant's policy installed (unless `floor`).
    fn ui_runtime(with_policy: bool) -> (Arc<AuditLog>, Arc<UiRuntime>) {
        let audit = Arc::new(AuditLog::in_memory());
        let ui = Arc::new(UiRuntime::in_memory(audit.clone()));
        if with_policy {
            ui.set_tenant_policy(crate::tenancy::DEFAULT_TENANT, test_policy());
        }
        (audit, ui)
    }

    async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        ensure_admin_env();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("x-apex-principal", "root")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
        ensure_admin_env();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-apex-principal", "root")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn wait_for_status(
        state: &Arc<AppState>,
        execution_id: &str,
        want: WorkflowState,
    ) -> apex_workflow::ExecutionState {
        for _ in 0..150 {
            if let Ok(Some(exec)) = state.workflows.query(execution_id).await
                && exec.status == want
            {
                return exec;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("execution `{execution_id}` did not reach {want:?} in time");
    }

    async fn wait_for_pending_frame(ui: &Arc<UiRuntime>) -> PendingFrame {
        for _ in 0..150 {
            if let Some(frame) = ui
                .frames()
                .list(crate::tenancy::DEFAULT_TENANT)
                .into_iter()
                .next()
            {
                return frame;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("no pending ui frame appeared in time");
    }

    fn audited(audit: &AuditLog, action: &str) -> Vec<apex_audit::AuditEntry> {
        audit
            .query(&AuditFilter {
                action: Some(action.to_string()),
                ..Default::default()
            })
            .expect("audit query")
    }

    /// UC4 (P1-109): a poisoned-source frame collecting a card number is
    /// blocked by the trust layer — the frame never becomes visible on any
    /// surface, the execution fails permanently, the block lands in the
    /// tamper-evident audit chain, and the chain verifies clean.
    const UC4_YAML: &str = "\
metadata:\n  name: ui-uc4-block\nspec:\n  activities:\n    - id: confirm\n      type: ui\n      inputs:\n        frame:\n          schema_version: 1.0.0\n          title: Confirm payment\n          root:\n            type: column\n            children:\n              - {type: text, text: Enter payment details to finish}\n              - {type: text_input, name: card_number, label: Card number}\n              - {type: button, action: pay, label: Continue, class: submit}\n";

    #[tokio::test]
    async fn uc4_credential_frame_is_blocked_never_visible_and_audited() {
        let store = InMemoryStore::new();
        let (audit, ui) = ui_runtime(true);
        let state = ui_state(&store, &ui, &audit).await;

        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": UC4_YAML, "execution_id": "uc4-block-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        // The trust layer fails the activity permanently: the execution fails…
        let exec = wait_for_status(&state, "uc4-block-test", WorkflowState::Failed).await;
        assert_eq!(exec.status, WorkflowState::Failed);

        // …the frame is never visible on any surface…
        assert!(ui.frames().list(crate::tenancy::DEFAULT_TENANT).is_empty());
        let (st, body) = get_json(crate::router(state.clone()), "/api/v1/ui/frames").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["data"], json!([]));

        // …the block is in the audit chain with the rule that fired…
        let blocks = audited(&audit, "ui.frame.block");
        assert_eq!(blocks.len(), 1, "exactly one block record");
        assert_eq!(blocks[0].event.outcome, Outcome::Denied);
        let reason = blocks[0].event.reason.as_deref().unwrap_or("");
        assert!(
            reason.contains(apex_ui_guard::rules::SENSITIVE_INPUT),
            "block reason names the rule: {reason}"
        );

        // …nothing was ever presented, and the chain verifies clean.
        assert!(audited(&audit, "ui.frame.present").is_empty());
        audit.verify().expect("audit chain verifies");
    }

    /// UC1 (P1-109): the safe variant renders (validated, provenance-stamped,
    /// policy-recorded), the pending decision **survives a server restart**
    /// (fresh engine + fresh UI runtime over the same durable workflow store),
    /// out-of-vocabulary decisions are rejected at the boundary without
    /// touching the workflow, and the valid approval resumes the execution to
    /// completion with the decision-taker recorded.
    const UC1_YAML: &str = "\
metadata:\n  name: ui-uc1-approve\nspec:\n  activities:\n    - id: confirm\n      type: ui\n      inputs:\n        frame:\n          schema_version: 1.0.0\n          title: Confirm order\n          root:\n            type: column\n            children:\n              - {type: text, text: Reorder 3 boxes of pipette tips?}\n              - {type: text_input, name: po_number, label: PO number, required: true}\n              - type: row\n                children:\n                  - {type: button, action: approve, label: Approve, class: approve}\n                  - {type: button, action: cancel, label: Cancel, class: cancel}\n";

    #[tokio::test]
    async fn uc1_frame_survives_restart_and_a_validated_decision_resumes() {
        let store = InMemoryStore::new();
        let (audit1, ui1) = ui_runtime(true);
        let state1 = ui_state(&store, &ui1, &audit1).await;

        let (st, body) = post_json(
            crate::router(state1.clone()),
            "/api/v1/workflows",
            json!({ "manifest": UC1_YAML, "execution_id": "uc1-restart-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        // The validated frame is pending, provenance-stamped, policy-recorded.
        let pending = wait_for_pending_frame(&ui1).await;
        assert_eq!(pending.execution_id, "uc1-restart-test");
        assert_eq!(pending.activity_id, "confirm");
        assert_eq!(pending.policy_ref, "test@v1");
        assert_eq!(
            pending.frame["provenance"]["execution_id"],
            json!("uc1-restart-test")
        );
        let (st, body) = get_json(
            crate::router(state1.clone()),
            &format!("/api/v1/ui/frames/{}", pending.frame_id),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["frame_hash"], json!(pending.frame_hash));

        // ── "Kill" the server: fresh engine + fresh UI runtime + fresh audit
        // over the SAME durable workflow store, then the startup resume pass.
        drop(state1);
        let (audit2, ui2) = ui_runtime(true);
        let state2 = ui_state(&store, &ui2, &audit2).await;
        crate::config::resume_in_flight_executions(&state2).await;

        // The resume re-presents the same frame — deterministic id and hash.
        let revived = wait_for_pending_frame(&ui2).await;
        assert_eq!(revived.frame_id, pending.frame_id);
        assert_eq!(revived.frame_hash, pending.frame_hash);

        let decide_uri = format!("/api/v1/ui/decisions/{}", revived.frame_id);

        // HIL-302, fail-closed at the boundary: an undeclared action…
        let (st, body) = post_json(
            crate::router(state2.clone()),
            &decide_uri,
            json!({ "action": "launch" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        // …and a missing required input are both rejected without resuming.
        let (st, _) = post_json(
            crate::router(state2.clone()),
            &decide_uri,
            json!({ "action": "approve" }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(
            ui2.frames().get(&revived.frame_id).is_some(),
            "rejected decisions leave the frame pending"
        );

        // The valid approval resumes the execution to completion.
        let (st, body) = post_json(
            crate::router(state2.clone()),
            &decide_uri,
            json!({ "action": "approve", "values": { "po_number": "PO-9" } }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "decided");

        let exec = wait_for_status(&state2, "uc1-restart-test", WorkflowState::Completed).await;
        // The activity's output is the decision, decision-taker included.
        let decision = &exec.variables["confirm"];
        assert_eq!(decision["action"], json!("approve"));
        assert_eq!(decision["values"]["po_number"], json!("PO-9"));
        assert_eq!(decision["decided_by"], json!("root"));
        assert_eq!(decision["frame_hash"], json!(revived.frame_hash));

        // The pending frame is consumed; audit pairs present + decision (with
        // the frame hash) and the chain verifies clean.
        assert!(ui2.frames().get(&revived.frame_id).is_none());
        let presents = audited(&audit2, "ui.frame.present");
        assert!(!presents.is_empty(), "re-presentation was audited");
        let decisions = audited(&audit2, "ui.decision.submit");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].event.actor.principal, "root");
        assert!(
            decisions[0].event.resource.id.contains(&revived.frame_hash),
            "the decision record pairs the frame hash it answered"
        );
        audit2.verify().expect("audit chain verifies");
    }

    /// GRD-207: with **no** policy configured, the hosted floor denies an
    /// interactive frame outright — and a display-only frame passes and
    /// completes immediately (nothing to decide, nothing left pending).
    #[tokio::test]
    async fn hosted_floor_denies_interactive_but_passes_display_only() {
        let store = InMemoryStore::new();
        let (audit, ui) = ui_runtime(false); // no tenant policy → floor
        let state = ui_state(&store, &ui, &audit).await;

        const INTERACTIVE: &str = "\
metadata:\n  name: ui-floor-interactive\nspec:\n  activities:\n    - id: ask\n      type: ui\n      inputs:\n        frame:\n          schema_version: 1.0.0\n          root:\n            type: column\n            children:\n              - {type: button, action: go, label: Go, class: confirm}\n";
        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": INTERACTIVE, "execution_id": "floor-interactive-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        wait_for_status(&state, "floor-interactive-test", WorkflowState::Failed).await;
        let blocks = audited(&audit, "ui.frame.block");
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0]
                .event
                .reason
                .as_deref()
                .unwrap_or("")
                .contains(apex_ui_guard::rules::HOSTED_FLOOR)
        );

        const DISPLAY: &str = "\
metadata:\n  name: ui-floor-display\nspec:\n  activities:\n    - id: show\n      type: ui\n      inputs:\n        frame:\n          schema_version: 1.0.0\n          root:\n            type: card\n            children:\n              - {type: badge, text: healthy, tone: success}\n              - {type: text, text: All queues nominal.}\n";
        let (st, body) = post_json(
            crate::router(state.clone()),
            "/api/v1/workflows",
            json!({ "manifest": DISPLAY, "execution_id": "floor-display-test" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let exec = wait_for_status(&state, "floor-display-test", WorkflowState::Completed).await;
        assert_eq!(exec.variables["show"]["presented"], json!(true));
        assert!(
            ui.frames().list(crate::tenancy::DEFAULT_TENANT).is_empty(),
            "display-only frames leave nothing pending"
        );
        audit.verify().expect("audit chain verifies");
    }

    /// The frame handle is deterministic per `(execution, activity)` — the
    /// property the restart convergence in UC1 rests on.
    #[test]
    fn frame_ids_are_deterministic_and_distinct() {
        assert_eq!(frame_id_for("e1", "confirm"), frame_id_for("e1", "confirm"));
        assert_ne!(frame_id_for("e1", "confirm"), frame_id_for("e2", "confirm"));
        assert_ne!(frame_id_for("e1", "confirm"), frame_id_for("e1", "other"));
        assert!(frame_id_for("e1", "confirm").starts_with("uif-"));
    }

    /// DUR-404 for pending frames: a file-backed store reloads its pending
    /// frames after a "restart" (fresh instance over the same path).
    #[test]
    fn file_backed_frame_store_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("apex_ui_frames_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("pending_frames.json");

        let frame = PendingFrame {
            frame_id: "uif-test".into(),
            tenant: "default".into(),
            execution_id: "e1".into(),
            activity_id: "confirm".into(),
            frame: json!({"schema_version": "1.0.0"}),
            frame_hash: "abc".into(),
            policy_ref: "test@v1".into(),
            created_at_ms: 1,
        };
        {
            let store = UiFrameStore::new(Some(path.clone()));
            assert!(store.upsert(frame.clone()));
            assert!(!store.upsert(frame), "same content is a no-op");
        }
        let reopened = UiFrameStore::new(Some(path));
        let revived = reopened.get("uif-test").expect("frame survives reopen");
        assert_eq!(revived.execution_id, "e1");
        assert_eq!(reopened.list("default").len(), 1);
        assert!(reopened.list("acme").is_empty(), "listing is tenant-scoped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
