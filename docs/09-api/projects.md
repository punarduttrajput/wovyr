<!--
File: docs/09-api/projects.md
Document ID: API-008
-->

# Projects API

**Document ID:** API-008  
**File Path:** `docs/09-api/projects.md`  
**Version:** 1.1.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-14

---

# 1. Purpose

This document defines the API for managing the platform's **tenancy hierarchy** — organizations, projects, and their settings, quotas, and members. These resources scope every other resource in the platform.

All endpoints inherit the [API conventions](overview.md) and require
[authentication](authentication.md).

---

# 2. Tenancy Model

```text
Tenant (billing/isolation boundary)
  └── Organization
        └── Project
              └── resources (agents, workflows, memory, plugins, …)
```

| Resource | Description |
|----------|-------------|
| `organization` | A company/group within a tenant |
| `project` | A workspace owning resources and config |
| `membership` | A user's role within an org/project |
| `quota` | Resource/cost limits for an org or project |

Every resource in the platform carries `tenant` + `project` scoping (see
[Overview §5](overview.md#5-standard-resource-envelope)); isolation is enforced
everywhere (e.g. [Memory](../06-memory-engine/storage-architecture.md#10-tenant-isolation),
[Tool Runtime](../07-tool-runtime/security-isolation.md#8-tenant-isolation)).

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| GET | `/api/v1/organizations` | `projects:read` |
| POST | `/api/v1/organizations` | `org.admin` |
| GET | `/api/v1/projects` | `projects:read` |
| POST | `/api/v1/projects` | `projects:admin` |
| GET | `/api/v1/projects/{id}` | `projects:read` |
| PATCH | `/api/v1/projects/{id}` | `projects:admin` |
| DELETE | `/api/v1/projects/{id}` | `projects:admin` |
| GET | `/api/v1/projects/{id}/members` | `projects:read` |
| POST | `/api/v1/projects/{id}/members` | `projects:admin` |
| DELETE | `/api/v1/projects/{id}/members/{uid}` | `projects:admin` |
| GET | `/api/v1/projects/{id}/quota` | `projects:read` |
| PATCH | `/api/v1/projects/{id}/quota` | `org.admin` |

---

# 4. Project Resource

```json
{
  "id": "prj_01H...",
  "object": "project",
  "name": "support-bot",
  "organization": "org_01H...",
  "tenant": "acme",
  "settings": {
    "default_model_class": "balanced",
    "marketplace_policy": { "require_verified": true }
  },
  "status": "active"
}
```

`settings` carry project-level defaults consumed by other subsystems — e.g. default
[routing class](../05-llm-gateway/routing.md#5-model-classes) and
[marketplace policy](../08-plugin-sdk/marketplace.md#7-governance--curation).

---

# 5. Quotas

Projects and organizations carry quotas enforced across subsystems:

```json
{
  "object": "quota",
  "scope": "project",
  "limits": {
    "llm_cost_per_day_usd": 250,
    "llm_tokens_per_day": 5000000,
    "concurrent_agent_runs": 50
  }
}
```

All three are enforced at the agent-run admission boundary (`X-Apex-Project`):
concurrent runs, the rolling day's USD spend, and — RM-AIM-P2 SRV-202 — the
rolling day's token usage (`llm_tokens_per_day`, the vendor-bill-independent
twin of the cost budget: a local model costs $0/token but still burns
capacity). Breaches return `429`/`402` per the
[error model](overview.md#8-error-model).

Two dimensions this section originally speculated (`tool_executions_per_minute`,
`memory_records`) were **removed in SRV-202 rather than enforced**: they were
declared but checked nowhere — dead config an operator could set and reasonably
believe was protecting them. Tool executions happen inside the agent loop where
no per-project window tracker exists (request-level abuse is the
[rate limiter](../18-roadmap/v1.0/phase1-security-floor-tickets.md)'s job, which
since SRV-202 also has an opt-in **per-tenant tier**,
`APEX_RATE_LIMIT_TENANT_PER_MIN`), and memory records are *tenant*-namespaced
while quotas are *project*-scoped, so "a project's record count" was never
well-defined. A stored quota still carrying the old fields deserializes fine —
they are simply ignored.

---

# 6. Membership & Roles

Members are assigned roles (see [Authentication §8](authentication.md#8-roles))
scoped to an org or project:

```http
POST /api/v1/projects/prj_01H.../members
{ "user": "user_01H...", "role": "editor" }
```

A user's effective permissions are the union of their memberships, intersected with
any API-key scope restriction.

---

# 7. Settings Inheritance

```text
Organization settings  (defaults)
        │  overridden by
Project settings
        │  overridden by
Per-request parameters
```

A project inherits org defaults and may override them; individual requests may
override further within policy-allowed bounds.

---

# 8. Lifecycle

- Deleting a project soft-deletes it and schedules its resources for cleanup
  (memory archived/purged per retention, plugins disabled, runs cancelled).
- Suspending a project blocks new operations while preserving data.

---

# 9. Events

Emits `project.created`, `project.updated`, `project.member.added`,
`quota.updated`, `project.suspended` to the
[Event Bus](../02-architecture/event-driven-architecture.md).

---

# 10. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`forbidden` (admin required), `conflict` (name exists), `quota_exceeded`.

---

# 11. Dependencies

- [`09-api/authentication.md`](authentication.md)
- [`09-api/users.md`](users.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 12. Related Documents

- [`09-api/overview.md`](overview.md)
- [`02-architecture/domain-driven-design.md`](../02-architecture/domain-driven-design.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Projects API specification |
| 1.1.0 | 2026-07-14 | §5: `llm_tokens_per_day` budget added; dead `tool_executions_per_minute`/`memory_records` dimensions removed (RM-AIM-P2 SRV-202) |
