<!--
File: docs/13-security/encryption.md
Document ID: SEC-004
-->

# Security: Encryption

**Document ID:** SEC-004  
**File Path:** `docs/13-security/encryption.md`  
**Version:** 1.6.0  
**Status:** Draft — §5's key hierarchy (`apex-kms`) and its three §4 consumers
(`apex-secrets`'s `EncryptedFileSecretStore`, `apex-memory`'s
`EncryptingMemoryStore`, `apex-events`'s `EncryptedFileWebhookStore`) are
**live in `apex-server`** (the latter two also in `apex-cli` where
applicable): memory encryption is always wrapped (opt-in per record via
`sensitive`), secret and webhook-secret encryption are each opt-in per
deployment (`APEX_SECRETS_ENCRYPT_AT_REST` / `APEX_WEBHOOKS_ENCRYPT_AT_REST`)
since each swaps the on-disk file rather than transparently coexisting with
existing plaintext. **A key-management surface now exists too** — tenant-key
rotate/destroy over `/api/v1/kms/tenant-key/*` and `apex kms rotate|destroy`,
RBAC-gated (`kms:write`/`kms:admin`) and audited (§9). Verified live,
including cross-process decryption (a CLI-sealed record read back through a
separately-running server) and the post-destroy fail-closed behavior.
**Pen-tested and compliance-mapped** — see
[compliance-mapping.md](compliance-mapping.md). Everything else in this
document (transit/at-rest infra encryption, PII handling beyond the three §4
consumers) remains infra-level and undocumented-in-code, as it was  
**Owner:** Security Team  
**Last Updated:** 2026-07-05

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
- Webhook subscription signing secrets ([overview §15](../09-api/overview.md#15-webhooks--events)) —
  **implemented and live**: `apex-events`'s `EncryptedFileWebhookStore` seals a
  subscription's `secret` (the HMAC key deliveries are signed/verified with)
  through `apex-kms`, keyed by the subscription's own `tenant`. `url`/
  `events`/`active` stay plaintext — no confidentiality need, and the
  subscription id is derived from them. `apex-server` selects it over the
  plaintext `FileWebhookStore` when `APEX_WEBHOOKS_ENCRYPT_AT_REST` is set —
  **opt-in per deployment**, same rationale as secrets: it reads/writes a
  distinct file (`webhooks.enc.json`) rather than migrating whatever is
  already in the plaintext `webhooks.json`.
- Broader config/PII fields (e.g. a future `User` resource's email — see
  [users.md](../09-api/users.md)) — not yet implemented; no such field exists
  in a durably-persisted store today outside the three items above (checked
  as part of scoping this item — `apex-tenancy`'s `Organization`/`Project`/
  `Membership` carry no PII-shaped fields, `Membership.user` is an opaque id,
  not an email)

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
into three real, live consumers** (§4 above): `apex-secrets`'s
`EncryptedFileSecretStore`, `apex-memory`'s `EncryptingMemoryStore`, and
`apex-events`'s `EncryptedFileWebhookStore`, all reachable from `apex-server`
(`default_kms` — one root key + tenant-key catalog at `~/.apex/kms`, shared
across every consumer; the CLI's `config::kms()` shares the same root/catalog
for its two consumers, secrets and memory — webhooks have no CLI surface).
**A
CLI/API surface for key management itself now exists too**: `POST
/api/v1/kms/tenant-key/rotate` (`kms:write`) / `.../destroy` (`kms:admin` —
a materially higher tier, since it's irreversible) and the matching `apex kms
rotate|destroy --tenant <t>` CLI commands (`destroy` requires `--yes`), both
operating on the same shared `Kms` — verified live, including the
post-destroy fail-closed 403/error on any further operation for that tenant.
**Key lifecycle events are now audited** (§9 below: `kms.tenant_key.rotate`/
`.destroy`, by tenant reference). **Pen-tested and compliance-mapped** — see
[compliance-mapping.md](compliance-mapping.md) for the SOC2/ISO27001/GDPR
control mapping and the adversarial test suite
(`crates/apex-kms/tests/adversarial.rs`) it's backed by, including a proven
residual-risk finding (documented there, not fixed here — narrowing it is a
systemic change outside this crate's scope). Not yet done: a cloud-KMS-/
HSM-backed root (the `Kms` trait is the boundary a real backend would
implement — only tenant-key wrap/unwrap would change), broader config/PII
field coverage beyond the three §4 consumers, and an actual *external* pen
test / formal audit. See `crates/apex-kms/src/lib.rs` and
`crates/apex-server/src/kms.rs`.

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

**Implemented:** tenant-key rotation and crypto-shredding are audited via
`apex-audit` (`kms.tenant_key.rotate`/`kms.tenant_key.destroy`, resource
type `kms_tenant_key`, referenced by tenant — never key material), exactly
like secret mutations. Key *creation* (first-use auto-provisioning inside
`generate_data_key`/`rotate_tenant_key`) is not separately audited — only
the explicit operator actions (rotate/destroy) are, since `apex-kms` itself
has no `apex-audit` dependency (auditing happens at the `apex-server` route
boundary, matching how `apex-secrets` isn't audit-aware internally either).

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
| 1.6.0 | 2026-07-05 | §4 gains a third real consumer: `apex-events`'s new `EncryptedFileWebhookStore` seals a webhook subscription's signing `secret` through `apex-kms` (keyed by the subscription's own `tenant`), selected over the plaintext `FileWebhookStore` via `APEX_WEBHOOKS_ENCRYPT_AT_REST` (opt-in, same file-swap rationale as secrets). `apex-events` now depends on `apex-kms`. Also: pen-testing (`crates/apex-kms/tests/adversarial.rs`) and a first compliance mapping ([compliance-mapping.md](compliance-mapping.md), new) closed out §5's last two open items, surfacing a proven residual-risk finding (documented, not fixed — see that doc's §7) |
| 1.5.0 | 2026-07-04 | Added a key-management surface: `POST /api/v1/kms/tenant-key/rotate` (`kms:write`) / `.../destroy` (`kms:admin` — a higher tier than routine writes, since crypto-shredding is irreversible) and matching `apex kms rotate\|destroy --tenant <t>` CLI commands (`destroy` requires `--yes`). Both audited (`kms.tenant_key.rotate`/`.destroy`, referenced by tenant). `AppState` gained a `kms` field (the same shared instance backing the two encrypting stores). New RBAC scopes `kms:write`/`kms:admin` added to `apex-tenancy`'s privilege-ladder test (no logic changes needed — they already fell out of the existing generic `is_write`/`ProjectAdmin`-grants-everything-except-3 patterns). 2 new server tests (RBAC tiering incl. post-destroy fail-closed rotate; audit trail). Verified live: rotate → destroy → subsequent rotate returns 403, all three audited in the tamper-evident log; CLI mirrors the same behavior locally |
| 1.4.0 | 2026-07-04 | Both §4 consumers made **live** in `apex-server`/`apex-cli`: a shared `default_kms()`/`config::kms()` (root key from `APEX_KMS_ROOT_KEY` or generated at `~/.apex/kms/root.key`, tenant catalog in the same directory) feeds both. Memory: `EncryptingMemoryStore` wraps every store unconditionally (safe no-op unless `sensitive: true`); exposed as `POST /api/v1/memory/records`' `sensitive` field and CLI `memory put --sensitive`. Secrets: `EncryptedFileSecretStore` selected via `APEX_SECRETS_ENCRYPT_AT_REST` (opt-in, since it's a distinct file rather than a transparent migration) — the CLI's plugin-secret-injection path honors the identical env var so the two processes never disagree about which file is live. Verified end-to-end against a running server: sensitive content sealed on disk/plaintext on query, non-sensitive/default-secrets behavior unchanged, and a CLI-sealed memory record successfully decrypted by a separately-running server process |
| 1.3.0 | 2026-07-04 | §4 gets a second real application-layer-encryption consumer: `apex-memory`'s `MemoryRecord.sensitive` flag + `EncryptingMemoryStore` decorator seals `content` through `apex-kms` when set, wrapping any `MemoryStore` (including the tiered Postgres/Qdrant backend, whose `sensitive` column round-trips the flag — verified against a live Postgres in this pass). Retrieval/ranking still see plaintext (unsealed transparently on read); pushdown is disabled for a wrapped store since a purpose-built index can't score ciphertext, so wrapping falls back to in-process ranking |
| 1.2.0 | 2026-07-04 | §4's first real application-layer-encryption consumer landed: `apex-secrets`'s `EncryptedFileSecretStore` seals secret values through `apex-kms`, keyed by the secret's own namespace as the KMS tenant. `secrets.enc.json` (distinct from the plaintext `FileSecretStore`'s `secrets.json`) never holds a plaintext value; verified by a test reading the raw file bytes |
| 1.1.0 | 2026-07-04 | §5's key hierarchy landed in code: `apex-kms` (`Kms` trait, `LocalKms`, rotation/rewrap, crypto-shredding). Not yet done: a cloud-KMS/HSM-backed root, wiring into a real consumer, server/CLI surface, audit integration |
| 1.0.0 | 2026-06-27 | Initial Encryption specification |
