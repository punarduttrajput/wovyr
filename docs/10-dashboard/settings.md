<!--
File: docs/10-dashboard/settings.md
Document ID: DASH-007
-->

# Settings & Administration

**Document ID:** DASH-007  
**File Path:** `docs/10-dashboard/settings.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **Settings & Administration** surfaces of the dashboard — managing organizations, projects, users, roles, API keys, quotas, and platform configuration. It is the visual front end over the [Projects API](../09-api/projects.md) and [Users API](../09-api/users.md).

---

# 2. Surfaces

| View | Manages |
|------|---------|
| Profile | The current user ([`/users/me`](../09-api/users.md#10-self-service)) |
| Organizations | Orgs within the tenant |
| Projects | Projects, settings, defaults |
| Members & Roles | Memberships, roles, teams |
| API Keys | Keys for users and service accounts |
| Quotas | Org/project resource & cost limits |
| Integrations | SSO/IdP, secrets, webhooks |

Visibility and editability follow the user's
[RBAC scopes](../09-api/authentication.md#7-scopes).

---

# 3. Tenancy Administration

Manage the [tenancy hierarchy](../09-api/projects.md#2-tenancy-model):

- Create/configure organizations and projects.
- Edit project [settings](../09-api/projects.md#4-project-resource) (default model
  class, marketplace policy, etc.) with clear
  [inheritance](../09-api/projects.md#7-settings-inheritance) indicators.
- Suspend/delete projects (guarded, with cleanup explanation).

---

# 4. Members, Roles & Teams

- Invite users (email/SSO), assign [roles](../09-api/authentication.md#8-roles).
- Create custom roles bounded by the admin's delegable scopes
  ([Users API §7](../09-api/users.md#7-roles--custom-roles)).
- Manage [teams](../09-api/users.md#8-teams) for bulk role assignment.
- Manage [service accounts](../09-api/users.md#5-service-accounts) for automation.

---

# 5. API Keys

Create, view (prefix only), rotate, and revoke
[API keys](../09-api/users.md#6-api-keys):

- The secret is shown **once** on creation, with a copy affordance and a clear
  warning.
- Per-key scope selection (subset of the subject's permissions), optional IP
  allowlist and expiry.
- Usage and last-seen surfaced; instant revocation.

---

# 6. Quotas & Budgets

Set and monitor [quotas](../09-api/projects.md#5-quotas):

- LLM cost/day, tool executions/minute, memory records, concurrent runs.
- Soft-threshold alerts (e.g. 80%) vs. hard limits.
- Current utilization shown against limits (links to
  [Cost Explorer](monitoring.md#5-cost-explorer)).

---

# 7. Integrations

| Integration | Configures |
|-------------|-----------|
| SSO / IdP | OIDC/SAML connection ([auth](../09-api/authentication.md#3-oauth2--oidc)) |
| Secrets | Secret references used by tools/providers (values never displayed) |
| Webhooks | Event subscriptions ([API webhooks](../09-api/overview.md#15-webhooks--events)) |
| Providers | Default provider/model preferences ([routing](../05-llm-gateway/routing.md)) |

---

# 8. Audit & Compliance

- A searchable audit log view across identity, project, plugin, and key events.
- Filter by principal, action, resource, time.
- Export for compliance, scoped to the user's authority.

(Audit content is produced platform-wide; this view consumes it — see the planned
[Security](../SUMMARY.md) section.)

---

# 9. Governance

- Administrative actions require the appropriate admin scope
  (`projects:admin`, `org.admin`, `users:admin`, `platform.admin`).
- Every change is audited and emits events.
- Secrets and tokens are never displayed after creation.

---

# 10. Dependencies

- [`09-api/projects.md`](../09-api/projects.md)
- [`09-api/users.md`](../09-api/users.md)
- [`09-api/authentication.md`](../09-api/authentication.md)

---

# 11. Related Documents

- [`10-dashboard/overview.md`](overview.md)
- [`10-dashboard/monitoring.md`](monitoring.md)
- [`10-dashboard/marketplace.md`](marketplace.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Settings & Administration specification |
