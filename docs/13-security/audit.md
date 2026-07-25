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

- Audit records are **append-only** and `fsync`-durable (file then directory) on
  every append.
- Records are **hash-chained** (each entry commits to the prior entry's hash), so
  deletion or modification of an *interior* record is detectable.

**Keyed MAC + head anchor (SEC-403).** A bare hash chain is only *consistency*
evidence: the hash is public, so an actor who can rewrite the log file can rewrite an
entry **and** recompute every downstream hash, and `verify()` would still pass; it also
cannot detect **tail truncation** (a shortened chain still links cleanly). The
production log therefore:

- Chains entries with a **keyed HMAC-SHA256** whose key is held *outside* the log
  file — `WOVYR_AUDIT_MAC_KEY` (hex, preferred: sourced from escrow before startup) or
  a generate-once `~/.wovyr/audit/audit.key`, via `wovyr_config::audit::build_audit_key`,
  the same sourcing shape as the KMS root key. Without the key an actor with full write
  access cannot recompute the chain after editing a record.
- Persists a monotonic **head anchor** (highest `seq`, its `hash`, and a keyed MAC over
  the pair) to a separate `audit.head` file on every append. `verify()` fails closed if
  the log is shorter than the anchor commits to (truncation) or if the anchor's own MAC
  doesn't validate (a forged anchor).

Key sourcing is **fail-closed** (the SEC-405 stance): a deployment with no durable key
material refuses to start rather than silently running a forgeable, consistency-only
chain. `WOVYR_AUDIT_ALLOW_UNKEYED=1` is the explicit throwaway/test opt-out (runs the
original unkeyed SHA-256 chain, which carries no tamper-resistance claim).

**Caveat.** The generate-once `audit.key` file is only as strong as the filesystem
permissions on the audit directory — an actor who can read that file can forge the
chain, exactly as for the KMS root-key file. The strong path is `WOVYR_AUDIT_MAC_KEY`
sourced from a secrets manager/HSM/escrow, never written beside the log.

- Optionally exported to a write-once store (WORM) or external SIEM for independent
  retention. The `NotarizationHook` interface (published head → external anchor) is the
  landed seam for this compliance tier; a concrete publisher is a follow-on.

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

**Implemented (SEC-301):** `GET /api/v1/audit` filters by principal/action and an
inclusive `[after_ms, before_ms]` epoch-ms time range, cursor-paginated
most-recent-first. Reads go through `AuditSink::query_page`, which `FileAuditSink`
serves via a bounded backward scan of `audit.jsonl` (stops once the page fills)
rather than re-reading the whole log per query; `total_estimate` is always `null`
on this route, since an exact count would require the full scan the paged read
exists to avoid. Resource/outcome filters and export remain future surface.

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
| 1.1.0 | 2026-07-24 | SEC-403: §4 documents the keyed HMAC chain, the durable head anchor (tail-truncation detection), fail-closed key sourcing (`WOVYR_AUDIT_MAC_KEY`/generate-once file, `WOVYR_AUDIT_ALLOW_UNKEYED=1` opt-out), and the `NotarizationHook` external-anchor seam. |
| 1.0.0 | 2026-06-27 | Initial Audit Logging specification |
