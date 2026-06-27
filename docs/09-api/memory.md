<!--
File: docs/09-api/memory.md
Document ID: API-005
-->

# Memory API (Management)

**Document ID:** API-005  
**File Path:** `docs/09-api/memory.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the control-plane API for **managing memory** — namespaces, records, retention policy, and inspection — exposed through the API Gateway.

For high-throughput store/retrieve operations, callers use the data-plane
[Memory Engine API](../06-memory-engine/memory-api.md) directly; this management
API governs and inspects memory rather than serving the hot path.

---

# 2. Scope of This API vs. the Engine Contract

| Use case | Endpoint |
|----------|----------|
| Configure namespaces, retention, policy | This API |
| Browse / inspect / export memory | This API |
| Bulk admin (purge, reindex) | This API |
| High-volume store/query at runtime | [Memory Engine API](../06-memory-engine/memory-api.md) |

The management API may proxy reads to the Engine for inspection but adds
governance, auditing, and admin operations.

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| GET | `/api/v1/memory/namespaces` | `memory:read` |
| POST | `/api/v1/memory/namespaces` | `memory:write` |
| GET | `/api/v1/memory/records` | `memory:read` |
| GET | `/api/v1/memory/records/{id}` | `memory:read` |
| POST | `/api/v1/memory/records` | `memory:write` |
| PATCH | `/api/v1/memory/records/{id}` | `memory:write` |
| DELETE | `/api/v1/memory/records/{id}` | `memory:write` |
| POST | `/api/v1/memory:query` | `memory:read` |
| POST | `/api/v1/memory:purge` | `memory:write` |
| POST | `/api/v1/memory:reindex` | `memory:admin` |
| POST | `/api/v1/memory:export` | `memory:read` |

---

# 4. Namespaces

A namespace scopes memory and its policy (tier defaults, retention, embedding model):

```json
{
  "id": "ns_01H...",
  "object": "memory_namespace",
  "name": "support-bot-knowledge",
  "project": "support-bot",
  "default_scope": "project",
  "embedding_model": "text-embedding-3-large",
  "retention": { "semantic": "permanent", "conversation": "90d" }
}
```

The `embedding_model` is fixed per namespace (see
[Semantic Memory §3](../06-memory-engine/semantic-memory.md#3-embeddings)); changing
it triggers a `:reindex`.

---

# 5. Records & Query

Record shape and query semantics follow the
[Memory Engine API](../06-memory-engine/memory-api.md#4-memory-record). The
management endpoints add admin filters (by tier, importance, age) and return the
same `score`/`score_breakdown` from [Ranking](../06-memory-engine/ranking.md).

```http
POST /api/v1/memory:query
{ "query": "refund window", "scope": ["project"], "limit": 10 }
```

---

# 6. Retention & Policy

```http
PATCH /api/v1/memory/namespaces/ns_01H...
{ "retention": { "conversation": "30d" } }
```

Retention changes are applied by the Engine's lifecycle reaper
([Overview §8](../06-memory-engine/overview.md#8-retention--archival)). Sensitive
namespaces can require elevated scope and enforce
[Policy Engine](../04-agent-framework/policy-engine.md) rules (e.g. PII handling).

---

# 7. Admin Operations

| Operation | Effect |
|-----------|--------|
| `:purge` | Delete records matching a filter (soft or `hard=true`) |
| `:reindex` | Rebuild vectors (e.g. new embedding model) — async operation |
| `:export` | Stream records (JSON/JSONL) for backup or migration |

`:reindex` and `:export` return an [operation](overview.md#11-asynchronous-operations).

---

# 8. Governance

- Every record access is tenant-isolated and audited.
- Reads respect [memory scopes](../06-memory-engine/memory-api.md#10-scopes--sharing)
  and the caller's RBAC/ABAC grants.
- Exports of sensitive data require `memory:admin` and are audited with the filter
  used.

---

# 9. Events

Emits `memory.namespace.created`, `memory.purged`, `memory.reindex.completed` to the
[Event Bus](../02-architecture/event-driven-architecture.md).

---

# 10. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`forbidden` (scope), `not_found`, `conflict` (namespace exists),
`storage_unavailable` (degraded backend).

---

# 11. Dependencies

- [`06-memory-engine/memory-api.md`](../06-memory-engine/memory-api.md)
- [`06-memory-engine/overview.md`](../06-memory-engine/overview.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 12. Related Documents

- [`09-api/agents.md`](agents.md)
- [`09-api/overview.md`](overview.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory (Management) API specification |
