//! Audit routes + helpers ([audit](../../docs/13-security/audit.md)).
//!
//! Security-sensitive handlers call [`record`] to append a tamper-evident audit entry to
//! `AppState.audit`; [`GET /api/v1/audit`](list_audit) reads them back, scoped to the
//! caller's tenant and RBAC-gated. Audit records reference resources by id (never value),
//! so a secret read/rotate audits the `secret://…` reference, not the secret.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::hardening::{DEFAULT_LIMIT, MAX_LIMIT, decode_cursor, encode_cursor};
use crate::{ApiError, AppState};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use wovyr_audit::{AuditEvent, AuditFilter};

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
pub(crate) struct AuditQuery {
    principal: Option<String>,
    action: Option<String>,
    /// Restrict to entries at or after this timestamp (epoch ms, inclusive; SEC-301).
    after_ms: Option<u64>,
    /// Restrict to entries at or before this timestamp (epoch ms, inclusive; SEC-301).
    before_ms: Option<u64>,
    limit: Option<usize>,
    /// Opaque pagination cursor from a prior page's `next_cursor` (overview §6,
    /// RM-GA-P4 API-701) — for this route it wraps a `seq` (SEC-301), not an offset,
    /// but uses the identical wire encoding every other list route's cursor does.
    cursor: Option<String>,
}

/// `GET /api/v1/audit` — the caller's tenant's audit trail, most-recent first,
/// filterable by `principal`/`action`/a `[after_ms, before_ms]` time range and
/// cursor-paginated (overview §6). RBAC-gated (`audit:read`) and always
/// tenant-scoped, so a caller only sees its own tenant's records.
///
/// Reads via [`wovyr_audit::AuditLog::query_page`] (SEC-301) rather than fetching
/// every matching entry and slicing in Rust: `FileAuditSink` serves this from a
/// bounded backward scan of the log instead of the whole-file read the old
/// `query()`-based implementation paid on every call. `total_estimate` is
/// deliberately `null` here (unlike every other paginated route) — computing an
/// exact count would require exactly the full scan this route exists to avoid.
#[utoipa::path(
    get,
    path = "/api/v1/audit",
    tag = "audit",
    params(
        ("principal" = Option<String>, Query, description = "Filter to a principal."),
        ("action" = Option<String>, Query, description = "Filter to an action."),
        ("after_ms" = Option<u64>, Query, description = "Only entries at or after this epoch-ms timestamp."),
        ("before_ms" = Option<u64>, Query, description = "Only entries at or before this epoch-ms timestamp."),
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "The tenant's audit trail, most-recent first.")),
)]
pub(crate) async fn list_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "audit:read")?;
    let filter = AuditFilter {
        tenant: Some(tenant),
        principal: q.principal,
        action: q.action,
        after_ms: q.after_ms,
        before_ms: q.before_ms,
        limit: None,
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let before_seq = q
        .cursor
        .as_deref()
        .and_then(decode_cursor)
        .map(|c| c as u64);

    let page = state.audit.query_page(&filter, before_seq, limit)?;
    let data: Vec<Value> = page
        .entries
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    let has_more = page.next_cursor.is_some();
    Ok(Json(json!({
        "data": data,
        "has_more": has_more,
        "next_cursor": page.next_cursor.map(|c| encode_cursor(c as usize)),
        "total_estimate": Value::Null,
    })))
}
