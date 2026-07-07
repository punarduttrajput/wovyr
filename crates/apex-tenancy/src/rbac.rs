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

/// Whether `scope` names `<domain>:<action>` with a non-empty domain — rejecting
/// malformed scopes (`":read"`, `"agents:"`, `""`) fail-closed so a suffix match
/// alone never authorizes.
fn is_well_formed(scope: &str, action: &str) -> bool {
    matches!(scope.strip_suffix(action), Some(domain) if domain.ends_with(':') && domain.len() > 1)
}

/// Whether `scope` is a read scope (`<domain>:read`).
fn is_read(scope: &str) -> bool {
    is_well_formed(scope, "read")
}

/// Whether `scope` is a (non-admin) write/action scope.
fn is_write(scope: &str) -> bool {
    is_well_formed(scope, "write")
        || scope == "workflows:run"
        || scope == "tools:invoke"
        || scope == "agents:run"
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
        assert!(r.grants("agents:run"));
        assert!(!Role::Viewer.grants("agents:run"));
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

    // ── Security suite: the RBAC default-deny matrix ([security-testing §3]). ──
    // These guard the whole authorization surface as a table, not per-scope
    // spot-checks, so a future permission change can't silently widen a role.

    /// Every scope the platform authorizes against, tagged by tier. The matrix
    /// below asserts each role grants exactly the tiers at or below its rank —
    /// the privilege ladder Viewer < Editor < ProjectAdmin < OrgAdmin <
    /// PlatformAdmin — and nothing above it (fail-closed).
    const READ_SCOPES: &[&str] = &[
        "agents:read",
        "workflows:read",
        "memory:read",
        "secrets:read",
        "audit:read",
        "projects:read",
    ];
    const WRITE_SCOPES: &[&str] = &[
        "agents:write",
        "agents:run",
        "workflows:run",
        "memory:write",
        "secrets:write",
        "tools:invoke",
        "kms:write",
    ];
    const PROJECT_ADMIN_SCOPES: &[&str] = &[
        "memory:admin",
        "projects:admin",
        "agents:admin",
        "kms:admin",
        // RM-GA-P1 SEC-103/SEC-104: plugin lifecycle + marketplace moderation are
        // admin-tier scopes, mirroring `kms:admin`'s placement on this ladder.
        "plugins:admin",
        "marketplace:moderate",
    ];
    const ORG_ADMIN_SCOPES: &[&str] = &["org.admin", "users:admin"];
    const PLATFORM_SCOPES: &[&str] = &["platform.admin"];

    /// The highest tier each role may reach (0=read … 4=platform). A role grants a
    /// scope iff the scope's tier ≤ the role's rank.
    fn rank(role: Role) -> usize {
        match role {
            Role::Viewer => 0,
            Role::Editor => 1,
            Role::ProjectAdmin => 2,
            Role::OrgAdmin => 3,
            Role::PlatformAdmin => 4,
        }
    }

    #[test]
    fn rbac_default_deny_matrix_is_a_strict_privilege_ladder() {
        let tiers: [(usize, &[&str]); 5] = [
            (0, READ_SCOPES),
            (1, WRITE_SCOPES),
            (2, PROJECT_ADMIN_SCOPES),
            (3, ORG_ADMIN_SCOPES),
            (4, PLATFORM_SCOPES),
        ];
        let roles = [
            Role::Viewer,
            Role::Editor,
            Role::ProjectAdmin,
            Role::OrgAdmin,
            Role::PlatformAdmin,
        ];
        for role in roles {
            for (tier, scopes) in tiers {
                let expected = tier <= rank(role); // grant iff at/below the role's rank
                for scope in scopes {
                    assert_eq!(
                        role.grants(scope),
                        expected,
                        "{role:?} vs `{scope}` (tier {tier}): expected grant={expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn unknown_and_malformed_scopes_are_denied_for_non_admins() {
        // A scope no tier recognizes must be refused by every non-superuser role —
        // fail-closed, so a typo or a newly-added scope never defaults to allowed.
        for scope in [
            "",
            "agents",
            "agents:",
            ":read",
            "agents:frobnicate",
            "AGENTS:READ",
        ] {
            assert!(!Role::Viewer.grants(scope), "viewer granted `{scope}`");
            assert!(!Role::Editor.grants(scope), "editor granted `{scope}`");
        }
        // Org/platform admins are deliberately broad (everything below their bar),
        // but the platform-admin scope itself stays exclusive to PlatformAdmin.
        assert!(!Role::OrgAdmin.grants("platform.admin"));
        assert!(!Role::ProjectAdmin.grants("org.admin"));
    }

    #[test]
    fn authorize_never_leaks_across_the_admin_boundary() {
        // A project admin acting with a full role union still cannot cross into
        // org/platform administration — the union is a max, not an escalation.
        let ctx = TenantContext {
            tenant: "acme".into(),
            project: Some("prj-x".into()),
            principal: "eve".into(),
            roles: vec![Role::Viewer, Role::Editor, Role::ProjectAdmin],
        };
        assert!(ctx.authorize("projects:admin").is_ok());
        assert!(matches!(
            ctx.authorize("org.admin").unwrap_err(),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            ctx.authorize("platform.admin").unwrap_err(),
            Error::Forbidden(_)
        ));
    }
}
