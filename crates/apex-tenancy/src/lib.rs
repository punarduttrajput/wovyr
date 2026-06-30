//! Multi-tenancy for the Apex AI Platform — the isolation and access-control
//! foundation ([Projects API](../../docs/09-api/projects.md),
//! [RBAC & ABAC](../../docs/13-security/rbac.md)).
//!
//! The tenancy hierarchy is **Tenant → Organization → Project → resources**; every
//! platform resource is scoped to a tenant + project, and access is gated by roles and
//! quotas. This crate provides that model independently of any subsystem (it depends
//! only on `apex-common`), so the server and engines can thread a [`TenantContext`]
//! through their request paths.
//!
//! - [`model`] — [`Organization`], [`Project`], [`Membership`], and [`QuotaLimits`]
//!   (with deterministic, slugged ids).
//! - [`rbac`] — built-in [`Role`]s, their scope grants, and [`TenantContext`] with a
//!   fail-closed [`authorize`](TenantContext::authorize) (default-deny).
//! - [`quota`] — pure quota-limit checks on [`QuotaLimits`] (caller supplies usage;
//!   windowing/accounting stay with the enforcing subsystem).
//! - [`store`] — the durable [`TenancyStore`] catalog ([`InMemoryTenancyStore`] /
//!   [`FileTenancyStore`]).
//!
//! Server wiring (org/project/membership/quota REST routes, request-context extraction,
//! and per-subsystem quota enforcement) builds on this crate in a later slice.

pub mod model;
pub mod quota;
pub mod rbac;
pub mod store;

pub use model::{
    MemberScope, Membership, Organization, Project, ProjectStatus, QuotaLimits,
};
pub use rbac::{Role, TenantContext, any_grants};
pub use store::{FileTenancyStore, InMemoryTenancyStore, TenancyState, TenancyStore};
