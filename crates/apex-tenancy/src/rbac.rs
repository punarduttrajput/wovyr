//! Role-based access control: built-in roles, scopes, and authorization
//! ([Security: RBAC & ABAC](../../docs/13-security/rbac.md)).
//!
//! ```text
//! Principal ──has──► Roles ──grant──► Scopes ──gate──► Operations
//! ```
//!
//! Scopes are `domain:action` strings (e.g. `agents:write`, `workflows:run`,
//! `memory:admin`, `projects:admin`) plus the org/platform admin scopes (`org.admin`,
//! `users:admin`, `platform.admin`). A principal's **effective scopes** are the union
//! over its assigned roles. Authorization is **default-deny**: no granting role ⇒ no
//! access ([rbac §8](../../docs/13-security/rbac.md#8-least-privilege)).

use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// A built-in role ([rbac §4](../../docs/13-security/rbac.md#4-built-in-roles)). Each
/// maps to a set of scope patterns via [`Role::grants`]; custom roles are a later slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read-only across resources (`*:read`).
    Viewer,
    /// Reads + writes + `workflows:run` / `tools:invoke` (no `*:admin`).
    Editor,
    /// Full control within a project (incl. `*:admin`, `projects:admin`).
    ProjectAdmin,
    /// Full control within an organization (project scope + `org.admin`, `users:admin`).
    OrgAdmin,
    /// Full control across the deployment (`platform.admin`).
    PlatformAdmin,
}

impl Role {
    /// Whether this role grants `scope`. Encodes the built-in role → scope mapping;
    /// admin roles subsume lower scopes within their level.
    pub fn grants(&self, scope: &str) -> bool {
        use Role::*;
        match self {
            PlatformAdmin => true,
            // Org admins may do anything except platform-wide administration.
            OrgAdmin => scope != "platform.admin",
            // Project admins: everything within a project — not org/platform/user admin.
            ProjectAdmin => !matches!(scope, "platform.admin" | "org.admin" | "users:admin"),
            // Editors: reads + writes + run/invoke, but no `*:admin` scopes.
            Editor => is_read(scope) || is_write(scope),
            // Viewers: reads only.
            Viewer => is_read(scope),
        }
    }
}

/// Whether `scope` is a read scope (`*:read`).
fn is_read(scope: &str) -> bool {
    scope.ends_with(":read")
}

/// Whether `scope` is a (non-admin) write/action scope.
fn is_write(scope: &str) -> bool {
    scope.ends_with(":write") || scope == "workflows:run" || scope == "tools:invoke"
}

/// Whether any of `roles` grants `scope` (the effective-scope union).
pub fn any_grants(roles: &[Role], scope: &str) -> bool {
    roles.iter().any(|r| r.grants(scope))
}

/// The authenticated request context: the principal, the tenant/project it is acting
/// in, and the roles in effect there. The unit of authorization.
#[derive(Clone, Debug, Default)]
pub struct TenantContext {
    /// The tenant (isolation boundary).
    pub tenant: String,
    /// The project in scope, if the operation is project-scoped.
    pub project: Option<String>,
    /// The acting principal (user id / api-key subject).
    pub principal: String,
    /// The roles the principal holds in this tenant/project (already resolved from
    /// memberships, narrowed to the in-scope org + project).
    pub roles: Vec<Role>,
}

impl TenantContext {
    /// Whether the context's roles grant `scope`.
    pub fn can(&self, scope: &str) -> bool {
        any_grants(&self.roles, scope)
    }

    /// Authorize `scope`, fail-closed: [`Error::Forbidden`] if no role grants it.
    pub fn authorize(&self, scope: &str) -> Result<()> {
        if self.can(scope) {
            Ok(())
        } else {
            Err(Error::forbidden(format!(
                "principal `{}` lacks required scope `{scope}`",
                self.principal
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_reads_only() {
        let r = Role::Viewer;
        assert!(r.grants("agents:read") && r.grants("memory:read"));
        assert!(!r.grants("agents:write") && !r.grants("workflows:run"));
        assert!(!r.grants("projects:admin"));
    }

    #[test]
    fn editor_reads_writes_runs_but_not_admin() {
        let r = Role::Editor;
        assert!(r.grants("agents:read") && r.grants("agents:write"));
        assert!(r.grants("workflows:run") && r.grants("tools:invoke"));
        assert!(!r.grants("memory:admin") && !r.grants("projects:admin"));
        assert!(!r.grants("org.admin"));
    }

    #[test]
    fn admin_roles_subsume_their_level() {
        assert!(Role::ProjectAdmin.grants("memory:admin"));
        assert!(Role::ProjectAdmin.grants("projects:admin"));
        assert!(!Role::ProjectAdmin.grants("org.admin"));
        assert!(!Role::ProjectAdmin.grants("users:admin"));

        assert!(Role::OrgAdmin.grants("org.admin") && Role::OrgAdmin.grants("users:admin"));
        assert!(!Role::OrgAdmin.grants("platform.admin"));

        assert!(Role::PlatformAdmin.grants("platform.admin"));
    }

    #[test]
    fn authorize_is_default_deny_and_unions_roles() {
        let ctx = TenantContext {
            tenant: "acme".into(),
            project: Some("prj-x".into()),
            principal: "user-1".into(),
            roles: vec![Role::Viewer, Role::Editor],
        };
        assert!(ctx.authorize("agents:write").is_ok()); // from Editor
        let err = ctx.authorize("projects:admin").unwrap_err();
        assert!(matches!(err, Error::Forbidden(_)));

        // No roles ⇒ no access.
        let empty = TenantContext::default();
        assert!(empty.authorize("agents:read").is_err());
    }
}
