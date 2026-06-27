<!--
File: docs/07-tool-runtime/overview.md
Document ID: TRT-001
-->

# Tool Runtime Overview

**Document ID:** TRT-001  
**File Path:** `docs/07-tool-runtime/overview.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **Tool Runtime**, the deployable service that executes tools for agents and workflows in the Apex AI Platform.

The Runtime is the operational counterpart to the [Tool Framework](../04-agent-framework/tool-framework.md). The framework defines the tool model, SDK, manifest, registry, and permission/sandbox *schemas*; the Runtime **provisions sandboxes, enforces policy, runs the tool, and returns the result** — safely and at scale.

---

# 2. Scope

The Tool Runtime is responsible for:

- A network API to invoke, stream, and cancel tool executions
- Resolving tools/versions via the registry and routing to workers
- Authorizing each execution against policy
- Provisioning isolated sandboxes per execution
- Enforcing CPU/memory/disk/time/network limits
- Injecting secrets and scoped credentials
- Rate limiting and fair scheduling across tenants
- Emitting audit, metrics, and traces

The Tool Runtime is **not** responsible for:

- Defining the tool model or SDK — see [Tool Framework](../04-agent-framework/tool-framework.md)
- Deciding *which* tool to call — that is the Agent Runtime / planner's job
- Packaging or distributing plugins — see the planned `08-plugin-sdk/`

---

# 3. Position in the Platform

```text
 Agent Runtime ─┐
 Workflow Engine├──► Tool Runtime ──► Sandbox backends (process / wasm / container / microVM)
 Dashboard      ┘        │          ──► Tool Registry (resolve + version)
                         │          ──► Secret Vault (scoped injection)
                         │          ──► Policy Engine (authorize)
                         └── execution events / audit ──► Event Bus
```

The Runtime is horizontally scalable. A stateless **control plane** (API +
dispatcher + scheduler) fronts a **data plane** of execution **workers** that own
sandboxes. See [C4 Container §4.6](../02-architecture/c4-container.md) and
[Worker Pool](worker-pool.md).

---

# 4. Control Plane vs. Data Plane

| Plane | Components | Scaling |
|-------|-----------|---------|
| Control plane | Execution API, Dispatcher, Permission Engine, Scheduler | Horizontal, stateless |
| Data plane | Worker Pool, Sandbox Manager, Runtime Adapters | Horizontal, capacity-bound |

Separating them lets the API stay highly available while heavy/untrusted
execution is isolated on workers that can be drained, recycled, or pinned to
hardened nodes.

---

# 5. Core Responsibilities

## 5.1 Dispatch

The Dispatcher resolves the requested tool and version against the
[registry](../04-agent-framework/tool-framework.md#12-tool-registry), selects an
appropriate worker (by capability, locality, and load), and routes the execution.

## 5.2 Authorization

Before any code runs, the Permission Engine evaluates the caller's grants against
the tool's required permissions via the
[Policy Engine](../04-agent-framework/policy-engine.md). Denied executions never
reach a sandbox.

## 5.3 Sandbox Provisioning

The Sandbox Manager allocates an isolated environment using the configured backend
(native process, WASI, container, microVM, gVisor, K8s pod, or remote worker) and
applies resource/network/filesystem policy. See [Sandbox Runtime](sandbox-runtime.md).

## 5.4 Execution

A Runtime Adapter injects context and secrets, runs the tool, enforces timeouts,
streams output, and collects the result. See [Execution API](execution-api.md).

## 5.5 Governance

Every execution is rate-limited, metered, audited, and tenant-isolated. See
[Security & Isolation](security-isolation.md).

---

# 6. Execution Lifecycle

```text
1. Receive invocation          (REST / gRPC)
2. Authenticate + resolve tenant/principal
3. Resolve tool + version      (Registry)
4. Authorize                   (Permission Engine → Policy Engine)
5. Pre-check rate limit + quota
6. Schedule onto a worker
7. Provision sandbox           (apply resource/network/fs policy)
8. Inject context + secrets
9. Execute (stream output, enforce timeout)
10. Collect result / handle error
11. Destroy sandbox + cleanup
12. Emit audit + metrics + execution event
13. Return result + usage
```

---

# 7. Deployment Modes

| Mode | Description |
|------|-------------|
| Embedded | Runtime runs in-process in the all-in-one dev binary (native sandbox only) |
| Standalone | Control plane + worker pool as separate services (enterprise default) |
| Node-local | A worker per node (DaemonSet) for data-locality-sensitive tools |
| Remote pool | Dedicated hardened worker fleet for untrusted/third-party tools |

The Execution API contract is identical across modes. See
[Deployment Architecture](../02-architecture/deployment-architecture.md).

---

# 8. Module Organization

```text
service-tool-runtime/
├── api/            # REST + gRPC handlers (invoke / stream / cancel)
├── dispatcher/     # tool resolution + worker routing
├── permissions/    # authorization (Policy Engine client)
├── scheduler/      # fair scheduling, concurrency, queueing
├── worker/         # execution worker (data plane)
├── sandbox/        # sandbox manager + adapters
│   ├── native/
│   ├── wasm/
│   ├── container/
│   ├── microvm/
│   └── remote/
├── secrets/        # scoped secret injection
├── governance/     # rate limits, quotas, tenant isolation, audit
├── telemetry/      # logs, metrics, traces
└── main.rs
```

The `sandbox/` adapters implement the backends enumerated in
[Tool Framework §31](../04-agent-framework/tool-framework.md#31-sandboxing).

---

# 9. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Dispatch + authorize overhead | < 10 ms p95 |
| Warm sandbox start (pooled) | < 20 ms p95 |
| Cold sandbox start (microVM) | < 200 ms p95 |
| Concurrent executions per worker | hundreds (backend-dependent) |
| Availability (control plane) | 99.99% |
| Cross-tenant isolation | hard (zero leakage) |

---

# 10. Failure Behavior

| Failure | Behavior |
|---------|----------|
| Tool not found / version missing | `404 tool_not_found`, no execution |
| Authorization denied | `403 forbidden`, no execution |
| Sandbox provisioning failure | Retry on another worker, then `503` |
| Timeout | Cancel sandbox, return `timeout` (retryable) |
| Tool crash / non-zero exit | Return `tool_error` with captured diagnostics |
| Worker death mid-execution | Reschedule if idempotent; else surface failure |
| Resource limit exceeded | Kill sandbox, return `resource_exceeded` |

Retry semantics integrate with the
[Retry Engine](../03-workflow-engine/retry-engine.md); only idempotent tools are
retried automatically.

---

# 11. Security

- All tools run sandboxed; nothing executes on the host or control plane.
- Default-deny network and filesystem; access is explicitly granted per tool.
- Secrets are injected into the sandbox at runtime and never logged.
- Untrusted/third-party tools run on hardened, isolated worker pools.
- Every execution is audited (tenant, principal, tool, version, inputs hash, result).

See [Security & Isolation](security-isolation.md) and
[Tool Framework §70](../04-agent-framework/tool-framework.md#70-security-requirements).

---

# 12. Observability

Each execution emits logs, metrics (queue time, start latency, run duration,
success/error rate, resource usage), OpenTelemetry traces, and an audit record.
Execution events publish to the [Event Bus](../03-workflow-engine/event-bus.md).
See [Observability & Ops](observability-ops.md).

---

# 13. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`03-workflow-engine/retry-engine.md`](../03-workflow-engine/retry-engine.md)
- [`03-workflow-engine/distributed-execution.md`](../03-workflow-engine/distributed-execution.md)

---

# 14. Related Documents

- [`07-tool-runtime/execution-api.md`](execution-api.md)
- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`07-tool-runtime/worker-pool.md`](worker-pool.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`07-tool-runtime/observability-ops.md`](observability-ops.md)

---

# 15. Future Enhancements

- Snapshot/restore sandboxes for sub-millisecond cold starts
- GPU-aware scheduling for ML tools
- Per-tool warm pools driven by demand prediction
- WASM component model for portable tools
- Cross-region execution routing for data residency

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Overview |
