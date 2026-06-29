<!--
File: docs/07-tool-runtime/security-isolation.md
Document ID: TRT-005
-->

# Tool Runtime Security & Isolation

**Document ID:** TRT-005  
**File Path:** `docs/07-tool-runtime/security-isolation.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **security model** of the Tool Runtime: the threat model, how untrusted code is contained, how network and filesystem access are constrained, how secrets are injected, and how tenants are isolated.

Tools are the platform's most dangerous surface — they run arbitrary, often third-party code. The Runtime treats every tool as **untrusted by default** and enforces least privilege end to end.

---

# 2. Threat Model

| Threat | Mitigation |
|--------|-----------|
| Malicious tool code | Sandboxing; default-deny network/fs; ephemeral execution |
| Sandbox escape | Strong backends (gVisor/microVM) for untrusted; hardened nodes |
| Data exfiltration | Egress allowlists; no ambient credentials; output size caps |
| Cross-tenant access | Per-tenant pools, namespaces, and storage; no reuse across tenants |
| Secret theft | In-memory injection; never logged; zeroed on teardown |
| Resource exhaustion (DoS) | Hard CPU/mem/disk/time/PID limits; fair scheduling |
| Supply-chain attack | Image signing + provenance verification |
| Privilege escalation | Non-root execution; dropped capabilities; read-only system paths |

---

# 3. Trust Classification

Every tool carries a trust class that drives isolation strength:

| Class | Source | Minimum backend |
|-------|--------|-----------------|
| First-party | Built by the platform team | native / wasm |
| Verified | Third-party, reviewed + signed | container / gVisor |
| Untrusted | Unreviewed / user-supplied | gVisor / microVM |

Tenant policy sets a **floor**: it may require a stronger backend than the tool's
class implies, never weaker (see
[Sandbox Runtime §3](sandbox-runtime.md#3-backend-selection)).

---

# 4. Authorization

Before execution, the Permission Engine evaluates the caller's grants against the
tool's required permissions
([Tool Framework §26–28](../04-agent-framework/tool-framework.md#26-permission-engine))
through the [Policy Engine](../04-agent-framework/policy-engine.md):

```text
Caller grants  ∩  Tool required permissions  ⊆  Policy allow  ⇒ execute
                                              else            ⇒ forbidden
```

Authorization is **fail-closed**: any evaluation error denies execution.

---

# 5. Network Isolation

Default policy is **deny-all egress**. Tools declare required destinations
([Tool Framework §34](../04-agent-framework/tool-framework.md#34-network-policies)),
enforced by the Runtime:

- An egress proxy / network policy allows only declared hosts/ports.
- DNS is restricted to allowed domains (prevents DNS exfiltration).
- No inbound connectivity to sandboxes.
- Per-tenant egress is metered; anomalous volume raises alerts.
- Untrusted tools route egress through an inspecting proxy.

```yaml
network:
  default: deny
  outbound_allow:
    - api.example.com:443
  dns_allow:
    - api.example.com
  inbound: deny
```

---

# 6. Filesystem Isolation

- Fresh, isolated root per execution; only declared paths mounted
  ([Tool Framework §35](../04-agent-framework/tool-framework.md#35-filesystem-policies)).
- Workspace is per-execution scratch, wiped on teardown.
- System paths mounted read-only; no host path is ever bind-mounted into untrusted
  sandboxes.
- Output is read back through a bounded channel (`max_output_bytes`), not by the
  host reading sandbox disk.

---

# 7. Secret Management

Tools never see raw long-lived credentials. The Secret Injector:

```text
1. Resolve secret references from the tool's grants (Secret Vault)
2. Mint short-lived, scoped credentials where possible
3. Inject into the sandbox via in-memory env / tmpfs (never persistent disk)
4. Zero secrets on teardown; never write them to logs or audit
```

Aligned with
[Tool Framework §29–30](../04-agent-framework/tool-framework.md#29-secret-management).
Secrets are scoped to the single execution and revoked after.

---

# 8. Tenant Isolation

- Workers are pooled **per trust class**, and sandboxes are **never reused across
  tenants** (see [Worker Pool §3](worker-pool.md#3-worker-classes)).
- Scratch, network namespaces, and caches are tenant-scoped.
- Result caches and execution records are tenant-partitioned.
- Cross-tenant isolation is a **hard requirement**, verified by tests
  (zero leakage), per [Tool Framework §70](../04-agent-framework/tool-framework.md#70-security-requirements).

---

# 9. Supply Chain & Provenance

For third-party tools and images:

- Artifacts are content-addressed and pulled by digest.
- Signatures are verified (e.g. Sigstore-style) before a tool may run.
- Provenance/SBOM is recorded; tools failing verification are quarantined.
- A tool's manifest, permissions, and image are pinned per version so a published
  version cannot silently change.

---

# 10. Non-Root & Capability Dropping

- Tools run as an unprivileged user inside the sandbox.
- Linux capabilities are dropped to the minimum; `no-new-privileges` is set.
- seccomp/syscall filtering restricts the syscall surface (especially under
  gVisor/microVM).

---

# 11. Audit

Every execution writes a tamper-evident audit record:

```json
{
  "execution_id": "exec_01H...",
  "tenant": "acme", "principal": "agent:order-assistant",
  "tool": "http.request", "version": "1.2.0",
  "input_hash": "sha256:...", "egress": ["api.example.com"],
  "sandbox": "gvisor", "status": "succeeded",
  "secrets_used": ["ref:example-api-key"],
  "timestamp": "2026-06-27T10:00:00Z"
}
```

Inputs are hashed (not stored raw) unless policy requires retention; secrets are
referenced, never valued.

---

# 12. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Authorization decision | < 5 ms p95 |
| Secret injection | < 5 ms p95 |
| Cross-tenant leakage | 0 (hard) |
| Egress policy enforcement | 100% of executions |

---

# 13. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#26-permission-engine)
- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`13-security`](../SUMMARY.md) *(planned: platform security)*

---

# 14. Related Documents

- [`07-tool-runtime/overview.md`](overview.md)
- [`07-tool-runtime/worker-pool.md`](worker-pool.md)
- [`07-tool-runtime/observability-ops.md`](observability-ops.md)
- [`07-tool-runtime/e2b-gap-analysis.md`](e2b-gap-analysis.md) — persistent
  sessions must preserve these isolation guarantees (deny-by-default egress,
  trust-class floors), not relax them for convenience.

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Security & Isolation specification |
