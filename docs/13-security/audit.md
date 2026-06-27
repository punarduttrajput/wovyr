<!--
File: docs/13-security/audit.md
Document ID: SEC-006
-->

# Security: Audit Logging

**Document ID:** SEC-006  
**File Path:** `docs/13-security/audit.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Security Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the platform's **audit logging** — the tamper-evident record of who did what, when, and with what outcome. Audit underpins security investigations, compliance, and accountability.

---

# 2. What Is Audited

| Domain | Examples |
|--------|----------|
| Authentication | login, token refresh, key use, failures |
| Authorization | allow/deny decisions with reason |
| Identity/admin | user/role/team/key changes |
| Resources | agent/workflow/plugin create, publish, delete |
| Execution | agent runs, workflow executions, tool invocations |
| Secrets | reads, rotations, revocations (by reference) |
| Data | sensitive memory access, exports |

Every sensitive action across subsystems emits an audit record — e.g.
[Tool Runtime](../07-tool-runtime/security-isolation.md#11-audit),
[Plugin permissions](../08-plugin-sdk/permissions.md#10-audit),
[API authz](../09-api/authentication.md#12-audit).

---

# 3. Record Schema

```json
{
  "id": "aud_01H...",
  "timestamp": "2026-06-27T10:00:00Z",
  "actor": { "principal": "user_01H...", "type": "user", "tenant": "acme" },
  "action": "workflow.execution.cancel",
  "resource": { "type": "execution", "id": "exe_01H..." },
  "outcome": "allowed",
  "reason": null,
  "context": { "ip": "203.0.113.4", "request_id": "req_01H...", "trace_id": "trace_01H..." }
}
```

Records are structured, correlation-linked (`request_id`/`trace_id`), and
PII/secret-masked (values referenced, not stored).

---

# 4. Integrity (Tamper-Evidence)

- Audit records are **append-only**.
- Records are hash-chained (each entry includes the prior entry's hash) so deletion
  or modification is detectable.
- Optionally exported to a write-once store (WORM) or external SIEM for independent
  retention.

---

# 5. Pipeline

```text
Service emits audit event
   │  Event Bus (audit.* topics)
   ▼
Audit collector ──► durable audit store (append-only)
                ──► optional SIEM / WORM export
```

Audit events flow over the
[Event Bus](../02-architecture/event-driven-architecture.md) and are persisted
independently of operational logs.

---

# 6. Access & Search

- The [Settings audit view](../10-dashboard/settings.md#8-audit--compliance) and API
  expose searchable audit, scoped to the caller's authority.
- Filter by principal, action, resource, time, outcome.
- Export is itself audited.

---

# 7. Retention

| Class | Default retention |
|-------|-------------------|
| Security/auth events | Long (compliance-driven) |
| Resource changes | Long |
| Execution audit | Configurable per tenant |

Retention is policy-driven and may be extended for regulated tenants.

---

# 8. Separation from Observability

Audit is **distinct** from operational [logging/metrics/tracing](../14-observability/index.md)
(planned): audit is security-grade, integrity-protected, and retained for
compliance, whereas observability is for operations and may be sampled/short-lived.

---

# 9. Alerting on Audit

High-signal audit events drive alerts: repeated authz denials, secret access
anomalies, admin actions outside change windows, mass exports
([runbooks](../07-tool-runtime/observability-ops.md#9-runbooks)).

---

# 10. Dependencies

- [`02-architecture/event-driven-architecture.md`](../02-architecture/event-driven-architecture.md)
- [`13-security/authorization.md`](authorization.md)
- [`13-security/secret-management.md`](secret-management.md)

---

# 11. Related Documents

- [`10-dashboard/settings.md`](../10-dashboard/settings.md)
- [`14-observability`](../SUMMARY.md) *(planned)*

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Audit Logging specification |
