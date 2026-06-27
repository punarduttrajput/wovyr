<!--
File: docs/07-tool-runtime/execution-api.md
Document ID: TRT-002
-->

# Tool Runtime Execution API

**Document ID:** TRT-002  
**File Path:** `docs/07-tool-runtime/execution-api.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **external contract** of the Tool Runtime — how callers invoke a tool, stream its output, and cancel it, independent of the sandbox backend that runs it.

The contract is exposed over **REST** (HTTP/JSON) and **gRPC** with identical semantics, and conforms to the tool input/output schemas from the [Tool Framework](../04-agent-framework/tool-framework.md#23-tool-input-schema).

---

# 2. Design Rules

1. Callers reference a tool by `name` + optional `version`; the Runtime resolves it.
2. Inputs/outputs conform to the tool's declared JSON schema.
3. Every response includes an `execution` block (timing, resources, sandbox).
4. Streaming and non-streaming share the same request schema.
5. Executions are addressable by `execution_id` for status and cancellation.
6. The contract is versioned under `/v1`.

---

# 3. Endpoints (REST)

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/v1/executions` | Invoke a tool (sync or async) |
| GET | `/v1/executions/{id}` | Fetch execution status/result |
| GET | `/v1/executions/{id}/stream` | Stream output (SSE) |
| POST | `/v1/executions/{id}/cancel` | Cancel a running execution |
| GET | `/v1/tools` | List available tools (registry view) |
| GET | `/v1/tools/{name}` | Tool metadata + input schema |
| GET | `/healthz`, `/readyz`, `/metrics` | Operations |

gRPC exposes `Invoke`, `InvokeStream`, `GetExecution`, `Cancel`, `ListTools`
on the `ToolRuntime` service.

---

# 4. Invocation Request

```json
{
  "tool": "http.request",
  "version": "1.2.0",
  "tenant": "acme",
  "project": "support-bot",
  "input": {
    "method": "GET",
    "url": "https://api.example.com/orders/123"
  },
  "context": {
    "workflow_id": "wf_01H...",
    "agent": "order-assistant",
    "correlation_id": "trace_01H..."
  },
  "limits": {
    "timeout_ms": 30000,
    "max_output_bytes": 1048576
  },
  "mode": "sync",
  "stream": false,
  "idempotency_key": "exec-order-123"
}
```

| Field | Notes |
|-------|-------|
| `tool` / `version` | Resolved via registry; omitting `version` uses the active version |
| `input` | Validated against the tool's input schema before execution |
| `context` | Propagated to the tool's [execution context](../04-agent-framework/tool-framework.md#22-tool-execution-context) |
| `limits` | Per-call overrides bounded by tenant/tool maximums |
| `mode` | `sync` (block for result) or `async` (return id, poll/stream) |
| `idempotency_key` | Dedupes retried invocations |

---

# 5. Invocation Response (Sync)

```json
{
  "execution_id": "exec_01H...",
  "tool": "http.request",
  "version": "1.2.0",
  "status": "succeeded",
  "output": {
    "status_code": 200,
    "body": "{\"order\":\"123\",\"state\":\"shipped\"}"
  },
  "execution": {
    "sandbox": "wasm",
    "worker": "worker-7",
    "queued_ms": 3,
    "started_ms": 12,
    "duration_ms": 84,
    "resources": { "cpu_ms": 70, "peak_memory_mb": 22 }
  }
}
```

Output is validated against the tool's
[output schema](../04-agent-framework/tool-framework.md#24-tool-output-schema)
before return.

---

# 6. Async Execution

With `mode: "async"`, the Runtime returns immediately:

```json
{ "execution_id": "exec_01H...", "status": "running" }
```

Callers then poll `GET /v1/executions/{id}` or attach to the stream. Async is
preferred for long-running tools and for workflow steps that checkpoint
(see [Worker Pool §8](worker-pool.md#8-long-running--checkpointed-executions)).

---

# 7. Streaming

When `stream: true` (or via `/stream`), output is delivered as ordered events,
aligned with the framework's
[streaming protocol](../04-agent-framework/tool-framework.md#51-streaming-protocol):

| Event `type` | Meaning |
|--------------|---------|
| `start` | Execution started; carries sandbox + worker |
| `stdout` / `stderr` | Incremental output chunks |
| `progress` | Structured progress (0–100 or stage) |
| `partial` | Incremental structured output |
| `log` | Tool-emitted log line |
| `done` | Terminal success; carries final `output` + `execution` |
| `error` | Terminal failure; carries normalized error |

REST uses Server-Sent Events; gRPC uses a server stream. Exactly one terminal
event (`done` or `error`) is emitted.

---

# 8. Cancellation

`POST /v1/executions/{id}/cancel` requests cooperative cancellation, escalating to
forced sandbox teardown:

```text
1. Signal the tool (cancellation token / SIGTERM)
2. Grace period (configurable, default 5s)
3. Force-destroy the sandbox (SIGKILL + reclaim)
```

Cancellation is idempotent. A cancelled execution returns `status: "cancelled"`
with any partial output captured before teardown.

---

# 9. Status Model

```text
queued → scheduled → running → (succeeded | failed | cancelled | timed_out)
```

`GET /v1/executions/{id}` returns the current status, and for terminal states the
full result. Execution records are retained for a configurable window for status
queries and audit.

---

# 10. Error Model

```json
{
  "error": {
    "code": "resource_exceeded",
    "message": "Execution exceeded memory limit (1Gi).",
    "type": "execution_error",
    "retryable": false,
    "details": { "limit": "1Gi", "observed": "1.1Gi" }
  }
}
```

| Code | HTTP | Retryable | Meaning |
|------|------|-----------|---------|
| `unauthenticated` | 401 | no | Missing/invalid credentials |
| `forbidden` | 403 | no | Permission/policy denied |
| `tool_not_found` | 404 | no | Unknown tool or version |
| `invalid_input` | 400 | no | Input failed schema validation |
| `invalid_output` | 502 | no | Tool produced schema-invalid output |
| `timeout` | 504 | yes | Exceeded execution timeout |
| `resource_exceeded` | 400 | no | Hit CPU/memory/disk limit |
| `sandbox_unavailable` | 503 | yes | Could not provision a sandbox |
| `tool_error` | 422 | maybe | Tool ran but returned an error |
| `rate_limited` | 429 | yes | Tenant/tool rate limit hit |

---

# 11. Idempotency & Retry

- `idempotency_key` dedupes retried invocations within its TTL, returning the
  original execution.
- Only tools declared **idempotent** in their manifest are auto-retried by the
  [Retry Engine](../03-workflow-engine/retry-engine.md); others surface the error
  to the caller.

---

# 12. Versioning

`/v1` is additive-compatible; breaking changes introduce `/v2`. Sandbox backend
changes never affect the contract version.

---

# 13. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#25-tool-invocation-lifecycle)
- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`07-tool-runtime/worker-pool.md`](worker-pool.md)
- [`03-workflow-engine/retry-engine.md`](../03-workflow-engine/retry-engine.md)

---

# 14. Related Documents

- [`07-tool-runtime/overview.md`](overview.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`09-api`](../SUMMARY.md) *(planned: platform REST API)*

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Execution API |
