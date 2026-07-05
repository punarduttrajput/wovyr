<!--
File: docs/13-security/index.md
Document ID: SEC-INDEX-001
-->

# Security Index

**Document ID:** SEC-INDEX-001  
**File Path:** `docs/13-security/index.md`  
**Version:** 1.1.0  
**Status:** Active  
**Owner:** Security Team  
**Last Updated:** 2026-07-05

---

# 1. Purpose

This document is the **central navigation and architecture index** for security across the Apex AI Platform. It consolidates the security mechanisms specified throughout the platform and defines the cross-cutting model: identity, authorization, encryption, secrets, and audit.

Security is a [core principle](../00-executive/vision.md) — **Secure by Default**.
This section is the canonical reference; subsystem docs implement it.

---

# 2. Security Pillars

| Pillar | Document | Enforced by |
|--------|----------|-------------|
| Authentication | [authentication.md](authentication.md) | API Gateway, IdP |
| Authorization | [authorization.md](authorization.md) | [Policy Engine](../04-agent-framework/policy-engine.md) |
| RBAC / ABAC | [rbac.md](rbac.md) | Policy Engine + API |
| Encryption | [encryption.md](encryption.md) | All services + datastores |
| Secrets | [secret-management.md](secret-management.md) | Secret vault |
| Audit | [audit.md](audit.md) | All services → Event Bus |

---

# 3. Defense in Depth

```text
Network    ── TLS/mTLS, network policies, egress allowlists
   │
Identity   ── OAuth2/OIDC, API keys, mTLS, service identity
   │
Authorization ── RBAC scopes + ABAC (Policy Engine), tenant isolation
   │
Execution  ── sandboxed tools/plugins, least privilege, resource limits
   │
Data       ── encryption at rest/in transit, PII masking, retention
   │
Audit      ── tamper-evident logs of every sensitive action
```

No single layer is trusted alone; each assumes the others may fail.

---

# 4. Where Security Lives

Security is implemented across the platform; this section ties it together:

| Concern | Primary spec |
|---------|--------------|
| Tool/plugin isolation | [Tool Runtime Security](../07-tool-runtime/security-isolation.md), [Plugin Sandbox](../08-plugin-sdk/sandbox.md) |
| Plugin permissions | [Plugin Permissions](../08-plugin-sdk/permissions.md) |
| API auth | [API Authentication](../09-api/authentication.md) |
| Governance rules | [Policy Engine](../04-agent-framework/policy-engine.md) |
| Supply chain | [Plugin Distribution](../08-plugin-sdk/distribution.md) |
| Memory access | [Memory security](../06-memory-engine/overview.md#12-security) |

---

# 5. Document Map

| Document | Responsibility |
|----------|----------------|
| [authentication.md](authentication.md) | Identity and credential verification |
| [authorization.md](authorization.md) | Access-decision model and enforcement |
| [rbac.md](rbac.md) | Roles, scopes, and attribute-based rules |
| [encryption.md](encryption.md) | Data protection in transit and at rest |
| [secret-management.md](secret-management.md) | Secret storage, injection, rotation |
| [audit.md](audit.md) | Audit logging and compliance |
| [compliance-mapping.md](compliance-mapping.md) | Control-by-control framework mapping (currently encryption/key management) |

---

# 6. Threat Model (Summary)

| Threat | Mitigation |
|--------|-----------|
| Credential theft | Short-lived tokens, rotation, mTLS, no plaintext secrets |
| Privilege escalation | Least-privilege RBAC/ABAC, fail-closed authorization |
| Malicious tool/plugin | Sandboxing, default-deny egress, signed packages |
| Cross-tenant access | Hard tenant isolation everywhere |
| Data exfiltration | Egress allowlists, PII masking, audit |
| Supply-chain attack | Signing, provenance/SBOM, revocation |
| Tampering | Encryption, tamper-evident audit |

---

# 7. Compliance Posture

The platform is designed to support common frameworks (SOC 2, ISO 27001, GDPR):
data isolation, encryption, auditability, access control, and retention controls
are first-class. Specific certifications are deployment-dependent.

[`compliance-mapping.md`](compliance-mapping.md) is the first concrete,
control-by-control slice of evidence behind that statement — currently scoped
to the encryption/key-management control family, with file/line citations and
adversarial tests, not yet a full-platform mapping or a third-party attestation.

---

# 8. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`09-api/authentication.md`](../09-api/authentication.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-05 | Added [`compliance-mapping.md`](compliance-mapping.md) to the Document Map (§5) and linked it from §7's Compliance Posture paragraph — the first control-by-control evidence slice (encryption/key management) behind that paragraph's claim |
| 1.0.0 | 2026-06-27 | Initial Security Index |
