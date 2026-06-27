<!--
File: docs/07-tool-runtime/worker-pool.md
Document ID: TRT-004
-->

# Tool Runtime Worker Pool

**Document ID:** TRT-004  
**File Path:** `docs/07-tool-runtime/worker-pool.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **execution data plane** of the Tool Runtime: the fleet of workers that own sandboxes, how executions are scheduled onto them, how the fleet scales, and how long-running and distributed executions are handled.

The control plane (API + dispatcher) is covered in [Overview §4](overview.md#4-control-plane-vs-data-plane); this document covers the workers.

---

# 2. Worker Model

A **worker** is a process (or node) that:

- Registers its capabilities (supported sandbox backends, CPU/mem capacity, trust class)
- Maintains warm sandbox pools (see [Sandbox Runtime §7](sandbox-runtime.md#7-warm-pooling))
- Accepts scheduled executions up to its concurrency limit
- Reports health and load to the scheduler
- Drains gracefully on shutdown

```text
Scheduler ──► Worker A [backends: native, wasm]   load 40%
          ──► Worker B [backends: container, gvisor] load 70%
          ──► Worker C [backends: microvm] (untrusted pool) load 25%
```

---

# 3. Worker Classes

Workers are grouped into **pools by trust and capability** so untrusted work is
physically separated from trusted work:

| Pool | Backends | Runs |
|------|----------|------|
| Trusted | native, wasm | First-party, verified tools |
| Standard | container, gVisor | General third-party tools |
| Untrusted | microVM, remote | Unverified / high-risk tools |
| Specialized | GPU / high-mem nodes | ML / heavy tools |

A tool is routed only to a pool that satisfies its
[backend selection](sandbox-runtime.md#3-backend-selection) and trust floor.

---

# 4. Scheduling

The Scheduler places each execution on a worker by:

```text
1. Filter workers by required backend + pool/trust class
2. Filter by capacity (free concurrency, memory headroom)
3. Prefer data locality (cached image, near data source)
4. Balance load (least-loaded among candidates)
5. Reserve a slot; dispatch
```

If no worker is immediately available, the execution is **queued** (per-tenant
queue with a bound) rather than rejected, up to a max wait after which it returns
`sandbox_unavailable`.

---

# 5. Fair Scheduling & Concurrency

To prevent one tenant from starving others:

- Each tenant/project has a **concurrency share** and a queue.
- The scheduler uses **weighted fair queueing** across tenants.
- Per-tool concurrency caps prevent a single hot tool from dominating.
- Priority classes (e.g. interactive vs. batch) bias ordering within fairness
  bounds.

Concurrency limits compose with [rate limiting](security-isolation.md) and the
framework's [rate limiting](../04-agent-framework/tool-framework.md#67-rate-limiting).

---

# 6. Autoscaling

The fleet scales on observed demand:

```text
Signals: queue depth, queue wait time, pool utilization, warm-pool hit ratio
   │
   ▼
Scale workers (HPA / cluster autoscaler) per pool
Scale warm sandbox pools within each worker
```

- **Worker autoscaling** adds/removes worker nodes per pool (e.g. Kubernetes HPA +
  cluster autoscaler).
- **Warm-pool autoscaling** tunes pre-warmed sandbox counts to hit start-latency
  SLOs without wasting capacity.
- Scale-down **drains** workers (finish in-flight, refuse new) before termination.

---

# 7. Parallel & Composed Executions

The Runtime supports the framework's
[composition](../04-agent-framework/tool-framework.md#61-tool-composition),
[chaining](../04-agent-framework/tool-framework.md#62-tool-chaining), and
[parallel execution](../04-agent-framework/tool-framework.md#63-parallel-execution):

- Parallel tool calls fan out to multiple workers concurrently.
- Chained calls may be co-scheduled on the same worker to reuse warm sandboxes and
  pass intermediate data with less overhead.
- The Workflow Engine's [DAG engine](../03-workflow-engine/dag-engine.md) drives
  cross-tool orchestration; the Runtime executes the individual nodes.

---

# 8. Long-Running & Checkpointed Executions

For long-running tools:

- Use `mode: async` ([Execution API §6](execution-api.md#6-async-execution)).
- The worker periodically reports progress; the execution record persists status.
- Integration with [checkpointing](../04-agent-framework/tool-framework.md#64-checkpoint-integration)
  lets durable workflows survive worker restarts: a tool's checkpoint is stored so
  execution can resume rather than restart.
- Idempotent long-runners can be rescheduled on a new worker after node loss.

---

# 9. Distributed Execution

Aligned with
[Tool Framework §65](../04-agent-framework/tool-framework.md#65-distributed-execution)
and [Workflow Distributed Execution](../03-workflow-engine/distributed-execution.md):

- Workers may span regions/zones; the scheduler honors locality and residency.
- Remote worker pools execute third-party tools in network-isolated environments,
  returning only results.
- Execution state (status, checkpoints) is externalized so any control-plane
  instance can serve status/cancel for any execution.

---

# 10. Health & Lifecycle

| Event | Behavior |
|-------|----------|
| Worker register | Advertises capabilities; joins a pool |
| Heartbeat miss | Marked unhealthy; no new work; in-flight monitored |
| Drain | Finish in-flight, refuse new, then leave |
| Crash | In-flight executions failed/rescheduled (if idempotent) |
| Recycle | Periodic recycling of long-lived workers to limit drift |

---

# 11. Caching

The Runtime can cache tool results for **pure, deterministic** tools
([Tool Framework §66](../04-agent-framework/tool-framework.md#66-caching)):

- Keyed by tool + version + normalized input.
- Stored with TTL; tenant-isolated.
- Only tools that declare themselves cacheable/pure participate; side-effecting
  tools (e.g. `email.send`) never cache.

---

# 12. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Schedule decision | < 5 ms p95 |
| Queue wait (healthy fleet) | < 50 ms p95 |
| Warm-pool hit ratio | > 80% for interactive tools |
| Drain completion | bounded by longest in-flight timeout |
| Scale-out reaction | < 30 s to add capacity |

---

# 13. Dependencies

- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`03-workflow-engine/dag-engine.md`](../03-workflow-engine/dag-engine.md)
- [`03-workflow-engine/distributed-execution.md`](../03-workflow-engine/distributed-execution.md)
- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#65-distributed-execution)

---

# 14. Related Documents

- [`07-tool-runtime/overview.md`](overview.md)
- [`07-tool-runtime/execution-api.md`](execution-api.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Worker Pool specification |
