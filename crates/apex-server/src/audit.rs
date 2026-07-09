//! Audit routes + helpers ([audit](../../docs/13-security/audit.md)).
//!
//! Security-sensitive handlers call [`record`] to append a tamper-evident audit entry to
//! `AppState.audit`; [`GET /api/v1/audit`](list_audit) reads them back, scoped to the
//! caller's tenant and RBAC-gated. Audit records reference resources by id (never value),
//! so a secret read/rotate audits the `secret://…` reference, not the secret.

use crate::hardening::{PageQuery, paginate};
use crate::{ApiError, AppState};
use apex_audit::{AuditEvent, AuditFilter};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/audit", get(list_audit))
}

/// Wall-clock at the request boundary, epoch milliseconds (the only clock read on the
/// audit path — the chain math itself stays deterministic).
pub(crate) fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append `event` to the audit log, best-effort: a sink failure is logged but never fails
/// the operation being audited (the audit path must not break the request path).
pub(crate) fn record(state: &AppState, event: AuditEvent) {
    if let Err(e) = state.audit.record(event) {
        tracing::warn!("audit record failed: {e}");
    }
}

/// Build and append a standard **allowed** audit entry for `action` on
/// `(resource_type, resource_id)`, attributed to the request's principal and
/// `tenant` and — when the request carries one (RM-GA-P4 OBS-802) — its
/// correlating request id. This is the shared shape every mutating handler's audit
/// call site now uses (RM-GA-P4 OBS-804: agents, plugins, tenancy, marketplace,
/// webhooks, workflow executions); `kms.rs`/`secrets.rs`'s own small
/// `audit_kms`/`audit_secret` wrappers predate this and now delegate to it too,
/// rather than each independently constructing an `AuditEvent`.
pub(crate) fn audit(
    state: &AppState,
    headers: &HeaderMap,
    tenant: &str,
    action: &str,
    resource_type: &str,
    resource_id: &str,
) {
    let mut event = AuditEvent::new(
        now_ms(),
        crate::tenancy::principal(headers),
        tenant,
        action,
        resource_type,
        resource_id,
    );
    if let Some(request_id) = crate::hardening::request_id_of(headers) {
        event = event.with_request_id(request_id);
    }
    record(state, event);
}

#[derive(Deserialize)]
struct AuditQuery {
    principal: Option<String>,
    action: Option<String>,
    /// `limit` + `cursor` (overview §6, RM-GA-P4 API-701).
    #[serde(flatten)]
    page: PageQuery,
}

/// `GET /api/v1/audit` — the caller's tenant's audit trail, most-recent first,
/// filterable by `principal`/`action` and cursor-paginated (overview §6).
/// RBAC-gated (`audit:read`) and always tenant-scoped, so a caller only sees
/// its own tenant's records.
async fn list_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "audit:read")?;
    let filter = AuditFilter {
        tenant: Some(tenant),
        principal: q.principal,
        action: q.action,
        // Fetch every matching entry; pagination slices it (RM-GA-P4 API-701 —
        // the standard envelope, matching every other list route).
        limit: None,
    };
    let mut entries = state.audit.query(&filter)?;
    entries.reverse(); // most-recent first
    let items: Vec<Value> = entries
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Ok(Json(paginate(items, &q.page.page())))
}
