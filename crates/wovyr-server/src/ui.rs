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
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use wovyr_audit::{ActorType, AuditEvent, AuditLog};
use wovyr_ui::{UiDecision, UiFrame, validate_decision};
use wovyr_ui_guard::{UiPolicy, Verdict, evaluate, hosted_floor};
use wovyr_workflow::{ActivityContext, ActivityError, ActivityExecutor};

/// Hard cap on the standalone decided-outcomes registry (`DecidedOutcomeStore`)
/// — see its doc comment for why this is a cap-only, no-TTL v1.
const MAX_DECIDED_OUTCOMES: usize = 10_000;

/// A validated frame awaiting a human decision. Only frames that passed the
/// trust layer are ever stored — `frame` is the exact JSON a renderer shows,
/// redactions already applied.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PendingFrame {
    /// Deterministic handle (`uif-…`) — derived from `(execution_id, activity_id)`
    /// for a workflow-backed frame (so a resume re-presenting the same activity
    /// converges on the same id), or from `(tenant, a monotonic counter,
    /// content_hash)` for a standalone one (EMB-701, `present_standalone`).
    pub frame_id: String,
    /// Owning tenant — decisions and reads are scoped to it.
    pub tenant: String,
    /// The suspended execution a decision resumes, and the suspended `ui`
    /// activity within it — `None`/`None` for a **standalone** frame
    /// (EMB-701, `POST /api/v1/ui/present`): presented with no
    /// workflow/agent involvement at all, so there is nothing to signal.
    /// Always both-`Some` or both-`None` together (never one alone) — see
    /// `present_standalone`/`execute_ui_activity`, the only two constructors.
    pub execution_id: Option<String>,
    pub activity_id: Option<String>,
    /// The validated (possibly redacted) `wovyr_ui::UiFrame` JSON.
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
                if let Err(e) = wovyr_common::fs::atomic_write(path, bytes) {
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

    /// Ordered by presentation time (ties broken by `frame_id` for full
    /// determinism), **not** raw `BTreeMap` iteration — the map is keyed by a
    /// content-hash-derived `frame_id` (`frame_id_for`), so iterating it
    /// directly sorts by that hash rather than chronologically. A renderer
    /// polling this list would otherwise see already-displayed frames jump
    /// position every time an unrelated new frame landed at an earlier hash.
    pub(crate) fn list(&self, tenant: &str) -> Vec<PendingFrame> {
        let mut frames: Vec<PendingFrame> = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|f| f.tenant == tenant)
            .cloned()
            .collect();
        frames.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.frame_id.cmp(&b.frame_id))
        });
        frames
    }
}

/// A recorded decision for a **standalone** frame (EMB-701, `present_standalone`)
/// — a workflow-backed frame's outcome is reflected in the workflow's own
/// state/output instead, via `signal_event`. This store exists purely so a
/// standalone caller (no workflow to poll) can retrieve what was decided
/// after the pending-frame record is gone (`GET /api/v1/ui/decisions/{id}`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DecidedOutcome {
    pub frame_id: String,
    pub tenant: String,
    pub action: String,
    pub values: BTreeMap<String, Value>,
    pub decided_by: String,
    pub decided_at_ms: u64,
    pub frame_hash: String,
}

/// Bounded, in-memory registry of standalone decision outcomes — the same
/// "cap + evict oldest" shape `wovyr-server`'s `RunStore`/`IdempotencyStore`
/// use elsewhere (DUR-404/SEC-205's discipline), since this is exactly the
/// same kind of short-lived, non-source-of-truth record. **Known limitation**
/// (documented, not solved this pass): no durability across a restart and no
/// TTL, only a hard entry cap — acceptable for a v1 of a feature nothing
/// depends on yet; a caller that actually wants a decision polls promptly.
pub(crate) struct DecidedOutcomeStore {
    inner: std::sync::Mutex<DecidedOutcomeInner>,
    max_entries: usize,
}

#[derive(Default)]
struct DecidedOutcomeInner {
    entries: BTreeMap<String, DecidedOutcome>,
    order: VecDeque<String>,
}

impl DecidedOutcomeStore {
    fn new(max_entries: usize) -> Self {
        Self {
            inner: std::sync::Mutex::new(DecidedOutcomeInner::default()),
            max_entries,
        }
    }

    pub(crate) fn record(&self, outcome: DecidedOutcome) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.entries.contains_key(&outcome.frame_id) {
            inner.order.push_back(outcome.frame_id.clone());
        }
        inner.entries.insert(outcome.frame_id.clone(), outcome);
        while inner.order.len() > self.max_entries {
            if let Some(oldest) = inner.order.pop_front() {
                inner.entries.remove(&oldest);
            }
        }
    }

    pub(crate) fn get(&self, frame_id: &str) -> Option<DecidedOutcome> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(frame_id)
            .cloned()
    }
}

/// How a tenant's frames are judged (GRD-206/207).
enum PolicySource {
    /// `WOVYR_UNRESTRICTED_UI=1` — the documented trusted-first-party escape
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
    /// `~/.wovyr/ui/policies/<tenant>.yaml` — reloaded fresh per presentation
    /// (the DUR-403 reload-fresh discipline; policy reads are rare and cheap).
    policies_dir: Option<PathBuf>,
    /// Fallback policy for tenants without their own (`WOVYR_UI_POLICY=<path>`).
    default_policy: Option<UiPolicy>,
    /// `WOVYR_UNRESTRICTED_UI=1` at startup.
    unrestricted: bool,
    audit: Arc<AuditLog>,
    /// Standalone (EMB-701) decision outcomes, retrievable after the pending
    /// record is gone — see [`DecidedOutcomeStore`].
    decisions: Arc<DecidedOutcomeStore>,
    /// Monotonic counter feeding standalone frame id generation
    /// (`present_standalone`) — a workflow-backed frame's id is derived from
    /// `(execution_id, activity_id)` instead, so it has no need of this.
    standalone_seq: AtomicU64,
}

impl UiRuntime {
    /// The production runtime: durable frame store + policy dir under
    /// `~/.wovyr/ui`, default policy and the unrestricted escape hatch from the
    /// environment. A malformed `WOVYR_UI_POLICY` file is logged and treated as
    /// absent — the *floor* then applies, which is stricter, not weaker
    /// (fail-closed in the safe direction).
    pub fn from_env(audit: Arc<AuditLog>) -> Self {
        let dir = wovyr_config::paths::ui_dir().ok();
        let default_policy = std::env::var("WOVYR_UI_POLICY").ok().and_then(|path| {
            match std::fs::read_to_string(&path) {
                Ok(yaml) => match UiPolicy::from_yaml(&yaml) {
                    Ok(policy) => Some(policy),
                    Err(e) => {
                        tracing::error!(error = %e, path, "WOVYR_UI_POLICY is malformed; \
                             ignoring it — the hosted floor applies instead");
                        None
                    }
                },
                Err(e) => {
                    tracing::error!(error = %e, path, "WOVYR_UI_POLICY is unreadable; \
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
            unrestricted: std::env::var("WOVYR_UNRESTRICTED_UI")
                .map(|v| v == "1")
                .unwrap_or(false),
            audit,
            decisions: Arc::new(DecidedOutcomeStore::new(MAX_DECIDED_OUTCOMES)),
            standalone_seq: AtomicU64::new(0),
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
            decisions: Arc::new(DecidedOutcomeStore::new(MAX_DECIDED_OUTCOMES)),
            standalone_seq: AtomicU64::new(0),
        }
    }

    /// The same runtime (shared frame store, policies, and decided-outcomes
    /// store) reporting into a different audit log — used by
    /// `AppState::with_audit` so the state's log and the UI runtime's never
    /// diverge in tests.
    #[cfg(test)]
    pub(crate) fn with_audit_log(&self, audit: Arc<AuditLog>) -> Self {
        Self {
            frames: self.frames.clone(),
            policies: self.policies.clone(),
            policies_dir: self.policies_dir.clone(),
            default_policy: self.default_policy.clone(),
            unrestricted: self.unrestricted,
            audit,
            decisions: self.decisions.clone(),
            standalone_seq: AtomicU64::new(self.standalone_seq.load(Ordering::SeqCst)),
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

    pub(crate) fn decisions(&self) -> &DecidedOutcomeStore {
        &self.decisions
    }

    /// Resolve how `tenant`'s frames are judged. A malformed on-disk tenant
    /// policy is an **error** (the activity fails; fail-closed), never a
    /// fall-through to a weaker source.
    fn policy_for(&self, tenant: &str) -> Result<PolicySource, wovyr_common::Error> {
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
                    return Err(wovyr_common::Error::Config(format!(
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

    /// The shared trust-layer pipeline (GRD-202): protocol-validate
    /// `frame_value` fail-closed, let `stamp` set provenance, judge it against
    /// `tenant`'s policy (or the hosted floor, or the unrestricted escape
    /// hatch), and audit a reject/block verdict. Used by **both** the
    /// workflow `ui` activity and the standalone `present_standalone` route
    /// (EMB-701) so there is exactly one code path enforcing the trust layer
    /// — never two that could quietly drift apart. Returns the validated
    /// (possibly redacted) frame plus the policy reference that judged it, or
    /// a [`PresentError`] naming why it was rejected (already audited either
    /// way, same as before this was factored out).
    fn judge_frame(
        &self,
        tenant: &str,
        frame_value: &Value,
        audit_id: &str,
        stamp: impl FnOnce(&mut UiFrame),
    ) -> Result<(UiFrame, String), PresentError> {
        let mut frame = UiFrame::from_value(frame_value).map_err(|e| {
            self.record_audit(
                system_event(tenant, "ui.frame.reject", audit_id)
                    .denied(format!("protocol validation failed: {e}")),
            );
            PresentError::Invalid(e.to_string())
        })?;
        stamp(&mut frame);

        let source = self
            .policy_for(tenant)
            .map_err(PresentError::PolicyResolution)?;
        let (verdict, policy_ref) = match source {
            PolicySource::Unrestricted => (Verdict::Allow, "unrestricted".to_string()),
            PolicySource::Floor => (hosted_floor(&frame), "hosted-floor".to_string()),
            PolicySource::Policy(policy) => {
                let reference = policy.reference();
                (evaluate(&policy, &frame), reference)
            }
        };

        match verdict {
            Verdict::Allow => Ok((frame, policy_ref)),
            Verdict::Redact { frame, .. } => Ok((*frame, policy_ref)),
            Verdict::Block { rule, reason } => {
                self.record_audit(
                    system_event(tenant, "ui.frame.block", audit_id)
                        .denied(format!("policy `{policy_ref}` rule `{rule}`: {reason}")),
                );
                // The caller-visible error names the rule, not the reason —
                // the blocklist stance: detail lives in the audit chain, not
                // in an error message a model (or end user) might see echoed
                // back.
                Err(PresentError::Blocked { rule })
            }
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

        let activity_id = ctx.id.clone();
        let (final_frame, policy_ref) = self
            .judge_frame(&tenant, frame_value, &frame_id, |frame| {
                // Provenance is runtime-stamped (UIP-102), never author-trusted.
                frame.provenance.execution_id = Some(execution_id.clone());
                frame.provenance.activity_id = Some(activity_id.clone());
            })
            .map_err(|e| e.into_activity_error(&ctx.id))?;

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
            execution_id: Some(execution_id),
            activity_id: Some(ctx.id.clone()),
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

    /// EMB-701: present a frame with **no workflow/agent involvement at
    /// all** — the definitional standalone-middleware entry point. Runs the
    /// exact same trust-layer pipeline as the workflow `ui` activity
    /// (`judge_frame`), then persists the validated frame as a standalone
    /// [`PendingFrame`] (`execution_id`/`activity_id` both `None`) so
    /// `GET`/`POST .../decisions` work identically to the workflow-backed
    /// path — deciding it just records a [`DecidedOutcome`] instead of
    /// signaling a workflow that doesn't exist. Unlike the workflow path,
    /// a display-only (no-action) frame is still persisted as pending rather
    /// than auto-completing — there's no "activity" here to complete; the
    /// caller decides for itself when it's done consulting `GET
    /// /api/v1/ui/frames/{id}` (a known, documented limitation: nothing
    /// evicts an abandoned standalone frame yet, same class of gap as
    /// `execution_locks`' unbounded growth elsewhere in this workspace).
    pub(crate) fn present_standalone(
        &self,
        tenant: &str,
        frame_value: &Value,
    ) -> Result<PendingFrame, PresentError> {
        let seq = self.standalone_seq.fetch_add(1, Ordering::SeqCst);
        let audit_id = format!("standalone:{tenant}:{seq}");
        // No provenance to stamp — this frame was never generated by an
        // Wovyr-native run at all (EMB-701's whole point).
        let (final_frame, policy_ref) = self.judge_frame(tenant, frame_value, &audit_id, |_| {})?;

        let frame_hash = final_frame.content_hash();
        let frame_id = frame_id_for_standalone(tenant, seq, &frame_hash);
        let frame_json = serde_json::to_value(&final_frame).map_err(|e| {
            PresentError::Invalid(format!("failed to serialize validated frame: {e}"))
        })?;

        let pending = PendingFrame {
            frame_id: frame_id.clone(),
            tenant: tenant.to_string(),
            execution_id: None,
            activity_id: None,
            frame: frame_json,
            frame_hash: frame_hash.clone(),
            policy_ref,
            created_at_ms: crate::audit::now_ms(),
        };
        self.frames.upsert(pending.clone());
        self.record_audit(system_event(
            tenant,
            "ui.frame.present",
            &format!("{frame_id}:{frame_hash}"),
        ));
        Ok(pending)
    }
}

/// Why [`UiRuntime::judge_frame`]/[`present_standalone`](UiRuntime::present_standalone)
/// rejected a frame — already audited by the time this is returned.
pub(crate) enum PresentError {
    /// Failed protocol validation (UIP-101/106).
    Invalid(String),
    /// The tenant's policy document couldn't be resolved (fail-closed).
    PolicyResolution(wovyr_common::Error),
    /// The trust layer blocked it; the rule id, not the reason (the
    /// blocklist stance — detail lives in the audit chain).
    Blocked { rule: String },
}

impl PresentError {
    /// The workflow `ui` activity's error shape — a permanent
    /// `ActivityError` naming `activity_id`.
    fn into_activity_error(self, activity_id: &str) -> ActivityError {
        match self {
            PresentError::Invalid(msg) => ActivityError::Permanent(format!(
                "activity `{activity_id}`: invalid ui frame ({msg})"
            )),
            PresentError::PolicyResolution(err) => ActivityError::Permanent(format!(
                "activity `{activity_id}`: ui policy resolution failed ({err}); failing closed"
            )),
            PresentError::Blocked { rule } => ActivityError::Permanent(format!(
                "activity `{activity_id}`: ui frame blocked by policy rule `{rule}`"
            )),
        }
    }

    /// The HTTP shape for `POST /api/v1/ui/present` (EMB-701).
    fn into_api_error(self) -> ApiError {
        match self {
            PresentError::Invalid(msg) => {
                ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", msg)
            }
            PresentError::PolicyResolution(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                err.to_string(),
            ),
            PresentError::Blocked { rule } => ApiError::new(
                StatusCode::FORBIDDEN,
                "blocked",
                format!("ui frame blocked by policy rule `{rule}`"),
            ),
        }
    }
}

/// A platform-actor audit event: the trust layer itself acted (presented/
/// rejected/blocked a frame, from either the workflow `ui` activity or the
/// standalone `present` route), not a request principal.
fn system_event(tenant: &str, action: &str, resource_id: &str) -> AuditEvent {
    AuditEvent::new(
        crate::audit::now_ms(),
        "wovyr-ui",
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

/// `uif-<16 hex>` for a **standalone** frame (EMB-701) — no execution/activity
/// to derive from, so this hashes `(tenant, a monotonic per-runtime counter,
/// content_hash)` instead. The counter alone would already guarantee
/// uniqueness within one runtime's lifetime; folding in the tenant and
/// content hash too just keeps the id from trivially leaking the raw
/// sequence number.
fn frame_id_for_standalone(tenant: &str, seq: u64, content_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tenant.as_bytes());
    hasher.update([0x1f]);
    hasher.update(seq.to_le_bytes());
    hasher.update([0x1f]);
    hasher.update(content_hash.as_bytes());
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
        .route("/api/v1/ui/present", post(present_handler))
        .route("/api/v1/ui/frames", get(list_frames_handler))
        .route("/api/v1/ui/frames/{frame_id}", get(get_frame_handler))
        .route(
            "/api/v1/ui/decisions/{frame_id}",
            post(decide_handler).get(get_decision_handler),
        )
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

/// The frame document a `POST /api/v1/ui/present` (EMB-701) call submits.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PresentRequest {
    /// A UiFrame document: `{schema_version, title?, root}`. See the
    /// generative-UI protocol docs.
    frame: Value,
}

/// `POST /api/v1/ui/present` — EMB-701's standalone middleware entry point:
/// present a frame with **no workflow or agent adoption required at all**.
/// Runs the identical trust-layer pipeline the workflow `ui` activity uses
/// (fail-closed protocol validation, tenant policy or the hosted floor,
/// audited verdict), then persists the validated frame exactly like a
/// workflow-presented one — `GET /api/v1/ui/frames[/{id}]` and
/// `POST /api/v1/ui/decisions/{id}` work identically either way. Any agent
/// stack that can make an HTTP call can use the trust runtime this way,
/// without adopting `wovyr-workflow`/`wovyr-agent` at all.
#[utoipa::path(
    post,
    path = "/api/v1/ui/present",
    tag = "ui",
    request_body = PresentRequest,
    responses(
        (status = 200, description = "The frame passed the trust layer and is now pending a decision."),
        (status = 400, description = "The frame failed protocol validation.", body = crate::openapi::ApiErrorBody),
        (status = 403, description = "The trust layer blocked the frame.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn present_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PresentRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "ui:write")?;
    let pending = state
        .ui
        .present_standalone(&tenant, &req.frame)
        .map_err(PresentError::into_api_error)?;
    crate::audit::audit(
        &state,
        &headers,
        &tenant,
        "ui.frame.present.standalone",
        "ui_frame",
        &pending.frame_id,
    );
    Ok(Json(frame_body(&pending)))
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
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "ui:read")?;
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
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "ui:read")?;
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

/// `POST /api/v1/ui/decisions/{frame_id}` — validate the typed decision
/// against the frame it answers, then either resume the suspended workflow
/// execution (HIL-303) or, for a **standalone** frame (EMB-701, no
/// `execution_id`/`activity_id`), record the outcome for later retrieval via
/// `GET /api/v1/ui/decisions/{frame_id}`. Either way the audit record pairs
/// the decision-taker and the frame hash it answered (HIL-306).
#[utoipa::path(
    post,
    path = "/api/v1/ui/decisions/{frame_id}",
    tag = "ui",
    params(("frame_id" = String, Path, description = "The pending frame id (`uif-…`).")),
    request_body = DecideRequest,
    responses(
        (status = 200, description = "Decision accepted; execution resumed (or, for a standalone frame, recorded)."),
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
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "ui:write")?;
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
    // never delivered to the workflow (or recorded standalone).
    validate_decision(&frame, &decision)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;

    let decided_by = crate::tenancy::principal(&headers);
    let decided_at_ms = crate::audit::now_ms();

    match (&pending.execution_id, &pending.activity_id) {
        (Some(execution_id), Some(activity_id)) => {
            let def =
                crate::workflow_runner::resolve_definition(&state, execution_id, None).await?;
            let payload = json!({
                "action": decision.action,
                "values": decision.values,
                "decided_by": decided_by,
                "decided_at_ms": decided_at_ms,
                "frame_id": pending.frame_id,
                "frame_hash": pending.frame_hash,
            });
            state
                .workflows
                .signal_event(&def, execution_id, activity_id, payload)
                .await
                .map_err(ApiError::from)?;
        }
        _ => {
            // Standalone (EMB-701): there's no workflow to signal — record
            // the outcome so an external, non-Wovyr-native caller can retrieve
            // what was decided.
            state.ui.decisions().record(DecidedOutcome {
                frame_id: pending.frame_id.clone(),
                tenant: tenant.clone(),
                action: decision.action.clone(),
                values: decision.values.clone(),
                decided_by: decided_by.clone(),
                decided_at_ms,
                frame_hash: pending.frame_hash.clone(),
            });
        }
    }

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

/// `GET /api/v1/ui/decisions/{frame_id}` — retrieve a **standalone** frame's
/// recorded decision (EMB-701) after the pending record is gone. A
/// workflow-backed frame's outcome lives in the workflow's own state/output
/// instead (`GET /api/v1/workflows/{id}`) — this route only ever has
/// something to return for a frame presented via `POST /api/v1/ui/present`.
#[utoipa::path(
    get,
    path = "/api/v1/ui/decisions/{frame_id}",
    tag = "ui",
    params(("frame_id" = String, Path, description = "The frame id (`uif-…`) a decision was submitted for.")),
    responses(
        (status = 200, description = "The recorded decision for a standalone frame."),
        (status = 404, description = "No recorded decision (unknown, still pending, workflow-backed, or a different tenant).", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn get_decision_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(frame_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "ui:read")?;
    match state.ui.decisions().get(&frame_id) {
        Some(outcome) if outcome.tenant == tenant => Ok(Json(json!({
            "frame_id": outcome.frame_id,
            "action": outcome.action,
            "values": outcome.values,
            "decided_by": outcome.decided_by,
            "decided_at_ms": outcome.decided_at_ms,
            "frame_hash": outcome.frame_hash,
        }))),
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no recorded decision for frame `{frame_id}`"),
        )),
    }
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
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;
    use wovyr_audit::{AuditFilter, AuditLog, Outcome};
    use wovyr_ui_guard::{PolicyRules, UiPolicy};
    use wovyr_workflow::{
        CheckpointStore, Engine, EventLog, InMemoryStore, InMemoryTimerStore, WorkflowState,
    };

    fn ensure_admin_env() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
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
            base.mcp.clone(),
            base.secrets.clone(),
        ));
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let timers: Arc<dyn wovyr_workflow::TimerStore> = Arc::new(InMemoryTimerStore::new());
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
                    .header("x-wovyr-principal", "root")
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
                    .header("x-wovyr-principal", "root")
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
    ) -> wovyr_workflow::ExecutionState {
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

    fn audited(audit: &AuditLog, action: &str) -> Vec<wovyr_audit::AuditEntry> {
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
            reason.contains(wovyr_ui_guard::rules::SENSITIVE_INPUT),
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
        assert_eq!(pending.execution_id.as_deref(), Some("uc1-restart-test"));
        assert_eq!(pending.activity_id.as_deref(), Some("confirm"));
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
                .contains(wovyr_ui_guard::rules::HOSTED_FLOOR)
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

    /// EMB-701: the definitional standalone-middleware claim, proven for
    /// real — present a frame via `POST /api/v1/ui/present` with **zero**
    /// workflow or agent involvement (no `wovyr-workflow`/`wovyr-agent` call
    /// anywhere in this test), decide it, and retrieve the recorded decision
    /// via `GET /api/v1/ui/decisions/{id}` — the same trust layer, the same
    /// audit chain, none of the workflow machinery.
    #[tokio::test]
    async fn emb701_standalone_present_decide_and_retrieve_with_no_workflow_at_all() {
        let store = InMemoryStore::new();
        let (audit, ui) = ui_runtime(true);
        let state = ui_state(&store, &ui, &audit).await;
        let router = crate::router(state.clone());

        let frame = json!({
            "schema_version": "1.0.0",
            "title": "Approve refund",
            "root": {
                "type": "column",
                "children": [
                    { "type": "text", "text": "Refund $42.00 to the customer?" },
                    { "type": "button", "action": "approve", "label": "Approve", "class": "approve" },
                    { "type": "button", "action": "deny", "label": "Deny", "class": "reject" }
                ]
            }
        });
        let (st, body) = post_json(
            router.clone(),
            "/api/v1/ui/present",
            json!({ "frame": frame }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let frame_id = body["frame_id"].as_str().expect("frame_id").to_string();
        // No execution/activity — this frame was never a workflow at all.
        assert_eq!(body["execution_id"], Value::Null);
        assert_eq!(body["activity_id"], Value::Null);
        let frame_hash = body["frame_hash"].as_str().expect("frame_hash").to_string();

        // It's genuinely pending, visible on the same pull surface a
        // workflow-backed frame uses.
        let (st, list_body) = get_json(router.clone(), "/api/v1/ui/frames").await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            list_body["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["frame_id"] == json!(frame_id))
        );

        // No decision recorded yet.
        let (st, _) = get_json(router.clone(), &format!("/api/v1/ui/decisions/{frame_id}")).await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // Decide it — the same endpoint a workflow-backed frame uses.
        let (st, decide_body) = post_json(
            router.clone(),
            &format!("/api/v1/ui/decisions/{frame_id}"),
            json!({ "action": "approve" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{decide_body}");
        assert_eq!(decide_body["status"], "decided");
        assert_eq!(decide_body["execution_id"], Value::Null);

        // The frame is consumed from the pending surface…
        let (st, list_body) = get_json(router.clone(), "/api/v1/ui/frames").await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            !list_body["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["frame_id"] == json!(frame_id))
        );

        // …but the decision is retrievable after the fact — the whole point
        // of a standalone caller with no workflow to poll instead.
        let (st, outcome) =
            get_json(router.clone(), &format!("/api/v1/ui/decisions/{frame_id}")).await;
        assert_eq!(st, StatusCode::OK, "{outcome}");
        assert_eq!(outcome["action"], "approve");
        assert_eq!(outcome["decided_by"], "root");
        assert_eq!(outcome["frame_hash"], json!(frame_hash));

        let presents = audited(&audit, "ui.frame.present");
        assert_eq!(presents.len(), 1);
        let decisions = audited(&audit, "ui.decision.submit");
        assert_eq!(decisions.len(), 1);
        audit.verify().expect("audit chain verifies");
    }

    /// EMB-701 + GRD-201: the standalone path runs through the exact same
    /// trust layer as the workflow path — a credential-harvesting standalone
    /// frame is blocked, never becomes pending, and is audited.
    #[tokio::test]
    async fn emb701_standalone_present_is_trust_layer_governed_too() {
        let store = InMemoryStore::new();
        let (audit, ui) = ui_runtime(true);
        let state = ui_state(&store, &ui, &audit).await;
        let router = crate::router(state.clone());

        let frame = json!({
            "schema_version": "1.0.0",
            "root": {
                "type": "column",
                "children": [
                    { "type": "text_input", "name": "card_number", "label": "Card number" },
                    { "type": "button", "action": "pay", "label": "Continue", "class": "submit" }
                ]
            }
        });
        let (st, body) = post_json(
            router.clone(),
            "/api/v1/ui/present",
            json!({ "frame": frame }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], "blocked");

        let (st, list_body) = get_json(router.clone(), "/api/v1/ui/frames").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            list_body["data"],
            json!([]),
            "a blocked frame is never pending"
        );

        let blocks = audited(&audit, "ui.frame.block");
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0]
                .event
                .reason
                .as_deref()
                .unwrap_or("")
                .contains(wovyr_ui_guard::rules::SENSITIVE_INPUT)
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
        let dir = std::env::temp_dir().join(format!("wovyr_ui_frames_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("pending_frames.json");

        let frame = PendingFrame {
            frame_id: "uif-test".into(),
            tenant: "default".into(),
            execution_id: Some("e1".into()),
            activity_id: Some("confirm".into()),
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
        assert_eq!(revived.execution_id.as_deref(), Some("e1"));
        assert_eq!(reopened.list("default").len(), 1);
        assert!(reopened.list("acme").is_empty(), "listing is tenant-scoped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
