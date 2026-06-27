<!--
File: docs/05-llm-gateway/caching.md
Document ID: LLM-007
-->

# LLM Gateway Caching

**Document ID:** LLM-007  
**File Path:** `docs/05-llm-gateway/caching.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines response caching in the LLM Gateway. Caching reduces cost and latency by reusing prior model responses for identical or semantically equivalent requests, while preserving correctness and tenant isolation.

---

# 2. Cache Modes

The request `cache` block selects behavior:

```json
{ "cache": { "mode": "semantic", "ttl_seconds": 3600 } }
```

| Mode | Behavior |
|------|----------|
| `off` | No lookup, no store |
| `exact` | Lookup/store on an exact request hash |
| `semantic` | Lookup by embedding similarity; store on miss |
| `read_only` | Lookup allowed; never store |
| `refresh` | Skip lookup; execute and overwrite the entry |

Default mode is tenant-configurable; `off` is the safe default for
non-deterministic or sensitive workloads.

---

# 3. Exact Cache

The exact cache keys on a stable hash of the **normalized** request:

```text
key = hash(
   tenant + model_class + messages + tools +
   response_format + temperature + max_tokens + relevant_params
)
```

Normalization rules:

- Whitespace-insensitive message normalization
- Excludes volatile fields (`request_id`, `trace_id`, timestamps)
- Includes parameters that affect output (`temperature`, `top_p`, `seed`, etc.)

Exact caching is most effective for deterministic requests (`temperature: 0`,
fixed `seed`).

---

# 4. Semantic Cache

The semantic cache retrieves prior responses for requests whose **meaning** is
close, even if wording differs.

```text
1. Embed the canonical request (e.g. concatenated user turns)
2. Vector search the tenant's cache namespace
3. If best match similarity >= threshold AND params compatible → hit
4. Else → miss
```

```yaml
semantic_cache:
  similarity_threshold: 0.95
  embedding_model: text-embedding-3-large
  max_candidates: 5
  param_compatibility: strict   # model/temperature must match
```

Vectors are stored in Qdrant (see
[C4 Container](../02-architecture/c4-container.md)); a higher threshold trades hit
rate for safety. Semantic hits are flagged so callers can distinguish them.

---

# 5. Cache Key Isolation

Cache entries are **strictly namespaced** to prevent cross-tenant leakage:

```text
namespace = tenant : project : model_class
```

A request can never read another tenant's cached response. Optional finer scoping
(per principal/agent) is available for sensitive projects.

---

# 6. Lookup Order

```text
cache.mode = semantic
   │
   ▼
Exact lookup ──► hit? return (cache: "exact")
   │ miss
   ▼
Semantic lookup ──► hit? return (cache: "semantic")
   │ miss
   ▼
Execute request ──► store in exact (+ semantic index)
```

`semantic` mode includes an exact check first because it is cheaper and stronger.

---

# 7. Store Policy

A response is cached only if **all** hold:

- `cache.mode` permits storing (`exact`, `semantic`, `refresh`)
- The response completed successfully (no `error`)
- The request is not marked `no_store` by policy
- The content is not flagged sensitive by [Policy Engine](../04-agent-framework/policy-engine.md)
- For streaming, the full stream completed (partial streams are not cached)

Stored entries carry: response body, `usage` (original), model, pricing version,
created-at, and TTL.

---

# 8. TTL & Invalidation

| Mechanism | Behavior |
|-----------|----------|
| TTL | Per-request `ttl_seconds`, bounded by tenant max |
| Manual purge | API to purge by tenant/project/key prefix |
| Model change | Entries pinned to a model are invalidated when it is retired |
| Pricing change | Entries remain valid; original `usage` is preserved |

There is no implicit cross-model reuse: an entry produced by one model is not
served for a request routed to a different model.

---

# 9. Usage & Cost on Cache Hits

On a cache hit:

- `usage.cost_usd` is reported as **0** for the served response.
- A cost event is still emitted with `cache: "exact"|"semantic"` and an
  `estimated_savings_usd` field (the cost the live call would have incurred).
- Cache hits **do** count toward rate limits but **not** toward spend quotas.

Savings roll up into the dashboard and
[Success Metrics](../00-executive/success-metrics.md).

---

# 10. Consistency & Safety

- Caching is **opt-in per request/tenant**; high-stakes flows should use `off`.
- Tool-calling responses are cached only when the resolved tool outputs are
  deterministic; by default tool-invoking chats are **not** cached.
- Embeddings are highly cacheable and cached by exact input hash by default.
- A `refresh` request lets callers force regeneration and replace stale entries.

---

# 11. Storage Backends

| Cache | Backend |
|-------|---------|
| Exact entries | Redis (with TTL) |
| Semantic index | Qdrant (per-tenant namespace) |
| Large payloads | Object storage, referenced from Redis |

If the cache backend is unavailable, the Gateway bypasses caching and serves live
(see [Resilience §9](resilience.md#9-degraded-modes)).

---

# 12. Non-Functional Targets

| Metric | Target |
|--------|--------|
| Exact lookup | < 3 ms p95 |
| Semantic lookup | < 12 ms p95 |
| Target hit ratio (cacheable traffic) | > 30% |
| Cross-tenant leakage | 0 (hard isolation) |

---

# 13. Dependencies

- [`05-llm-gateway/token-management.md`](token-management.md)
- [`05-llm-gateway/routing.md`](routing.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)

---

# 14. Related Documents

- [`05-llm-gateway/overview.md`](overview.md)
- [`05-llm-gateway/provider-api.md`](provider-api.md)
- [`05-llm-gateway/resilience.md`](resilience.md)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Caching specification |
