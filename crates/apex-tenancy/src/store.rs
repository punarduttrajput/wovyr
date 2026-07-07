//! The tenancy catalog: a durable store of organizations, projects, memberships, and
//! quotas. The control-plane state behind the [Projects API](../../docs/09-api/projects.md).
//!
//! [`InMemoryTenancyStore`] (tests/single-process) and [`FileTenancyStore`] (a single
//! `tenancy.json`) share their CRUD logic via [`TenancyState`]. Operations are
//! fail-closed: a duplicate name is [`Error::Conflict`](apex_common::Error::Conflict),
//! and updating/deleting an absent resource is
//! [`Error::NotFound`](apex_common::Error::NotFound).

use crate::model::{MemberScope, Membership, Organization, Project, ProjectStatus, QuotaLimits};
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The full tenancy catalog, serialized as one document by [`FileTenancyStore`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TenancyState {
    /// Organizations by id.
    pub orgs: BTreeMap<String, Organization>,
    /// Projects by id.
    pub projects: BTreeMap<String, Project>,
    /// Role assignments.
    pub memberships: Vec<Membership>,
    /// Quota limits keyed by target id (an org or project id).
    pub quotas: BTreeMap<String, QuotaLimits>,
}

impl TenancyState {
    fn create_org(&mut self, org: Organization) -> Result<Organization> {
        if self.orgs.contains_key(&org.id) {
            return Err(Error::conflict(format!(
                "organization `{}` already exists",
                org.name
            )));
        }
        self.orgs.insert(org.id.clone(), org.clone());
        Ok(org)
    }

    fn create_project(&mut self, project: Project) -> Result<Project> {
        if !self.orgs.contains_key(&project.organization) {
            return Err(Error::NotFound(format!(
                "organization `{}` does not exist",
                project.organization
            )));
        }
        if self.projects.contains_key(&project.id) {
            return Err(Error::conflict(format!(
                "project `{}` already exists",
                project.name
            )));
        }
        self.projects.insert(project.id.clone(), project.clone());
        Ok(project)
    }

    fn update_project(&mut self, project: Project) -> Result<()> {
        if !self.projects.contains_key(&project.id) {
            return Err(Error::NotFound(format!(
                "project `{}` not found",
                project.id
            )));
        }
        self.projects.insert(project.id.clone(), project);
        Ok(())
    }

    fn delete_project(&mut self, id: &str) -> Result<()> {
        let project = self
            .projects
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("project `{id}` not found")))?;
        project.status = ProjectStatus::Deleted;
        Ok(())
    }

    fn add_membership(&mut self, m: Membership) -> Result<()> {
        // Replace any existing assignment for the same (user, scope) — role upsert.
        self.memberships
            .retain(|x| !(x.user == m.user && x.scope == m.scope));
        self.memberships.push(m);
        Ok(())
    }

    fn remove_membership(&mut self, user: &str, scope: &MemberScope) -> Result<()> {
        let before = self.memberships.len();
        self.memberships
            .retain(|x| !(x.user == user && &x.scope == scope));
        if self.memberships.len() == before {
            return Err(Error::NotFound(format!(
                "no membership for user `{user}` in the given scope"
            )));
        }
        Ok(())
    }
}

/// A durable catalog of tenancy resources.
pub trait TenancyStore: Send + Sync {
    /// Create an organization (conflict if its id already exists).
    fn create_org(&self, org: Organization) -> Result<Organization>;
    /// Look up an organization by id.
    fn get_org(&self, id: &str) -> Result<Option<Organization>>;
    /// All organizations in `tenant`, sorted by id.
    fn list_orgs(&self, tenant: &str) -> Result<Vec<Organization>>;

    /// Create a project (org must exist; conflict if its id already exists).
    fn create_project(&self, project: Project) -> Result<Project>;
    /// Look up a project by id.
    fn get_project(&self, id: &str) -> Result<Option<Project>>;
    /// All projects in `tenant`, sorted by id.
    fn list_projects(&self, tenant: &str) -> Result<Vec<Project>>;
    /// Replace an existing project (not found if absent) — settings/status updates.
    fn update_project(&self, project: Project) -> Result<()>;
    /// Soft-delete a project (status → `deleted`).
    fn delete_project(&self, id: &str) -> Result<()>;

    /// Assign a role to a user in a scope (upserts an existing assignment).
    fn add_membership(&self, membership: Membership) -> Result<()>;
    /// Remove a user's assignment in a scope (not found if absent).
    fn remove_membership(&self, user: &str, scope: &MemberScope) -> Result<()>;
    /// All role assignments for `user`.
    fn memberships_for_user(&self, user: &str) -> Result<Vec<Membership>>;
    /// All role assignments in a given scope (e.g. a project's members).
    fn list_memberships(&self, scope: &MemberScope) -> Result<Vec<Membership>>;

    /// Set the quota limits for a target (org or project id).
    fn set_quota(&self, target: &str, limits: QuotaLimits) -> Result<()>;
    /// The quota limits for a target, if any.
    fn get_quota(&self, target: &str) -> Result<Option<QuotaLimits>>;
}

/// In-process tenancy store (tests / single process).
#[derive(Default)]
pub struct InMemoryTenancyStore {
    state: Mutex<TenancyState>,
}

impl InMemoryTenancyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TenancyState> {
        self.state.lock().expect("tenancy state mutex poisoned")
    }
}

impl TenancyStore for InMemoryTenancyStore {
    fn create_org(&self, org: Organization) -> Result<Organization> {
        self.lock().create_org(org)
    }
    fn get_org(&self, id: &str) -> Result<Option<Organization>> {
        Ok(self.lock().orgs.get(id).cloned())
    }
    fn list_orgs(&self, tenant: &str) -> Result<Vec<Organization>> {
        Ok(self
            .lock()
            .orgs
            .values()
            .filter(|o| o.tenant == tenant)
            .cloned()
            .collect())
    }
    fn create_project(&self, project: Project) -> Result<Project> {
        self.lock().create_project(project)
    }
    fn get_project(&self, id: &str) -> Result<Option<Project>> {
        Ok(self.lock().projects.get(id).cloned())
    }
    fn list_projects(&self, tenant: &str) -> Result<Vec<Project>> {
        Ok(self
            .lock()
            .projects
            .values()
            .filter(|p| p.tenant == tenant)
            .cloned()
            .collect())
    }
    fn update_project(&self, project: Project) -> Result<()> {
        self.lock().update_project(project)
    }
    fn delete_project(&self, id: &str) -> Result<()> {
        self.lock().delete_project(id)
    }
    fn add_membership(&self, membership: Membership) -> Result<()> {
        self.lock().add_membership(membership)
    }
    fn remove_membership(&self, user: &str, scope: &MemberScope) -> Result<()> {
        self.lock().remove_membership(user, scope)
    }
    fn memberships_for_user(&self, user: &str) -> Result<Vec<Membership>> {
        Ok(self
            .lock()
            .memberships
            .iter()
            .filter(|m| m.user == user)
            .cloned()
            .collect())
    }
    fn list_memberships(&self, scope: &MemberScope) -> Result<Vec<Membership>> {
        Ok(self
            .lock()
            .memberships
            .iter()
            .filter(|m| &m.scope == scope)
            .cloned()
            .collect())
    }
    fn set_quota(&self, target: &str, limits: QuotaLimits) -> Result<()> {
        self.lock().quotas.insert(target.to_string(), limits);
        Ok(())
    }
    fn get_quota(&self, target: &str) -> Result<Option<QuotaLimits>> {
        Ok(self.lock().quotas.get(target).cloned())
    }
}

/// A durable tenancy store backed by a single `tenancy.json` document under a directory.
/// Reads/writes the whole state per operation (control-plane scale). Mutations are
/// serialized by a process-local lock **and** a cross-process advisory file lock
/// (RM-GA-P2 DUR-403), since the CLI and server share this directory by design.
pub struct FileTenancyStore {
    dir: PathBuf,
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileTenancyStore {
    /// Open (or create) a store under `dir`, holding `dir/tenancy.json`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("tenancy.json"),
            dir,
            lock: Mutex::new(()),
        })
    }

    fn load(&self) -> Result<TenancyState> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::invalid(format!("corrupt tenancy store: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TenancyState::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn save(&self, state: &TenancyState) -> Result<()> {
        apex_common::fs::atomic_write(&self.path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    /// Run `f` against the loaded state under both locks, persisting on success. The
    /// cross-process lock spans load→mutate→save so a concurrent writer (this or
    /// another process) can't silently lose the other's update.
    fn with_mut<T>(&self, f: impl FnOnce(&mut TenancyState) -> Result<T>) -> Result<T> {
        let _guard = self.lock.lock().expect("tenancy file lock poisoned");
        let _flock = apex_common::fs::FileLock::acquire(&self.dir)
            .map_err(|e| Error::config(format!("lock tenancy store: {e}")))?;
        let mut state = self.load()?;
        let out = f(&mut state)?;
        self.save(&state)?;
        Ok(out)
    }

    fn with_ref<T>(&self, f: impl FnOnce(&TenancyState) -> T) -> Result<T> {
        let _guard = self.lock.lock().expect("tenancy file lock poisoned");
        Ok(f(&self.load()?))
    }
}

impl TenancyStore for FileTenancyStore {
    fn create_org(&self, org: Organization) -> Result<Organization> {
        self.with_mut(|s| s.create_org(org))
    }
    fn get_org(&self, id: &str) -> Result<Option<Organization>> {
        self.with_ref(|s| s.orgs.get(id).cloned())
    }
    fn list_orgs(&self, tenant: &str) -> Result<Vec<Organization>> {
        self.with_ref(|s| {
            s.orgs
                .values()
                .filter(|o| o.tenant == tenant)
                .cloned()
                .collect()
        })
    }
    fn create_project(&self, project: Project) -> Result<Project> {
        self.with_mut(|s| s.create_project(project))
    }
    fn get_project(&self, id: &str) -> Result<Option<Project>> {
        self.with_ref(|s| s.projects.get(id).cloned())
    }
    fn list_projects(&self, tenant: &str) -> Result<Vec<Project>> {
        self.with_ref(|s| {
            s.projects
                .values()
                .filter(|p| p.tenant == tenant)
                .cloned()
                .collect()
        })
    }
    fn update_project(&self, project: Project) -> Result<()> {
        self.with_mut(|s| s.update_project(project))
    }
    fn delete_project(&self, id: &str) -> Result<()> {
        self.with_mut(|s| s.delete_project(id))
    }
    fn add_membership(&self, membership: Membership) -> Result<()> {
        self.with_mut(|s| s.add_membership(membership))
    }
    fn remove_membership(&self, user: &str, scope: &MemberScope) -> Result<()> {
        self.with_mut(|s| s.remove_membership(user, scope))
    }
    fn memberships_for_user(&self, user: &str) -> Result<Vec<Membership>> {
        self.with_ref(|s| {
            s.memberships
                .iter()
                .filter(|m| m.user == user)
                .cloned()
                .collect()
        })
    }
    fn list_memberships(&self, scope: &MemberScope) -> Result<Vec<Membership>> {
        self.with_ref(|s| {
            s.memberships
                .iter()
                .filter(|m| &m.scope == scope)
                .cloned()
                .collect()
        })
    }
    fn set_quota(&self, target: &str, limits: QuotaLimits) -> Result<()> {
        self.with_mut(|s| {
            s.quotas.insert(target.to_string(), limits);
            Ok(())
        })
    }
    fn get_quota(&self, target: &str) -> Result<Option<QuotaLimits>> {
        self.with_ref(|s| s.quotas.get(target).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbac::Role;

    fn seeded(store: &dyn TenancyStore) -> (Organization, Project) {
        let org = store
            .create_org(Organization::new("acme", "Platform"))
            .unwrap();
        let prj = store
            .create_project(Project::new(&org, "support-bot"))
            .unwrap();
        (org, prj)
    }

    fn exercise(store: &dyn TenancyStore) {
        let (org, prj) = seeded(store);

        // Conflict on duplicate.
        assert!(matches!(
            store.create_org(Organization::new("acme", "Platform")),
            Err(Error::Conflict(_))
        ));
        // Project under a missing org → not found.
        let orphan = Project {
            organization: "org-acme-missing".into(),
            ..Project::new(&org, "x")
        };
        assert!(matches!(
            store.create_project(orphan),
            Err(Error::NotFound(_))
        ));

        // Listing by tenant.
        assert_eq!(store.list_orgs("acme").unwrap().len(), 1);
        assert_eq!(store.list_projects("acme").unwrap()[0].id, prj.id);

        // Membership upsert + removal.
        let scope = MemberScope::Project(prj.id.clone());
        store
            .add_membership(Membership {
                user: "u1".into(),
                role: Role::Viewer,
                scope: scope.clone(),
            })
            .unwrap();
        store
            .add_membership(Membership {
                user: "u1".into(),
                role: Role::Editor, // upsert: replaces Viewer
                scope: scope.clone(),
            })
            .unwrap();
        let ms = store.memberships_for_user("u1").unwrap();
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].role, Role::Editor);
        store.remove_membership("u1", &scope).unwrap();
        assert!(store.memberships_for_user("u1").unwrap().is_empty());
        assert!(store.remove_membership("u1", &scope).is_err());

        // Quota set/get.
        store
            .set_quota(
                &prj.id,
                QuotaLimits {
                    concurrent_agent_runs: Some(5),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store
                .get_quota(&prj.id)
                .unwrap()
                .unwrap()
                .concurrent_agent_runs,
            Some(5)
        );

        // Soft delete.
        store.delete_project(&prj.id).unwrap();
        assert_eq!(
            store.get_project(&prj.id).unwrap().unwrap().status,
            ProjectStatus::Deleted
        );
        assert!(store.delete_project("prj-nope").is_err());
    }

    #[test]
    fn in_memory_store_round_trips() {
        exercise(&InMemoryTenancyStore::new());
    }

    #[test]
    fn file_store_round_trips_and_persists() {
        let dir = std::env::temp_dir().join(format!("apex_tenancy_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileTenancyStore::new(&dir).unwrap();
        exercise(&store);

        // A fresh handle over the same dir sees the persisted state.
        let reopened = FileTenancyStore::new(&dir).unwrap();
        assert_eq!(reopened.list_orgs("acme").unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
