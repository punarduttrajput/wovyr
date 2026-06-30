//! Multi-tenancy HTTP routes: organizations, projects, memberships, and quotas
//! ([Projects API](../../docs/09-api/projects.md)), backed by the [`AppState`] tenancy
//! catalog and gated by [RBAC](../../docs/13-security/rbac.md).
//!
//! **Request context.** Each request acts as a `principal` in a `tenant`, optionally
//! within a `project`, carried by headers:
//!
//! - `X-Apex-Tenant` — the tenant (defaults to `default`).
//! - `X-Apex-Principal` — the acting user id (falls back to the bearer token).
//! - principals listed in `APEX_PLATFORM_ADMINS` (comma-separated) are platform admins.
//!
//! The principal's [`Role`]s are resolved from its memberships (narrowed to the
//! in-scope project's org + the project itself), and every handler authorizes the
//! scope from the [endpoint table](../../docs/09-api/projects.md#3-endpoints)
//! **fail-closed** (default-deny → `403`).

use crate::{ApiError, AppState};
use apex_tenancy::{
    MemberScope, Membership, Organization, Project, ProjectStatus, QuotaLimits, Role, TenantContext,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// The tenancy sub-router, merged into the main app router.
pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/organizations",
            get(list_orgs).post(create_org),
        )
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{id}",
            get(get_project).patch(patch_project).delete(delete_project),
        )
        .route(
            "/api/v1/projects/{id}/members",
            get(list_members).post(add_member),
        )
        .route(
            "/api/v1/projects/{id}/members/{uid}",
            axum::routing::delete(remove_member),
        )
        .route(
            "/api/v1/projects/{id}/quota",
            get(get_quota).patch(set_quota),
        )
}

// --- request context ------------------------------------------------------------

const DEFAULT_TENANT: &str = "default";

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The platform-admin principals from `APEX_PLATFORM_ADMINS` (comma-separated).
fn is_platform_admin(principal: &str) -> bool {
    !principal.is_empty()
        && std::env::var("APEX_PLATFORM_ADMINS")
            .ok()
            .is_some_and(|v| v.split(',').map(str::trim).any(|p| p == principal))
}

/// Build the [`TenantContext`] for a request, resolving the principal's effective roles
/// against the tenancy store (narrowed to `project` and its org when project-scoped).
fn context(
    state: &AppState,
    headers: &HeaderMap,
    project: Option<String>,
) -> TenantContext {
    let tenant = header(headers, "x-apex-tenant").unwrap_or(DEFAULT_TENANT).to_string();
    let principal = header(headers, "x-apex-principal")
        .or_else(|| header(headers, "authorization").and_then(|a| a.strip_prefix("Bearer ")))
        .unwrap_or("")
        .to_string();

    let mut roles = Vec::new();
    if is_platform_admin(&principal) {
        roles.push(Role::PlatformAdmin);
    }
    if !principal.is_empty() {
        let project_org = project
            .as_deref()
            .and_then(|p| state.tenancy.get_project(p).ok().flatten())
            .map(|p| p.organization);
        for m in state.tenancy.memberships_for_user(&principal).unwrap_or_default() {
            let applies = match &m.scope {
                MemberScope::Project(p) => project.as_deref() == Some(p.as_str()),
                // Org roles apply to the in-scope project's org, and to org-level
                // (project-less) operations within the tenant.
                MemberScope::Organization(o) => {
                    project_org.as_deref() == Some(o.as_str()) || project.is_none()
                }
            };
            if applies {
                roles.push(m.role);
            }
        }
    }
    TenantContext {
        tenant,
        project,
        principal,
        roles,
    }
}

// --- organizations ---------------------------------------------------------------

#[derive(Deserialize)]
struct CreateOrgRequest {
    name: String,
}

async fn list_orgs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("projects:read")?;
    let orgs = state.tenancy.list_orgs(&ctx.tenant)?;
    Ok(Json(json!({ "organizations": orgs })))
}

async fn create_org(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateOrgRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("org.admin")?;
    let org = state
        .tenancy
        .create_org(Organization::new(&ctx.tenant, req.name))?;
    Ok((StatusCode::CREATED, Json(json!(org))))
}

// --- projects --------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateProjectRequest {
    name: String,
    organization: String,
}

#[derive(Deserialize)]
struct PatchProjectRequest {
    #[serde(default)]
    settings: Option<std::collections::BTreeMap<String, Value>>,
    #[serde(default)]
    status: Option<ProjectStatus>,
}

async fn list_projects(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("projects:read")?;
    let projects = state.tenancy.list_projects(&ctx.tenant)?;
    Ok(Json(json!({ "projects": projects })))
}

async fn create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("projects:admin")?;
    let org = state
        .tenancy
        .get_org(&req.organization)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "organization not found"))?;
    let project = state.tenancy.create_project(Project::new(&org, req.name))?;
    Ok((StatusCode::CREATED, Json(json!(project))))
}

async fn get_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:read")?;
    let project = state
        .tenancy
        .get_project(&id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "project not found"))?;
    Ok(Json(json!(project)))
}

async fn patch_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<PatchProjectRequest>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    let mut project = state
        .tenancy
        .get_project(&id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "project not found"))?;
    if let Some(settings) = req.settings {
        project.settings = settings;
    }
    if let Some(status) = req.status {
        project.status = status;
    }
    state.tenancy.update_project(project.clone())?;
    Ok(Json(json!(project)))
}

async fn delete_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    state.tenancy.delete_project(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- memberships -----------------------------------------------------------------

#[derive(Deserialize)]
struct AddMemberRequest {
    user: String,
    role: Role,
}

async fn list_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:read")?;
    let members = state
        .tenancy
        .list_memberships(&MemberScope::Project(id))?;
    Ok(Json(json!({ "members": members })))
}

async fn add_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    let membership = Membership {
        user: req.user,
        role: req.role,
        scope: MemberScope::Project(id),
    };
    state.tenancy.add_membership(membership.clone())?;
    Ok((StatusCode::CREATED, Json(json!(membership))))
}

async fn remove_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    state
        .tenancy
        .remove_membership(&uid, &MemberScope::Project(id))?;
    Ok(StatusCode::NO_CONTENT)
}

// --- quotas ----------------------------------------------------------------------

async fn get_quota(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:read")?;
    let limits = state.tenancy.get_quota(&id)?.unwrap_or_default();
    Ok(Json(json!({ "scope": "project", "limits": limits })))
}

async fn set_quota(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(limits): Json<QuotaLimits>,
) -> Result<Json<Value>, ApiError> {
    // Quota changes are an org-level operation (§3 endpoint table).
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("org.admin")?;
    state.tenancy.set_quota(&id, limits.clone())?;
    Ok(Json(json!({ "scope": "project", "limits": limits })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Issue a request with a tenant + principal header, returning (status, body).
    async fn req(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        principal: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let resp = crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("x-apex-tenant", "acme")
                    .header("x-apex-principal", principal)
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn state() -> Arc<AppState> {
        Arc::new(AppState::from_env().with_tenancy(Arc::new(
            apex_tenancy::InMemoryTenancyStore::new(),
        )))
    }

    #[tokio::test]
    async fn rbac_gates_the_tenancy_lifecycle() {
        // SAFETY: single-threaded test; bootstrap a platform admin via env.
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
        let st = state();

        // A non-admin principal cannot create an org (default-deny → 403).
        let (s, _) = req(&st, "POST", "/api/v1/organizations", "nobody", json!({"name":"Platform"})).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // The platform admin can.
        let (s, org) = req(&st, "POST", "/api/v1/organizations", "root", json!({"name":"Platform"})).await;
        assert_eq!(s, StatusCode::CREATED);
        let org_id = org["id"].as_str().unwrap().to_string();

        // Create a project under it.
        let (s, prj) = req(
            &st,
            "POST",
            "/api/v1/projects",
            "root",
            json!({"name":"support","organization": org_id}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let prj_id = prj["id"].as_str().unwrap().to_string();

        // Grant alice the editor role on the project.
        let (s, _) = req(
            &st,
            "POST",
            &format!("/api/v1/projects/{prj_id}/members"),
            "root",
            json!({"user":"alice","role":"editor"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);

        // alice (editor) may read the project, but not delete it (needs projects:admin).
        let (s, _) = req(&st, "GET", &format!("/api/v1/projects/{prj_id}"), "alice", Value::Null).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = req(&st, "DELETE", &format!("/api/v1/projects/{prj_id}"), "alice", Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // A stranger can't even read it.
        let (s, _) = req(&st, "GET", &format!("/api/v1/projects/{prj_id}"), "mallory", Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // Quota: set (org.admin = root) then read (projects:read = alice).
        let (s, _) = req(
            &st,
            "PATCH",
            &format!("/api/v1/projects/{prj_id}/quota"),
            "root",
            json!({"concurrent_agent_runs": 5}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, q) = req(&st, "GET", &format!("/api/v1/projects/{prj_id}/quota"), "alice", Value::Null).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(q["limits"]["concurrent_agent_runs"], 5);
    }

    #[tokio::test]
    async fn duplicate_org_is_conflict() {
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
        let st = state();
        let (s, _) = req(&st, "POST", "/api/v1/organizations", "root", json!({"name":"Dup"})).await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, _) = req(&st, "POST", "/api/v1/organizations", "root", json!({"name":"Dup"})).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }
}
