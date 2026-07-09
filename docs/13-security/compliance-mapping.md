<!--
File: docs/13-security/compliance-mapping.md
Document ID: SEC-007
-->

# Security: Compliance Mapping (Encryption & Key Management)

**Document ID:** SEC-007  
**File Path:** `docs/13-security/compliance-mapping.md`  
**Version:** 1.4.0  
**Status:** Draft — first mapping pass, scoped to the encryption/key-management
control family (`apex-kms`, `apex-secrets`, `apex-memory`'s encrypting store,
`apex-events`'s encrypting webhook store, `apex-audit`). Verified against real
code and a new adversarial test suite, not a third-party attestation.  
**Owner:** Security Team  
**Last Updated:** 2026-07-09. §7 items 1, 2, and 3 (the anonymous default-tenant
bypass, `kms.json` file permissions, and the non-Unix root-key/`kms.json` ACL)
have since been closed — see their entries.

---

# 1. Purpose

[`13-security/index.md`](index.md) §7 states the platform is "designed to
support" SOC 2 / ISO 27001 / GDPR, with certification itself
deployment-dependent. That paragraph is a posture statement, not evidence.
This document is the first concrete slice of evidence: it maps specific
framework controls to the specific code that implements them, with file/line
citations and the test that proves each claim — for the encryption and
key-management control family only ([Encryption §5](encryption.md#5-key-management),
`apex-kms`, and its three §4 consumers).

**This is an internal self-assessment, not a certification, attestation, or
substitute for a third-party audit.** No external pen test or formal
SOC 2/ISO 27001/GDPR audit has been performed against this codebase. Treat
"Met" below as "the described control exists in code and is covered by an
automated test that would fail if it regressed" — not as a compliance sign-off.

---

# 2. Scope & Method

**In scope:** encryption at rest (`apex-kms`, `EncryptedFileSecretStore`,
`EncryptingMemoryStore`, `EncryptedFileWebhookStore`), key lifecycle (rotation,
crypto-shredding), and the audit trail for key-management actions.

**Out of scope (not mapped here):** authentication/session management, network
transport security (TLS/mTLS), sandbox/tool isolation, supply-chain signing —
these have their own specs ([authentication.md](authentication.md),
[security-isolation.md](../07-tool-runtime/security-isolation.md),
[distribution.md](../08-plugin-sdk/distribution.md)) and would need their own
mapping pass.

**Method:** for each control, cite the implementing function/type, and the
test (unit, adversarial, or integration) that exercises it. Where the
platform's own architecture note already states a limitation (e.g. `apex-kms`'s
`LocalKms` being a single-host stand-in for a real KMS/HSM), that limitation is
carried into the "Status" column rather than glossed over.

---

# 3. SOC 2 (Trust Services Criteria) Mapping

| Control | Requirement | Implementation | Evidence | Status |
|---------|-------------|-----------------|----------|--------|
| CC6.1 | Logical access to sensitive data/systems is restricted | `/api/v1/kms/tenant-key/*` gated by `tenant_authorize` (`kms:write`/`kms:admin` scopes); [kms.rs](../../crates/apex-server/src/kms.rs) | `kms_rotate_is_routine_but_destroy_needs_a_higher_tier` ([lib.rs](../../crates/apex-server/src/lib.rs)) | **Partially Met** — see §7 item 1, the anonymous-default-tenant bypass |
| CC6.6 | Data is protected via encryption in line with its risk classification | `apex-kms::envelope::seal`/`open` (AES-256-GCM via `ring`); wraps `apex-secrets`' current+previous values, `apex-memory`'s `sensitive`-flagged content, and `apex-events`'s webhook signing secrets | `crates/apex-kms/tests/adversarial.rs` (tamper/forgery tests); `apex-secrets`/`apex-memory`/`apex-events` round-trip + on-disk-never-plaintext tests | **Met** |
| CC6.7 | Data-in-use/transmission is restricted to authorized parties via key management | Root→tenant→DEK hierarchy (`LocalKms`); DEKs are per-call, tenant keys are versioned and tenant-scoped | `tenant_isolation_a_dek_wrapped_for_one_tenant_will_not_unwrap_under_another`, `attacker_cannot_launder_a_dek_across_tenants_via_rewrap` | **Met** for the app-layer boundary; root key itself is a single-process stand-in (§7 item 4) |
| CC6.8 | Unauthorized/malicious data disposal is prevented; authorized disposal is complete | `Kms::destroy_tenant_key` crypto-shreds all key versions; every downstream ciphertext becomes permanently unrecoverable | `crypto_shredding_a_tenant_makes_every_operation_fail_closed`; `a_dek_captured_before_crypto_shredding_is_useless_after_even_via_envelope` | **Met** |
| CC7.2 | Security events are logged and monitored | `kms.tenant_key.rotate`/`.destroy` recorded in the tamper-evident audit log, by tenant reference, never key material | `kms_tenant_key_mutations_are_audited` | **Met** |

---

# 4. ISO/IEC 27001:2022 (Annex A) Mapping

| Control | Requirement | Implementation | Evidence | Status |
|---------|-------------|-----------------|----------|--------|
| A.8.24 | Use of cryptography | AES-256-GCM envelope encryption (`crates/apex-kms/src/crypto.rs`); no ambient key reuse (`generate_data_key` mints a fresh DEK per call) | `each_call_mints_an_independent_dek_even_for_identical_plaintext`; `nonce_is_never_reused_across_many_seals_for_the_same_tenant` | **Met** |
| A.5.15 | Access control | RBAC scopes `kms:write`/`kms:admin`, tenant-scoped (`apex-tenancy`'s `Role`/`grants`) | `rbac_default_deny_matrix_is_a_strict_privilege_ladder` ([rbac.rs](../../crates/apex-tenancy/src/rbac.rs)) | **Partially Met** — see §7 item 1 |
| A.8.10 | Information deletion | Crypto-shredding (`destroy_tenant_key`) as the deletion mechanism for sealed data — permanent, verifiable, no residual recoverability | `crypto_shredding_a_tenant_makes_every_operation_fail_closed` | **Met** |
| A.8.15 | Logging | Every key-management mutation audited, hash-chained, tamper-evident (`apex-audit`) | `kms_tenant_key_mutations_are_audited` | **Met** |
| A.5.31 | Legal/regulatory/contractual requirements (incl. cryptography) | Documented key hierarchy ([encryption.md §5](encryption.md#5-key-management)), this mapping doc itself | — | **Partially Met** — no legal/regulatory review has actually occurred; this is architecture documentation, not a compliance opinion |

---

# 5. GDPR Mapping

| Article | Requirement | Implementation | Evidence | Status |
|---------|-------------|-----------------|----------|--------|
| Art. 32 | Security of processing (incl. "encryption of personal data") | `EncryptingMemoryStore`/`EncryptedFileSecretStore`/`EncryptedFileWebhookStore` seal content at rest through `apex-kms` | `crates/apex-kms/tests/adversarial.rs`; `apex-memory`/`apex-secrets`/`apex-events` encrypting-store tests | **Met** for data explicitly flagged `sensitive`/opted into `APEX_SECRETS_ENCRYPT_AT_REST`/`APEX_WEBHOOKS_ENCRYPT_AT_REST` — **not** applied platform-wide by default (opt-in, see [encryption.md](encryption.md)) |
| Art. 17 | Right to erasure ("right to be forgotten") | `destroy_tenant_key` crypto-shredding is a strong technical mechanism for erasure: once a tenant's key material is destroyed, every DEK ever wrapped under it — and hence every ciphertext it protects, regardless of where that ciphertext physically lives — becomes permanently unrecoverable | `crypto_shredding_a_tenant_makes_every_operation_fail_closed` | **Met** as a *mechanism*; actual GDPR erasure also requires deleting (or accepting as inert ciphertext) the plaintext-adjacent metadata/backups outside `apex-kms`'s control — an operational/process step, not something this crate alone guarantees |

---

# 6. Adversarial Verification (Pen-Test Summary)

`crates/apex-kms/tests/adversarial.rs` (new) attacks the `LocalKms`/`envelope`
boundary directly — no `docker`/network access needed, runs unconditionally in
CI:

- **Cross-tenant laundering via `rewrap`** — confirms `rewrap_data_key` cannot
  be used to move a DEK onto a *different* tenant's key, even when that
  tenant happens to have a same-numbered key version.
- **Tenant-key-layer tampering** — corrupts the wrapped tenant key itself
  (not just the DEK, which the pre-existing unit tests already covered) via
  direct `KmsStore` access, confirming the failure propagates rather than
  producing garbage key material that "succeeds."
- **Nonce reuse at volume** — 256 consecutive seals for one tenant, checked
  for AES-GCM nonce uniqueness at both the DEK-wrapper and payload layers
  (the existing unit test only checked a single pair).
- **Version-number forgery** — relabels a real, captured ciphertext with a
  different `tenant_key_version` after rotation; confirms AEAD rejects the
  mismatch rather than decrypting to silently-wrong plaintext.
- **Blind forgery** — a wrapped value with no legitimate ciphertext at all
  (the "probing the endpoint" case, distinct from relabeling real data).
- **Post-crypto-shred replay** — a `SealedData` captured *before*
  `destroy_tenant_key`, replayed through the higher-level `envelope::open`
  API (the one real consumers call) *after* — confirms the erasure guarantee
  holds against a cached/captured credential, not just a fresh lookup.

All six pass against the real implementation (not mocked). See
`crates/apex-kms/tests/adversarial.rs` for the tests themselves — per this
codebase's established convention ([security-testing.md §5](../15-testing/security-testing.md#5-sandbox--isolation-tests)),
the test code is the authoritative record; this section summarizes it.

---

# 7. Residual Risk and Gaps

Found during this pass, not fixed here (see rationale per item):

1. **Anonymous default-tenant bypass reaching `kms:admin` — CLOSED (RM-GA-P4/
   GA-003, narrowing RM-GA-P1 SEC-102).** `tenant_authorize`
   ([tenancy.rs](../../crates/apex-server/src/tenancy.rs)) used to skip its RBAC
   check entirely for a request with no `X-Apex-Principal` against the default
   tenant whenever `AppState.anonymous_allowed` (`APEX_ALLOW_ANONYMOUS=1`) —
   meaning an anonymous caller got *every* scope, including `kms:admin`
   (crypto-shredding), with zero grant. That short-circuit is now deleted:
   `anonymous_allowed` governs **only** whether
   [`auth::authenticate`](../../crates/apex-server/src/auth.rs)'s
   `disabled-loopback` mode lets an unauthenticated request reach a handler at
   all (still off by default, still refused by
   [`serve()`](../../crates/apex-server/src/lib.rs) on any non-loopback bind) —
   it no longer implies any authorization outcome once it does. An anonymous
   caller is now authorized exactly like any other principal with no
   memberships: `Role::grants` guarantees an empty role set denies every scope,
   so the same request that used to succeed now `403`s regardless of
   `APEX_ALLOW_ANONYMOUS`. Proven by
   `anonymous_default_tenant_caller_reaches_kms_admin_only_up_to_the_auth_layer_now`
   (flag on → `403`, not the old `200`/"destroyed") and
   `anonymous_default_tenant_caller_is_denied_when_the_flag_is_off` (flag off →
   `401`, rejected even earlier, at authentication) in
   [lib.rs](../../crates/apex-server/src/lib.rs). Local/dev convenience for
   tenant-scoped routes now requires a real credential (e.g.
   `APEX_PLATFORM_ADMINS=<principal>` plus that principal's own header) — the
   same path a real deployment already uses; nothing tenant-scoped is reachable
   "for free" via anonymity alone anymore. Additionally, SEC-101 closes the
   deeper hole this item didn't cover: raw `X-Apex-Principal`/bearer values are
   no longer trusted outright once `APEX_AUTH_MODE=jwt|apikey` is configured —
   the verified auth middleware ([auth.rs](../../crates/apex-server/src/auth.rs))
   overwrites the header with the verified principal before any handler runs.
   The remaining, unavoidable characteristic of the `disabled-loopback` default
   (when `APEX_AUTH_MODE` is unset) is that it still trusts raw headers for
   *identity* on a loopback bind — but identity alone no longer buys RBAC access
   the way anonymity used to.
2. **Closed.** `kms.json` (the wrapped tenant-key catalog) is now restricted to
   owner-only access after every write —
   `FileKmsStore::persist`'s `restrict_permissions`
   ([store.rs](../../crates/apex-kms/src/store.rs)), the same treatment
   `root::from_file` already gave `root.key`. Proven by
   `store::tests::file_store_restricts_kms_json_to_owner_only`.
3. **Closed (2026-07-09, authored and verified on a real Windows host).**
   Root-key and `kms.json` file permissions used to be a no-op on non-Unix —
   `root::restrict_permissions`/`store.rs`'s equivalent only ever called
   `chmod` under `#[cfg(unix)]`, so on Windows these files kept the OS default
   ACL rather than being locked to the owning process. The three previously
   independent copies of that `#[cfg(unix)]`/`#[cfg(not(unix))]` pair
   (`apex-kms`'s `root.rs` and `store.rs`, the CLI's `credentials.json`
   handling in [config.rs](../../apps/apex-cli/src/config.rs)) are now one
   shared primitive, `apex_common::fs::restrict_to_owner`
   ([fs.rs](../../crates/apex-common/src/fs.rs)): `chmod 0600` on Unix
   unchanged, and on Windows a real ACL restriction via `icacls
   /inheritance:r /grant:r <user>:F` (std has no ACL-editing API; this shells
   out to the tool bundled with every Windows install, the same
   external-tool-via-`Command` pattern `apex-tools`' egress lockdown already
   uses for `iptables`/`nsenter`, rather than adding a Windows-ACL crate
   dependency for one call site). Proven live on Windows — not just
   reasoned about — by `fs::tests::restrict_to_owner_locks_the_file_down`,
   `root::tests::from_file_generates_once_then_persists_across_calls`, and
   `store::tests::file_store_restricts_kms_json_to_owner_only`, each of which
   asserts via `icacls` output that no inherited ACE survives and only the
   invoking user is granted access.
4. **No cloud-KMS-/HSM-backed root.** `LocalKms` holds the root key in-process
   by design ([kms.rs](../../crates/apex-kms/src/kms.rs) doc comment) — a
   documented, intentional single-host stand-in. The `Kms` trait is the
   boundary a real backend would implement instead; this is unstarted.
5. **No PII/config field encryption.** `apex-kms` is wired into
   `apex-secrets` and `apex-memory` only; no field-level encryption exists
   for tenancy/config records.
6. **No external verification.** No third-party penetration test, and no
   formal SOC 2/ISO 27001/GDPR audit, has been performed. This document is a
   self-assessment against the codebase as of 2026-07-05.

---

# 8. Dependencies

- [`13-security/index.md`](index.md) §7 (platform-wide compliance posture)
- [`13-security/encryption.md`](encryption.md) §5 (key hierarchy this maps)
- [`13-security/audit.md`](audit.md) (the audit trail cited in §3/§4)
- [`15-testing/security-testing.md`](../15-testing/security-testing.md) §9
  (penetration testing & reviews)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|--------------|
| 1.4.0 | 2026-07-09 | Closed §7 item 3: root-key/`kms.json` file permissions were a no-op on non-Unix. Consolidated the three independent `#[cfg(unix)]`-only permission-restriction copies (`apex-kms`'s `root.rs`/`store.rs`, the CLI's `config.rs`) into one shared `apex_common::fs::restrict_to_owner`, which on Windows shells out to `icacls /inheritance:r /grant:r <user>:F`. Authored and proven live on a real Windows host (not reasoned-about-only), closing the gap the prior 1.3.0 entry left open for lack of a Windows environment |
| 1.3.0 | 2026-07-09 | Merged two independent closures: §7 item 1 (`tenant_authorize`'s anonymous default-tenant RBAC bypass — `APEX_ALLOW_ANONYMOUS=1` used to grant an anonymous caller every scope, incl. `kms:admin` — is deleted, `anonymous_allowed` now governs only whether an unauthenticated request reaches a handler at all) and §7 item 2 (`kms.json`, the wrapped tenant-key catalog, now `chmod 0600` on Unix after every write, same treatment as `root.key`). Item 3 (non-Unix ACL) remains open |
| 1.1.0 | 2026-07-05 | Added `apex-events`'s new `EncryptedFileWebhookStore` (webhook subscription signing secrets, `APEX_WEBHOOKS_ENCRYPT_AT_REST`) as a third §4 encrypting-store consumer, updated CC6.6/Art. 32 evidence accordingly |
| 1.0.0 | 2026-07-05 | Initial mapping: SOC 2 (CC6.1/6.6/6.7/6.8/7.2), ISO 27001 Annex A (8.24/5.15/8.10/8.15/5.31), and GDPR (Art. 32, Art. 17) controls mapped to `apex-kms`/`apex-secrets`/`apex-memory`/`apex-audit`, backed by a new adversarial test suite (`crates/apex-kms/tests/adversarial.rs`) and a documented, proven residual-risk finding (anonymous default-tenant bypass reaching `kms:admin`) |
