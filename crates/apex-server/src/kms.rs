//! Key-management routes ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)).
//!
//! A thin, **tenant-scoped** surface over the platform `Kms` (the same instance backing
//! the secret vault's `EncryptedFileSecretStore` and the memory engine's
//! `EncryptingMemoryStore`): an operator can roll a new tenant-key version
//! (`kms:write` — routine, low-stakes) or permanently **crypto-shred** a tenant's key
//! material (`kms:admin` — irreversible, gated at a materially higher tier than routine
//! writes). Both actions are audited by tenant reference, never touching key material.

use crate::{ApiError, AppState};
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/kms/tenant-key/rotate", post(rotate_tenant_key))
        .route("/api/v1/kms/tenant-key/destroy", post(destroy_tenant_key))
}

/// Record a key-management action against the audit log — by tenant reference, never
/// key material ([audit §2](../../docs/13-security/audit.md#2-what-is-audited)).
fn audit_kms(state: &AppState, headers: &HeaderMap, tenant: &str, action: &str) {
    crate::audit::audit(state, headers, tenant, action, "kms_tenant_key", tenant);
}

/// `POST /api/v1/kms/tenant-key/rotate` — roll a new tenant-key version. Existing
/// wrapped data keys remain valid under their original version (nothing is
/// re-encrypted); only new seals use the new version until a consumer explicitly
/// rewraps.
async fn rotate_tenant_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "kms:write")?;
    let version = state.kms.rotate_tenant_key(&tenant)?;
    audit_kms(&state, &headers, &tenant, "kms.tenant_key.rotate");
    Ok(Json(
        json!({ "tenant": tenant, "version": version, "status": "rotated" }),
    ))
}

/// `POST /api/v1/kms/tenant-key/destroy` — **crypto-shred** the tenant's key material.
/// Irreversible: every data key ever wrapped under this tenant becomes permanently
/// unrecoverable, so any sealed secret/memory content for it is gone for good. Gated
/// at `kms:admin`, a materially higher tier than the routine `kms:write`/`secrets:write`
/// scopes.
async fn destroy_tenant_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "kms:admin")?;
    state.kms.destroy_tenant_key(&tenant)?;
    audit_kms(&state, &headers, &tenant, "kms.tenant_key.destroy");
    Ok(Json(json!({ "tenant": tenant, "status": "destroyed" })))
}
