<!--
File: docs/15-testing/security-testing.md
Document ID: TEST-006
-->

# Security Testing

**Document ID:** TEST-006  
**File Path:** `docs/15-testing/security-testing.md`  
**Version:** 1.1.0  
**Status:** Partially implemented — automated coverage exists for the authorization
matrix, tenant isolation, secrets, and the supply chain (see the per-section
**Implemented** notes). Adversarial sandbox-escape testing against the strong
backends and the CI scanning pipeline (§8) remain.  
**Owner:** Quality Engineering Team · Security Team  
**Last Updated:** 2026-07-03

---

# 1. Purpose

This document defines **security testing** for the Apex AI Platform — the automated and manual testing that validates the [security](../13-security/index.md) model: authentication, authorization, isolation, secrets, and supply chain.

---

# 2. Scope

| Area | Validates |
|------|-----------|
| AuthN | Credential handling, token lifecycle, session security |
| AuthZ | RBAC/ABAC decisions, fail-closed behavior |
| Tenant isolation | No cross-tenant access anywhere |
| Sandbox isolation | Tools/plugins cannot escape or over-reach |
| Secrets | No leakage in logs/responses; rotation/revocation |
| Supply chain | Signature/provenance enforcement |

---

# 3. Authorization Test Matrix

Every protected operation is tested across principals:

```text
for each endpoint/action:
  with required scope        → allowed
  without required scope     → 403 (fail-closed)
  wrong tenant/project       → 403 / not-found (no leakage)
  expired/invalid credential → 401
```

This guards the [authorization model](../13-security/authorization.md) and
[RBAC/ABAC](../13-security/rbac.md) rules continuously.

**Implemented:** the RBAC default-deny matrix
(`apex-tenancy` `rbac_default_deny_matrix_is_a_strict_privilege_ladder` — every role
× every scope tier, asserting the Viewer < Editor < ProjectAdmin < OrgAdmin <
PlatformAdmin ladder and nothing above it), malformed-scope rejection
(`unknown_and_malformed_scopes_are_denied_for_non_admins` — a hardened `is_read`/
`is_write` refuse `":read"`/`"agents:"`/`""` so a suffix match alone never
authorizes), and the admin-boundary check (`authorize_never_leaks_across_the_admin_boundary`).
Per-route enforcement is exercised over HTTP by `apex-server` `rbac_gates_the_tenancy_lifecycle`.

---

# 4. Tenant Isolation Tests

Automated tests assert **zero cross-tenant leakage** — a hard requirement — across:

- [Memory](../06-memory-engine/storage-architecture.md#10-tenant-isolation) (queries, vectors, cache)
- [Tool Runtime](../07-tool-runtime/security-isolation.md#8-tenant-isolation) (sandboxes, results)
- API resource access

A test that surfaces another tenant's data is a release blocker.

**Implemented:** `apex-server` `agents_are_isolated_per_tenant`,
`workflows_are_isolated_per_tenant`, `memory_is_isolated_per_tenant`, and
`secrets_are_isolated_masked_and_rbac_gated` — each proves invisibility across
tenants and rejects a spoofed `X-Apex-Tenant` (a principal with no membership in the
claimed tenant → 403).

---

# 5. Sandbox & Isolation Tests

Adversarial tests attempt to break tool/plugin isolation:

- Egress to non-allowlisted hosts → blocked
  ([network isolation](../07-tool-runtime/security-isolation.md#5-network-isolation))
- Filesystem access outside granted paths → denied
- Resource-limit breaches → killed, contained
- Plugin host-call without a grant → denied
  ([plugin sandbox](../08-plugin-sdk/sandbox.md))

Untrusted-code escape attempts run against the strong backends (gVisor/microVM).

---

# 6. Secrets Tests

- Assert secrets never appear in logs, traces, audit, or API responses
  ([masking](../13-security/secret-management.md#9-masking)).
- Verify in-memory injection and **zeroing on teardown**
  ([tool secrets](../07-tool-runtime/security-isolation.md#7-secret-management)).
- Verify rotation and instant revocation disable access.

**Implemented:** `apex-secrets` masks values in `Debug`/`Display` and refuses to
serialize them (unit-tested); `apex-server`
`secrets_are_isolated_masked_and_rbac_gated` asserts the value never appears in any
create/rotate response, and `secret_mutations_are_audited` confirms secrets are
logged **by reference** (`secret://…`), never by value.

---

# 7. Supply-Chain Tests

- Unsigned / tampered plugin packages are **rejected** on install
  ([distribution](../08-plugin-sdk/distribution.md#7-install--pull-flow)).
- Revoked versions are force-disabled.
- SBOM/provenance policy enforcement is exercised.

**Implemented:** `apex-plugin` `rejects_untrusted_publisher`, `rejects_tampered_manifest`,
`rejects_missing_or_mismatched_artifact` (publisher-key mode), and the keyless tamper
battery `keyless_install_rejects_every_tampering` — tampered manifest, unpinned-CA
certificate, forged transparency-log timestamp (SET), publisher-namespace policy
violation, and a stripped bundle are each rejected at install with nothing registered.
Publish-time trust + scan gating is covered in `apex-marketplace` (signature verify,
`scan_severity_ceiling_blocks_publish_fail_closed`, `keyless_publish_*`), and the
`ProvenancePolicy` (require provenance/SBOM, trusted builders) has its own units.

---

# 8. Automated Scanning (CI)

| Scan | Tool class |
|------|-----------|
| Dependency CVEs | SCA (e.g. cargo-audit) |
| Static analysis | SAST / linters with security rules |
| Secret scanning | Pre-commit + CI secret detectors |
| Container scanning | Image vulnerability scanners |
| Fuzzing | Parsers/validators ([unit fuzz](unit-tests.md#7-property--fuzz-testing)) |

These gate the [CI pipeline](index.md#5-ci-pipeline-overview).

---

# 9. Penetration Testing & Reviews

- Periodic third-party penetration tests.
- Security review for changes touching auth, isolation, or crypto
  (ties to the project's security-review practice).
- A responsible-disclosure process for external reports.

---

# 10. Dependencies

- [`13-security/index.md`](../13-security/index.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md)
- [`08-plugin-sdk/distribution.md`](../08-plugin-sdk/distribution.md)

---

# 11. Related Documents

- [`15-testing/index.md`](index.md)
- [`15-testing/chaos-testing.md`](chaos-testing.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Security Testing specification |
| 1.1.0 | 2026-07-03 | Status → partially implemented: per-section notes for the RBAC default-deny matrix (+ malformed-scope hardening), tenant-isolation + spoof-rejection suite, secret masking/by-reference audit, and the supply-chain tamper battery (publisher-key + keyless). Remaining: adversarial sandbox-escape testing on the strong backends and the CI scanning pipeline (§8) |
