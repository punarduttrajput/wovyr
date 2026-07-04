<!--
File: docs/13-security/encryption.md
Document ID: SEC-004
-->

# Security: Encryption

**Document ID:** SEC-004  
**File Path:** `docs/13-security/encryption.md`  
**Version:** 1.4.0  
**Status:** Draft — §5's key hierarchy (`apex-kms`) and its two §4 consumers
(`apex-secrets`'s `EncryptedFileSecretStore`, `apex-memory`'s
`EncryptingMemoryStore`) are now **live in `apex-server`/`apex-cli`**, not
just library capabilities: memory encryption is always wrapped (opt-in per
record via `sensitive`), secret encryption is opt-in per deployment
(`APEX_SECRETS_ENCRYPT_AT_REST`) since it swaps the on-disk file rather than
transparently coexisting with existing plaintext. Verified live, including
cross-process decryption (a CLI-sealed record read back through a
separately-running server via the shared `~/.apex/kms` root key). Everything
else in this document (transit/at-rest infra encryption, PII handling)
remains infra-level and undocumented-in-code, as it was  
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

- Secret values ([secret-management](secret-management.md)) — **implemented and live**:
  `apex-secrets`'s `EncryptedFileSecretStore` seals a secret's current and
  retained-previous value through `apex-kms` before they reach disk, keyed
  by the secret's own namespace as the KMS tenant. `apex-server`/`apex-cli`
  select it over the plaintext `FileSecretStore` when
  `APEX_SECRETS_ENCRYPT_AT_REST` is set — **opt-in per deployment**, since it
  reads/writes a distinct file (`secrets.enc.json`) rather than transparently
  migrating whatever is already in the plaintext `secrets.json`.
- Memory records flagged sensitive ([Memory security](../06-memory-engine/overview.md#12-security)) —
  **implemented and live**: `apex-memory`'s `MemoryRecord.sensitive` flag +
  `EncryptingMemoryStore` decorator seals `content` through `apex-kms` (tenant
  = the record's namespace) before it reaches the inner store (any
  `MemoryStore`, including the tiered Postgres/Qdrant backend), transparently
  unsealing on every read so retrieval/ranking still see plaintext.
  `apex-server`/`apex-cli` wrap **every** memory store this way unconditionally
  — safe by construction, since it's a no-op for the default `sensitive: false`.
  Exposed as `POST /api/v1/memory/records`' `sensitive` body field and `memory
  put --sensitive`.
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
into two real, live consumers** (§4 above): `apex-secrets`'s `EncryptedFileSecretStore`
and `apex-memory`'s `EncryptingMemoryStore`, both reachable from `apex-server`
and `apex-cli` (`default_kms` / `config::kms()` — one root key + tenant-key
catalog at `~/.apex/kms`, shared by both processes and both consumers). Not
yet done: a cloud-KMS-/HSM-backed root (the `Kms` trait is the boundary a
real backend would implement — only tenant-key wrap/unwrap would change),
config/PII fields, a CLI/API surface for key management itself (rotate/
destroy a tenant key operator-side), and audit-logging key lifecycle events
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
| 1.4.0 | 2026-07-04 | Both §4 consumers made **live** in `apex-server`/`apex-cli`: a shared `default_kms()`/`config::kms()` (root key from `APEX_KMS_ROOT_KEY` or generated at `~/.apex/kms/root.key`, tenant catalog in the same directory) feeds both. Memory: `EncryptingMemoryStore` wraps every store unconditionally (safe no-op unless `sensitive: true`); exposed as `POST /api/v1/memory/records`' `sensitive` field and CLI `memory put --sensitive`. Secrets: `EncryptedFileSecretStore` selected via `APEX_SECRETS_ENCRYPT_AT_REST` (opt-in, since it's a distinct file rather than a transparent migration) — the CLI's plugin-secret-injection path honors the identical env var so the two processes never disagree about which file is live. Verified end-to-end against a running server: sensitive content sealed on disk/plaintext on query, non-sensitive/default-secrets behavior unchanged, and a CLI-sealed memory record successfully decrypted by a separately-running server process |
| 1.3.0 | 2026-07-04 | §4 gets a second real application-layer-encryption consumer: `apex-memory`'s `MemoryRecord.sensitive` flag + `EncryptingMemoryStore` decorator seals `content` through `apex-kms` when set, wrapping any `MemoryStore` (including the tiered Postgres/Qdrant backend, whose `sensitive` column round-trips the flag — verified against a live Postgres in this pass). Retrieval/ranking still see plaintext (unsealed transparently on read); pushdown is disabled for a wrapped store since a purpose-built index can't score ciphertext, so wrapping falls back to in-process ranking |
| 1.2.0 | 2026-07-04 | §4's first real application-layer-encryption consumer landed: `apex-secrets`'s `EncryptedFileSecretStore` seals secret values through `apex-kms`, keyed by the secret's own namespace as the KMS tenant. `secrets.enc.json` (distinct from the plaintext `FileSecretStore`'s `secrets.json`) never holds a plaintext value; verified by a test reading the raw file bytes |
| 1.1.0 | 2026-07-04 | §5's key hierarchy landed in code: `apex-kms` (`Kms` trait, `LocalKms`, rotation/rewrap, crypto-shredding). Not yet done: a cloud-KMS/HSM-backed root, wiring into a real consumer, server/CLI surface, audit integration |
| 1.0.0 | 2026-06-27 | Initial Encryption specification |
