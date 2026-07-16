<!--
File: docs/08-plugin-sdk/sandbox.md
Document ID: PLG-004
-->

# Plugin Sandbox & Loading

**Document ID:** PLG-004  
**File Path:** `docs/08-plugin-sdk/sandbox.md`  
**Version:** 1.1.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-16

---

# 1. Purpose

This document defines how plugin code is **loaded and isolated**. Plugins extend a privileged platform with third-party code, so isolation is mandatory: a plugin must not be able to reach anything it was not granted, nor destabilize the host.

It builds on the [Tool Runtime sandbox model](../07-tool-runtime/sandbox-runtime.md)
(which isolates tool *executions*) and extends isolation to the other capability
kinds.

---

# 2. Why Plugins Are Isolated

| Risk | Consequence without isolation |
|------|-------------------------------|
| Malicious plugin | Host compromise, data theft |
| Buggy plugin | Crash or resource exhaustion of a core service |
| Over-reach | Access to ungranted secrets/data |
| Supply-chain tampering | Trojaned capability runs with platform trust |

Isolation contains all four. Permission grants ([Permissions](permissions.md))
define *what* is allowed; the sandbox *enforces* it.

---

# 3. Isolation by Capability Kind

Different capability kinds run in different places, so isolation differs:

| Kind | Host | Isolation |
|------|------|-----------|
| `tool` | [Tool Runtime](../07-tool-runtime/index.md) | Per-execution sandbox (native→microVM) |
| `provider` | [LLM Gateway](../05-llm-gateway/index.md) | WASM/process adapter; egress allowlist |
| `memory_backend` | [Memory Engine](../06-memory-engine/index.md) | WASM/process adapter; scoped store access |
| `workflow_activity` | [Tool Runtime](../07-tool-runtime/index.md) | Per-execution sandbox |
| `policy` | [Policy Engine](../04-agent-framework/policy-engine.md) | Pure, sandboxed evaluation (no I/O) |

Tool and activity capabilities reuse the Tool Runtime's
[isolation backends](../07-tool-runtime/sandbox-runtime.md#2-isolation-backends)
verbatim; this document focuses on how the *other* kinds are contained.

---

# 4. Loading Models

| Model | Description | Used for |
|-------|-------------|----------|
| In-process (native) | Linked/dynamically loaded into the host | First-party, fully trusted only |
| WASM | Plugin compiled to WebAssembly, run in an embedded VM | Default for verified plugins |
| Out-of-process | Plugin runs as a separate process/service, host calls over IPC/gRPC | Heavy or untrusted capabilities |
| Sandboxed (gVisor/microVM) | Strong OS/HW isolation | Community/untrusted code |

**WASM is the default** loading model: capability-based, deterministic, memory-safe,
language-portable, and cheap to instantiate. Native in-process loading is reserved
for first-party plugins.

**Implemented today** (`apex-plugin`'s `runtime` module): the WASM loader
(`WasiCapabilityRuntime`, behind the `wasi` cargo feature) and the container loader
(`ContainerCapabilityRuntime` — Docker/Podman, gVisor via `runsc`; always compiled,
ECO-303). Both speak the same capability ABI (request JSON on stdin → response JSON
on stdout, `APEX_SECRET_*` env injection); a capability picks its loader via the
manifest's `sandbox` field (`wasm` vs `container`/`gvisor`), and each loader refuses
the other's capabilities fail-closed — a `gvisor` capability is refused by a
plain-Docker runtime rather than demoted. The out-of-process (IPC/gRPC) and microVM
models remain future work.

---

# 5. WASM Host Interface

WASM plugins interact with the host only through an explicit, capability-gated host
interface — there is no ambient syscall access.

```text
WASM plugin
   │ imports (host functions, gated by grants)
   ▼
Host shim ──► net.egress(host, req)      [requires net:egress grant]
          ──► secret.read(ref)           [requires secret:read grant]
          ──► memory.query(q)            [requires memory:read grant]
          ──► log/metric (always)
```

Each host function checks the plugin's grants before acting
([Permissions §7](permissions.md#7-runtime-enforcement)). An ungranted import call
is denied and audited.

---

# 6. Resource Limits

Plugin execution is bounded like any other untrusted workload:

| Resource | Enforced via |
|----------|-------------|
| CPU / fuel | WASM fuel metering or cgroup quota |
| Memory | WASM linear-memory cap / cgroup `memory.max` |
| Time | Execution timeout → cancel |
| Egress | Network allowlist (granted hosts only) |
| Concurrency | Per-plugin and per-tenant caps |

Limits mirror the [Tool Runtime resource enforcement](../07-tool-runtime/sandbox-runtime.md#5-resource-enforcement).

---

# 7. Lifecycle Isolation

- A plugin's failure (panic, OOM, timeout) is contained to its sandbox and surfaced
  as a capability error — the host service stays healthy.
- Disabling or uninstalling a plugin tears down its sandboxes and unregisters its
  host imports.
- Crashing plugins are subject to circuit-breaking: repeated failures auto-disable
  the capability and alert operators.

---

# 8. Tenant Isolation

- Plugin instances and their state are tenant-scoped; one tenant's plugin
  invocation cannot observe another's.
- Sandboxes are never reused across tenants (consistent with
  [Tool Runtime §8](../07-tool-runtime/security-isolation.md#8-tenant-isolation)).

---

# 9. Trust → Isolation Mapping

| Trust class | Default loading model |
|-------------|-----------------------|
| First-party | In-process / native |
| Verified | WASM |
| Community / untrusted | Out-of-process + gVisor/microVM |

Tenant policy may require a stronger model than the default, never weaker.

---

# 10. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| WASM instantiation | < 2 ms p95 |
| Host-call permission check | < 1 ms |
| Failure containment | 100% (no host crash from plugin fault) |
| Cross-tenant leakage | 0 (hard) |

---

# 11. Dependencies

- [`07-tool-runtime/sandbox-runtime.md`](../07-tool-runtime/sandbox-runtime.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md)
- [`08-plugin-sdk/permissions.md`](permissions.md)

---

# 12. Related Documents

- [`08-plugin-sdk/overview.md`](overview.md)
- [`08-plugin-sdk/plugin-api.md`](plugin-api.md)
- [`08-plugin-sdk/distribution.md`](distribution.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-16 | §4: noted the implemented loading models — the WASM loader (`wasi` feature) and the new container/gVisor loader (`ContainerCapabilityRuntime`, ECO-303) with their shared stdin/stdout ABI and fail-closed loader routing |
| 1.0.0 | 2026-06-27 | Initial Plugin Sandbox & Loading specification |
