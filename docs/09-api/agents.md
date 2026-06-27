<!--
File: docs/09-api/agents.md
Document ID: API-003
-->

# Agents API

**Document ID:** API-003  
**File Path:** `docs/09-api/agents.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the REST/gRPC API for managing **agents** — their definitions, versions, and runs. It is the control-plane interface to the [Agent Runtime](../04-agent-framework/agent-runtime-protocol.md) and the [Agent Definition](../04-agent-framework/agent-definition.md) model.

All endpoints inherit the [API conventions](overview.md) and require
[authentication](authentication.md).

---

# 2. Resources

| Resource | Description |
|----------|-------------|
| `agent` | A versioned agent definition |
| `agent_version` | An immutable published version of an agent |
| `run` | A single execution of an agent against a goal |
| `session` | A multi-run conversational context |

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| POST | `/api/v1/agents` | `agents:write` |
| GET | `/api/v1/agents` | `agents:read` |
| GET | `/api/v1/agents/{id}` | `agents:read` |
| PATCH | `/api/v1/agents/{id}` | `agents:write` |
| DELETE | `/api/v1/agents/{id}` | `agents:write` |
| POST | `/api/v1/agents/{id}:publish` | `agents:write` |
| GET | `/api/v1/agents/{id}/versions` | `agents:read` |
| POST | `/api/v1/agents/{id}:run` | `agents:run` |
| GET | `/api/v1/runs/{id}` | `agents:read` |
| GET | `/api/v1/runs/{id}/stream` | `agents:read` |
| POST | `/api/v1/runs/{id}:cancel` | `agents:run` |
| POST | `/api/v1/sessions` | `agents:run` |

---

# 4. Agent Definition

```json
{
  "id": "agt_01H...",
  "object": "agent",
  "name": "order-assistant",
  "description": "Handles customer order questions",
  "model_selector": { "capability": "chat", "class": "balanced" },
  "instructions": "You are a helpful order assistant.",
  "tools": ["http.request", "lookup_order"],
  "memory": { "scopes": ["project", "organization"], "enabled": true },
  "policies": ["pii-guard"],
  "version": 3,
  "status": "published"
}
```

Fields map to the [Agent Definition spec](../04-agent-framework/agent-definition.md):
`model_selector` resolves via the [LLM Gateway](../05-llm-gateway/routing.md);
`tools` reference the [Tools API](tools.md); `memory` configures
[Memory Engine](../06-memory-engine/index.md) access; `policies` are enforced by
the [Policy Engine](../04-agent-framework/policy-engine.md).

---

# 5. Running an Agent

```http
POST /api/v1/agents/agt_01H...:run
Idempotency-Key: run-order-123
```

```json
{
  "input": { "message": "Where is order 123?" },
  "session_id": "ses_01H...",
  "stream": true,
  "budget": { "max_cost_usd": 0.25 },
  "context": { "correlation_id": "trace_01H..." }
}
```

Response (non-streaming):

```json
{
  "run_id": "run_01H...",
  "status": "succeeded",
  "output": { "message": "Order 123 shipped yesterday." },
  "steps": 4,
  "usage": { "total_tokens": 1820, "cost_usd": 0.021, "tool_calls": 2 }
}
```

`budget` is enforced via [LLM Gateway token management](../05-llm-gateway/token-management.md).

---

# 6. Run Lifecycle & Streaming

```text
queued → planning → executing → (succeeded | failed | cancelled)
```

`GET /api/v1/runs/{id}/stream` (SSE) emits the agent's progress: planner steps,
tool invocations, model deltas, and memory reads — a superset aligned with the
[Agent Runtime Protocol](../04-agent-framework/agent-runtime-protocol.md) and the
[LLM Gateway streaming events](../05-llm-gateway/streaming.md).

---

# 7. Sessions

A session preserves conversational context across runs:

```json
{ "id": "ses_01H...", "agent": "agt_01H...", "turns": 6, "memory_scope": "session" }
```

Runs referencing a `session_id` share working/conversation memory and (optionally)
sticky model routing (see [Routing §8](../05-llm-gateway/routing.md#8-sticky-routing)).

---

# 8. Versioning & Publishing

- Editing an agent creates a draft; `:publish` produces an immutable
  `agent_version`.
- In-flight runs continue on the version they started with.
- A run may pin `version`; otherwise the active published version is used.

---

# 9. Events

Mutations emit `agent.created`, `agent.published`, `agent.run.started`,
`agent.run.completed`, `agent.run.failed` to the
[Event Bus](../02-architecture/event-driven-architecture.md); webhooks mirror these.

---

# 10. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`model_not_found` (selector unsatisfiable), `budget_exceeded`, `tool_not_found`,
`forbidden` (policy denied).

---

# 11. Dependencies

- [`04-agent-framework/agent-definition.md`](../04-agent-framework/agent-definition.md)
- [`04-agent-framework/agent-runtime-protocol.md`](../04-agent-framework/agent-runtime-protocol.md)
- [`05-llm-gateway/index.md`](../05-llm-gateway/index.md)
- [`09-api/tools.md`](tools.md)

---

# 12. Related Documents

- [`09-api/workflows.md`](workflows.md)
- [`09-api/memory.md`](memory.md)
- [`09-api/overview.md`](overview.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Agents API specification |
