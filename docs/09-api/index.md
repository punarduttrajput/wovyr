<!--
File: docs/09-api/index.md
Document ID: API-INDEX-001
-->

# Platform API Index

**Document ID:** API-INDEX-001  
**File Path:** `docs/09-api/index.md`  
**Version:** 1.1.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-04

---

# 1. Purpose

This document is the **central navigation and architecture index** for the Wovyr AI Platform API — the external REST/gRPC surface through which clients, SDKs, and the dashboard manage agents, workflows, memory, tools, plugins, projects, and users.

The API is fronted by the **API Gateway** (c4-container §4.1) and is the single, governed front door to the platform.

---

# 2. API Surface

```text
Clients (Dashboard / CLI / SDK / external)
        │  HTTPS (REST) · gRPC · WebSocket
        ▼
    API Gateway ── authn · authz · rate limit · validation · versioning
        │
        ├──► Agents API        → Agent Runtime
        ├──► Workflows API     → Workflow Engine
        ├──► Memory API        → Memory Engine
        ├──► Tools API         → Tool Runtime
        ├──► Plugins API       → Plugin Engine
        ├──► Projects API      → Platform Services
        └──► Users/Auth API    → Platform Services
```

The Gateway routes each resource group to the owning subsystem. See
[C4 Container §4.1](../02-architecture/c4-container.md).

---

# 3. Management API vs. Service Contracts

This section documents the **management / control-plane API** (CRUD + actions on
platform resources). Several subsystems also expose a **data-plane service
contract** for high-throughput operations; those are specified in their own
sections and referenced here:

| Domain | Management API (this section) | Data-plane contract |
|--------|-------------------------------|---------------------|
| Inference | — | [LLM Gateway Provider API](../05-llm-gateway/provider-api.md) |
| Memory | [memory.md](memory.md) | [Memory Engine API](../06-memory-engine/memory-api.md) |
| Tools | [tools.md](tools.md) | [Tool Runtime Execution API](../07-tool-runtime/execution-api.md) |
| Plugins | [plugins.md](plugins.md) | [Plugin Engine](../08-plugin-sdk/overview.md) |

The management API is for *configuring and operating* resources; the data-plane
contracts are for *running* them at scale.

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [overview.md](overview.md) | API conventions: REST/gRPC, versioning, pagination, errors, idempotency |
| [deprecation-policy.md](deprecation-policy.md) | Breaking-change definition, deprecation window, `/v2` parallel-run rule |
| [authentication.md](authentication.md) | AuthN/Z: OAuth2, JWT, API keys, RBAC scopes |
| [agents.md](agents.md) | Agent definitions, runs, sessions |
| [workflows.md](workflows.md) | Workflow definitions, executions, control |
| [memory.md](memory.md) | Memory management (namespaces, records, policy) |
| [tools.md](tools.md) | Tool registry + invocation |
| [plugins.md](plugins.md) | Plugin install, lifecycle, grants |
| [projects.md](projects.md) | Organizations, projects, tenants |
| [users.md](users.md) | Users, roles, teams, API keys |

---

# 5. Design Principles

1. **One front door** — all external access goes through the API Gateway.
2. **Resource-oriented** — predictable REST resources with consistent verbs.
3. **Versioned** — `/v1`; additive changes are backward compatible.
4. **Secure by default** — every request authenticated, authorized, and audited.
5. **Consistent** — shared conventions for pagination, errors, idempotency.
6. **Dual protocol** — REST and gRPC expose equivalent semantics.
7. **Observable** — every call is traced, metered, and rate-limited.

---

# 6. Dependencies

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md) — API Gateway container
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md) — authorization
- [`01-product/system-overview.md`](../01-product/system-overview.md) — request flow

---

# 7. Related Documents

- [`09-api/overview.md`](overview.md)
- [`10-dashboard`](../SUMMARY.md) *(planned: consumes this API)*
- [`11-cli`](../SUMMARY.md) *(planned: consumes this API)*

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-04 | Added [deprecation-policy.md](deprecation-policy.md) to the document map |
| 1.0.0 | 2026-06-27 | Initial Platform API Index |
