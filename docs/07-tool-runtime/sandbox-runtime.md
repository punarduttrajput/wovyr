<!--
File: docs/07-tool-runtime/sandbox-runtime.md
Document ID: TRT-003
-->

# Tool Runtime Sandbox Runtime

**Document ID:** TRT-003  
**File Path:** `docs/07-tool-runtime/sandbox-runtime.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies how the Tool Runtime **provisions and operates sandboxes** — the isolated environments in which tools execute. It covers the supported isolation backends, their tradeoffs, the sandbox lifecycle, resource enforcement, and warm pooling.

The [Tool Framework §31–36](../04-agent-framework/tool-framework.md#31-sandboxing)
defines sandbox *types and policy schemas*. This document defines how the Runtime
*implements* them as a fleet operation.

---

# 2. Isolation Backends

The Sandbox Manager supports a spectrum of backends, trading startup cost for
isolation strength:

| Backend | Isolation | Cold start | Use |
|---------|-----------|-----------|-----|
| Native process | OS process + namespaces | ~ms | Trusted first-party tools |
| WASI / WASM | Capability-based, in-process VM | sub-ms | Pure, deterministic tools |
| Container (Docker/OCI) | Namespaces + cgroups | 100s ms | General third-party tools |
| gVisor | User-space kernel | ~100 ms | Untrusted tools needing syscalls |
| Firecracker microVM | Hardware-virtualized | ~125–200 ms | Strongly untrusted tools |
| Kubernetes Pod | Pod + policies | seconds | Heavy / clustered tools |
| Remote worker | Network-isolated pool | network RTT | Third-party / data-residency |

The backend is selected per tool from its manifest, overridable by tenant policy
(a tenant may force a stronger backend than the tool requests, never weaker).

---

# 3. Backend Selection

```text
1. Read tool manifest sandbox preference
2. Apply tenant policy floor (minimum isolation level)
3. Apply trust classification (first-party / verified / untrusted)
4. Choose strongest of (preference, policy floor, trust requirement)
5. Check worker capability (node supports the backend)
```

Untrusted or unverified tools are floored to gVisor or microVM regardless of
their stated preference. See [Security & Isolation](security-isolation.md).

---

# 4. Sandbox Lifecycle

Implements the framework's
[lifecycle](../04-agent-framework/tool-framework.md#32-sandbox-lifecycle):

```text
Allocate ─► Initialize ─► Inject Context ─► Execute ─► Collect ─► Destroy ─► Cleanup
```

| Stage | Runtime action |
|-------|----------------|
| Allocate | Acquire a warm sandbox or provision a new one |
| Initialize | Apply cgroups/limits, mount allowed paths, set env |
| Inject Context | Pass execution context + secrets (memory, not disk) |
| Execute | Run the tool entrypoint with the timeout armed |
| Collect | Capture stdout/stderr/structured output |
| Destroy | Terminate process/VM; never reuse for another tenant |
| Cleanup | Reclaim disk, scratch, network namespace; zero secrets |

**Ephemeral by default**: a sandbox serves exactly one execution and is destroyed.
Reuse across executions is only allowed within the same tenant+tool for trusted,
pooled backends (see [Warm Pooling](#7-warm-pooling)).

---

# 5. Resource Enforcement

Limits from the tool manifest / request
([Tool Framework §33](../04-agent-framework/tool-framework.md#33-resource-limits))
are enforced by the backend's primitives:

| Resource | Enforced via |
|----------|-------------|
| CPU | cgroup `cpu` quota / vCPU cap |
| Memory | cgroup `memory.max` / VM memory; OOM-kill on breach |
| Disk | scratch quota / ephemeral volume size |
| PIDs | cgroup `pids.max` |
| Time | runtime timeout → SIGTERM → SIGKILL |
| File descriptors | rlimit |
| Egress | network policy (see [Security & Isolation](security-isolation.md)) |

Breaching a hard limit terminates the sandbox and returns `resource_exceeded`
([Execution API §10](execution-api.md#10-error-model)).

---

# 6. Filesystem & Process Model

- Each sandbox gets a **fresh, isolated root** with only explicitly allowed paths
  mounted (e.g. `/workspace`, `/tmp`), per
  [Tool Framework §35](../04-agent-framework/tool-framework.md#35-filesystem-policies).
- `/usr`, `/etc` and similar are mounted **read-only** when present.
- Each execution runs in a **dedicated PID namespace** with no visibility into
  host or sibling processes
  ([Tool Framework §36](../04-agent-framework/tool-framework.md#36-process-isolation)).
- Scratch space is wiped on destroy; nothing persists between executions.

---

# 7. Warm Pooling

Cold starts (especially microVMs) add latency. The Runtime maintains **warm pools**
of pre-initialized sandboxes:

```text
Pool per (backend, image, tenant-class)
   │
   ├─ pre-warmed, idle sandboxes
   ▼
On invoke: take warm sandbox ─► inject context ─► execute
   │
   └─ on destroy: discard (untrusted) OR return to pool (trusted, reset)
```

Rules:

- Warm sandboxes are **never shared across tenants**; pools are tenant-class scoped.
- Untrusted backends are **discarded after one use**, not returned to a pool.
- Pool size adapts to demand (see [Worker Pool §6](worker-pool.md#6-autoscaling)).
- A pooled sandbox is reset (scratch wiped, env reset) before reuse for the same
  tenant.

Snapshot/restore (e.g. Firecracker snapshots) is a planned optimization for
near-instant cold starts (see [Overview §15](overview.md#15-future-enhancements)).

---

# 8. Image & Dependency Management

- Tool images/artifacts are content-addressed and pulled from the artifact store,
  verified by digest before use.
- Images are cached per node; first use on a node pays the pull cost.
- Image provenance and signatures are checked for third-party tools (supply-chain
  protection) — see [Security & Isolation §9](security-isolation.md#9-supply-chain--provenance).

---

# 9. Failure & Recovery

| Failure | Behavior |
|---------|----------|
| Provision failure | Retry on another worker, then `sandbox_unavailable` |
| Sandbox crash | Capture diagnostics; return `tool_error` |
| OOM / limit breach | Kill; return `resource_exceeded` |
| Hung tool | Timeout → SIGTERM → SIGKILL; `timeout` |
| Worker node loss | Sandbox lost; reschedule if idempotent |

All teardown paths guarantee secret zeroing and resource reclamation, even on
crash, via a reaper that sweeps orphaned sandboxes.

---

# 10. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Warm sandbox start | < 20 ms p95 |
| Cold microVM start | < 200 ms p95 |
| WASM instantiation | < 2 ms p95 |
| Teardown + reclaim | < 30 ms p95 |
| Orphan sweep interval | < 10 s |

---

# 11. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#31-sandboxing)
- [`07-tool-runtime/worker-pool.md`](worker-pool.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)

---

# 12. Related Documents

- [`07-tool-runtime/overview.md`](overview.md)
- [`07-tool-runtime/execution-api.md`](execution-api.md)
- [`02-architecture/deployment-architecture.md`](../02-architecture/deployment-architecture.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Sandbox Runtime specification |
