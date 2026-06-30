//! Tenancy resources: organizations, projects, memberships, and quotas
//! ([Projects API §2](../../docs/09-api/projects.md#2-tenancy-model)).
//!
//! ```text
//! Tenant (billing/isolation boundary)
//!   └── Organization
//!         └── Project
//!               └── resources (agents, workflows, memory, plugins, …)
//! ```
//!
//! Ids are derived deterministically from the tenant + name (`org-<tenant>-<slug>`,
//! `prj-<org-slug>-<slug>`) so the model stays free of ambient randomness
//! ([coding-standards §7](../../docs/19-implementation-guide/coding-standards.md)); a
//! duplicate name within a scope is a [`Error::Conflict`](apex_common::Error::Conflict).

use crate::rbac::Role;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An organization: a company/group within a tenant, owning projects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organization {
    /// Derived id, `org-<tenant>-<slug>`.
    pub id: String,
    /// Human-readable organization name (unique within the tenant).
    pub name: String,
    /// The tenant (billing/isolation boundary) this org belongs to.
    pub tenant: String,
}

impl Organization {
    /// A new organization with a derived id.
    pub fn new(tenant: impl Into<String>, name: impl Into<String>) -> Self {
        let tenant = tenant.into();
        let name = name.into();
        let id = format!("org-{}-{}", slug(&tenant), slug(&name));
        Self { id, name, tenant }
    }
}

/// Lifecycle state of a project ([Projects API §8](../../docs/09-api/projects.md#8-lifecycle)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Operational.
    #[default]
    Active,
    /// New operations blocked; data preserved.
    Suspended,
    /// Soft-deleted; resources scheduled for cleanup.
    Deleted,
}

/// A project: a workspace owning resources and config, within an organization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Derived id, `prj-<org-slug>-<slug>`.
    pub id: String,
    /// Human-readable project name (unique within the organization).
    pub name: String,
    /// Owning organization id.
    pub organization: String,
    /// The tenant this project belongs to (inherited from the org).
    pub tenant: String,
    /// Project-level settings consumed by other subsystems (defaults, policies).
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    /// Lifecycle status.
    #[serde(default)]
    pub status: ProjectStatus,
    /// Monotonic version for optimistic concurrency (`ETag` / `If-Match`,
    /// [overview §5/§10](../../docs/09-api/overview.md#10-concurrency-control)); bumped
    /// on every update.
    #[serde(default = "one")]
    pub version: u64,
}

/// Default version for projects (and for catalogs written before versioning).
fn one() -> u64 {
    1
}

impl Project {
    /// A new active project under `org`, with a derived id, at version 1.
    pub fn new(org: &Organization, name: impl Into<String>) -> Self {
        let name = name.into();
        let id = format!("prj-{}-{}", slug(&org.name), slug(&name));
        Self {
            id,
            name,
            organization: org.id.clone(),
            tenant: org.tenant.clone(),
            settings: BTreeMap::new(),
            status: ProjectStatus::Active,
            version: 1,
        }
    }
}

/// The scope a [`Membership`] (and its role) applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum MemberScope {
    /// Role applies across the whole organization (and all its projects).
    Organization(String),
    /// Role applies within a single project.
    Project(String),
}

/// A user's role assignment within an org or project
/// ([Projects API §6](../../docs/09-api/projects.md#6-membership--roles)).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    /// The user this membership grants a role to.
    pub user: String,
    /// The assigned role.
    pub role: Role,
    /// What the role applies to (an org or a project).
    pub scope: MemberScope,
}

/// Resource/cost limits for an org or project
/// ([Projects API §5](../../docs/09-api/projects.md#5-quotas)). `None` = unlimited.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaLimits {
    /// Max LLM spend per rolling day, USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_cost_per_day_usd: Option<f64>,
    /// Max tool executions per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_executions_per_minute: Option<u64>,
    /// Max stored memory records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_records: Option<u64>,
    /// Max concurrent agent runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent_agent_runs: Option<u64>,
}

/// A normalized lowercase slug (alphanumerics kept, runs of other chars → single `-`).
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_stable_slugged_ids() {
        let org = Organization::new("acme", "Platform Team");
        assert_eq!(org.id, "org-acme-platform-team");
        let prj = Project::new(&org, "Support Bot!");
        assert_eq!(prj.id, "prj-platform-team-support-bot");
        assert_eq!(prj.organization, org.id);
        assert_eq!(prj.tenant, "acme");
        assert_eq!(prj.status, ProjectStatus::Active);
    }

    #[test]
    fn quota_limits_omit_unset_fields() {
        let q = QuotaLimits {
            concurrent_agent_runs: Some(50),
            ..Default::default()
        };
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(json, r#"{"concurrent_agent_runs":50}"#);
    }
}
