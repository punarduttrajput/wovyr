<!--
File: docs/05-llm-gateway/provider-api.md
Document ID: LLM-002
-->

# LLM Gateway Provider API

**Document ID:** LLM-002  
**File Path:** `docs/05-llm-gateway/provider-api.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **external contract** of the LLM Gateway — the provider-neutral request and response schema that every caller uses, independent of the underlying model vendor.

The contract is exposed over three transports with identical semantics:

- **REST** (HTTP/JSON) — general clients
- **gRPC** — internal services, low latency
- **WebSocket** — bidirectional streaming

---

# 2. Design Rules

1. One schema for all providers; no vendor fields in the public contract.
2. Callers select a *capability* and a *model selector*, not a raw vendor model when routing is desired.
3. Responses always include a `usage` block (tokens + cost).
4. Streaming and non-streaming share the same request schema.
5. The contract is versioned under `/v1`.

---

# 3. Endpoints (REST)

| Method | Path | Capability |
|--------|------|------------|
| POST | `/v1/chat` | Chat completion |
| POST | `/v1/completions` | Text completion |
| POST | `/v1/embeddings` | Embeddings |
| POST | `/v1/images` | Image generation |
| POST | `/v1/moderations` | Content moderation |
| GET  | `/v1/models` | List available models |
| GET  | `/v1/models/{id}` | Model metadata |
| GET  | `/healthz`, `/readyz`, `/metrics` | Operations |

gRPC exposes the same operations as `LlmGateway` service methods
(`Chat`, `Completions`, `Embed`, `GenerateImage`, `Moderate`, `ListModels`).

---

# 4. Common Request Envelope

Every inference request shares a common envelope:

```json
{
  "model": "claude-opus-4-8",
  "model_selector": {
    "capability": "chat",
    "class": "frontier",
    "strategy": "lowest_latency"
  },
  "tenant": "acme",
  "project": "support-bot",
  "metadata": {
    "request_id": "req_01H...",
    "trace_id": "trace_01H..."
  },
  "budget": {
    "max_cost_usd": 0.50,
    "max_tokens": 4096
  },
  "cache": {
    "mode": "semantic",
    "ttl_seconds": 3600
  },
  "stream": false
}
```

Rules:

- Provide **either** `model` (pin a specific model) **or** `model_selector`
  (let the [Router](routing.md) choose). If both are present, `model` wins unless
  it is unavailable, in which case `model_selector` is used as fallback.
- `tenant` and `project` drive quota, cost attribution, and policy.
- `budget` is enforced before and during execution (see
  [Token Management](token-management.md)).
- `cache` controls lookup/store behavior (see [Caching](caching.md)).

---

# 5. Chat Request

```json
{
  "model_selector": { "capability": "chat", "class": "balanced" },
  "messages": [
    { "role": "system", "content": "You are a support agent." },
    { "role": "user", "content": "Where is my order?" }
  ],
  "tools": [
    {
      "name": "lookup_order",
      "description": "Look up an order by id",
      "parameters": { "type": "object", "properties": { "id": { "type": "string" } } }
    }
  ],
  "response_format": { "type": "json_schema", "schema": { "type": "object" } },
  "temperature": 0.2,
  "max_tokens": 1024,
  "stream": true
}
```

Field notes:

- `messages` follows a role-tagged format (`system`, `user`, `assistant`, `tool`).
- `tools` uses the normalized function schema; provider-specific tool formats are
  produced by the [Provider SDK](../04-agent-framework/provider-sdk.md#14-function-calling).
- `response_format` requests structured output (`text`, `json`, `json_schema`).

---

# 6. Chat Response (Non-Streaming)

```json
{
  "id": "resp_01H...",
  "model": "claude-opus-4-8",
  "provider": "anthropic",
  "created": 1750000000,
  "choices": [
    {
      "index": 0,
      "finish_reason": "stop",
      "message": {
        "role": "assistant",
        "content": "Your order shipped yesterday.",
        "tool_calls": []
      }
    }
  ],
  "usage": {
    "prompt_tokens": 412,
    "completion_tokens": 37,
    "cached_tokens": 0,
    "total_tokens": 449,
    "cost_usd": 0.0061
  },
  "routing": {
    "strategy": "balanced",
    "selected_provider": "anthropic",
    "failovers": 0,
    "cache": "miss"
  }
}
```

Every response includes `usage` and `routing` blocks so callers can observe cost
and routing behavior without separate queries.

---

# 7. Streaming Responses

When `stream: true`, the response is a sequence of unified events (see
[Streaming](streaming.md)). REST uses Server-Sent Events; gRPC uses a server
stream; WebSocket uses framed messages. The terminal event always carries the
final `usage` and `routing` blocks.

---

# 8. Embeddings

Request:

```json
{
  "model_selector": { "capability": "embeddings" },
  "input": ["first text", "second text"]
}
```

Response:

```json
{
  "model": "text-embedding-3-large",
  "provider": "openai",
  "data": [
    { "index": 0, "embedding": [0.01, -0.02, "..."] },
    { "index": 1, "embedding": [0.03, 0.04, "..."] }
  ],
  "usage": { "prompt_tokens": 8, "total_tokens": 8, "cost_usd": 0.0000016 }
}
```

---

# 9. Model Discovery

`GET /v1/models` returns the merged, capability-annotated registry:

```json
{
  "models": [
    {
      "id": "claude-opus-4-8",
      "provider": "anthropic",
      "family": "claude",
      "capabilities": ["chat", "vision", "function_calling", "json"],
      "context_window": 200000,
      "max_output_tokens": 64000,
      "pricing": { "input_per_1k": 0.005, "output_per_1k": 0.025 },
      "status": "available"
    }
  ]
}
```

Model metadata derives from the [Provider SDK model registry](../04-agent-framework/provider-sdk.md#8-model-registry).

---

# 10. Error Model

Errors use a stable, provider-neutral shape:

```json
{
  "error": {
    "code": "budget_exceeded",
    "message": "Request would exceed project budget.",
    "type": "client_error",
    "provider": null,
    "retryable": false,
    "details": { "limit_usd": 0.50, "estimated_usd": 0.71 }
  }
}
```

| Code | HTTP | Retryable | Meaning |
|------|------|-----------|---------|
| `unauthenticated` | 401 | no | Missing/invalid credentials |
| `forbidden` | 403 | no | Policy denied the request |
| `model_not_found` | 404 | no | No model matches the selector |
| `budget_exceeded` | 402 | no | Would exceed configured budget |
| `quota_exceeded` | 429 | yes | Tenant/project quota hit |
| `provider_rate_limited` | 429 | yes | Upstream provider 429 |
| `provider_unavailable` | 503 | yes | All candidate providers failed |
| `timeout` | 504 | yes | Upstream timed out after retries |
| `invalid_request` | 400 | no | Schema validation failed |

The mapping from raw provider errors to these codes is normalized by the
[Resilience Engine](resilience.md).

---

# 11. Idempotency

Callers may pass an `Idempotency-Key` header (or `metadata.idempotency_key`).
The Gateway deduplicates retried requests within the key's TTL and returns the
original response, preventing double-billing on client retries.

---

# 12. Versioning

- The contract is namespaced `/v1`.
- Additive fields are backward-compatible (minor version).
- Breaking changes introduce `/v2` and run in parallel during deprecation.
- Provider/model availability changes do **not** change the contract version.

---

# 13. Dependencies

- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md)
- [`05-llm-gateway/streaming.md`](streaming.md)
- [`05-llm-gateway/token-management.md`](token-management.md)

---

# 14. Related Documents

- [`05-llm-gateway/overview.md`](overview.md)
- [`05-llm-gateway/routing.md`](routing.md)
- [`05-llm-gateway/caching.md`](caching.md)
- [`09-api`](../SUMMARY.md) *(planned: platform REST API)*

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Provider API |
