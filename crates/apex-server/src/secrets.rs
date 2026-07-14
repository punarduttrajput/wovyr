//! Secret-vault management routes ([secret-management](../../docs/13-security/secret-management.md)).
//!
//! A thin, **tenant-scoped** HTTP surface over [`apex_secrets::Vault`]: the caller's
//! tenant *is* the secret namespace (resolved + authorized via the same membership-bound
//! `tenant_authorize` used by the agent/workflow/memory routes), so a caller can only
//! create/rotate/list/delete secrets in its own tenant. Responses carry **metadata only**
//! — a secret's value is never returned here; values leave the vault only through the
//! resolution/injection path ([§5](../../docs/13-security/secret-management.md#5-injection-into-tools--plugins)).
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::hardening::{PageQuery, paginate};
use crate::{ApiError, AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use utoipa::ToSchema;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/secrets", get(list_secrets).post(create_secret))
        .route(
            "/api/v1/secrets/{name}",
            get(get_secret).delete(delete_secret),
        )
        .route("/api/v1/secrets/{name}/rotate", post(rotate_secret))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateRequest {
    name: String,
    value: String,
}

/// `POST /api/v1/secrets` — create a secret in the caller's tenant. Returns metadata
/// (never the value).
#[utoipa::path(
    post,
    path = "/api/v1/secrets",
    tag = "secrets",
    request_body = CreateRequest,
    responses((status = 200, description = "Secret metadata (value never returned).")),
)]
pub(crate) async fn create_secret(
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
    crate::audit::audit(state, headers, tenant, action, "secret", reference);
}

/// `GET /api/v1/secrets` — list the caller's tenant's secrets (metadata only),
/// cursor-paginated (overview §6, RM-GA-P4 API-701).
#[utoipa::path(
    get,
    path = "/api/v1/secrets",
    tag = "secrets",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "A paginated list of the tenant's secrets (metadata only).")),
)]
pub(crate) async fn list_secrets(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "secrets:read")?;
    let items: Vec<Value> = state
        .secrets
        .list(&tenant)?
        .into_iter()
        .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
        .collect();
    Ok(Json(paginate(items, &page.page())))
}

/// `GET /api/v1/secrets/{name}` — a secret's metadata (never its value).
#[utoipa::path(
    get,
    path = "/api/v1/secrets/{name}",
    tag = "secrets",
    params(("name" = String, Path, description = "The secret's name.")),
    responses(
        (status = 200, description = "The secret's metadata."),
        (status = 404, description = "Unknown secret.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn get_secret(
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

#[derive(Deserialize, ToSchema)]
pub(crate) struct RotateRequest {
    value: String,
}

/// `POST /api/v1/secrets/{name}/rotate` — rotate a secret's value (bumps the version).
#[utoipa::path(
    post,
    path = "/api/v1/secrets/{name}/rotate",
    tag = "secrets",
    params(("name" = String, Path, description = "The secret's name.")),
    request_body = RotateRequest,
    responses((status = 200, description = "Rotated; metadata with the bumped version.")),
)]
pub(crate) async fn rotate_secret(
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
#[utoipa::path(
    delete,
    path = "/api/v1/secrets/{name}",
    tag = "secrets",
    params(("name" = String, Path, description = "The secret's name.")),
    responses(
        (status = 204, description = "Secret deleted."),
        (status = 404, description = "Unknown secret.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn delete_secret(
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
