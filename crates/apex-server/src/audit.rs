//! Audit routes + helpers ([audit](../../docs/13-security/audit.md)).
//!
//! Security-sensitive handlers call [`record`] to append a tamper-evident audit entry to
//! `AppState.audit`; [`GET /api/v1/audit`](list_audit) reads them back, scoped to the
//! caller's tenant and RBAC-gated. Audit records reference resources by id (never value),
//! so a secret read/rotate audits the `secret://…` reference, not the secret.

use crate::{ApiError, AppState};
use apex_audit::{AuditEvent, AuditFilter};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
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

#[derive(Deserialize)]
struct AuditQuery {
    principal: Option<String>,
    action: Option<String>,
    limit: Option<usize>,
}

/// `GET /api/v1/audit` — the caller's tenant's audit trail (most-recent first when
/// `limit` is set), filterable by `principal`/`action`. RBAC-gated (`audit:read`) and
/// always tenant-scoped, so a caller only sees its own tenant's records.
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
        limit: q.limit,
    };
    let entries = state.audit.query(&filter)?;
    Ok(Json(json!({ "entries": entries, "total": entries.len() })))
}
