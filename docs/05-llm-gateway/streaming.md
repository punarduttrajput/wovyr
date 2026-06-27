<!--
File: docs/05-llm-gateway/streaming.md
Document ID: LLM-005
-->

# LLM Gateway Streaming

**Document ID:** LLM-005  
**File Path:** `docs/05-llm-gateway/streaming.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **unified streaming protocol** of the LLM Gateway. Callers receive incremental model output through one event model regardless of provider wire format or transport.

The [Provider SDK](../04-agent-framework/provider-sdk.md#13-streaming-support) normalizes provider-specific streams into these events; the Gateway forwards them to callers and appends metering data.

---

# 2. Transports

| Transport | Mechanism |
|-----------|-----------|
| REST | Server-Sent Events (`text/event-stream`) |
| gRPC | Server-streaming RPC |
| WebSocket | Framed JSON messages (bidirectional) |

Semantics are identical across transports; only framing differs. WebSocket
additionally supports client→server control frames (e.g. cancel).

---

# 3. Event Model

Every streamed item is a typed event:

```json
{ "type": "delta", "seq": 12, "data": { "content": "Your order " } }
```

| Event `type` | Meaning |
|--------------|---------|
| `start` | Stream opened; carries `model`, `provider`, `request_id` |
| `delta` | Incremental content token(s) |
| `tool_call` | Incremental or complete tool/function call |
| `reasoning` | Incremental reasoning trace (if model/tenant enables it) |
| `progress` | Non-text progress (e.g. image generation %) |
| `usage` | Interim or final token/cost accounting |
| `error` | Mid-stream error (normalized code) |
| `done` | Terminal event; carries final `usage` and `routing` |

Events are strictly ordered by `seq`. Consumers must treat unknown event types as
ignorable for forward compatibility.

---

# 4. Stream Lifecycle

```text
start
  └─ delta* (content tokens)
  └─ tool_call* (if tools invoked)
  └─ reasoning* (optional)
  └─ usage (interim, optional)
done   ← always last on success
```

On failure before `done`, an `error` event is emitted and the stream closes. If
the failure occurs **before** the first `delta`, the Gateway may transparently
[fail over](resilience.md#5-failover) and restart the stream — the caller still
sees a single logical stream beginning at `start`.

---

# 5. Example (SSE)

```text
event: start
data: {"type":"start","model":"claude-opus-4-8","provider":"anthropic","request_id":"req_01H..."}

event: delta
data: {"type":"delta","seq":1,"data":{"content":"Your "}}

event: delta
data: {"type":"delta","seq":2,"data":{"content":"order shipped."}}

event: usage
data: {"type":"usage","data":{"prompt_tokens":412,"completion_tokens":5}}

event: done
data: {"type":"done","usage":{"prompt_tokens":412,"completion_tokens":5,"total_tokens":417,"cost_usd":0.0057},"routing":{"selected_provider":"anthropic","failovers":0,"cache":"miss"}}
```

---

# 6. Tool-Call Streaming

Tool calls may stream incrementally (arguments built across deltas) or arrive
whole. The Gateway normalizes both into `tool_call` events:

```json
{
  "type": "tool_call",
  "seq": 7,
  "data": {
    "id": "call_1",
    "name": "lookup_order",
    "arguments_delta": "{\"id\":\"12",
    "complete": false
  }
}
```

A final `tool_call` with `"complete": true` carries the fully assembled
`arguments`. Callers that cannot handle partial arguments may buffer until
`complete`.

---

# 7. Cancellation

| Transport | Cancellation |
|-----------|--------------|
| REST (SSE) | Client closes the HTTP connection |
| gRPC | Client cancels the call context |
| WebSocket | Client sends `{ "type": "cancel" }` |

On cancellation the Gateway aborts the upstream provider request promptly to stop
billing, emits a final `usage` event for tokens already consumed, and closes.

---

# 8. Backpressure

For slow consumers the Gateway applies bounded buffering per stream. If a consumer
cannot keep up beyond the buffer limit, the Gateway:

1. Applies flow control (pauses upstream reads where the provider supports it), then
2. Terminates the stream with an `error` (`code: "consumer_too_slow"`) if the
   buffer is exceeded.

`idle_stream_timeout` (see [Resilience §3](resilience.md#3-timeouts)) closes
streams that stall.

---

# 9. Usage & Cost in Streams

- An optional interim `usage` event may be emitted periodically.
- The terminal `done` event **always** contains the authoritative final `usage`
  (including `cost_usd`) and `routing` blocks.
- Cost is metered even for cancelled streams, based on tokens actually consumed.

Accounting rules are defined in [Token Management](token-management.md).

---

# 10. Ordering & Reliability Guarantees

- Events within a single stream are ordered and gap-free by `seq`.
- Exactly one terminal event (`done` or `error`) is emitted.
- The Gateway does not buffer completed streams for replay; reliability across
  reconnects is the caller's responsibility (use non-streaming + idempotency key
  for at-least-once semantics).

---

# 11. Non-Functional Targets

| Metric | Target |
|--------|--------|
| Added first-token latency vs. raw provider | < 15 ms |
| Per-event forwarding overhead | < 1 ms |
| Max concurrent streams per instance | 10k+ |

---

# 12. Dependencies

- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md#13-streaming-support)
- [`05-llm-gateway/resilience.md`](resilience.md)
- [`05-llm-gateway/token-management.md`](token-management.md)

---

# 13. Related Documents

- [`05-llm-gateway/overview.md`](overview.md)
- [`05-llm-gateway/provider-api.md`](provider-api.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Streaming specification |
