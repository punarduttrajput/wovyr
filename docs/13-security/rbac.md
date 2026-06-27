<!--
File: docs/13-security/rbac.md
Document ID: SEC-003
-->

# Security: RBAC & ABAC

**Document ID:** SEC-003  
**File Path:** `docs/13-security/rbac.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Security Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the platform's **Role-Based Access Control (RBAC)** combined with **Attribute-Based Access Control (ABAC)** — the concrete roles, scopes, and rules that drive [authorization](authorization.md) decisions.

---

# 2. Model

```text
Principal ──has──► Roles ──grant──► Scopes ──gate──► Operations
                                   (RBAC)
                              + ABAC rules (context/attributes)
```

RBAC answers "may this role do this kind of thing?"; ABAC refines with context
(data class, region, time, risk). Both are evaluated by the
[Policy Engine](../04-agent-framework/policy-engine.md).

---

# 3. Scopes

Scopes follow `resource:action`, matching the
[API scopes](../09-api/authentication.md#7-scopes):

```text
agents:read|write|run
workflows:read|write|run|cancel
memory:read|write|admin
tools:read|invoke
plugins:read|admin
projects:admin   users:admin   org.admin   platform.admin
```

A principal's **effective scopes** = union over assigned roles, intersected with
any credential restriction (e.g. an [API key](../09-api/users.md#6-api-keys) scope
subset).

---

# 4. Built-in Roles

| Role | Scope summary |
|------|---------------|
| `viewer` | `*:read` |
| `operator` | reads + `*:run` |
| `editor` | reads + writes |
| `project.admin` | full within a project |
| `org.admin` | full within an organization |
| `platform.admin` | full across the deployment |

Defined once and referenced by the
[API](../09-api/authentication.md#8-roles) and [Settings UI](../10-dashboard/settings.md#4-members-roles--teams).

---

# 5. Custom Roles

Organizations define custom roles bundling specific scopes
([Users API §7](../09-api/users.md#7-roles--custom-roles)):

```yaml
role:
  name: incident-responder
  scopes: [workflows:run, memory:read, tools:invoke]
```

A role's scopes are **bounded by what the creating admin may delegate** — no
privilege escalation by role creation.

---

# 6. Role Assignment

- Direct: a role assigned to a user within an org/project.
- Via [teams](../09-api/users.md#8-teams): assigning a role to a team grants it to
  all members.
- Scoped: assignments are bound to an organization or project.

---

# 7. ABAC Rules

ABAC adds conditions RBAC cannot express:

| Attribute | Example rule |
|-----------|--------------|
| Data classification | `pii:true` requires `pii.access` |
| Region / residency | EU data only accessible from EU |
| Time | Destructive ops only during business hours |
| Risk | Step-up auth for high-risk actions |
| Resource ownership | Edit only own drafts unless admin |

Rules are authored as [policies](../04-agent-framework/policy-engine.md), versioned
and testable.

---

# 8. Least Privilege

- Default deny: no scope ⇒ no access.
- Delegated identities (agents/workflows/plugins) get the **minimum** scopes
  needed, never the operator's full set.
- Broad/wildcard grants are flagged and require elevated approval (e.g.
  [plugin permissions](../08-plugin-sdk/permissions.md#8-consent-ux)).

---

# 9. Separation of Duties

Sensitive workflows can require distinct principals for request vs. approval (e.g.
human-task approvals in workflows), enforceable via ABAC.

---

# 10. Auditing Access

Role and scope changes, and access decisions, are audited
([audit.md](audit.md)). Periodic access reviews are supported by exportable
role/assignment reports.

---

# 11. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`09-api/authentication.md`](../09-api/authentication.md)
- [`13-security/authorization.md`](authorization.md)

---

# 12. Related Documents

- [`09-api/users.md`](../09-api/users.md)
- [`10-dashboard/settings.md`](../10-dashboard/settings.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial RBAC & ABAC specification |
