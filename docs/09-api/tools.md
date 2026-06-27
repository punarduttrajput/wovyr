<!--
File: docs/09-api/tools.md
Document ID: API-006
-->

# Tools API

**Document ID:** API-006  
**File Path:** `docs/09-api/tools.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the API for **discovering, inspecting, and invoking tools**. It is the control-plane view over the [Tool Registry](../04-agent-framework/tool-framework.md#12-tool-registry) and a convenience entry point to the [Tool Runtime](../07-tool-runtime/index.md).

All endpoints inherit the [API conventions](overview.md) and require
[authentication](authentication.md).

---

# 2. Management vs. Execution

| Use case | Endpoint |
|----------|----------|
| List/inspect tools and versions | This API |
| Enable/disable a tool for a project | This API |
| Invoke a tool (general) | This API → proxies to Tool Runtime |
| High-throughput / streaming execution | [Tool Runtime Execution API](../07-tool-runtime/execution-api.md) |

Tools are provided by built-ins and by [plugins](plugins.md); the registry is the
union of both.

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| GET | `/api/v1/tools` | `tools:read` |
| GET | `/api/v1/tools/{name}` | `tools:read` |
| GET | `/api/v1/tools/{name}/versions` | `tools:read` |
| GET | `/api/v1/tools/{name}/schema` | `tools:read` |
| POST | `/api/v1/tools/{name}:enable` | `tools:read` (project admin) |
| POST | `/api/v1/tools/{name}:disable` | `tools:read` (project admin) |
| POST | `/api/v1/tools/{name}:invoke` | `tools:invoke` |
| GET | `/api/v1/executions/{id}` | `tools:read` |

---

# 4. Tool Resource

```json
{
  "name": "http.request",
  "object": "tool",
  "version": "1.2.0",
  "source": "plugin:acme/http-core",
  "categories": ["network"],
  "capabilities": ["streaming"],
  "permissions_required": ["net:egress:*"],
  "sandbox": "wasm",
  "status": "active"
}
```

Metadata comes from the
[Tool Manifest](../04-agent-framework/tool-framework.md#10-tool-manifest);
`permissions_required` mirrors the plugin's declared
[permissions](../08-plugin-sdk/permissions.md).

---

# 5. Discovery

```http
GET /api/v1/tools?category=network&capability=streaming
```

Returns tools available to the caller's project, filtered by category, capability,
or required permissions (e.g. "tools needing no egress"). The list reflects
project-level enablement.

---

# 6. Schema Introspection

`GET /api/v1/tools/{name}/schema` returns the tool's input/output JSON schemas so
clients (and the agent planner) can construct valid calls. Schemas are validated on
invocation by the [Tool Runtime](../07-tool-runtime/execution-api.md#5-invocation-response-sync).

---

# 7. Invocation

```http
POST /api/v1/tools/http.request:invoke
Idempotency-Key: fetch-order-123
```

```json
{
  "version": "1.2.0",
  "input": { "method": "GET", "url": "https://api.example.com/orders/123" },
  "mode": "sync",
  "limits": { "timeout_ms": 30000 }
}
```

This is a thin façade over the
[Tool Runtime Execution API](../07-tool-runtime/execution-api.md): the Gateway
authorizes, then forwards. Authorization checks `tools:invoke` **and** the tool's
required [permissions](../08-plugin-sdk/permissions.md) for the caller. Streaming
and async follow the Execution API semantics.

---

# 8. Enablement

Tools can be enabled/disabled per project so teams curate their catalog:

```http
POST /api/v1/tools/docker.run:disable
{ "project": "support-bot", "reason": "not needed" }
```

Disabling removes the tool from discovery and blocks invocation for that project.

---

# 9. Governance

- Invocation is authorized, rate-limited, metered, and audited (see
  [Tool Runtime Security](../07-tool-runtime/security-isolation.md)).
- Untrusted/community tools are sandboxed per their
  [trust class](../08-plugin-sdk/overview.md#7-trust-model).

---

# 10. Events

Emits `tool.enabled`, `tool.disabled`, and `tool.execution.*` to the
[Event Bus](../02-architecture/event-driven-architecture.md).

---

# 11. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`tool_not_found`, `invalid_input`, `forbidden` (missing permission),
`rate_limited`, plus the
[Execution API codes](../07-tool-runtime/execution-api.md#10-error-model) on invoke.

---

# 12. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md)
- [`07-tool-runtime/execution-api.md`](../07-tool-runtime/execution-api.md)
- [`08-plugin-sdk/permissions.md`](../08-plugin-sdk/permissions.md)

---

# 13. Related Documents

- [`09-api/plugins.md`](plugins.md)
- [`09-api/agents.md`](agents.md)
- [`09-api/overview.md`](overview.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tools API specification |
