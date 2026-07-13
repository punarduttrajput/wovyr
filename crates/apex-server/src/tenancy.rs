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
    MemberScope, Membership, Organization, Project, ProjectStatus, QuotaLimits, Role, TenancyStore,
    TenantContext,
};
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

/// The platform-admin principals from `APEX_PLATFORM_ADMINS` (comma-separated).
fn is_platform_admin(principal: &str) -> bool {
    !principal.is_empty()
        && std::env::var("APEX_PLATFORM_ADMINS")
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
    let tenant = header(headers, "x-apex-tenant")
        .unwrap_or(DEFAULT_TENANT)
        .to_string();
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
        for m in state
            .tenancy
            .memberships_for_user(&principal)
            .unwrap_or_default()
        {
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

async fn create_project(
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

async fn get_project(
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

async fn patch_project(
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

async fn delete_project(
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
    let members = state.tenancy.list_memberships(&MemberScope::Project(id))?;
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

async fn remove_member(
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
    /// `(day, usd)` LLM spend for the current rolling day, by project id.
    cost: BTreeMap<String, (u64, f64)>,
}

/// Tracks per-project quota usage for the run path ([Projects API §5](../../docs/09-api/projects.md#5-quotas)).
///
/// The daily-cost accumulator persists (RM-GA-P2 DUR-404) when opened with a path —
/// without this, a crash-loop reset every project's spend to $0, silently bypassing
/// its daily budget. `concurrent` (in-flight runs) deliberately does **not** persist:
/// a restart means nothing is actually still running, so carrying over a stale count
/// would incorrectly throttle the runs that follow.
pub(crate) struct QuotaTracker {
    usage: Mutex<QuotaUsage>,
    path: Option<PathBuf>,
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
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            usage: Mutex::new(QuotaUsage {
                concurrent: BTreeMap::new(),
                cost,
            }),
            path,
        }
    }

    /// The project's LLM spend recorded for the current rolling day, if any —
    /// the read half of [`record_run_cost`] (RM-AIM-P2 RUN-202: lets tests
    /// observe that a sub-agent run's cost actually landed in the accumulator).
    /// Test-only until a route needs it (e.g. a future usage endpoint).
    #[cfg(test)]
    pub(crate) fn spent_today(&self, project: &str) -> Option<f64> {
        let usage = self.usage.lock().expect("quota mutex poisoned");
        usage
            .cost
            .get(project)
            .filter(|(day, _)| *day == current_day())
            .map(|(_, usd)| *usd)
    }

    /// Persist the daily-cost accumulator (best-effort — logged, not propagated,
    /// since the in-memory update this follows has already succeeded either way).
    fn persist(&self, cost: &BTreeMap<String, (u64, f64)>) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(error = %e, "failed to create quota accumulator directory");
            return;
        }
        match serde_json::to_vec_pretty(cost) {
            Ok(bytes) => {
                if let Err(e) = apex_common::fs::atomic_write(path, bytes) {
                    tracing::error!(error = %e, "failed to persist quota accumulator");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to encode quota accumulator"),
        }
    }
}

/// The current rolling-day bucket (UTC days since the epoch). Wall-clock is read only
/// here, at the server boundary — never in core engine logic.
fn current_day() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
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
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        if !self.metered {
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
pub(crate) fn admit_run(
    tenancy: &Arc<dyn TenancyStore>,
    quota: &Arc<QuotaTracker>,
    project: Option<&str>,
) -> Result<RunPermit, ApiError> {
    let unmetered = |project: String| RunPermit {
        quota: quota.clone(),
        project,
        metered: false,
    };
    let Some(project) = project else {
        return Ok(unmetered(String::new()));
    };
    let Some(limits) = tenancy.get_quota(project)? else {
        return Ok(unmetered(project.to_string()));
    };

    let day = current_day();
    let mut u = quota.usage.lock().expect("quota mutex poisoned");
    let current = u.concurrent.get(project).copied().unwrap_or(0);
    limits.check_concurrent_runs(current)?;
    let spent = match u.cost.get(project) {
        Some((d, c)) if *d == day => *c,
        _ => 0.0,
    };
    // Admit while the day's spend is still within budget; the run's own cost is
    // recorded afterwards, so the *next* run is blocked once the limit is crossed.
    limits.check_llm_cost(spent, 0.0)?;
    *u.concurrent.entry(project.to_string()).or_insert(0) += 1;
    Ok(RunPermit {
        quota: quota.clone(),
        project: project.to_string(),
        metered: true,
    })
}

/// Record `cost` USD of LLM spend against `project`'s current-day budget (after a
/// run), persisting the accumulator (RM-GA-P2 DUR-404) so it survives a restart
/// within the same UTC day.
pub(crate) fn record_run_cost(quota: &Arc<QuotaTracker>, project: Option<&str>, cost: f64) {
    let Some(project) = project else {
        return;
    };
    let day = current_day();
    let snapshot = {
        let mut u = quota.usage.lock().expect("quota mutex poisoned");
        let entry = u.cost.entry(project.to_string()).or_insert((day, 0.0));
        if entry.0 != day {
            *entry = (day, 0.0);
        }
        entry.1 += cost;
        u.cost.clone()
    };
    quota.persist(&snapshot);
}

/// Resolve the request's effective roles **narrowed to the asserted tenant**: an org
/// membership counts only if its org belongs to `X-Apex-Tenant`, and a project
/// membership only if the project's org does. This is the isolation primitive for
/// resources owned by a *tenant* (rather than a specific project) — it stops a principal
/// from authorizing against a tenant it merely *names* in the header but holds no
/// membership in. (Platform admins are unconditionally in-scope.)
pub(crate) fn tenant_context(state: &AppState, headers: &HeaderMap) -> TenantContext {
    let tenant = header(headers, "x-apex-tenant")
        .unwrap_or(DEFAULT_TENANT)
        .to_string();
    let principal = header(headers, "x-apex-principal")
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
/// (→ `403`), which requires real membership — so `X-Apex-Tenant` cannot be spoofed.
///
/// **No anonymous-default-tenant RBAC bypass** (RM-GA-P4/GA-003, narrowing SEC-102):
/// this used to short-circuit `Ok(ctx.tenant)` for an anonymous caller against the
/// `default` tenant whenever `state.anonymous_allowed` — i.e. `APEX_ALLOW_ANONYMOUS=1`
/// granted such a caller *every* scope, including `kms:admin` (crypto-shredding), with
/// zero RBAC check at all. That was SEC-102's own literal design (a documented,
/// intentional "local/dev convenience," not an oversight — see
/// [compliance-mapping.md §7](../../docs/13-security/compliance-mapping.md#7-residual-risk-and-gaps)),
/// but GA-003 scoped it as a residual finding to close: `anonymous_allowed` now governs
/// **only** whether [`auth::authenticate`]'s `disabled-loopback` mode lets an
/// unauthenticated request *reach* a handler at all (`APEX_ALLOW_ANONYMOUS=1`, refused
/// by [`crate::serve`] on any non-loopback bind) — it no longer implies any particular
/// *authorization* outcome once it does. An anonymous caller is authorized exactly like
/// any other principal with no memberships: `ctx.authorize(scope)` below, which
/// [`Role::grants`] guarantees denies every scope for an empty role set (`403`), same
/// as a real principal with no grants. Local/dev convenience for tenant-scoped routes
/// now requires a real credential — e.g. `APEX_PLATFORM_ADMINS=<principal>` plus that
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

/// The acting principal from `X-Apex-Principal` (or a bearer token); empty if anonymous.
/// Used to attribute audit records to an actor.
pub(crate) fn principal(headers: &HeaderMap) -> String {
    header(headers, "x-apex-principal")
        .or_else(|| header(headers, "authorization").and_then(|a| a.strip_prefix("Bearer ")))
        .unwrap_or("")
        .to_string()
}

/// The in-scope project for a run, from the `X-Apex-Project` request header.
pub(crate) fn run_project(headers: &HeaderMap) -> Option<String> {
    header(headers, "x-apex-project").map(str::to_string)
}

/// The in-scope tenant for a run, from `X-Apex-Tenant` (defaults to `default`).
pub(crate) fn run_tenant(headers: &HeaderMap) -> String {
    header(headers, "x-apex-tenant")
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
                    .header("x-apex-tenant", "acme")
                    .header("x-apex-principal", principal)
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
        let mut st = AppState::from_env()
            .await
            .with_tenancy(Arc::new(apex_tenancy::InMemoryTenancyStore::new()));
        // `AppState::from_env()` otherwise points the idempotency cache at the real
        // `~/.apex/server/idempotency.json` (DUR-404) shared by every process on this
        // machine — including a previous `cargo test` run. A fixed key like this
        // module's `idempotency_key_replays_...` test uses would then replay a
        // *prior run's* cached response instead of exercising the fresh in-memory
        // tenancy store this helper just built, exactly as `duplicate_org_is_conflict`
        // and friends already assume a clean slate. Swap in a purely in-memory store.
        st.idempotency = crate::hardening::IdempotencyStore::default();
        Arc::new(st)
    }

    #[tokio::test]
    async fn rbac_gates_the_tenancy_lifecycle() {
        // SAFETY: single-threaded test; bootstrap a platform admin via env.
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
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

        let permit = admit_run(&st.tenancy, &st.quota, Some("prj-x")).expect("first run admitted");
        // A second concurrent run is rejected.
        assert!(matches!(
            admit_run(&st.tenancy, &st.quota, Some("prj-x")),
            Err(e) if e.status == StatusCode::TOO_MANY_REQUESTS
        ));
        // Releasing the slot lets the next run in.
        drop(permit);
        assert!(admit_run(&st.tenancy, &st.quota, Some("prj-x")).is_ok());

        // A project with no quota, and a run with no project, are unmetered.
        assert!(admit_run(&st.tenancy, &st.quota, Some("prj-none")).is_ok());
        assert!(admit_run(&st.tenancy, &st.quota, None).is_ok());
    }

    /// RM-AIM-P1 PRV-101 acceptance: a real, price-book-computed `cost_usd` (the exact
    /// value `OpenAiProvider` now stamps onto `output.usage.cost_usd`, which
    /// `apex-runtime` feeds to `record_run_cost`) advances a project's daily accumulator
    /// by that amount — not the old hardcoded $0 that silently disabled quota enforcement.
    #[test]
    fn priced_run_cost_advances_the_daily_accumulator() {
        use apex_common::Usage;
        use apex_provider::PriceBook;

        // The same table `OpenAiProvider::from_env` uses; price a known call.
        let book = PriceBook::with_defaults();
        // gpt-4o-mini: $0.15/1M in, $0.60/1M out — 1000 in + 500 out.
        let usage = Usage::new(1000, 500, 0.0);
        let cost = book.cost("gpt-4o-mini", &usage);
        let expected = (1000.0 * 0.15 + 500.0 * 0.60) / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-12, "cost {cost}");
        assert!(cost > 0.0, "a priced call must not be free");

        // Record it against a project, twice, exactly as a two-run day would.
        let quota = Arc::new(QuotaTracker::new(None));
        record_run_cost(&quota, Some("prj-priced"), cost);
        record_run_cost(&quota, Some("prj-priced"), cost);

        let spent = quota.usage.lock().unwrap().cost["prj-priced"].1;
        assert!(
            (spent - 2.0 * cost).abs() < 1e-12,
            "the accumulator must advance by the computed cost each run, got {spent}"
        );
    }

    /// RM-GA-P2 DUR-404 acceptance: a daily-cost accumulator survives a restart within
    /// the same UTC day. A fresh `QuotaTracker` opened against the same path (the same
    /// "simulated restart" stand-in used throughout this workspace's crash-recovery
    /// tests) picks up right where the recorded spend left off — a crash-loop must not
    /// silently reset a project back to a $0 budget.
    #[test]
    fn quota_accumulator_survives_a_restart_within_the_same_day() {
        let dir =
            std::env::temp_dir().join(format!("apex_server_quota_restart_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("quota.json");

        {
            let quota = Arc::new(QuotaTracker::new(Some(path.clone())));
            record_run_cost(&quota, Some("prj-restart"), 3.5);
            record_run_cost(&quota, Some("prj-restart"), 1.5);
        }

        // A fresh tracker — no in-memory state carried over — reopened against the
        // same path, the same shape a server restart takes. The prior $5.00 spend is
        // still enforced: a $4.00/day limit now rejects a run that would exceed it.
        let reopened = QuotaTracker::new(Some(path));
        let spent = reopened.usage.lock().unwrap().cost["prj-restart"].1;
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
                    .header("x-apex-project", "prj-block")
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
        use apex_audit::{AuditFilter, AuditLog};

        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
        let mut st = AppState::from_env()
            .await
            .with_tenancy(Arc::new(apex_tenancy::InMemoryTenancyStore::new()));
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
                principal: None,
                action: None,
                limit: None,
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
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
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
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
        let st = state().await;

        async fn create(st: &Arc<AppState>, key: &str) -> (StatusCode, Value) {
            let resp = crate::router(st.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/organizations")
                        .header("content-type", "application/json")
                        .header("x-apex-tenant", "acme")
                        .header("x-apex-principal", "root")
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
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
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
                .header("x-apex-tenant", "acme")
                .header("x-apex-principal", "root");
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
    /// it sets (the flow verified live against `apex dev`).
    #[tokio::test]
    async fn teams_self_serve_the_full_lifecycle_with_enforced_quotas() {
        // SAFETY: single-threaded test; bootstrap the operator as a platform admin.
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };
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
                .header("x-apex-tenant", "acme")
                .header("x-apex-principal", who);
            if let Some(p) = project {
                b = b.header("x-apex-project", p);
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
