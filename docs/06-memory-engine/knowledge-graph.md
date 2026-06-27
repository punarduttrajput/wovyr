<!--
File: docs/06-memory-engine/knowledge-graph.md
Document ID: MEM-007
-->

# Memory Engine Knowledge Graph

**Document ID:** MEM-007  
**File Path:** `docs/06-memory-engine/knowledge-graph.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **knowledge graph** maintained by the Memory Engine: a structured layer of entities and relationships that complements vector and keyword retrieval with **relational, multi-hop** context.

Vector search answers "what is similar to this?"; the graph answers "what is *connected* to this?" Together they give agents both semantic recall and relational reasoning.

---

# 2. Model

```text
(Entity) ──[Relationship]──► (Entity)
   │
   └─ mentioned_in ─► (Memory)
```

| Element | Description |
|---------|-------------|
| Entity | A node: person, team, system, product, policy, concept |
| Relationship | A typed, directed edge between entities |
| Mention | A link from an entity to a memory that references it |
| Property | Key/value attributes on entities and edges |

Entities and edges are **tenant-scoped**; the graph is never shared across tenants.

---

# 3. Entity & Edge Schema

```json
{
  "entity": {
    "id": "team:finance",
    "type": "team",
    "name": "Finance",
    "properties": { "region": "eu" },
    "tenant": "acme"
  },
  "edge": {
    "from": "policy:refunds",
    "to": "team:finance",
    "rel": "owned_by",
    "properties": { "since": "2025-01-01" },
    "weight": 1.0
  }
}
```

Common relationship types: `owned_by`, `part_of`, `depends_on`, `related_to`,
`caused_by`, `supersedes`, `mentions`. Tenants may register custom types with
declared direction and constraints.

---

# 4. Construction

The graph is populated from memories as they are ingested:

```text
Memory write
   │
   ▼
Entity extraction (NER + linking)  ── via LLM Gateway or rules
   │
   ▼
Relationship extraction
   │
   ▼
Entity resolution (merge aliases to canonical ids)
   │
   ▼
Upsert entities + edges + mention links
```

- **Entity linking** resolves surface forms ("Finance team", "the finance dept")
  to a canonical entity id.
- Extraction can be model-assisted (via the [LLM Gateway](../05-llm-gateway/index.md))
  or rule-based for structured sources.
- Extraction confidence is stored on edges and used to weight traversal.

---

# 5. Storage

Per [Storage §2](storage-architecture.md#2-backends), the graph starts inside
PostgreSQL:

```sql
CREATE TABLE kg_entity (
  id TEXT, tenant TEXT, type TEXT, name TEXT,
  properties JSONB, PRIMARY KEY (tenant, id)
);
CREATE TABLE kg_edge (
  tenant TEXT, src TEXT, dst TEXT, rel TEXT,
  properties JSONB, weight REAL,
  PRIMARY KEY (tenant, src, dst, rel)
);
CREATE TABLE kg_mention (
  tenant TEXT, entity_id TEXT, memory_id TEXT,
  PRIMARY KEY (tenant, entity_id, memory_id)
);
```

Traversal uses recursive CTEs initially. If traversal volume or depth grows, the
graph migrates to a dedicated graph database behind the same API — callers are
unaffected.

---

# 6. Traversal & Queries

The graph supports:

| Operation | Use |
|-----------|-----|
| Neighbors | Entities directly related to X |
| N-hop expansion | Context within K relationships of X |
| Path finding | How are X and Y connected? |
| Subgraph | All entities/edges for a project or topic |
| Mentions | Memories referencing an entity |

Traversal is **bounded** (max hops, max nodes) to keep latency predictable, and
applies a **hop-decay weight** so distant nodes contribute less.

---

# 7. Integration with Retrieval

When a [query](memory-api.md#6-query-request) sets `include_graph` (or uses the
`graph` strategy):

```text
1. Resolve query entities (link query terms to graph nodes)
2. Expand N hops from those entities
3. Collect memories mentioned by the expanded entities
4. Merge with vector/keyword candidates
5. Rank with proximity (hop distance) as a signal
```

This surfaces relationally-relevant memories that pure similarity would miss
(e.g. "the policy owned by the team that owns this incident"). See
[Retrieval §7](retrieval.md#7-graph-expansion) and
[Ranking §2](ranking.md#2-scoring-signals).

---

# 8. Consistency & Maintenance

- Edges and mentions are updated as memories are created, versioned, or deleted;
  deleting a memory removes its mentions but keeps entities (which may be
  referenced elsewhere).
- Orphan entities (no edges, no mentions) are pruned by a background job.
- Entity merges (resolving duplicates) rewrite edges/mentions transactionally.
- The graph is rebuildable from memories + extraction, like other derived stores
  (see [Storage §9](storage-architecture.md#9-reindex--recovery)).

---

# 9. Governance

- The graph is tenant-isolated; queries are constrained by `tenant`.
- Entity/edge visibility inherits the scope of the memories that created them;
  a principal sees only the subgraph derivable from memories they may read.
- Extraction respects PII policy: sensitive entities are tagged and gated by ABAC.

---

# 10. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Neighbor lookup | < 10 ms p95 |
| 3-hop expansion (bounded) | < 30 ms p95 |
| Extraction on write | async, non-blocking |
| Max traversal nodes | configurable (default 500) |

---

# 11. Dependencies

- [`06-memory-engine/storage-architecture.md`](storage-architecture.md)
- [`06-memory-engine/retrieval.md`](retrieval.md)
- [`05-llm-gateway/index.md`](../05-llm-gateway/index.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 12. Related Documents

- [`06-memory-engine/overview.md`](overview.md)
- [`06-memory-engine/semantic-memory.md`](semantic-memory.md)
- [`06-memory-engine/ranking.md`](ranking.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Knowledge Graph specification |
