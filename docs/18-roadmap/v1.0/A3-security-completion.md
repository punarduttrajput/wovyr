<!--
File: docs/18-roadmap/v1.0/A3-security-completion.md
Document ID: GA-003
-->

# GA Completion: Security — Root-of-Trust, PII Coverage & External Validation

**Document ID:** GA-003
**File Path:** `docs/18-roadmap/v1.0/A3-security-completion.md`
**Version:** 1.0.0
**Status:** In progress — KMS live + pen-tested + compliance-mapped; root-of-trust and external validation remain
**Owner:** Security Team
**Last Updated:** 2026-07-05

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
- **Residual findings are documented, not silently carried**
  ([compliance-mapping §7](../../13-security/compliance-mapping.md#7-residual-risk-and-gaps)):
  the anonymous default-tenant RBAC bypass reaching `kms:admin`, `kms.json` file
  permissions, and the non-Unix root-key ACL.
- **The root key is a single-host stand-in.** `LocalKms` holds it in-process
  (`root::from_env` / `root::from_file`); no cloud-KMS/HSM backing.

---

# 3. Gap

1. No **cloud-KMS-/HSM-backed root** — production deployments need a managed
   root-of-trust, not an in-process key.
2. **PII field encryption** covers secrets/memory/webhook-secrets but not future
   PII resources (e.g. a `User.email` — [users.md](../../09-api/users.md)).
3. The documented **residual findings** are unaddressed by design (scoped out of
   the pen-test slice, deferred to a hardening pass).
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
- A **scoped hardening pass** closing the residual findings — notably narrowing
  the anonymous default-tenant bypass (a *systemic* change across every
  tenant-scoped route, deliberately deferred from the pen-test slice).
- Engagement of an **external penetration test** and a **formal
  compliance-mapping audit**.

## 4.2 Non-functional
- The hardening pass must preserve back-compat where the anonymous default-tenant
  mode is currently relied upon, or migrate callers deliberately.
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
| Anonymous-bypass fix breaks back-compat callers | Deliberate migration; the documented-gap test guards the current behavior until then |
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
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for security; records the live+pen-tested+mapped KMS slice and scopes the root-of-trust, PII, hardening, and external-validation remainder |
