<!--
File: docs/06-memory-engine/retrieval.md
Document ID: MEM-004
-->

# Memory Engine Retrieval

**Document ID:** MEM-004  
**File Path:** `docs/06-memory-engine/retrieval.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the Memory Engine finds candidate memories for a query. Retrieval produces a candidate set; [Ranking](ranking.md) orders it and [Compression](compression.md) fits it to a token budget.

Retrieval is **hybrid by default**: it combines vector similarity, keyword search, metadata filtering, and graph traversal because no single method maximizes both recall and precision.

---

# 2. Retrieval Strategies

| Strategy | Method | Strength |
|----------|--------|----------|
| `vector` | Embedding similarity (Qdrant) | Semantic recall, paraphrase-tolerant |
| `keyword` | Full-text / BM25 (PostgreSQL) | Exact terms, names, codes |
| `hybrid` | Vector + keyword fused | Best general default |
| `graph` | Knowledge-graph traversal | Multi-hop, relational context |
| `metadata` | Pure filter (no scoring) | Deterministic lookups |

The strategy is chosen by the [query](memory-api.md#6-query-request); `hybrid` is
the default.

---

# 3. Hybrid Pipeline

```text
Query
  │
  ├─► Embed query (LLM Gateway) ─► Vector search (Qdrant, top-K_v)
  │
  ├─► Tokenize query ───────────► Keyword search (Postgres FTS, top-K_k)
  │
  ├─► Apply metadata filters (tenant, scope, tags, labels, time)
  │
  ▼
Fusion (combine vector + keyword candidate sets)
  │
  ▼
Optional graph expansion (related entities)
  │
  ▼
Candidate set ──► Ranking
```

Both branches run concurrently. Metadata filters are pushed **into** each branch
(Qdrant payload filter, SQL `WHERE`) so they prune before scoring.

---

# 4. Fusion

Vector and keyword candidate lists are merged using **Reciprocal Rank Fusion (RRF)**:

```text
score_rrf(d) = Σ over lists L of  1 / (k + rank_L(d))
```

with a smoothing constant `k` (default 60). RRF is robust because it combines
*rankings* rather than incomparable raw scores (cosine vs. BM25). The fused score
becomes the `relevance` input to [Ranking](ranking.md).

Tenants may switch fusion to **weighted linear** (normalized cosine + normalized
BM25 with configurable weights) when they prefer tunable blending.

---

# 5. Scope & Permission Filtering

Retrieval only considers memories the principal may read. Scope filters
(`private … public`) are applied as hard predicates **before** scoring, so
unauthorized records never enter the candidate set. A second policy pass after
ranking enforces ABAC rules that depend on record content. See
[Memory API §10](memory-api.md#10-scopes--sharing).

---

# 6. Metadata Filtering

Supported filters (combinable):

- `tenant`, `scope`, `project`, `agent`, `type`
- `tags` (any/all), `labels` (key/value)
- `created_after` / `created_before`, `updated_*`
- `importance >= n`

Filters map to Qdrant payload conditions and SQL predicates so they are evaluated
inside the search, not as a post-filter (which would distort top-K).

---

# 7. Graph Expansion

When `include_graph` is set, the Engine expands the top candidates by traversing
the [knowledge graph](knowledge-graph.md) up to N hops, pulling in related
entities and the memories that mention them. Expanded items are tagged
`match: "graph"` and scored with a hop-decay penalty so distant relations rank
lower.

---

# 8. Tier-Aware Retrieval

Retrieval consults tiers in order of latency:

```text
1. Hot (Redis)   — cached query results / hot records
2. Warm (PG+Qdrant) — primary search path
3. Cold (PG+Qdrant) — included when scope spans knowledge bases
4. Archive (object) — only when explicitly requested (slow)
```

Archive is excluded by default; a query may set `include_archive: true` to search
cold storage at higher latency.

---

# 9. Query Caching

Normalized queries (query text + scope + filters + strategy) are cached in Redis
with a short TTL. Cache entries are invalidated for a tenant/scope when a relevant
`memory.created/updated` event arrives, preventing stale reads. Cache hits skip
embedding and search entirely.

---

# 10. Degraded Retrieval

| Condition | Fallback | Effect |
|-----------|----------|--------|
| Qdrant down | `keyword` only | Reduced semantic recall |
| Embedding (Gateway) down | `keyword` only | No vector branch |
| Graph store down | Skip graph expansion | No relational context |
| Postgres FTS slow | `vector` only | Reduced exact-term recall |

Degraded responses set `degraded: true` in the
[query response](memory-api.md#7-query-response).

---

# 11. Determinism

For a fixed corpus version, identical queries return identical ordered results.
Embedding model and fusion parameters are pinned per query so retrieval is
reproducible and auditable (a requirement from
[Memory System §3](../04-agent-framework/memory-system.md#3-design-principles)).

---

# 12. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Vector search (warm) | < 20 ms p95 |
| Keyword search | < 15 ms p95 |
| Fusion + filter | < 5 ms |
| End-to-end retrieval (excl. ranking) | < 30 ms p95 |

---

# 13. Dependencies

- [`06-memory-engine/storage-architecture.md`](storage-architecture.md)
- [`06-memory-engine/ranking.md`](ranking.md)
- [`05-llm-gateway/index.md`](../05-llm-gateway/index.md)
- [`06-memory-engine/knowledge-graph.md`](knowledge-graph.md)

---

# 14. Related Documents

- [`06-memory-engine/overview.md`](overview.md)
- [`06-memory-engine/memory-api.md`](memory-api.md)
- [`06-memory-engine/semantic-memory.md`](semantic-memory.md)
- [`06-memory-engine/compression.md`](compression.md)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Retrieval specification |
