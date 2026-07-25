<!--
File: docs/09-api/workflows.md
Document ID: API-004
-->

# Workflows API

**Document ID:** API-004  
**File Path:** `docs/09-api/workflows.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the REST/gRPC API for managing **workflows** — their definitions and durable executions. It is the control-plane interface to the [Workflow Engine](../03-workflow-engine/overview.md).

All endpoints inherit the [API conventions](overview.md) and require
[authentication](authentication.md).

---

# 2. Resources

| Resource | Description |
|----------|-------------|
| `workflow` | A versioned workflow definition (compiled from the DSL) |
| `workflow_version` | An immutable published version |
| `execution` | A durable run of a workflow |
| `task` | A human/approval task within an execution |

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| POST | `/api/v1/workflows` | `workflows:write` |
| GET | `/api/v1/workflows` | `workflows:read` |
| GET | `/api/v1/workflows/{id}` | `workflows:read` |
| PATCH | `/api/v1/workflows/{id}` | `workflows:write` |
| POST | `/api/v1/workflows/{id}:validate` | `workflows:write` |
| POST | `/api/v1/workflows/{id}:publish` | `workflows:write` |
| POST | `/api/v1/workflows/{id}:run` | `workflows:run` |
| GET | `/api/v1/executions` | `workflows:read` |
| GET | `/api/v1/executions/{id}` | `workflows:read` |
| GET | `/api/v1/executions/{id}/stream` | `workflows:read` |
| POST | `/api/v1/executions/{id}:cancel` | `workflows:cancel` |
| POST | `/api/v1/executions/{id}:signal` | `workflows:run` |
| POST | `/api/v1/tasks/{id}:complete` | `workflows:run` |

---

# 4. Workflow Definition

A workflow is authored in the [Workflow DSL](../03-workflow-engine/workflow-dsl.md)
and submitted as YAML or JSON:

```json
{
  "id": "wf_01H...",
  "object": "workflow",
  "name": "invoice-approval",
  "definition_format": "yaml",
  "definition": "apiVersion: workflow.wovyr.io/v1\nkind: Workflow\n...",
  "version": "2.1.0",
  "status": "published"
}
```

`:validate` compiles the definition to the
[WIR](../03-workflow-engine/workflow-dsl.md#25-workflow-intermediate-representation-wir)
and returns schema/graph errors without publishing.

---

# 5. Running a Workflow

```http
POST /api/v1/workflows/wf_01H...:run
Idempotency-Key: invoice-2026-06-27
```

```json
{
  "input": { "customerId": "c_42", "invoiceAmount": 12000, "currency": "USD" },
  "version": "2.1.0"
}
```

Response — an async operation referencing the execution:

```json
{ "execution_id": "exe_01H...", "status": "running", "workflow": "wf_01H..." }
```

Executions are **durable**: they survive restarts via
[checkpointing](../03-workflow-engine/checkpointing-specification.md) and resume
deterministically.

---

# 6. Execution Lifecycle

```text
running → (completed | failed | cancelled | compensating | suspended)
```

Aligned with the [State Machine](../03-workflow-engine/state-machine.md).
`suspended` covers waits on timers, events, or human tasks.

`GET /api/v1/executions/{id}/stream` emits step transitions, activity
results, retries, and compensation events.

---

# 7. Signals & Events

```http
POST /api/v1/executions/exe_01H...:signal
{ "event": "PaymentReceived", "payload": { "amount": 12000 } }
```

Signals resume executions waiting on
[event activities](../03-workflow-engine/workflow-dsl.md#17-event-wait), routed via
the [Event Bus](../03-workflow-engine/event-bus.md).

---

# 8. Human Tasks

Executions paused on a [human task](../03-workflow-engine/workflow-dsl.md#15-human-task)
expose a `task` resource:

```http
POST /api/v1/tasks/tsk_01H...:complete
{ "decision": "approved", "comment": "Within budget" }
```

Completing the task resumes the execution.

---

# 9. Cancellation & Compensation

`:cancel` requests graceful cancellation; if the workflow defines
[compensation](../03-workflow-engine/compensation-engine.md), the engine runs the
configured compensating activities (saga rollback) before terminating.

---

# 10. Versioning

- `:publish` creates an immutable `workflow_version`.
- Running executions continue on their start version
  ([DSL §23](../03-workflow-engine/workflow-dsl.md#23-workflow-versioning)).
- New runs use the version requested, or the active published version.

---

# 11. Events

Emits `workflow.published`, `execution.started`, `execution.completed`,
`execution.failed`, `task.created`, `task.completed` to the
[Event Bus](../02-architecture/event-driven-architecture.md); webhooks mirror these.

---

# 12. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`validation_failed` (DSL/compile errors with details), `conflict` (version),
`forbidden` (policy/permission).

---

# 13. Dependencies

- [`03-workflow-engine/overview.md`](../03-workflow-engine/overview.md)
- [`03-workflow-engine/workflow-dsl.md`](../03-workflow-engine/workflow-dsl.md)
- [`03-workflow-engine/state-machine.md`](../03-workflow-engine/state-machine.md)

---

# 14. Related Documents

- [`09-api/agents.md`](agents.md)
- [`09-api/tools.md`](tools.md)
- [`09-api/overview.md`](overview.md)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Workflows API specification |
