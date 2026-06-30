//! Secret-vault management routes ([secret-management](../../docs/13-security/secret-management.md)).
//!
//! A thin, **tenant-scoped** HTTP surface over [`apex_secrets::Vault`]: the caller's
//! tenant *is* the secret namespace (resolved + authorized via the same membership-bound
//! `tenant_authorize` used by the agent/workflow/memory routes), so a caller can only
//! create/rotate/list/delete secrets in its own tenant. Responses carry **metadata only**
//! — a secret's value is never returned here; values leave the vault only through the
//! resolution/injection path ([§5](../../docs/13-security/secret-management.md#5-injection-into-tools--plugins)).

use crate::{ApiError, AppState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/secrets", get(list_secrets).post(create_secret))
        .route(
            "/api/v1/secrets/{name}",
            get(get_secret).delete(delete_secret),
        )
        .route("/api/v1/secrets/{name}/rotate", post(rotate_secret))
}

#[derive(Deserialize)]
struct CreateRequest {
    name: String,
    value: String,
}

/// `POST /api/v1/secrets` — create a secret in the caller's tenant. Returns metadata
/// (never the value).
async fn create_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "secrets:write")?;
    let meta = state.secrets.create(&tenant, &req.name, &req.value)?;
    audit_secret(&state, &headers, &tenant, "secret.create", &meta.reference);
    Ok(Json(json!(meta)))
}

/// Record a secret-management action against the audit log — by **reference**, never
/// value ([audit §2 Secrets](../../docs/13-security/audit.md#2-what-is-audited)).
fn audit_secret(
    state: &AppState,
    headers: &HeaderMap,
    tenant: &str,
    action: &str,
    reference: &str,
) {
    let principal = crate::tenancy::principal(headers);
    crate::audit::record(
        state,
        apex_audit::AuditEvent::new(
            crate::audit::now_ms(),
            principal,
            tenant,
            action,
            "secret",
            reference,
        ),
    );
}

/// `GET /api/v1/secrets` — list the caller's tenant's secrets (metadata only).
async fn list_secrets(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "secrets:read")?;
    let items = state.secrets.list(&tenant)?;
    Ok(Json(json!({ "secrets": items, "total": items.len() })))
}

/// `GET /api/v1/secrets/{name}` — a secret's metadata (never its value).
async fn get_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "secrets:read")?;
    let meta = state.secrets.metadata(&tenant, &name)?.ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("secret `{name}` not found"),
        )
    })?;
    Ok(Json(json!(meta)))
}

#[derive(Deserialize)]
struct RotateRequest {
    value: String,
}

/// `POST /api/v1/secrets/{name}/rotate` — rotate a secret's value (bumps the version).
async fn rotate_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<RotateRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "secrets:write")?;
    let meta = state.secrets.rotate(&tenant, &name, &req.value)?;
    audit_secret(&state, &headers, &tenant, "secret.rotate", &meta.reference);
    Ok(Json(json!(meta)))
}

/// `DELETE /api/v1/secrets/{name}` — remove a secret from the caller's tenant.
async fn delete_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "secrets:write")?;
    if state.secrets.delete(&tenant, &name)? {
        audit_secret(
            &state,
            &headers,
            &tenant,
            "secret.delete",
            &format!("secret://{tenant}/{name}"),
        );
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("secret `{name}` not found"),
        ))
    }
}
