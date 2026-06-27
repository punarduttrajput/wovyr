<!--
File: docs/08-plugin-sdk/permissions.md
Document ID: PLG-003
-->

# Plugin Permissions

**Document ID:** PLG-003  
**File Path:** `docs/08-plugin-sdk/permissions.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **permission model** for plugins: how a plugin declares what it needs, how those requests are granted, and how grants are enforced at runtime.

The model is least-privilege and declarative: a plugin can only do what its manifest requests **and** an operator/tenant has granted. Anything else is unreachable.

---

# 2. Principles

1. **Declared up front** — permissions live in the manifest; no silent escalation.
2. **Explicitly granted** — install/enable requires a grant decision.
3. **Least privilege** — request the minimum; defaults deny.
4. **Scoped** — grants are bound to tenant, project, and resource specifics.
5. **Revocable** — grants can be withdrawn live, disabling the capability.
6. **Audited** — grants and usage are logged.

---

# 3. Permission Syntax

Permissions are structured strings: `domain:action:resource`.

```text
net:egress:api.github.com
secret:read:github-token
memory:read:project
memory:write:project
tool:invoke:http.request
fs:read:/workspace
provider:register:*
event:publish:plugin.*
```

| Domain | Example actions |
|--------|-----------------|
| `net` | `egress` |
| `fs` | `read`, `write` |
| `secret` | `read` |
| `memory` | `read`, `write` |
| `tool` | `invoke` |
| `provider` | `register` |
| `policy` | `register` |
| `event` | `publish`, `subscribe` |

Wildcards (`*`) are allowed but flagged as broad and require elevated grant
approval.

---

# 4. Capability-Implied Permissions

A capability's *kind* implies a baseline that still must be granted explicitly:

| Capability kind | Typical permissions |
|-----------------|---------------------|
| `tool` | `net:egress:*`, `secret:read:*`, `fs:*` as declared |
| `provider` | `provider:register`, `net:egress:<provider host>` |
| `memory_backend` | `memory:read`, `memory:write` |
| `policy` | `policy:register` |
| `workflow_activity` | `event:publish`, `tool:invoke` as declared |

The Plugin SDK's tests assert a plugin uses **no permission it did not declare**
(see [Plugin API §10](plugin-api.md#10-testing-support)).

---

# 5. Grant Flow

```text
Install/enable a plugin
   │
   ▼
Plugin Engine extracts requested permissions
   │
   ▼
Present to grantor (operator / tenant admin) for consent
   │
   ├── grant all / grant subset / deny
   ▼
Persist grant set (per tenant/project)
   │
   ▼
Capabilities enabled with exactly the granted scope
```

If only a subset is granted, capabilities whose minimum permissions are unmet are
**not enabled** (the rest may still run). The grantor sees which capabilities each
permission unlocks.

---

# 6. Grant Scoping

A grant is bound to a scope so the same plugin can have different access per tenant:

```yaml
grant:
  plugin: acme/github@1.4.0
  tenant: acme
  project: support-bot          # optional narrower scope
  permissions:
    - net:egress:api.github.com
    - secret:read:github-token
  expires_at: 2026-12-31T00:00:00Z   # optional
```

Grants may carry expiry and may be narrowed (e.g. a specific secret ref rather than
`secret:read:*`).

---

# 7. Runtime Enforcement

Declaration and grant are necessary but not sufficient — enforcement happens at
the point of use:

```text
Plugin attempts an action (e.g. egress)
   │
   ▼
Host checks: requested ⊆ declared ⊆ granted
   │
   ▼
Policy Engine evaluates contextual rules (ABAC)
   │
   ├── allow → proceed
   └── deny  → blocked + audited
```

For tool plugins, enforcement is the same default-deny network/filesystem model as
the [Tool Runtime](../07-tool-runtime/security-isolation.md#5-network-isolation).
Contextual rules are evaluated by the
[Policy Engine](../04-agent-framework/policy-engine.md). Enforcement is
**fail-closed**.

---

# 8. Consent UX

- The dashboard presents requested permissions grouped by risk (e.g. network
  egress and secret access are highlighted).
- Broad/wildcard permissions are called out distinctly.
- Operators can pre-approve trusted publishers to streamline grants.
- Community (unreviewed) plugins default to the most restrictive grant set.

---

# 9. Revocation & Changes

- Revoking a grant disables the affected capabilities immediately (no restart).
- A plugin **upgrade** that requests *new* permissions requires a fresh grant; the
  upgrade stages but does not enable new capabilities until granted (see
  [Versioning §7](versioning.md#7-lifecycle-operations)).
- Permission diffs between versions are shown to the grantor.

---

# 10. Audit

Every grant, revocation, and denied attempt is audited:

```json
{
  "event": "plugin.permission.denied",
  "plugin": "acme/github@1.4.0",
  "tenant": "acme",
  "permission": "secret:read:stripe-key",
  "reason": "not_granted",
  "timestamp": "2026-06-27T10:00:00Z"
}
```

Denied attempts are a strong abuse signal and feed alerting.

---

# 11. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md)
- [`08-plugin-sdk/plugin-api.md`](plugin-api.md)

---

# 12. Related Documents

- [`08-plugin-sdk/overview.md`](overview.md)
- [`08-plugin-sdk/sandbox.md`](sandbox.md)
- [`08-plugin-sdk/versioning.md`](versioning.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin Permissions specification |
