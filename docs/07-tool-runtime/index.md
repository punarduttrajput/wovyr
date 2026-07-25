<!--
File: docs/07-tool-runtime/index.md
Document ID: TRT-INDEX-001
-->

# Tool Runtime Index

**Document ID:** TRT-INDEX-001  
**File Path:** `docs/07-tool-runtime/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for the Tool Runtime in the Wovyr AI Platform.

The Tool Runtime is the deployable service that **executes tools** — safely, with isolation, resource limits, and governance — on behalf of agents and workflows. It operationalizes the model defined by the [Tool Framework](../04-agent-framework/tool-framework.md): the framework specifies *what a tool is and how to build one*; the Runtime is *where tools actually run* at scale.

---

# 2. Runtime vs. Framework

As with the [LLM Gateway](../05-llm-gateway/index.md) and the
[Memory Engine](../06-memory-engine/index.md), the platform separates the
**model** from the **operated service**.

| Concern | Tool Framework (`04-agent-framework`) | Tool Runtime (`07-tool-runtime`) |
|---------|----------------------------------------|----------------------------------|
| Defines | Tool model, SDK, manifest, traits | The execution service & fleet |
| Audience | Tool authors | Operators + calling services |
| Registry | Registry *model* and discovery API | Operates the registry at runtime |
| Permissions | Permission *model* & policy schema | Enforces them per execution |
| Sandboxing | Sandbox *types* & policy schema | Provisions and runs real sandboxes |
| Scaling | Out of scope | Worker pools, autoscaling, distribution |

The Runtime **implements** the framework's `Dispatcher`, `Permission Engine`,
`Sandbox Manager`, and `Runtime Adapter`
([Tool Framework §6](../04-agent-framework/tool-framework.md)) as a deployable
container. See [C4 Container §4.6](../02-architecture/c4-container.md).

---

# 3. Runtime Subsystems

```text
Tool Runtime
│
├── Execution API     (invoke / stream / cancel)
├── Dispatcher        (resolve tool → route to worker)
├── Permission Engine (authorize before execution)
├── Sandbox Manager   (provision isolated environments)
├── Runtime Adapters  (native / wasm / container / microVM / remote)
├── Worker Pool       (fleet of execution workers)
├── Resource Governor (limits, quotas, fairness)
├── Secret Injector   (mount secrets into sandboxes)
└── Telemetry         (logs, metrics, traces, audit)
```

---

# 4. Request Lifecycle (High Level)

```text
Caller (Agent Runtime / Workflow)
        │  REST / gRPC
        ▼
   Execution API ──► AuthN/Z + tenant resolution
        │
        ▼
   Dispatcher    ──► resolve tool + version (Registry)
        │
        ▼
 Permission Eng. ──► authorize (Policy Engine)
        │
        ▼
 Sandbox Manager ──► provision isolated environment
        │
        ▼
 Runtime Adapter ──► execute tool with limits + secrets
        │
        ▼
 Collect / stream result ──► audit + metrics
        │
        ▼
   Destroy sandbox ──► return result
```

A detailed lifecycle appears in [Overview §6](overview.md).

---

# 5. Document Map

| Document | Responsibility |
|----------|----------------|
| [overview.md](overview.md) | Service responsibilities, architecture, lifecycle, NFRs |
| [execution-api.md](execution-api.md) | Invoke / stream / cancel contract (REST + gRPC) |
| [sandbox-runtime.md](sandbox-runtime.md) | Isolation backends, sandbox lifecycle, resource enforcement |
| [worker-pool.md](worker-pool.md) | Execution fleet, scheduling, scaling, distributed execution |
| [security-isolation.md](security-isolation.md) | Network/filesystem isolation, secrets, tenant isolation, threat model |
| [observability-ops.md](observability-ops.md) | Health, metrics, tracing, audit, SLOs, runbooks |
| [e2b-gap-analysis.md](e2b-gap-analysis.md) | *(Planned)* E2B gap closure: persistent sessions, filesystem/process APIs, streaming, SDK |

---

# 6. Design Principles

1. **Untrusted by default** — every tool runs sandboxed; nothing executes on the host.
2. **Least privilege** — no network or filesystem access unless explicitly granted.
3. **Ephemeral execution** — sandboxes are created per execution and destroyed after.
4. **Governed** — authorize, meter, rate-limit, and audit every invocation.
5. **Isolated tenancy** — one tenant's execution cannot observe or affect another's.
6. **Bounded** — CPU, memory, disk, time, and egress are always capped.
7. **Observable** — every execution emits logs, metrics, traces, and an audit record.

---

# 7. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md) — tool model the Runtime executes
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md) — authorization rules
- [`03-workflow-engine/retry-engine.md`](../03-workflow-engine/retry-engine.md) — retry semantics for failed tools
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md) — execution events
- [`08-plugin-sdk`](../SUMMARY.md) *(planned: plugin packaging & distribution)*

---

# 8. Related Documents

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)
- [`02-architecture/c4-component.md`](../02-architecture/c4-component.md)
- [`03-workflow-engine/distributed-execution.md`](../03-workflow-engine/distributed-execution.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Index |
