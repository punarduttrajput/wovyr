<!--
File: docs/15-testing/security-testing.md
Document ID: TEST-006
-->

# Security Testing

**Document ID:** TEST-006  
**File Path:** `docs/15-testing/security-testing.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Quality Engineering Team · Security Team  
**Last Updated:** 2026-06-27

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

---

# 4. Tenant Isolation Tests

Automated tests assert **zero cross-tenant leakage** — a hard requirement — across:

- [Memory](../06-memory-engine/storage-architecture.md#10-tenant-isolation) (queries, vectors, cache)
- [Tool Runtime](../07-tool-runtime/security-isolation.md#8-tenant-isolation) (sandboxes, results)
- API resource access

A test that surfaces another tenant's data is a release blocker.

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

---

# 7. Supply-Chain Tests

- Unsigned / tampered plugin packages are **rejected** on install
  ([distribution](../08-plugin-sdk/distribution.md#7-install--pull-flow)).
- Revoked versions are force-disabled.
- SBOM/provenance policy enforcement is exercised.

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
