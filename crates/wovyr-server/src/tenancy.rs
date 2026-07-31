//! Multi-tenancy HTTP routes: organizations, projects, memberships, and quotas
//! ([Projects API](../../docs/09-api/projects.md)), backed by the [`AppState`] tenancy
//! catalog and gated by [RBAC](../../docs/13-security/rbac.md).
//!
//! **Request context.** Each request acts as a `principal` in a `tenant`, optionally
//! within a `project`, carried by headers:
//!
//! - `X-Wovyr-Tenant` — the tenant (defaults to `default`).
//! - `X-Wovyr-Principal` — the acting user id (falls back to the bearer token).
//! - principals listed in `WOVYR_PLATFORM_ADMINS` (comma-separated) are platform admins.
//!
//! The principal's [`Role`]s are resolved from its memberships (narrowed to the
//! in-scope project's org + the project itself), and every handler authorizes the
//! scope from the [endpoint table](../../docs/09-api/projects.md#3-endpoints)
//! **fail-closed** (default-deny → `403`).
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic. The mutex-poison `.expect()`s this file still has are internal
//! invariants, not request-dependent, and each carries an explicit `#[allow]`.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::{ApiError, AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use utoipa::ToSchema;
use wovyr_tenancy::{
    MemberScope, Membership, Organization, Project, ProjectStatus, QuotaLimits, Role, TenancyStore,
    TenantContext,
};

/// The tenancy sub-router, merged into the main app router.
pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/organizations", get(list_orgs).post(create_org))
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

pub(crate) const DEFAULT_TENANT: &str = "default";

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The platform-admin principals from `WOVYR_PLATFORM_ADMINS` (comma-separated).
fn is_platform_admin(principal: &str) -> bool {
    !principal.is_empty()
        && std::env::var("WOVYR_PLATFORM_ADMINS")
            .ok()
            .is_some_and(|v| v.split(',').map(str::trim).any(|p| p == principal))
}

/// Build the [`TenantContext`] for a request, resolving the principal's effective roles
/// against the tenancy store (narrowed to `project` and its org when project-scoped).
pub(crate) fn context(
    state: &AppState,
    headers: &HeaderMap,
    project: Option<String>,
) -> TenantContext {
    let tenant = header(headers, "x-wovyr-tenant")
        .unwrap_or(DEFAULT_TENANT)
        .to_string();
    let principal = header(headers, "x-wovyr-principal")
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
        for m in state
            .tenancy
            .memberships_for_user(&principal)
            .unwrap_or_default()
        {
            let applies = match &m.scope {
                MemberScope::Project(p) => project.as_deref() == Some(p.as_str()),
                MemberScope::Organization(o) => {
                    if project.is_some() {
                        // Project-scoped request: an org role applies to the
                        // in-scope project's owning org.
                        project_org.as_deref() == Some(o.as_str())
                    } else {
                        // Org-level (project-less) request: an org role applies
                        // only if that org belongs to the *request's* tenant —
                        // never unconditionally (RM-AR-P1 SEC-402). `X-Wovyr-Tenant`
                        // is an unverified client header, so the old
                        // `|| project.is_none()` escape let an org admin in tenant
                        // A spoof `X-Wovyr-Tenant: B` and pass org-level authz
                        // (`create_org`, `list_orgs`, `list_projects`) for tenant
                        // B with no membership there. A genuinely tenant-global
                        // operation is gated on `platform.admin` (pushed above),
                        // not on the absence of a project.
                        state
                            .tenancy
                            .get_org(o)
                            .ok()
                            .flatten()
                            .is_some_and(|org| org.tenant == tenant)
                    }
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

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateOrgRequest {
    name: String,
}

/// List the caller's tenant's organizations.
#[utoipa::path(
    get,
    path = "/api/v1/organizations",
    tag = "tenancy",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "A paginated list of the caller's tenant's organizations.")),
)]
pub(crate) async fn list_orgs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<crate::hardening::PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("projects:read")?;
    let items = state
        .tenancy
        .list_orgs(&ctx.tenant)?
        .into_iter()
        .map(|o| serde_json::to_value(o).unwrap_or(Value::Null))
        .collect();
    Ok(Json(crate::hardening::paginate(items, &page.page())))
}

/// Create an organization in the caller's tenant.
#[utoipa::path(
    post,
    path = "/api/v1/organizations",
    tag = "tenancy",
    request_body = CreateOrgRequest,
    responses(
        (status = 201, description = "Organization created."),
        (status = 403, description = "Caller lacks org.admin.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn create_org(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateOrgRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("org.admin")?;
    let org = state
        .tenancy
        .create_org(Organization::new(&ctx.tenant, req.name))?;
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "organization.create",
        "organization",
        &org.id,
    );
    crate::webhooks::emit(&state, "organization.created", &ctx.tenant, json!(org));
    Ok((StatusCode::CREATED, Json(json!(org))))
}

// --- projects --------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateProjectRequest {
    name: String,
    organization: String,
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct PatchProjectRequest {
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    settings: Option<std::collections::BTreeMap<String, Value>>,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    status: Option<ProjectStatus>,
}

/// List the caller's tenant's projects.
#[utoipa::path(
    get,
    path = "/api/v1/projects",
    tag = "tenancy",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "A paginated list of the caller's tenant's projects.")),
)]
pub(crate) async fn list_projects(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<crate::hardening::PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("projects:read")?;
    let items = state
        .tenancy
        .list_projects(&ctx.tenant)?
        .into_iter()
        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
        .collect();
    Ok(Json(crate::hardening::paginate(items, &page.page())))
}

/// Create a project under an organization.
#[utoipa::path(
    post,
    path = "/api/v1/projects",
    tag = "tenancy",
    request_body = CreateProjectRequest,
    responses(
        (status = 201, description = "Project created (ETag header carries its version)."),
        (status = 404, description = "Unknown organization.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateProjectRequest>,
) -> Result<Response, ApiError> {
    let ctx = context(&state, &headers, None);
    ctx.authorize("projects:admin")?;
    let org = state.tenancy.get_org(&req.organization)?.ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", "organization not found")
    })?;
    let project = state.tenancy.create_project(Project::new(&org, req.name))?;
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "project.create",
        "project",
        &project.id,
    );
    crate::webhooks::emit(&state, "project.created", &ctx.tenant, json!(project));
    let etag = crate::hardening::etag(project.version);
    Ok((
        StatusCode::CREATED,
        [(header::ETAG, etag)],
        Json(json!(project)),
    )
        .into_response())
}

/// Fetch a project by id.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}",
    tag = "tenancy",
    params(("id" = String, Path, description = "The project id.")),
    responses(
        (status = 200, description = "The project (ETag header carries its version)."),
        (status = 404, description = "Unknown project.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn get_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:read")?;
    let project = state
        .tenancy
        .get_project(&id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "project not found"))?;
    // Reads return an ETag (the resource version) for optimistic concurrency (§10).
    let etag = crate::hardening::etag(project.version);
    Ok(([(header::ETAG, etag)], Json(json!(project))).into_response())
}

/// Update a project's settings/status.
#[utoipa::path(
    patch,
    path = "/api/v1/projects/{id}",
    tag = "tenancy",
    params(
        ("id" = String, Path, description = "The project id."),
        ("If-Match" = Option<String>, Header, description = "The expected resource version (optimistic concurrency, §10); a stale value is rejected 409."),
    ),
    request_body = PatchProjectRequest,
    responses(
        (status = 200, description = "The updated project (ETag header carries its new version)."),
        (status = 404, description = "Unknown project.", body = crate::openapi::ApiErrorBody),
        (status = 409, description = "Stale If-Match.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn patch_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<PatchProjectRequest>,
) -> Result<Response, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    let mut project = state
        .tenancy
        .get_project(&id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "project not found"))?;

    // Optimistic concurrency (§10): a stale `If-Match` loses to a concurrent update.
    if let Some(expected) = crate::hardening::if_match(&headers)
        && expected != project.version
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!(
                "project `{id}` is at version {}, but If-Match was {expected}",
                project.version
            ),
        ));
    }

    if let Some(settings) = req.settings {
        project.settings = settings;
    }
    if let Some(status) = req.status {
        project.status = status;
    }
    project.version += 1;
    state.tenancy.update_project(project.clone())?;
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "project.update",
        "project",
        &id,
    );
    crate::webhooks::emit(&state, "project.updated", &ctx.tenant, json!(project));
    let etag = crate::hardening::etag(project.version);
    Ok(([(header::ETAG, etag)], Json(json!(project))).into_response())
}

/// Delete a project.
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{id}",
    tag = "tenancy",
    params(("id" = String, Path, description = "The project id.")),
    responses(
        (status = 204, description = "Project deleted."),
        (status = 404, description = "Unknown project.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn delete_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    state.tenancy.delete_project(&id)?;
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "project.delete",
        "project",
        &id,
    );
    crate::webhooks::emit(&state, "project.deleted", &ctx.tenant, json!({ "id": id }));
    Ok(StatusCode::NO_CONTENT)
}

// --- memberships -----------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
pub(crate) struct AddMemberRequest {
    user: String,
    #[schema(value_type = String)]
    role: Role,
}

/// List a project's memberships.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/members",
    tag = "tenancy",
    params(("id" = String, Path, description = "The project id.")),
    responses((status = 200, description = "The project's memberships.")),
)]
pub(crate) async fn list_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:read")?;
    let members = state.tenancy.list_memberships(&MemberScope::Project(id))?;
    Ok(Json(json!({ "members": members })))
}

/// Add a member to a project.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/members",
    tag = "tenancy",
    params(("id" = String, Path, description = "The project id.")),
    request_body = AddMemberRequest,
    responses((status = 201, description = "Membership added.")),
)]
pub(crate) async fn add_member(
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
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "member.add",
        "membership",
        &membership.user,
    );
    crate::webhooks::emit(&state, "member.added", &ctx.tenant, json!(membership));
    Ok((StatusCode::CREATED, Json(json!(membership))))
}

/// Remove a member from a project.
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{id}/members/{uid}",
    tag = "tenancy",
    params(
        ("id" = String, Path, description = "The project id."),
        ("uid" = String, Path, description = "The member's user id."),
    ),
    responses((status = 204, description = "Membership removed.")),
)]
pub(crate) async fn remove_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:admin")?;
    state
        .tenancy
        .remove_membership(&uid, &MemberScope::Project(id.clone()))?;
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "member.remove",
        "membership",
        &uid,
    );
    crate::webhooks::emit(
        &state,
        "member.removed",
        &ctx.tenant,
        json!({ "user": uid, "project": id }),
    );
    Ok(StatusCode::NO_CONTENT)
}

// --- quotas ----------------------------------------------------------------------

/// Fail closed on a quota operation naming a project that doesn't exist.
///
/// Quotas are stored in their own map keyed by project id, so without this check
/// both handlers happily operated on *any* string: `PATCH
/// /api/v1/projects/does-not-exist/quota` (or even an empty id, from a doubled
/// slash) returned `200` and persisted a limit under a key no run would ever look
/// up, while `GET` returned `200 {}` for the same nonexistent project — even though
/// `GET /api/v1/projects/{id}` itself correctly 404s. An operator who fat-fingered a
/// project id got a success response for a budget that enforces nothing, which is
/// the same silent-no-op class of failure as an unpriced model reporting `$0`.
fn require_project(state: &AppState, id: &str) -> Result<(), ApiError> {
    state
        .tenancy
        .get_project(id)?
        .map(|_| ())
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "project not found"))
}

/// Get a project's quota limits.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/quota",
    tag = "tenancy",
    params(("id" = String, Path, description = "The project id.")),
    responses(
        (status = 200, description = "The project's quota limits."),
        (status = 404, description = "No such project.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn get_quota(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("projects:read")?;
    require_project(&state, &id)?;
    let limits = state.tenancy.get_quota(&id)?.unwrap_or_default();
    Ok(Json(json!({ "scope": "project", "limits": limits })))
}

/// `limits` is `wovyr_tenancy::QuotaLimits` (an external-crate type, so it has no
/// generated schema here — see `GET .../quota`'s response for its shape).
#[utoipa::path(
    patch,
    path = "/api/v1/projects/{id}/quota",
    tag = "tenancy",
    params(("id" = String, Path, description = "The project id.")),
    responses(
        (status = 200, description = "The updated quota limits."),
        (status = 403, description = "Caller lacks org.admin.", body = crate::openapi::ApiErrorBody),
        (status = 404, description = "No such project.", body = crate::openapi::ApiErrorBody),
    ),
)]
pub(crate) async fn set_quota(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(limits): Json<QuotaLimits>,
) -> Result<Json<Value>, ApiError> {
    // Quota changes are an org-level operation (§3 endpoint table).
    let ctx = context(&state, &headers, Some(id.clone()));
    ctx.authorize("org.admin")?;
    require_project(&state, &id)?;
    state.tenancy.set_quota(&id, limits.clone())?;
    crate::audit::audit(
        &state,
        &headers,
        &ctx.tenant,
        "quota.update",
        "project",
        &id,
    );
    crate::webhooks::emit(
        &state,
        "quota.updated",
        &ctx.tenant,
        json!({ "project": id, "limits": limits }),
    );
    Ok(Json(json!({ "scope": "project", "limits": limits })))
}

// --- run-path quota enforcement --------------------------------------------------

/// Per-project runtime usage: in-flight agent runs and the current day's LLM spend.
#[derive(Default)]
struct QuotaUsage {
    /// In-flight agent runs, by project id.
    concurrent: BTreeMap<String, u64>,
    /// `(day, usd, tokens)` LLM usage for the current rolling day, by project id
    /// (tokens since RM-AIM-P2 SRV-202 — prompt + completion, the observe half of
    /// the `llm_tokens_per_day` budget).
    cost: BTreeMap<String, (u64, f64, u64)>,
}

/// One persisted day entry. The pre-SRV-202 accumulator stored `[day, usd]`; an
/// upgraded binary must keep loading that shape (tokens default to 0) rather than
/// treating the file as corrupt and silently resetting every project's spend to
/// $0 — the exact failure DUR-404 exists to prevent.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum StoredDayUsage {
    V2(u64, f64, u64),
    V1(u64, f64),
}

impl From<StoredDayUsage> for (u64, f64, u64) {
    fn from(v: StoredDayUsage) -> Self {
        match v {
            StoredDayUsage::V2(day, usd, tokens) => (day, usd, tokens),
            StoredDayUsage::V1(day, usd) => (day, usd, 0),
        }
    }
}

/// Tracks per-project quota usage for the run path ([Projects API §5](../../docs/09-api/projects.md#5-quotas)).
///
/// The daily-cost accumulator persists (RM-GA-P2 DUR-404) when opened with a path —
/// without this, a crash-loop reset every project's spend to $0, silently bypassing
/// its daily budget. `concurrent` (in-flight runs) deliberately does **not** persist:
/// a restart means nothing is actually still running, so carrying over a stale count
/// would incorrectly throttle the runs that follow.
///
/// **Distributed enforcement (RM-AIM-P3 SRV-307):** `concurrent` is in-process by
/// default — correct for one node, but N nodes each enforcing their own copy of a
/// project's `concurrent_agent_runs` limit multiplies the effective budget by N. Behind
/// the `redis` cargo feature with `WOVYR_QUOTA_REDIS_URL` set, the concurrency slot
/// lives in a shared Redis counter instead (same degrade-to-local-never-to-unlimited
/// posture as [`crate::rate_limit::RateLimiter`]'s SRV-201 sibling): admission is an
/// atomic "increment only if under the limit" Lua script, and a `PEXPIRE` safety net
/// self-heals a slot a node's hard crash left stranded (no `Drop` runs on `kill -9`) —
/// a documented, bounded tradeoff, not full crash-recovery semantics.
pub(crate) struct QuotaTracker {
    usage: Mutex<QuotaUsage>,
    path: Option<PathBuf>,
    #[cfg(feature = "redis")]
    shared_concurrency: Option<Arc<redis_concurrency::SharedConcurrency>>,
}

impl Default for QuotaTracker {
    fn default() -> Self {
        Self::new(None)
    }
}

impl QuotaTracker {
    /// Open a tracker, loading any persisted daily-cost accumulator from `path`
    /// (best-effort: a missing or corrupt file starts empty). `path: None` is a
    /// purely in-memory tracker (what tests use).
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        let cost = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| {
                serde_json::from_slice::<BTreeMap<String, StoredDayUsage>>(&bytes).ok()
            })
            .map(|stored| stored.into_iter().map(|(k, v)| (k, v.into())).collect())
            .unwrap_or_default();
        Self {
            usage: Mutex::new(QuotaUsage {
                concurrent: BTreeMap::new(),
                cost,
            }),
            path,
            #[cfg(feature = "redis")]
            shared_concurrency: None,
        }
    }

    /// Back this tracker's concurrency slots with a shared Redis (SRV-307),
    /// namespaced under `prefix` (so a fleet's quota counters never collide with
    /// e.g. rate-limit buckets sharing the same Redis). Mirrors
    /// [`crate::rate_limit::RateLimiter::with_redis`] exactly: lazily-dialed,
    /// re-dialed after an error, degrades to the in-process `BTreeMap` on failure.
    #[cfg(feature = "redis")]
    pub(crate) fn with_redis(mut self, client: redis::Client, prefix: impl Into<String>) -> Self {
        self.shared_concurrency = Some(Arc::new(redis_concurrency::SharedConcurrency::new(
            client,
            prefix.into(),
        )));
        self
    }

    /// Open a tracker from the environment: Redis-shared concurrency when the
    /// server is compiled with the `redis` feature and `WOVYR_QUOTA_REDIS_URL` is
    /// set, else purely in-process. Setting the variable on a binary built
    /// *without* the feature logs a loud warning rather than silently running
    /// per-node concurrency limits.
    pub(crate) fn from_env(path: Option<PathBuf>) -> Self {
        let tracker = Self::new(path);
        let Ok(url) = std::env::var("WOVYR_QUOTA_REDIS_URL") else {
            return tracker;
        };
        #[cfg(feature = "redis")]
        {
            match redis::Client::open(url.as_str()) {
                Ok(client) => {
                    tracing::info!("quota: shared Redis concurrency counters enabled");
                    return tracker.with_redis(client, "wovyr:quota:concur");
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "WOVYR_QUOTA_REDIS_URL is invalid; falling back to per-node concurrency limits"
                    );
                }
            }
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = &url;
            tracing::error!(
                "WOVYR_QUOTA_REDIS_URL is set but this binary was built without the `redis` \
                 feature — concurrency limits are per-node, not fleet-wide"
            );
        }
        tracker
    }

    /// The project's `(usd, tokens)` LLM usage recorded for the current rolling
    /// day, if any — the read half of [`record_run_usage`] (RM-AIM-P2 RUN-202/
    /// SRV-202: lets tests observe that a run's cost/tokens actually landed in
    /// the accumulator). Test-only until a route needs it (e.g. a future usage
    /// endpoint).
    #[cfg(test)]
    pub(crate) fn used_today(&self, project: &str) -> Option<(f64, u64)> {
        self.used_on_day(project, current_day())
    }

    /// Total quota-admitted runs currently in flight across every project — the
    /// in-flight gauge (OBS-301), recomputed from the permit ledger at every
    /// scrape. Node-local by design, like the ledger itself (SRV-307's optional
    /// Redis sharing changes admission, not this local view).
    pub(crate) fn concurrent_total(&self) -> u64 {
        // A poisoned ledger reads as 0 rather than panicking the scrape — the
        // gauge is advisory, and the next healthy scrape self-corrects.
        self.usage
            .lock()
            .map(|u| u.concurrent.values().sum())
            .unwrap_or(0)
    }

    /// Like [`Self::used_today`] but for an explicit day bucket — lets a test
    /// assert usage landed under a non-UTC reset boundary (SRV-203).
    #[cfg(test)]
    pub(crate) fn used_on_day(&self, project: &str, day: u64) -> Option<(f64, u64)> {
        let usage = self.usage.lock().expect("quota mutex poisoned");
        usage
            .cost
            .get(project)
            .filter(|(d, _, _)| *d == day)
            .map(|(_, usd, tokens)| (*usd, *tokens))
    }

    /// Persist the daily-cost accumulator (best-effort — logged, not propagated,
    /// since the in-memory update this follows has already succeeded either way).
    fn persist(&self, cost: &BTreeMap<String, (u64, f64, u64)>) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(error = %e, "failed to create quota accumulator directory");
            return;
        }
        match serde_json::to_vec_pretty(cost) {
            Ok(bytes) => {
                if let Err(e) = wovyr_common::fs::atomic_write(path, bytes) {
                    tracing::error!(error = %e, "failed to persist quota accumulator");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to encode quota accumulator"),
        }
    }
}

/// The Redis-shared concurrency-slot store (SRV-307), behind the `redis` cargo
/// feature. Structurally mirrors [`crate::rate_limit`]'s `redis_shared` module
/// (lazily-dialed connection, re-dialed on error, a `REDIS_BUDGET` timeout) — the
/// two differ only in what they atomically compute: a token-bucket refill there,
/// a bounded increment/decrement counter here.
#[cfg(feature = "redis")]
mod redis_concurrency {
    use std::time::Duration;

    /// Atomic "increment only if under the limit" — executed inside Redis so
    /// concurrent nodes serialize on one counter instead of each independently
    /// reading-then-incrementing (which could race two nodes both past the
    /// limit). Returns `1` (admitted) or `0` (at limit). The `PEXPIRE` on every
    /// successful increment is the safety net described on [`SLOT_TTL`].
    const TRY_ADMIT_SCRIPT: &str = r#"
        local limit = tonumber(ARGV[1])
        local ttl_ms = tonumber(ARGV[2])
        local current = tonumber(redis.call('GET', KEYS[1]) or '0')
        if current < limit then
            redis.call('INCR', KEYS[1])
            redis.call('PEXPIRE', KEYS[1], ttl_ms)
            return 1
        end
        return 0
    "#;

    /// Atomic "decrement, floored at 0" — a stray extra release (e.g. a
    /// double-release bug) must never push the counter negative, which would
    /// silently widen the effective limit for every node sharing it.
    const RELEASE_SCRIPT: &str = r#"
        local current = tonumber(redis.call('GET', KEYS[1]) or '0')
        if current > 0 then
            redis.call('DECR', KEYS[1])
        end
        return 1
    "#;

    /// A held slot's safety-net TTL. `RunPermit::drop` releases the slot
    /// explicitly on the normal path, but `Drop` never runs on a hard crash
    /// (`kill -9`, power loss) — without this, a node that dies mid-run would
    /// permanently strand that project's concurrency budget for the whole
    /// fleet. Generous enough that no legitimate run should outlive it in
    /// practice; a documented, bounded tradeoff, not full crash-recovery
    /// semantics (a run genuinely still in flight past 24h would have its slot
    /// expire and could race a fresh admission past the limit).
    const SLOT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

    /// Cap on how long the shared path may take before the caller degrades to
    /// the local counter — mirrors [`crate::rate_limit`]'s identical budget.
    const REDIS_BUDGET: Duration = Duration::from_secs(1);

    pub(super) struct SharedConcurrency {
        client: redis::Client,
        /// Lazily-dialed multiplexed connection; cleared on command failure so
        /// the next call re-dials instead of erroring forever on a dead socket.
        conn: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
        prefix: String,
        admit_script: redis::Script,
        release_script: redis::Script,
    }

    impl SharedConcurrency {
        pub(super) fn new(client: redis::Client, prefix: String) -> Self {
            Self {
                client,
                conn: tokio::sync::Mutex::new(None),
                prefix,
                admit_script: redis::Script::new(TRY_ADMIT_SCRIPT),
                release_script: redis::Script::new(RELEASE_SCRIPT),
            }
        }

        async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, String> {
            let mut slot = self.conn.lock().await;
            if let Some(conn) = slot.as_ref() {
                return Ok(conn.clone());
            }
            let conn = self
                .client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| format!("redis connect: {e}"))?;
            *slot = Some(conn.clone());
            Ok(conn)
        }

        fn key(&self, project: &str) -> String {
            format!("{}:{project}", self.prefix)
        }

        /// Try to admit one more concurrent run for `project` under `limit`.
        /// `Ok(true)`/`Ok(false)` is the real admission decision; `Err` means the
        /// store itself is unavailable (caller degrades to the local counter).
        pub(super) async fn try_admit(&self, project: &str, limit: u64) -> Result<bool, String> {
            let attempt = async {
                let mut conn = self.connection().await?;
                let allowed: i64 = self
                    .admit_script
                    .key(self.key(project))
                    .arg(limit)
                    .arg(SLOT_TTL.as_millis() as u64)
                    .invoke_async(&mut conn)
                    .await
                    .map_err(|e| format!("redis eval: {e}"))?;
                Ok(allowed == 1)
            };
            match tokio::time::timeout(REDIS_BUDGET, attempt).await {
                Ok(Ok(decision)) => Ok(decision),
                Ok(Err(reason)) => {
                    *self.conn.lock().await = None;
                    Err(reason)
                }
                Err(_) => {
                    *self.conn.lock().await = None;
                    Err(format!("redis timed out after {REDIS_BUDGET:?}"))
                }
            }
        }

        /// Release a previously-admitted slot. Best-effort: a failure here just
        /// means the slot stays held until its `PEXPIRE` safety net expires it —
        /// logged, never propagated (there's no meaningful recovery action for a
        /// caller, especially the fire-and-forget release `RunPermit::drop` spawns).
        pub(super) async fn release(&self, project: &str) {
            let attempt = async {
                let mut conn = self.connection().await?;
                let _: i64 = self
                    .release_script
                    .key(self.key(project))
                    .invoke_async(&mut conn)
                    .await
                    .map_err(|e| format!("redis eval: {e}"))?;
                Ok::<(), String>(())
            };
            match tokio::time::timeout(REDIS_BUDGET, attempt).await {
                Ok(Ok(_)) => {}
                Ok(Err(reason)) => {
                    *self.conn.lock().await = None;
                    tracing::warn!(error = %reason, project, "failed to release shared quota slot");
                }
                Err(_) => {
                    *self.conn.lock().await = None;
                    tracing::warn!(project, "timed out releasing shared quota slot");
                }
            }
        }
    }
}

/// The day bucket `epoch_secs` falls in for a reset boundary `offset_minutes`
/// east of UTC (RM-AIM-P2 SRV-203) — pure, so the local-midnight boundary math is
/// deterministically testable. `offset 0` = the original UTC days-since-epoch;
/// `+330` (IST) makes the bucket flip at 00:00 IST instead of 00:00 UTC. The
/// offset is clamped to ±24 h (real timezones span −12 h..+14 h), so a garbage
/// stored value can't skew a budget window by more than a day.
fn day_bucket(epoch_secs: u64, offset_minutes: i32) -> u64 {
    let offset_secs = i64::from(offset_minutes.clamp(-1_440, 1_440)) * 60;
    (epoch_secs as i64 + offset_secs).div_euclid(86_400).max(0) as u64
}

/// The current rolling-day bucket for a quota's configured reset boundary
/// ([`QuotaLimits::day_reset_offset_minutes`], default UTC). Wall-clock is read
/// only here, at the server boundary — never in core engine logic.
fn current_day_with_offset(offset_minutes: i32) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    day_bucket(now, offset_minutes)
}

/// The current UTC day bucket — what a project with no configured offset uses.
/// Only the test-only [`QuotaTracker::used_today`] still calls this directly;
/// the enforcement path always goes through [`current_day_with_offset`].
#[cfg(test)]
fn current_day() -> u64 {
    current_day_with_offset(0)
}

/// Releases a project's concurrency slot when dropped (after the run completes, succeeds
/// or fails). A no-op when the run was unmetered (no project / no quota). Depends only on
/// the [`QuotaTracker`] itself (not the full [`AppState`]), so it can be shared with
/// callers — like the workflow engine's
/// [`StoredAgentResolver`](crate::workflow_runner::StoredAgentResolver)
/// — that are constructed before an `AppState` exists.
pub(crate) struct RunPermit {
    quota: Arc<QuotaTracker>,
    project: String,
    metered: bool,
    /// Whether this slot was granted via the shared Redis counter (SRV-307) —
    /// release must go back through the same store it was admitted through.
    #[cfg(feature = "redis")]
    via_shared: bool,
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        if !self.metered {
            return;
        }
        #[cfg(feature = "redis")]
        if self.via_shared
            && let Some(shared) = self.quota.shared_concurrency.clone()
        {
            // `Drop::drop` is sync, but releasing a shared slot is a network call —
            // spawned rather than blocked on, fire-and-forget like this codebase's
            // other best-effort release paths (e.g. webhook delivery). Safe because
            // a `RunPermit` only ever drops from within request-handling code,
            // always inside an active Tokio runtime.
            let project = self.project.clone();
            tokio::spawn(async move { shared.release(&project).await });
            return;
        }
        if let Ok(mut u) = self.quota.usage.lock()
            && let Some(c) = u.concurrent.get_mut(&self.project)
        {
            *c = c.saturating_sub(1);
        }
    }
}

/// Admit an agent run for the optional in-scope `project`, enforcing its quota
/// (concurrent runs + the day's LLM spend). Returns a [`RunPermit`] holding the
/// concurrency slot until dropped; `Err` ([`Error::QuotaExceeded`] → `429`) if a limit
/// is hit. Unmetered (returns a no-op permit) when there is no project or no quota.
///
/// **Concurrency is checked via the shared Redis store first (SRV-307)** when the
/// tracker is configured for it and the quota actually bounds concurrency — an
/// atomic "increment only if under the limit" that a fleet of nodes shares, so N
/// nodes enforce one combined budget instead of N independent ones. Any shared-store
/// failure (unreachable, timed out) degrades to the in-process counter, never to
/// unlimited. Cost/token budgets stay per-node (each node's own disk-persisted
/// accumulator, RM-GA-P2 DUR-404) — only the concurrency dimension is fleet-shared;
/// widening cost/token sharing the same way is a documented follow-on, not silently
/// assumed to already work.
pub(crate) async fn admit_run(
    tenancy: &Arc<dyn TenancyStore>,
    quota: &Arc<QuotaTracker>,
    project: Option<&str>,
) -> Result<RunPermit, ApiError> {
    let unmetered = |project: String| RunPermit {
        quota: quota.clone(),
        project,
        metered: false,
        #[cfg(feature = "redis")]
        via_shared: false,
    };
    let Some(project) = project else {
        return Ok(unmetered(String::new()));
    };
    let Some(limits) = tenancy.get_quota(project)? else {
        return Ok(unmetered(project.to_string()));
    };

    // The day bucket honors the quota's configured reset boundary (SRV-203):
    // usage recorded under another bucket (an earlier local day) reads as zero.
    let day = current_day_with_offset(limits.day_reset_offset_minutes.unwrap_or(0));

    #[cfg(feature = "redis")]
    if let (Some(shared), Some(limit)) = (
        quota.shared_concurrency.clone(),
        limits.concurrent_agent_runs,
    ) {
        match shared.try_admit(project, limit).await {
            Ok(true) => {
                // Slot granted by the shared store; cost/token checks still go
                // through the local accumulator (see this fn's doc comment).
                let (spent, tokens_used) = {
                    #[allow(clippy::expect_used)] // SRV-306: mutex-poison invariant
                    let u = quota.usage.lock().expect("quota mutex poisoned");
                    match u.cost.get(project) {
                        Some((d, c, t)) if *d == day => (*c, *t),
                        _ => (0.0, 0),
                    }
                };
                if let Err(e) = limits
                    .check_llm_cost(spent, 0.0)
                    .and_then(|()| limits.check_llm_tokens(tokens_used, 0))
                {
                    shared.release(project).await;
                    return Err(e.into());
                }
                return Ok(RunPermit {
                    quota: quota.clone(),
                    project: project.to_string(),
                    metered: true,
                    via_shared: true,
                });
            }
            Ok(false) => {
                return Err(wovyr_common::Error::quota_exceeded(format!(
                    "concurrent_agent_runs: shared fleet-wide limit {limit} reached"
                ))
                .into());
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "shared quota store unavailable; degrading to per-node concurrency limiting"
                );
            }
        }
    }

    #[allow(clippy::expect_used)] // SRV-306: mutex-poison invariant, not request data
    let mut u = quota.usage.lock().expect("quota mutex poisoned");
    let current = u.concurrent.get(project).copied().unwrap_or(0);
    limits.check_concurrent_runs(current)?;
    let (spent, tokens_used) = match u.cost.get(project) {
        Some((d, c, t)) if *d == day => (*c, *t),
        _ => (0.0, 0),
    };
    // Admit while the day's spend/token usage is still within budget; the run's
    // own usage is recorded afterwards, so the *next* run is blocked once a limit
    // is crossed.
    limits.check_llm_cost(spent, 0.0)?;
    limits.check_llm_tokens(tokens_used, 0)?;
    *u.concurrent.entry(project.to_string()).or_insert(0) += 1;
    Ok(RunPermit {
        quota: quota.clone(),
        project: project.to_string(),
        metered: true,
        #[cfg(feature = "redis")]
        via_shared: false,
    })
}

/// Record a run's LLM usage — `cost` USD and `tokens` (prompt + completion) —
/// against `project`'s current-day budgets (after a run), persisting the
/// accumulator (RM-GA-P2 DUR-404) so it survives a restart within the same day.
/// Tokens joined cost in RM-AIM-P2 SRV-202 (`llm_tokens_per_day`); the day
/// bucket honors the quota's configured reset boundary (SRV-203) — looked up
/// here so recording and [`admit_run`] always agree on which day usage lands in.
pub(crate) fn record_run_usage(
    tenancy: &Arc<dyn TenancyStore>,
    quota: &Arc<QuotaTracker>,
    project: Option<&str>,
    cost: f64,
    tokens: u64,
) {
    let Some(project) = project else {
        return;
    };
    let offset = tenancy
        .get_quota(project)
        .ok()
        .flatten()
        .and_then(|limits| limits.day_reset_offset_minutes)
        .unwrap_or(0);
    let day = current_day_with_offset(offset);
    let snapshot = {
        #[allow(clippy::expect_used)] // SRV-306: mutex-poison invariant, not request data
        let mut u = quota.usage.lock().expect("quota mutex poisoned");
        let entry = u.cost.entry(project.to_string()).or_insert((day, 0.0, 0));
        if entry.0 != day {
            *entry = (day, 0.0, 0);
        }
        entry.1 += cost;
        entry.2 = entry.2.saturating_add(tokens);
        u.cost.clone()
    };
    quota.persist(&snapshot);
}

/// Resolve the request's effective roles **narrowed to the asserted tenant**: an org
/// membership counts only if its org belongs to `X-Wovyr-Tenant`, and a project
/// membership only if the project's org does. This is the isolation primitive for
/// resources owned by a *tenant* (rather than a specific project) — it stops a principal
/// from authorizing against a tenant it merely *names* in the header but holds no
/// membership in. (Platform admins are unconditionally in-scope.)
pub(crate) fn tenant_context(state: &AppState, headers: &HeaderMap) -> TenantContext {
    let tenant = header(headers, "x-wovyr-tenant")
        .unwrap_or(DEFAULT_TENANT)
        .to_string();
    let principal = header(headers, "x-wovyr-principal")
        .or_else(|| header(headers, "authorization").and_then(|a| a.strip_prefix("Bearer ")))
        .unwrap_or("")
        .to_string();

    let mut roles = Vec::new();
    if is_platform_admin(&principal) {
        roles.push(Role::PlatformAdmin);
    }
    if !principal.is_empty() {
        // The orgs that actually belong to the asserted tenant.
        let tenant_orgs: std::collections::BTreeSet<String> = state
            .tenancy
            .list_orgs(&tenant)
            .unwrap_or_default()
            .into_iter()
            .map(|o| o.id)
            .collect();
        for m in state
            .tenancy
            .memberships_for_user(&principal)
            .unwrap_or_default()
        {
            let in_tenant = match &m.scope {
                MemberScope::Organization(o) => tenant_orgs.contains(o),
                MemberScope::Project(p) => state
                    .tenancy
                    .get_project(p)
                    .ok()
                    .flatten()
                    .is_some_and(|pr| tenant_orgs.contains(&pr.organization)),
            };
            if in_tenant {
                roles.push(m.role);
            }
        }
    }
    TenantContext {
        tenant,
        project: None,
        principal,
        roles,
    }
}

/// Authorize a tenant-scoped resource operation and return the caller's authorized
/// tenant (the key resources like agents/workflows are scoped by). Fail-closed: a named
/// tenant or any authenticated principal must hold a tenant-scoped role granting `scope`
/// (→ `403`), which requires real membership — so `X-Wovyr-Tenant` cannot be spoofed.
///
/// **No anonymous-default-tenant RBAC bypass** (RM-GA-P4/GA-003, narrowing SEC-102):
/// this used to short-circuit `Ok(ctx.tenant)` for an anonymous caller against the
/// `default` tenant whenever `state.anonymous_allowed` — i.e. `WOVYR_ALLOW_ANONYMOUS=1`
/// granted such a caller *every* scope, including `kms:admin` (crypto-shredding), with
/// zero RBAC check at all. That was SEC-102's own literal design (a documented,
/// intentional "local/dev convenience," not an oversight — see
/// [compliance-mapping.md §7](../../docs/13-security/compliance-mapping.md#7-residual-risk-and-gaps)),
/// but GA-003 scoped it as a residual finding to close: `anonymous_allowed` now governs
/// **only** whether [`auth::authenticate`]'s `disabled-loopback` mode lets an
/// unauthenticated request *reach* a handler at all (`WOVYR_ALLOW_ANONYMOUS=1`, refused
/// by [`crate::serve`] on any non-loopback bind) — it no longer implies any particular
/// *authorization* outcome once it does. An anonymous caller is authorized exactly like
/// any other principal with no memberships: `ctx.authorize(scope)` below, which
/// [`Role::grants`] guarantees denies every scope for an empty role set (`403`), same
/// as a real principal with no grants. Local/dev convenience for tenant-scoped routes
/// now requires a real credential — e.g. `WOVYR_PLATFORM_ADMINS=<principal>` plus that
/// principal's own header — the same path a real deployment already uses; nothing
/// tenant-scoped is reachable "for free" via anonymity alone anymore.
pub(crate) fn tenant_authorize(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
) -> std::result::Result<String, ApiError> {
    let ctx = tenant_context(state, headers);
    ctx.authorize(scope)?;
    Ok(ctx.tenant)
}

/// The acting principal from `X-Wovyr-Principal` (or a bearer token); empty if anonymous.
/// Used to attribute audit records to an actor.
pub(crate) fn principal(headers: &HeaderMap) -> String {
    header(headers, "x-wovyr-principal")
        .or_else(|| header(headers, "authorization").and_then(|a| a.strip_prefix("Bearer ")))
        .unwrap_or("")
        .to_string()
}

/// The in-scope project for a run, from the `X-Wovyr-Project` request header.
pub(crate) fn run_project(headers: &HeaderMap) -> Option<String> {
    header(headers, "x-wovyr-project").map(str::to_string)
}

/// The in-scope tenant for a run, from `X-Wovyr-Tenant` (defaults to `default`).
pub(crate) fn run_tenant(headers: &HeaderMap) -> String {
    header(headers, "x-wovyr-tenant")
        .unwrap_or(DEFAULT_TENANT)
        .to_string()
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
                    .header("x-wovyr-tenant", "acme")
                    .header("x-wovyr-principal", principal)
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

    async fn state() -> Arc<AppState> {
        // `for_test()` (not `from_env()`) already gives this module the in-memory
        // idempotency cache its fixed-`Idempotency-Key` test needs, plus the
        // non-accumulating agent store — see `AppState::for_test`'s own doc comment.
        Arc::new(
            AppState::for_test()
                .await
                .with_tenancy(Arc::new(wovyr_tenancy::InMemoryTenancyStore::new())),
        )
    }

    #[tokio::test]
    async fn rbac_gates_the_tenancy_lifecycle() {
        // SAFETY: single-threaded test; bootstrap a platform admin via env.
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
        let st = state().await;

        // A non-admin principal cannot create an org (default-deny → 403).
        let (s, _) = req(
            &st,
            "POST",
            "/api/v1/organizations",
            "nobody",
            json!({"name":"Platform"}),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // The platform admin can.
        let (s, org) = req(
            &st,
            "POST",
            "/api/v1/organizations",
            "root",
            json!({"name":"Platform"}),
        )
        .await;
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
        let (s, _) = req(
            &st,
            "GET",
            &format!("/api/v1/projects/{prj_id}"),
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = req(
            &st,
            "DELETE",
            &format!("/api/v1/projects/{prj_id}"),
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // A stranger can't even read it.
        let (s, _) = req(
            &st,
            "GET",
            &format!("/api/v1/projects/{prj_id}"),
            "mallory",
            Value::Null,
        )
        .await;
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
        let (s, q) = req(
            &st,
            "GET",
            &format!("/api/v1/projects/{prj_id}/quota"),
            "alice",
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(q["limits"]["concurrent_agent_runs"], 5);
    }

    /// A quota operation on a project that doesn't exist must 404, not succeed.
    ///
    /// Both handlers used to write/read the quota map by raw id with no existence
    /// check, so a typo'd (or empty, via a doubled slash) project id returned `200`
    /// for a budget that no run would ever consult — an operator would believe a
    /// spend limit was in force when nothing was enforcing it.
    #[tokio::test]
    async fn quota_on_a_nonexistent_project_is_not_found() {
        let st = state().await;

        let (s, _) = req(
            &st,
            "PATCH",
            "/api/v1/projects/prj-does-not-exist/quota",
            "root",
            json!({"llm_cost_per_day_usd": 9.0}),
        )
        .await;
        assert_eq!(
            s,
            StatusCode::NOT_FOUND,
            "setting a quota on a ghost project must 404"
        );

        let (s, _) = req(
            &st,
            "GET",
            "/api/v1/projects/prj-does-not-exist/quota",
            "root",
            Value::Null,
        )
        .await;
        assert_eq!(
            s,
            StatusCode::NOT_FOUND,
            "reading a ghost project's quota must 404"
        );

        // And the rejected write must not have persisted anything.
        assert!(
            st.tenancy
                .get_quota("prj-does-not-exist")
                .unwrap()
                .is_none(),
            "a 404'd quota write must leave no stored limits behind"
        );
    }

    #[tokio::test]
    async fn quota_tracker_enforces_concurrency() {
        let st = state().await;
        st.tenancy
            .set_quota(
                "prj-x",
                QuotaLimits {
                    concurrent_agent_runs: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();

        let permit = admit_run(&st.tenancy, &st.quota, Some("prj-x"))
            .await
            .expect("first run admitted");
        // A second concurrent run is rejected.
        assert!(matches!(
            admit_run(&st.tenancy, &st.quota, Some("prj-x")).await,
            Err(e) if e.status == StatusCode::TOO_MANY_REQUESTS
        ));
        // Releasing the slot lets the next run in.
        drop(permit);
        assert!(
            admit_run(&st.tenancy, &st.quota, Some("prj-x"))
                .await
                .is_ok()
        );

        // A project with no quota, and a run with no project, are unmetered.
        assert!(
            admit_run(&st.tenancy, &st.quota, Some("prj-none"))
                .await
                .is_ok()
        );
        assert!(admit_run(&st.tenancy, &st.quota, None).await.is_ok());
    }

    /// RM-AIM-P1 PRV-101 acceptance: a real, price-book-computed `cost_usd` (the exact
    /// value `OpenAiProvider` now stamps onto `output.usage.cost_usd`, which
    /// `wovyr-runtime` feeds to `record_run_usage`) advances a project's daily accumulator
    /// by that amount — not the old hardcoded $0 that silently disabled quota enforcement.
    /// Token usage accumulates alongside it (RM-AIM-P2 SRV-202).
    #[test]
    fn priced_run_cost_advances_the_daily_accumulator() {
        use wovyr_common::Usage;
        use wovyr_provider::PriceBook;

        // The same table `OpenAiProvider::from_env` uses; price a known call.
        let book = PriceBook::with_defaults();
        // gpt-4o-mini: $0.15/1M in, $0.60/1M out — 1000 in + 500 out.
        let usage = Usage::new(1000, 500, 0.0);
        let cost = book.cost("gpt-4o-mini", &usage);
        let expected = (1000.0 * 0.15 + 500.0 * 0.60) / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-12, "cost {cost}");
        assert!(cost > 0.0, "a priced call must not be free");

        // Record it against a project, twice, exactly as a two-run day would.
        let tenancy: Arc<dyn TenancyStore> =
            Arc::new(wovyr_tenancy::InMemoryTenancyStore::default());
        let quota = Arc::new(QuotaTracker::new(None));
        record_run_usage(&tenancy, &quota, Some("prj-priced"), cost, 1500);
        record_run_usage(&tenancy, &quota, Some("prj-priced"), cost, 1500);

        let (spent, tokens) = quota.used_today("prj-priced").unwrap();
        assert!(
            (spent - 2.0 * cost).abs() < 1e-12,
            "the accumulator must advance by the computed cost each run, got {spent}"
        );
        assert_eq!(tokens, 3000, "token usage accumulates alongside cost");
    }

    /// RM-AIM-P2 SRV-202 acceptance: once a project's recorded token usage crosses
    /// `llm_tokens_per_day`, the next run is refused at admission — even with no
    /// cost limit set (a $0-per-token local model still burns capacity).
    #[tokio::test]
    async fn token_budget_blocks_admission_at_threshold() {
        let st = wovyr_tenancy::InMemoryTenancyStore::default();
        st.set_quota(
            "prj-tokens",
            QuotaLimits {
                llm_tokens_per_day: Some(1_000),
                ..Default::default()
            },
        )
        .unwrap();
        let tenancy: Arc<dyn TenancyStore> = Arc::new(st);
        let quota = Arc::new(QuotaTracker::new(None));

        // Under budget: admitted (999 < 1000).
        record_run_usage(&tenancy, &quota, Some("prj-tokens"), 0.0, 999);
        assert!(
            admit_run(&tenancy, &quota, Some("prj-tokens"))
                .await
                .is_ok(),
            "under the token budget the run is admitted"
        );

        // Crossed budget (1001 > 1000): refused with the quota error — same
        // observe-then-enforce boundary as the cost budget (a run is admitted
        // while usage is within the limit; the *next* run after crossing is
        // blocked). Unmetered projects stay unaffected.
        record_run_usage(&tenancy, &quota, Some("prj-tokens"), 0.0, 2);
        let err = match admit_run(&tenancy, &quota, Some("prj-tokens")).await {
            Err(e) => e,
            Ok(_) => panic!("an exhausted token budget must refuse admission"),
        };
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            err.message.contains("llm_tokens_per_day"),
            "{}",
            err.message
        );
        assert!(admit_run(&tenancy, &quota, Some("prj-other")).await.is_ok());
    }

    /// RM-AIM-P2 SRV-203, the boundary math: a quota with a non-UTC offset resets
    /// at *its* local midnight, not UTC's. Pure — `day_bucket` is exactly what
    /// `admit_run`/`record_run_usage` call with the wall clock plugged in.
    #[test]
    fn day_bucket_flips_at_the_configured_local_midnight() {
        let utc_midnight_day_20000 = 86_400u64 * 20_000;

        // IST (+5:30, offset 330): local midnight falls 19 800 s *before* UTC
        // midnight — so at 18:30 UTC the IST day has already flipped.
        let ist_midnight = utc_midnight_day_20000 + 86_400 - 19_800; // 00:00 IST, day 20001
        assert_eq!(day_bucket(ist_midnight - 1, 330), 20_000, "23:59:59 IST");
        assert_eq!(
            day_bucket(ist_midnight, 330),
            20_001,
            "flips exactly at 00:00 IST"
        );
        assert_eq!(
            day_bucket(ist_midnight, 0),
            20_000,
            "the UTC bucket hasn't flipped yet — the boundaries genuinely differ"
        );

        // EST (−5:00, offset −300): local midnight falls 18 000 s *after* UTC
        // midnight.
        let est_midnight = utc_midnight_day_20000 + 18_000;
        assert_eq!(day_bucket(est_midnight - 1, -300), 19_999, "23:59:59 EST");
        assert_eq!(
            day_bucket(est_midnight, -300),
            20_000,
            "flips exactly at 00:00 EST"
        );

        // No offset = the original UTC days-since-epoch.
        assert_eq!(day_bucket(utc_midnight_day_20000, 0), 20_000);
        // A garbage stored offset is clamped to ±24 h, never skewing further.
        assert_eq!(
            day_bucket(utc_midnight_day_20000, i32::MAX),
            day_bucket(utc_midnight_day_20000, 1_440)
        );
    }

    /// SRV-203, the wiring: with a configured reset offset, `record_run_usage`
    /// and `admit_run` agree on the (non-UTC) day bucket — recorded usage counts
    /// against the budget, and it landed under the offset bucket, not UTC's.
    #[tokio::test]
    async fn offset_quota_records_and_enforces_under_its_own_day_bucket() {
        // Pick whichever ±12 h offset puts "now" in a *different* day bucket
        // than UTC — guaranteed for one of the two at any time of day.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let offset = if now % 86_400 < 43_200 { -720 } else { 720 };
        assert_ne!(
            day_bucket(now, offset),
            day_bucket(now, 0),
            "the chosen offset must produce a different current day than UTC"
        );

        let st = wovyr_tenancy::InMemoryTenancyStore::default();
        st.set_quota(
            "prj-tz",
            QuotaLimits {
                llm_tokens_per_day: Some(100),
                day_reset_offset_minutes: Some(offset),
                ..Default::default()
            },
        )
        .unwrap();
        let tenancy: Arc<dyn TenancyStore> = Arc::new(st);
        let quota = Arc::new(QuotaTracker::new(None));

        // Usage recorded for this project lands under *its* day bucket…
        record_run_usage(&tenancy, &quota, Some("prj-tz"), 0.0, 101);
        assert!(
            quota.used_today("prj-tz").is_none(),
            "nothing recorded under the UTC bucket"
        );
        assert_eq!(
            quota.used_on_day("prj-tz", day_bucket(now, offset)),
            Some((0.0, 101)),
            "usage landed under the offset day bucket"
        );

        // …and admission reads the same bucket, so the crossed budget blocks —
        // if admit had used the UTC bucket it would have seen zero usage.
        assert!(
            admit_run(&tenancy, &quota, Some("prj-tz")).await.is_err(),
            "the exhausted budget under the offset bucket must refuse admission"
        );
    }

    /// SRV-202's persistence-compat guarantee: a `quota.json` written by a
    /// pre-token binary (`[day, usd]` entries) still loads — spend is preserved
    /// and tokens default to 0 — instead of being treated as corrupt and silently
    /// resetting every project to a fresh budget.
    #[test]
    fn pre_token_quota_file_loads_with_spend_preserved() {
        let dir =
            std::env::temp_dir().join(format!("wovyr_server_quota_compat_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quota.json");
        let day = current_day();
        std::fs::write(&path, format!(r#"{{"prj-legacy": [{day}, 7.25]}}"#)).unwrap();

        let quota = QuotaTracker::new(Some(path));
        let (spent, tokens) = quota
            .used_today("prj-legacy")
            .expect("the legacy entry must load");
        assert_eq!(spent, 7.25, "pre-upgrade spend must be preserved");
        assert_eq!(tokens, 0, "tokens default to zero for a legacy entry");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RM-GA-P2 DUR-404 acceptance: a daily-cost accumulator survives a restart within
    /// the same UTC day. A fresh `QuotaTracker` opened against the same path (the same
    /// "simulated restart" stand-in used throughout this workspace's crash-recovery
    /// tests) picks up right where the recorded spend left off — a crash-loop must not
    /// silently reset a project back to a $0 budget.
    #[test]
    fn quota_accumulator_survives_a_restart_within_the_same_day() {
        let dir =
            std::env::temp_dir().join(format!("wovyr_server_quota_restart_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("quota.json");

        {
            let tenancy: Arc<dyn TenancyStore> =
                Arc::new(wovyr_tenancy::InMemoryTenancyStore::default());
            let quota = Arc::new(QuotaTracker::new(Some(path.clone())));
            record_run_usage(&tenancy, &quota, Some("prj-restart"), 3.5, 100);
            record_run_usage(&tenancy, &quota, Some("prj-restart"), 1.5, 200);
        }

        // A fresh tracker — no in-memory state carried over — reopened against the
        // same path, the same shape a server restart takes. The prior $5.00 spend is
        // still enforced: a $4.00/day limit now rejects a run that would exceed it.
        let reopened = QuotaTracker::new(Some(path));
        let (spent, tokens) = reopened.used_today("prj-restart").unwrap();
        assert_eq!(
            tokens, 300,
            "token usage survives the restart too (SRV-202)"
        );
        assert_eq!(
            spent, 5.0,
            "the prior $3.50 + $1.50 spend must round-trip exactly"
        );
        let limits = QuotaLimits {
            llm_cost_per_day_usd: Some(4.0),
            ..Default::default()
        };
        assert!(
            limits.check_llm_cost(spent, 0.0).is_err(),
            "the $5.00 spent before the simulated restart must still count against today's budget"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_endpoint_returns_429_when_quota_exceeded() {
        let st = state().await;
        // A restrictive quota (no concurrent runs allowed) blocks every metered run.
        st.tenancy
            .set_quota(
                "prj-block",
                QuotaLimits {
                    concurrent_agent_runs: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();

        let manifest = "metadata:\n  name: q\nspec:\n  instructions: Hi.\n";
        let body = json!({ "manifest": manifest, "input": {"message": "hi"} });

        // With the project header → quota enforced → 429.
        let resp = crate::router(st.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .header("x-wovyr-project", "prj-block")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // Without the project header → unmetered → runs normally (200).
        let resp = crate::router(st.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents:run")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// RM-GA-P4 OBS-804: org/project mutations are audited, by resource id, with the
    /// acting principal + tenant attributed.
    #[tokio::test]
    async fn org_and_project_mutations_are_audited() {
        use wovyr_audit::{AuditFilter, AuditLog};

        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
        let mut st = AppState::for_test()
            .await
            .with_tenancy(Arc::new(wovyr_tenancy::InMemoryTenancyStore::new()));
        st.idempotency = crate::hardening::IdempotencyStore::default();
        let st = Arc::new(st.with_audit(AuditLog::in_memory()));

        let (s, org) = req(
            &st,
            "POST",
            "/api/v1/organizations",
            "root",
            json!({"name":"AuditCo"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let org_id = org["id"].as_str().unwrap().to_string();

        let (s, prj) = req(
            &st,
            "POST",
            "/api/v1/projects",
            "root",
            json!({"name":"p","organization": org_id}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let prj_id = prj["id"].as_str().unwrap().to_string();

        let entries = st
            .audit
            .query(&AuditFilter {
                tenant: Some("acme".to_string()),
                ..Default::default()
            })
            .unwrap();
        let actions: Vec<&str> = entries.iter().map(|e| e.event.action.as_str()).collect();
        assert!(
            actions.contains(&"organization.create"),
            "actions: {actions:?}"
        );
        assert!(actions.contains(&"project.create"), "actions: {actions:?}");
        let org_entry = entries
            .iter()
            .find(|e| e.event.action == "organization.create")
            .unwrap();
        assert_eq!(org_entry.event.actor.principal, "root");
        assert_eq!(org_entry.event.resource.id, org_id);
        let prj_entry = entries
            .iter()
            .find(|e| e.event.action == "project.create")
            .unwrap();
        assert_eq!(prj_entry.event.resource.id, prj_id);
    }

    #[tokio::test]
    async fn duplicate_org_is_conflict() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
        let st = state().await;
        let (s, _) = req(
            &st,
            "POST",
            "/api/v1/organizations",
            "root",
            json!({"name":"Dup"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, _) = req(
            &st,
            "POST",
            "/api/v1/organizations",
            "root",
            json!({"name":"Dup"}),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    /// RM-GA-P4 API-703: `Idempotency-Key` replay now covers every mutating route, not
    /// just `agents:run` — including this one, which has no per-handler idempotency
    /// code of its own. A retry with the same key must replay the original response
    /// rather than hit the store's real duplicate-name conflict; a different key is a
    /// genuinely new request and still conflicts, proving this isn't just dedup the
    /// store would have done anyway.
    #[tokio::test]
    async fn idempotency_key_replays_across_a_route_that_would_otherwise_conflict() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
        let st = state().await;

        async fn create(st: &Arc<AppState>, key: &str) -> (StatusCode, Value) {
            let resp = crate::router(st.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/organizations")
                        .header("content-type", "application/json")
                        .header("x-wovyr-tenant", "acme")
                        .header("x-wovyr-principal", "root")
                        .header("idempotency-key", key)
                        .body(axum::body::Body::from(json!({"name":"Idem"}).to_string()))
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

        let (s1, b1) = create(&st, "k1").await;
        assert_eq!(s1, StatusCode::CREATED);

        let (s2, b2) = create(&st, "k1").await;
        assert_eq!(s2, StatusCode::CREATED);
        assert_eq!(
            b2["id"], b1["id"],
            "same key must replay the original response"
        );

        let (s3, _) = create(&st, "k2").await;
        assert_eq!(
            s3,
            StatusCode::CONFLICT,
            "a different key is a real new request and hits the genuine conflict"
        );
    }

    #[tokio::test]
    async fn project_updates_use_etag_optimistic_concurrency() {
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
        let st = state().await;

        // Issue a request with optional extra headers, returning (status, ETag, body).
        async fn send(
            st: &Arc<AppState>,
            method: &str,
            uri: &str,
            extra: &[(&str, &str)],
            body: Value,
        ) -> (StatusCode, Option<String>, Value) {
            let mut builder = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-wovyr-tenant", "acme")
                .header("x-wovyr-principal", "root");
            for (k, v) in extra {
                builder = builder.header(*k, *v);
            }
            let resp = crate::router(st.clone())
                .oneshot(
                    builder
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status();
            let etag = resp
                .headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
            (
                status,
                etag,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        let (_, org) = req(
            &st,
            "POST",
            "/api/v1/organizations",
            "root",
            json!({"name":"Org"}),
        )
        .await;
        let org_id = org["id"].as_str().unwrap().to_string();
        let (s, etag, prj) = send(
            &st,
            "POST",
            "/api/v1/projects",
            &[],
            json!({"name":"p","organization":org_id}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        assert_eq!(etag.as_deref(), Some("\"1\""));
        assert_eq!(prj["version"], 1);
        let prj_id = prj["id"].as_str().unwrap().to_string();
        let uri = format!("/api/v1/projects/{prj_id}");

        // A read returns the current ETag.
        let (s, etag, _) = send(&st, "GET", &uri, &[], Value::Null).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(etag.as_deref(), Some("\"1\""));

        // A stale If-Match loses → 409 conflict.
        let (s, _, body) = send(
            &st,
            "PATCH",
            &uri,
            &[("if-match", "\"0\"")],
            json!({"status":"suspended"}),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "conflict");

        // The matching If-Match succeeds and bumps the version (new ETag "2").
        let (s, etag, prj) = send(
            &st,
            "PATCH",
            &uri,
            &[("if-match", "\"1\"")],
            json!({"status":"suspended"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(etag.as_deref(), Some("\"2\""));
        assert_eq!(prj["version"], 2);
        assert_eq!(prj["status"], "suspended");

        // Re-using the now-stale version fails again.
        let (s, _, _) = send(
            &st,
            "PATCH",
            &uri,
            &[("if-match", "\"1\"")],
            json!({"status":"active"}),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);

        // Without If-Match, the update is unconditional (no lost-update protection).
        let (s, etag, _) = send(&st, "PATCH", &uri, &[], json!({"status":"active"})).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(etag.as_deref(), Some("\"3\""));
    }

    /// The v0.3 exit criterion: **teams operate self-serve via the dashboard with
    /// enforced quotas.** This drives the exact HTTP routes the dashboard's
    /// `settings.service.ts` calls — org → project → member → quota → agent run —
    /// end to end, proving a team bootstraps, self-serves, and is held to the quota
    /// it sets (the flow verified live against `wovyr dev`).
    #[tokio::test]
    async fn teams_self_serve_the_full_lifecycle_with_enforced_quotas() {
        // SAFETY: single-threaded test; bootstrap the operator as a platform admin.
        unsafe { std::env::set_var("WOVYR_PLATFORM_ADMINS", "root") };
        let st = state().await;

        // A request as `who`, optionally naming a project (for run quota metering).
        async fn as_user(
            st: &Arc<AppState>,
            method: &str,
            uri: &str,
            who: &str,
            project: Option<&str>,
            body: Value,
        ) -> (StatusCode, Value) {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-wovyr-tenant", "acme")
                .header("x-wovyr-principal", who);
            if let Some(p) = project {
                b = b.header("x-wovyr-project", p);
            }
            let resp = crate::router(st.clone())
                .oneshot(b.body(axum::body::Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        // 1. Operator bootstraps the org, then a project under it.
        let (s, org) = as_user(
            &st,
            "POST",
            "/api/v1/organizations",
            "root",
            None,
            json!({"name":"Acme"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let org_id = org["id"].as_str().unwrap().to_string();
        let (s, prj) = as_user(
            &st,
            "POST",
            "/api/v1/projects",
            "root",
            None,
            json!({"name":"Prod","organization":org_id}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let prj_id = prj["id"].as_str().unwrap().to_string();

        // 2. Operator grants alice org_admin — she now self-serves the tenant.
        let (s, _) = as_user(
            &st,
            "POST",
            &format!("/api/v1/projects/{prj_id}/members"),
            "root",
            None,
            json!({"user":"alice","role":"org_admin","scope":{"organization":org_id}}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);

        // 3. Self-serve is authorized, not open: a non-member cannot set the quota.
        let quota_uri = format!("/api/v1/projects/{prj_id}/quota");
        let (s, _) = as_user(
            &st,
            "PATCH",
            &quota_uri,
            "mallory",
            None,
            json!({"concurrent_agent_runs":1}),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "non-member must not set quota");

        // 4. Alice self-serves a quota of one concurrent run.
        let (s, q) = as_user(
            &st,
            "PATCH",
            &quota_uri,
            "alice",
            None,
            json!({"concurrent_agent_runs":1}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(q["limits"]["concurrent_agent_runs"], 1);

        // 5. A metered run under the project is admitted.
        let run = json!({"manifest":"metadata:\n  name: hello\nspec:\n  instructions: hi\n","input":{"message":"hi"}});
        let (s, _) = as_user(
            &st,
            "POST",
            "/api/v1/agents:run",
            "alice",
            Some(&prj_id),
            run.clone(),
        )
        .await;
        assert_eq!(s, StatusCode::OK);

        // 6. Alice tightens the quota to zero, then the next run is refused — the team
        // is held to the limit it set itself.
        let (s, _) = as_user(
            &st,
            "PATCH",
            &quota_uri,
            "alice",
            None,
            json!({"concurrent_agent_runs":0}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, err) = as_user(
            &st,
            "POST",
            "/api/v1/agents:run",
            "alice",
            Some(&prj_id),
            run,
        )
        .await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err["error"]["code"], "quota_exceeded");
    }
}

/// Live integration tests for Redis-shared concurrency slots (RM-AIM-P3 SRV-307) —
/// capability-gated exactly like [`crate::rate_limit`]'s `redis_tests`: read
/// `WOVYR_REDIS_URL`, skip cleanly (a `skipping:` line CI's service-container job
/// fails on) when unset or unreachable, so the suite still passes offline.
///
/// ```bash
/// WOVYR_REDIS_URL=redis://127.0.0.1:6379 \
///   cargo test -p wovyr-server --features redis --lib tenancy::redis_tests -- --nocapture
/// ```
#[cfg(all(test, feature = "redis"))]
mod redis_tests {
    use super::{QuotaTracker, admit_run};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use wovyr_tenancy::{InMemoryTenancyStore, QuotaLimits, TenancyStore};

    /// A unique key prefix per run so repeated runs (and parallel tests) don't collide.
    fn prefix(name: &str) -> String {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("wovyr:it:quota:{name}:{nonce}")
    }

    /// Open a Redis client, or `None` (logging a skip) when unconfigured/unreachable.
    async fn client() -> Option<redis::Client> {
        let url = match std::env::var("WOVYR_REDIS_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("skipping: WOVYR_REDIS_URL not set");
                return None;
            }
        };
        let client = match redis::Client::open(url.as_str()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping: invalid WOVYR_REDIS_URL {url}: {e}");
                return None;
            }
        };
        match client.get_multiplexed_async_connection().await {
            Ok(_) => Some(client),
            Err(e) => {
                eprintln!("skipping: redis unreachable at {url}: {e}");
                None
            }
        }
    }

    fn tenancy_with_quota(project: &str, limits: QuotaLimits) -> Arc<dyn TenancyStore> {
        let store = InMemoryTenancyStore::default();
        store.set_quota(project, limits).unwrap();
        Arc::new(store)
    }

    /// The ticket's literal acceptance criterion: two `QuotaTracker`s over one shared
    /// Redis prefix — standing in for two server nodes in a fleet — enforce a
    /// **combined** concurrency budget, not 2×.
    #[tokio::test]
    async fn two_nodes_share_one_concurrency_budget() {
        let Some(client) = client().await else { return };
        let p = prefix("combined");
        let tenancy = tenancy_with_quota(
            "prj-fleet",
            QuotaLimits {
                concurrent_agent_runs: Some(2),
                ..Default::default()
            },
        );

        // Two independently-constructed trackers over the same Redis prefix = two
        // server nodes admitting against the same project.
        let node_a = Arc::new(QuotaTracker::new(None).with_redis(client.clone(), p.clone()));
        let node_b = Arc::new(QuotaTracker::new(None).with_redis(client.clone(), p.clone()));

        // Alternating across nodes, exactly `concurrent_agent_runs` (2) permits are
        // admitted…
        let permit_a = admit_run(&tenancy, &node_a, Some("prj-fleet"))
            .await
            .expect("1/2 via node A");
        let _permit_b = admit_run(&tenancy, &node_b, Some("prj-fleet"))
            .await
            .expect("2/2 via node B");

        // …and the 3rd is rejected on *both* nodes: one shared budget, not 2×.
        let Err(err) = admit_run(&tenancy, &node_a, Some("prj-fleet")).await else {
            panic!("node A must see the combined budget exhausted");
        };
        assert_eq!(err.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert!(
            admit_run(&tenancy, &node_b, Some("prj-fleet"))
                .await
                .is_err(),
            "node B must see the combined budget exhausted too"
        );

        // Releasing a slot on node A frees capacity node B can immediately use —
        // proving the release path is shared too, not just the admit path.
        drop(permit_a);
        // The release is a spawned fire-and-forget task (`RunPermit::drop`); give it
        // a moment to actually run before asserting the freed slot is visible.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        admit_run(&tenancy, &node_b, Some("prj-fleet"))
            .await
            .expect("the slot node A released is visible to node B");
    }

    /// Two projects (two different Redis keys under the same prefix) don't share a
    /// budget with each other.
    #[tokio::test]
    async fn shared_concurrency_is_still_per_project() {
        let Some(client) = client().await else { return };
        let p = prefix("per-project");
        let tenancy = tenancy_with_quota(
            "prj-one",
            QuotaLimits {
                concurrent_agent_runs: Some(1),
                ..Default::default()
            },
        );
        tenancy
            .set_quota(
                "prj-two",
                QuotaLimits {
                    concurrent_agent_runs: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        let quota = Arc::new(QuotaTracker::new(None).with_redis(client, p));

        let _permit_one = admit_run(&tenancy, &quota, Some("prj-one"))
            .await
            .expect("prj-one's own slot is free");
        // prj-one is now exhausted…
        assert!(
            admit_run(&tenancy, &quota, Some("prj-one")).await.is_err(),
            "prj-one is exhausted"
        );
        // …but prj-two's counter is untouched.
        admit_run(&tenancy, &quota, Some("prj-two"))
            .await
            .expect("prj-two has its own independent budget");
    }

    /// SRV-307's degrade contract, testable offline: a Redis-configured tracker whose
    /// Redis is unreachable falls back to the in-process counter — per-node limiting,
    /// never unlimited. Unlike the two tests above, this doesn't need a live Redis.
    #[tokio::test]
    async fn unreachable_redis_degrades_to_local_concurrency_limiting_not_unlimited() {
        // A local TCP listener nothing ever accepts on: the connection attempt hangs
        // rather than being refused immediately, so this reliably exercises the
        // `REDIS_BUDGET` timeout path (a `connection refused` port, as `rate_limit`'s
        // equivalent test uses, was found to behave inconsistently across platforms).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = redis::Client::open(format!("redis://{addr}")).unwrap();
        let tenancy = tenancy_with_quota(
            "prj-degrade",
            QuotaLimits {
                concurrent_agent_runs: Some(1),
                ..Default::default()
            },
        );
        let quota =
            Arc::new(QuotaTracker::new(None).with_redis(client, "wovyr:quota:test-degrade"));

        let permit = admit_run(&tenancy, &quota, Some("prj-degrade"))
            .await
            .expect("degraded first slot");
        assert!(
            admit_run(&tenancy, &quota, Some("prj-degrade"))
                .await
                .is_err(),
            "the local fallback counter still enforces the budget"
        );
        drop(permit);
        admit_run(&tenancy, &quota, Some("prj-degrade"))
            .await
            .expect("releasing the local slot frees it for the next run");
    }
}
