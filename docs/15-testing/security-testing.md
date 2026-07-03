<!--
File: docs/15-testing/security-testing.md
Document ID: TEST-006
-->

# Security Testing

**Document ID:** TEST-006  
**File Path:** `docs/15-testing/security-testing.md`  
**Version:** 1.4.0  
**Status:** Partially implemented — automated coverage exists for the authorization
matrix, tenant isolation, secrets, the supply chain, adversarial sandbox/isolation
testing including true in-guest escape attempts against the strong backends
(egress-proxy bypass, filesystem escape, PID/forkbomb containment, plugin
host-call denial, gVisor mount/`/proc/kcore` escape denial, Firecracker guest-OOM
containment — §5 fully covered), and the CI scanning pipeline (§8: dependency
audit, secret scanning, container image scanning — see the per-section
**Implemented** notes). Remaining: fuzz-target infrastructure (§8's last open row).  
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

**Implemented:** `apex-tools` `egress_adversarial.rs` drives the `EgressProxy`
directly (no `docker` needed, runs unconditionally) with adversarial CONNECT
traffic — an IP-literal dial of an allow-listed *hostname* (denied, since the
allow-list is a string match, not a resolved-address match), a non-CONNECT method
used to sidestep host-checking (405), a malformed empty-target CONNECT (denied,
proxy stays alive for the next client), and an oversized/unterminated header
flood (rejected promptly, no hang or unbounded buffering). `sandbox_backends.rs`
(docker-gated) adds two filesystem-escape attempts —
`container_read_only_rootfs_denies_writes_outside_workspace` (a write outside
`/workspace` fails on the `--read-only` rootfs) and
`container_workspace_mount_does_not_expose_host_sibling_directory` (`..`
traversal from `/workspace` can't reach an unmounted sibling host directory) —
plus a resource-limit adversarial test,
`container_pids_limit_contains_a_fork_bomb` (a `--pids-limit` cap survives 40
attempted forkbomb forks, keeping the live process count far below the attempt
count). `apex-plugin` `engine.rs`
`ungranted_capability_is_denied_before_the_runtime_is_ever_invoked` proves the
plugin host-call denial is enforced *before* the capability runtime is ever
invoked (a counting spy runtime records zero invocations), not just that its
result is discarded. Known, explicitly out-of-scope gap: a container on
`bridge` networking can bypass the egress proxy entirely by ignoring
`HTTPS_PROXY` and dialing out directly — the documented "L3 egress
bypass-blocking" item, deferred past v0.3. **True in-guest escape attempts
against the strong backends have now landed too:**
`gvisor_denies_privileged_mount_syscall` (a compromised guest attempting to
`mount` — a classic escape/pivot primitive — is denied by gVisor's sentry
intercepting the syscall in its own user-space kernel rather than the host) and
`gvisor_denies_reading_host_physical_memory_via_proc_kcore` (`/proc/kcore`, a
known container-escape/info-leak vector exposing physical memory, is
inaccessible under gVisor's synthetic procfs); `firecracker_memory_ceiling_contains_a_guest_oom`
proves the microVM's `mem_size_mib` is a real hardware-virtualized ceiling — a
runaway process is OOM-killed by the guest's own kernel well before the
wall-clock timeout, mirroring the container backend's cgroup memory test but
for the VM boundary itself. §5 is now fully
covered.

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

**Implemented:** [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) — the
existing `rust` job's `cargo clippy --workspace --all-targets -- -D warnings` step
*is* the static-analysis gate (already blocking merge). Two new jobs close the
remaining rows: **`security`** runs `rustsec/audit-check` (SCA — the RustSec
advisory DB against `Cargo.lock`, posted as a check run) and a standalone
`gitleaks detect --no-git` pass over the working tree (secret scanning, run as
the plain binary rather than the wrapper Action to avoid its
organizational-use license gate); **`container-scan`** builds
`deployment/docker/Dockerfile` (the single-binary image's first real CI build —
previously only built manually) and runs `aquasecurity/trivy-action` against it,
failing on HIGH/CRITICAL vulnerabilities with a known fix available
(`ignore-unfixed: true`, since an unfixed CVE isn't actionable). Fuzzing remains
deferred (no proptest/fuzz-target infrastructure exists yet — see
[unit-tests.md §7](unit-tests.md#7-property--fuzz-testing)); this is the one row
still open. Not yet run against a live GitHub Actions environment (developed and
reasoned about offline) — the first real run should be watched for false
positives, particularly gitleaks against the crypto test fixtures in
`apex-plugin`'s `keyless`/`verify` modules and `deployment/rekor/`.

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
| 1.4.0 | 2026-07-03 | True in-guest escape attempts against the strong backends (§5, closing the section): `gvisor_denies_privileged_mount_syscall` and `gvisor_denies_reading_host_physical_memory_via_proc_kcore` (`sandbox_backends.rs`) prove gVisor's sentry denies a compromised guest's `mount` attempt and blocks `/proc/kcore` physical-memory disclosure; `firecracker_memory_ceiling_contains_a_guest_oom` proves the microVM's `mem_size_mib` is a real hardware-virtualized ceiling (guest-kernel OOM, not a hang). §5 is now fully covered; remaining work is entirely in §8 (fuzzing) |
| 1.3.0 | 2026-07-03 | CI scanning pipeline (§8) landed: `.github/workflows/ci.yml` gained a `security` job (RustSec `cargo-audit` dependency check + a standalone `gitleaks --no-git` secret scan) and a `container-scan` job (builds `deployment/docker/Dockerfile` — its first real CI build — and runs Trivy against the image, failing on HIGH/CRITICAL fixable CVEs). Static analysis was already covered by the existing clippy gate. Not yet run against live GitHub Actions; fuzzing remains the one open row in §8 |
| 1.2.0 | 2026-07-03 | First slice of adversarial sandbox-escape tests (§5): egress-proxy bypass attempts (`apex-tools` `egress_adversarial.rs` — IP-literal hostname bypass, non-CONNECT method smuggling, malformed CONNECT, oversized header flood), filesystem escape (`sandbox_backends.rs` — read-only rootfs write, workspace sibling-directory traversal), a PID/forkbomb containment test, and a plugin host-call-without-a-grant denial test (`apex-plugin` `engine.rs`, proving zero runtime invocations on denial). Documented the known L3 egress-bypass gap (bridge-networked container ignoring `HTTPS_PROXY`) rather than asserting a protection that doesn't exist. Remaining: true in-guest escape attempts against gVisor/Firecracker themselves |
| 1.1.0 | 2026-07-03 | Status → partially implemented: per-section notes for the RBAC default-deny matrix (+ malformed-scope hardening), tenant-isolation + spoof-rejection suite, secret masking/by-reference audit, and the supply-chain tamper battery (publisher-key + keyless). Remaining: adversarial sandbox-escape testing on the strong backends and the CI scanning pipeline (§8) |
| 1.0.0 | 2026-06-27 | Initial Security Testing specification |
