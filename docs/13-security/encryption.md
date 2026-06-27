<!--
File: docs/13-security/encryption.md
Document ID: SEC-004
-->

# Security: Encryption

**Document ID:** SEC-004  
**File Path:** `docs/13-security/encryption.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Security Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how data is protected **in transit** and **at rest** across the Apex AI Platform, and how encryption keys are managed.

---

# 2. Encryption in Transit

| Path | Protection |
|------|-----------|
| Client → API Gateway | TLS 1.3 |
| Service ↔ service | mTLS |
| Service ↔ datastores | TLS (PostgreSQL, Redis, Qdrant, NATS) |
| Tool/plugin egress | TLS, via controlled proxy ([egress](../07-tool-runtime/security-isolation.md#5-network-isolation)) |

Plaintext internal traffic is not permitted; mTLS provides mutual authentication in
addition to confidentiality.

---

# 3. Encryption at Rest

| Store | At-rest protection |
|-------|--------------------|
| PostgreSQL | Volume/database encryption |
| Qdrant | Encrypted volumes |
| Redis | Encrypted volumes (ephemeral data) |
| Object storage | Server-side encryption (per-tenant keys where supported) |
| Backups/snapshots | Encrypted |

All persistent backends encrypt at rest; managed datastores use cloud KMS-backed
encryption (see [Terraform](../12-deployment/terraform.md)).

---

# 4. Application-Layer Encryption

Sensitive fields can be encrypted **above** the datastore with per-tenant keys, so
a compromised database alone does not expose them:

- Memory records flagged sensitive ([Memory security](../06-memory-engine/overview.md#12-security))
- Selected configuration and PII fields

This is envelope encryption: data keys wrapped by a tenant key in the KMS.

---

# 5. Key Management

```text
Root key (KMS / HSM)
   │ wraps
Tenant key
   │ wraps
Data encryption keys (DEKs)
```

- Keys live in a KMS/HSM; the platform references them, never holds root material.
- **Rotation** is scheduled; rotating a tenant key re-wraps DEKs without
  re-encrypting all data.
- Per-tenant keys support crypto-shredding (delete the key to render data
  unrecoverable) for data-deletion guarantees.

---

# 6. Secrets vs. Data

Encryption keys protect *data*; **secrets** (API keys, provider credentials) are
handled by the [secret vault](secret-management.md) — distinct mechanisms with
distinct lifecycles.

---

# 7. PII Handling

- PII is classified and may be masked before logging
  ([audit](audit.md)) and before leaving a boundary
  ([tool runtime](../07-tool-runtime/security-isolation.md)).
- PII access is gated by ABAC ([rbac.md](rbac.md)).
- Residency rules can pin PII to a region (ABAC + storage placement).

---

# 8. Cryptographic Standards

- TLS 1.3 preferred (1.2 minimum where required).
- AES-256-GCM for symmetric, modern asymmetric primitives for signing/wrapping.
- Signing (releases, plugins) uses transparency-log-backed signatures
  ([plugin signing](../08-plugin-sdk/distribution.md#3-signing)).

---

# 9. Audit & Compliance

Key creation, rotation, and access are audited. Encryption posture supports
compliance frameworks ([index §7](index.md#7-compliance-posture)).

---

# 10. Dependencies

- [`13-security/secret-management.md`](secret-management.md)
- [`12-deployment/terraform.md`](../12-deployment/terraform.md)
- [`13-security/audit.md`](audit.md)

---

# 11. Related Documents

- [`06-memory-engine/overview.md`](../06-memory-engine/overview.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Encryption specification |
