<!--
File: docs/13-security/encryption.md
Document ID: SEC-004
-->

# Security: Encryption

**Document ID:** SEC-004  
**File Path:** `docs/13-security/encryption.md`  
**Version:** 1.3.0  
**Status:** Draft — §5's key hierarchy has a code counterpart (`apex-kms`),
now with two real §4 application-layer-encryption consumers
(`apex-secrets`'s `EncryptedFileSecretStore` and `apex-memory`'s
`EncryptingMemoryStore`, both listed in §4's bullets); everything else in
this document (transit/at-rest infra encryption, PII handling) remains
infra-level and undocumented-in-code, as it was  
**Owner:** Security Team  
**Last Updated:** 2026-07-04

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

- Secret values ([secret-management](secret-management.md)) — **implemented**:
  `apex-secrets`'s `EncryptedFileSecretStore` seals a secret's current and
  retained-previous value through `apex-kms` before they reach disk, keyed
  by the secret's own namespace as the KMS tenant.
- Memory records flagged sensitive ([Memory security](../06-memory-engine/overview.md#12-security)) —
  **implemented**: `apex-memory`'s `MemoryRecord.sensitive` flag +
  `EncryptingMemoryStore` decorator seals `content` through `apex-kms` (tenant
  = the record's namespace) before it reaches the inner store (any
  `MemoryStore`, including the tiered Postgres/Qdrant backend), transparently
  unsealing on every read so retrieval/ranking still see plaintext
- Selected configuration and PII fields — not yet implemented

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

**Implemented (`apex-kms`):** the hierarchy above, the `Kms` trait, and
`LocalKms` — a single-host implementation (AES-256-GCM via `ring`) where the
root key is generated/held in-process (`root::from_env` or `root::from_file`)
rather than a real KMS/HSM. Rotation is `rotate_tenant_key` (rolls a new
tenant key version, retaining old ones) plus `rewrap_data_key` (the caller
moves each DEK it holds onto the new version — the crate has no visibility
into which DEKs a consumer has stored, so it cannot rewrap them itself).
Crypto-shredding is `destroy_tenant_key`, fail-closed thereafter. **Wired
into two real consumers** (§4 above): `apex-secrets`'s `EncryptedFileSecretStore`
and `apex-memory`'s `EncryptingMemoryStore`. Not yet done: a cloud-KMS-/HSM-backed
root (the `Kms` trait is the boundary a real backend would implement — only
tenant-key wrap/unwrap would change), config/PII fields, server routes/CLI
surface, and audit-logging key lifecycle events
(§9 below). See `crates/apex-kms/src/lib.rs`.

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
| 1.3.0 | 2026-07-04 | §4 gets a second real application-layer-encryption consumer: `apex-memory`'s `MemoryRecord.sensitive` flag + `EncryptingMemoryStore` decorator seals `content` through `apex-kms` when set, wrapping any `MemoryStore` (including the tiered Postgres/Qdrant backend, whose `sensitive` column round-trips the flag — verified against a live Postgres in this pass). Retrieval/ranking still see plaintext (unsealed transparently on read); pushdown is disabled for a wrapped store since a purpose-built index can't score ciphertext, so wrapping falls back to in-process ranking |
| 1.2.0 | 2026-07-04 | §4's first real application-layer-encryption consumer landed: `apex-secrets`'s `EncryptedFileSecretStore` seals secret values through `apex-kms`, keyed by the secret's own namespace as the KMS tenant. `secrets.enc.json` (distinct from the plaintext `FileSecretStore`'s `secrets.json`) never holds a plaintext value; verified by a test reading the raw file bytes |
| 1.1.0 | 2026-07-04 | §5's key hierarchy landed in code: `apex-kms` (`Kms` trait, `LocalKms`, rotation/rewrap, crypto-shredding). Not yet done: a cloud-KMS/HSM-backed root, wiring into a real consumer, server/CLI surface, audit integration |
| 1.0.0 | 2026-06-27 | Initial Encryption specification |
