<!--
File: docs/13-security/authorization.md
Document ID: SEC-002
-->

# Security: Authorization

**Document ID:** SEC-002  
**File Path:** `docs/13-security/authorization.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Security Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the platform **authorization** model — how, once a principal is [authenticated](authentication.md), the platform decides whether an action is permitted.

Authorization is enforced consistently by the
[Policy Engine](../04-agent-framework/policy-engine.md) and applied at every
subsystem boundary.

---

# 2. Decision Pipeline

```text
Authenticated principal + requested action + target resource + context
        │
        ▼
1. RBAC      ── does a role grant the required scope?
        │
        ▼
2. Scoping   ── is the resource within the principal's tenant/project?
        │
        ▼
3. ABAC      ── do attribute rules allow it? (Policy Engine)
        │
        ├── allow → proceed
        └── deny  → reject + audit
```

All three must pass. The model combines coarse RBAC with fine-grained ABAC; see
[rbac.md](rbac.md).

---

# 3. Enforcement Points

Authorization is checked wherever an action occurs — never only at the edge:

| Action | Enforced at |
|--------|-------------|
| API call | API Gateway ([API auth](../09-api/authentication.md#6-authorization-model-rbac--abac)) |
| Tool execution | [Tool Runtime](../07-tool-runtime/security-isolation.md#4-authorization) |
| Plugin capability use | [Plugin Permissions](../08-plugin-sdk/permissions.md#7-runtime-enforcement) |
| Memory access | [Memory scopes](../06-memory-engine/memory-api.md#10-scopes--sharing) |
| Model inference | [LLM Gateway](../05-llm-gateway/overview.md#4-governance) policy checks |

Defense in depth: a request authorized at the edge is still re-checked at the point
of use.

---

# 4. Fail-Closed

Any authorization error — policy evaluation failure, unreachable Policy Engine
(per configured fallback), or ambiguous decision — results in **deny**. The system
never fails open for access decisions.

---

# 5. Tenant Isolation

Authorization always resolves a single tenant; cross-tenant access is structurally
impossible (separate namespaces, per-tenant stores). This is verified by tests as a
hard requirement across
[Memory](../06-memory-engine/storage-architecture.md#10-tenant-isolation),
[Tool Runtime](../07-tool-runtime/security-isolation.md#8-tenant-isolation), and
the API.

---

# 6. Delegation & Impersonation

- Agents and workflows act under a **delegated identity** scoped to no more than the
  initiating principal's permissions (least privilege).
- Service accounts cannot exceed the scopes granted to them.
- Administrative impersonation (support) is gated, time-boxed, and fully audited.

---

# 7. Policy as Code

Authorization rules beyond static RBAC are expressed as
[Policy Engine](../04-agent-framework/policy-engine.md) policies (data
classification, region/residency, time-of-day, risk). Policies are versioned,
testable, and auditable.

---

# 8. Consent & Grants

Some authorizations require explicit, recorded **consent** — notably
[plugin permission grants](../08-plugin-sdk/permissions.md#5-grant-flow). Grants are
scoped, revocable, and audited.

---

# 9. Audit

Every authorization decision (allow/deny, with reason and matched policy) is
auditable per [audit.md](audit.md). Denials are a key abuse/misconfiguration
signal.

---

# 10. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`13-security/rbac.md`](rbac.md)
- [`13-security/authentication.md`](authentication.md)

---

# 11. Related Documents

- [`09-api/authentication.md`](../09-api/authentication.md)
- [`13-security/audit.md`](audit.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Security Authorization specification |
