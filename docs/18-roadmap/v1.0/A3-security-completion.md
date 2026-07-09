<!--
File: docs/18-roadmap/v1.0/A3-security-completion.md
Document ID: GA-003
-->

# GA Completion: Security — Root-of-Trust, PII Coverage & External Validation

**Document ID:** GA-003
**File Path:** `docs/18-roadmap/v1.0/A3-security-completion.md`
**Version:** 1.2.0
**Status:** In progress — KMS live + pen-tested + compliance-mapped; all three
§4.1 hardening-pass residual findings (anonymous default-tenant RBAC bypass,
`kms.json` file permissions, non-Unix root-key/`kms.json` ACL) are now
**closed**; root-of-trust and external validation remain
**Owner:** Security Team
**Last Updated:** 2026-07-09

---

# 1. Purpose

Turn the "Security: Root-of-Trust, PII Coverage, and External Validation" GA gap
([PRD-002 §5.3](../../01-product/prd-future.md#53-security-root-of-trust-pii-coverage-and-external-validation),
[v1.0 §3 Security row](../v1.0.md#3-in-scope)) into a delivery plan.

Committed GA-completion work. The envelope-encryption slice is done; what remains
is the production root-of-trust, broader PII coverage, and *external* validation.

---

# 2. Current State

- **KMS is live** across three consumers — `apex-secrets`
  (`EncryptedFileSecretStore`), `apex-memory` (`EncryptingMemoryStore`), and
  `apex-events` (`EncryptedFileWebhookStore`) — with a key-management surface
  (`/api/v1/kms/tenant-key/rotate|destroy`, `apex kms rotate|destroy`), audited
  (`kms.tenant_key.*`). See [encryption §5](../../13-security/encryption.md#5-key-management).
- **Pen-tested and compliance-mapped.** `crates/apex-kms/tests/adversarial.rs`
  attacks the key boundary (cross-tenant laundering, tamper/forgery,
  post-crypto-shred replay); [compliance-mapping.md](../../13-security/compliance-mapping.md)
  maps SOC 2 / ISO 27001 / GDPR controls to the implementation — **as an internal
  self-assessment**.
- **All three hardening-pass residual findings are now closed**
  ([compliance-mapping §7](../../13-security/compliance-mapping.md#7-residual-risk-and-gaps)):
  the anonymous default-tenant RBAC bypass reaching `kms:admin` is fixed
  (`tenant_authorize`'s short-circuit deleted — see §3 item 3 below);
  `kms.json` file permissions are now owner-only after every write; and the
  non-Unix root-key/`kms.json` ACL gap is closed too — authored and verified
  live on a real Windows host via a new shared `apex_common::fs::restrict_to_owner`
  primitive (Unix: `chmod 0600`; Windows: `icacls /inheritance:r /grant:r`).
- **The root key is a single-host stand-in.** `LocalKms` holds it in-process
  (`root::from_env` / `root::from_file`); no cloud-KMS/HSM backing.

---

# 3. Gap

1. No **cloud-KMS-/HSM-backed root** — production deployments need a managed
   root-of-trust, not an in-process key.
2. **PII field encryption** covers secrets/memory/webhook-secrets but not future
   PII resources (e.g. a `User.email` — [users.md](../../09-api/users.md)).
3. ~~The documented **residual findings** are unaddressed by design (scoped out
   of the pen-test slice, deferred to a hardening pass).~~ **Done (2026-07-09)**
   for all three: `tenant_authorize`
   ([tenancy.rs](../../../crates/apex-server/src/tenancy.rs)) no longer
   short-circuits RBAC for an anonymous caller against the default tenant
   (`APEX_ALLOW_ANONYMOUS=1` now governs only whether such a request reaches a
   handler at all, never its authorization outcome); `kms.json` is now
   `chmod 0600`-equivalent after every write; and the non-Unix ACL gap for both
   `root.key` and `kms.json` is closed via `apex_common::fs::restrict_to_owner`
   (`icacls` on Windows), authored and proven live on a real Windows host.
4. No **external** pen test or **formal** compliance audit has occurred — the
   current mapping is a self-assessment.

---

# 4. Scope & Requirements

## 4.1 Functional / deliverables
- A **cloud-KMS-/HSM-backed `Kms` implementation** behind the existing trait
  boundary — only tenant-key wrap/unwrap changes, since `Kms` is the port
  (documented explicitly in [encryption §5](../../13-security/encryption.md#5-key-management)).
- **Field-level encryption for any PII-bearing resource added later**, reusing
  the `envelope::seal`/`open` pattern already proven in three consumers.
- ~~A **scoped hardening pass** closing the residual findings — notably narrowing
  the anonymous default-tenant bypass (a *systemic* change across every
  tenant-scoped route, deliberately deferred from the pen-test slice).~~ **Done**
  for all three residual findings (2026-07-09).
- Engagement of an **external penetration test** and a **formal
  compliance-mapping audit**.

## 4.2 Non-functional
- ~~The hardening pass must preserve back-compat where the anonymous
  default-tenant mode is currently relied upon, or migrate callers
  deliberately.~~ **Resolved by migrating callers**: local/dev convenience for
  tenant-scoped routes now requires a real credential (`APEX_PLATFORM_ADMINS`
  + a principal header) rather than bare anonymity — the same path a real
  deployment already uses. ~10 apex-server tests that relied on the bypass were
  migrated to this pattern rather than left passing on stale assumptions.
- New backends implement the trait; the spine does not change.

---

# 5. Exit Criteria

> An **external** pen-test report with **no unresolved high/critical findings**,
> and a **third-party** control-mapping review completed — replacing today's
> internal self-assessment. Plus a production root-of-trust backed by a managed
> KMS/HSM.

This is the direct input to the v1.0 exit criterion "passes security review and
external pen test" ([v1.0 §5](../v1.0.md#5-exit-criteria)).

---

# 6. Dependencies & Environment Caveats

- **Cloud KMS/HSM and external audit/pen-test vendors** are real-world
  engagements not available in the dev environment — the trait-level
  implementation can be authored here, but validation against a live managed KMS
  cannot.
- The `User` resource for PII field encryption **does not exist yet**; that
  sub-item is contingent on the resource being added.
- Interacts with [A2 backup/restore](A2-reliability-ha-dr.md): a managed root
  changes what the KMS-catalog backup must cover.

---

# 7. Risks

| Risk | Mitigation |
|------|-----------|
| Anonymous-bypass fix breaks back-compat callers | **Materialized as expected, resolved by migration**: ~10 apex-server tests (mostly `workflow_runner.rs`, plus a handful in `lib.rs`) asserted success for a credential-less caller against a `tenant_authorize`-gated route; all migrated to a real `APEX_PLATFORM_ADMINS` principal rather than left passing on stale assumptions |
| Self-assessment mistaken for certification | compliance-mapping.md is explicit it is not an attestation; this doc's exit criteria require *external* validation |
| PII item blocked on a non-existent resource | Scope it contingent on the `User` resource; don't claim coverage that has no target |

---

# 8. Related Documents

- [`01-product/prd-future.md`](../../01-product/prd-future.md) §5.3 — requirements
- [`13-security/encryption.md`](../../13-security/encryption.md#5-key-management)
- [`13-security/compliance-mapping.md`](../../13-security/compliance-mapping.md) (incl. §7 residual risk)
- [`18-roadmap/v1.0.md`](../v1.0.md) — Security row + §5 exit criteria

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-09 | Closed the remaining two §4.1 hardening-pass residual findings: `kms.json` is now owner-only after every write (already landed separately), and the non-Unix root-key/`kms.json` ACL gap is closed via a new shared `apex_common::fs::restrict_to_owner` primitive (`icacls /inheritance:r /grant:r` on Windows), authored and proven live on a real Windows host rather than left as a documented-but-unbuildable gap. Cloud-KMS/HSM and external validation remain |
| 1.1.0 | 2026-07-09 | Closed the anonymous default-tenant RBAC bypass (§3 item 3 / §4.1's hardening pass): deleted `tenant_authorize`'s short-circuit in `crates/apex-server/src/tenancy.rs` so `APEX_ALLOW_ANONYMOUS=1` no longer implies any RBAC grant, only authentication pass-through. Migrated ~10 tests that relied on the old permissive behavior to a real `APEX_PLATFORM_ADMINS` principal. `kms.json` permissions and the non-Unix root-key ACL remain open, as does cloud-KMS/HSM and external validation |
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for security; records the live+pen-tested+mapped KMS slice and scopes the root-of-trust, PII, hardening, and external-validation remainder |
