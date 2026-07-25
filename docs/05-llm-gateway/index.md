<!--
File: docs/05-llm-gateway/index.md
Document ID: LLM-INDEX-001
-->

# LLM Gateway Index

**Document ID:** LLM-INDEX-001  
**File Path:** `docs/05-llm-gateway/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for the LLM Gateway in the Wovyr AI Platform.

The LLM Gateway is the single, governed entry point through which every subsystem reaches an AI model provider. It turns the in-process [Provider SDK](../04-agent-framework/provider-sdk.md) into a shared, network-accessible platform service with centralized routing, resilience, cost control, caching, and observability.

---

# 2. Gateway vs. Provider SDK

These two components are deliberately separated. Understanding the boundary is essential.

| Concern | Provider SDK (`04-agent-framework`) | LLM Gateway (`05-llm-gateway`) |
|---------|-------------------------------------|--------------------------------|
| Form | In-process Rust library | Deployable service / container |
| Scope | A single process | The whole platform / many tenants |
| Audience | Agent Runtime code | Any service over REST / gRPC / WebSocket |
| Credentials | Reads provider keys at call site | Holds keys centrally; callers never see them |
| Routing | Library-level provider selection | Fleet-wide, policy- and budget-aware routing |
| State | Stateless helper | Shared cache, quotas, circuit-breaker state |
| Governance | None | Tenant isolation, quotas, audit, cost ceilings |

The Gateway **embeds** the Provider SDK to talk to providers. The SDK defines the
provider abstraction; the Gateway operates it as infrastructure.

See [C4 Container §4.5](../02-architecture/c4-container.md) for where the Gateway sits among deployable containers.

---

# 3. Gateway Subsystems

```text
LLM Gateway
│
├── Provider API          (external request/response contract)
├── Router                (provider & model selection)
├── Resilience Engine     (failover, retry, circuit breaking)
├── Streaming Engine      (unified token / event streaming)
├── Token Manager         (accounting, budgets, cost control)
├── Cache                 (exact + semantic response caching)
├── Credential Vault      (secret references, key rotation)
└── Telemetry             (logs, metrics, traces, cost events)
```

---

# 4. Request Lifecycle (High Level)

```text
Caller (Agent Runtime / Workflow / Service)
        │  REST / gRPC / WebSocket
        ▼
   Provider API  ──► AuthN/Z + tenant resolution
        │
        ▼
   Token Manager ──► budget & quota pre-check
        │
        ▼
      Cache      ──► hit? return immediately
        │ miss
        ▼
     Router      ──► select provider + model
        │
        ▼
 Resilience Eng. ──► attempt, retry, failover
        │
        ▼
  Provider SDK   ──► provider adapter → provider API
        │
        ▼
 Stream / collect ──► usage metering + cost event
        │
        ▼
   Cache store   ──► response returned to caller
```

A detailed sequence appears in [Overview §6](overview.md).

---

# 5. Document Map

| Document | Responsibility |
|----------|----------------|
| [overview.md](overview.md) | Service responsibilities, architecture, lifecycle, NFRs |
| [provider-api.md](provider-api.md) | External request/response contract (REST + gRPC) |
| [routing.md](routing.md) | Provider/model selection strategies and policies |
| [resilience.md](resilience.md) | Failover, retries, timeouts, circuit breaking |
| [streaming.md](streaming.md) | Unified streaming event protocol |
| [token-management.md](token-management.md) | Token accounting, budgets, cost control |
| [caching.md](caching.md) | Exact and semantic response caching |

---

# 6. Design Principles

1. **One door for all models** — no subsystem calls a provider directly.
2. **Provider-neutral contract** — callers speak one schema regardless of vendor.
3. **Governed by default** — every request is authenticated, metered, and audited.
4. **Resilient** — provider failure degrades gracefully via failover.
5. **Cost-aware** — budgets and quotas are enforced before spend occurs.
6. **Observable** — every request emits logs, metrics, traces, and a cost event.
7. **Stateless callers, stateful gateway** — shared cache and quotas live here.

---

# 7. Dependencies

- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md) — provider abstraction the Gateway embeds
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md) — governance rules enforced on requests
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md) — cost and usage events

---

# 8. Related Documents

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)
- [`02-architecture/c4-component.md`](../02-architecture/c4-component.md)
- [`01-product/system-overview.md`](../01-product/system-overview.md)
- [`00-executive/success-metrics.md`](../00-executive/success-metrics.md) — LLM Gateway KPIs

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Index |
