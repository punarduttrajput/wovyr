<!--
File: docs/10-dashboard/memory-explorer.md
Document ID: DASH-004
-->

# Memory Explorer

**Document ID:** DASH-004  
**File Path:** `docs/10-dashboard/memory-explorer.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **Memory Explorer** — the UI for browsing, searching, inspecting, and managing agent memory. It is the visual front end over the [Memory (Management) API](../09-api/memory.md) and the [Memory Engine](../06-memory-engine/index.md).

---

# 2. Surfaces

| View | Purpose |
|------|---------|
| Namespaces | Browse/manage [namespaces](../09-api/memory.md#4-namespaces) and retention |
| Browser | List/filter records by type, scope, tags, tier, age |
| Search | Hybrid query with ranked, explainable results |
| Record detail | View content, metadata, versions, mentions |
| Graph view | Visualize the [knowledge graph](../06-memory-engine/knowledge-graph.md) |
| Admin | Purge, reindex, export |

---

# 3. Search & Ranking Transparency

The search box runs a [hybrid query](../06-memory-engine/retrieval.md) and shows,
for each result, the `score` and `score_breakdown`
([relevance/recency/importance](../06-memory-engine/ranking.md#9-output)). Users can
adjust ranking weights live to understand and tune retrieval.

```text
Query → results with: title · type · scope · score (relevance/recency/importance)
```

A `degraded` banner appears if retrieval fell back (e.g. vector store unavailable).

---

# 4. Record Inspection

For a selected record:

- Full content and metadata (tags, labels, source, confidence)
- Version history with diffs ([versioning](../06-memory-engine/overview.md#7-memory-lifecycle))
- Linked knowledge-graph entities and the memories that mention them
- Tier and retention status

---

# 5. Knowledge Graph View

An interactive graph renders entities and relationships
([knowledge graph](../06-memory-engine/knowledge-graph.md)):

- Click an entity to see neighbors and mentioning memories
- Expand N hops; filter by relationship type
- Trace paths between two entities

The view is bounded (max nodes) to stay responsive, mirroring the engine's
traversal limits.

---

# 6. Management Actions

| Action | API |
|--------|-----|
| Edit/delete a record | [`PATCH`/`DELETE /memory/records`](../09-api/memory.md#3-endpoints) |
| Purge by filter | [`:purge`](../09-api/memory.md#7-admin-operations) |
| Reindex namespace | [`:reindex`](../09-api/memory.md#7-admin-operations) (async) |
| Export | [`:export`](../09-api/memory.md#7-admin-operations) |

Destructive and admin actions require elevated scope and confirm dialogs; reindex
and export surface as [operations](../09-api/overview.md#11-asynchronous-operations)
with progress.

---

# 7. Governance & Privacy

- Only records within the user's [scopes](../06-memory-engine/memory-api.md#10-scopes--sharing)
  are visible.
- PII-tagged content is masked unless the user has the required scope.
- Exports are audited with the filter used.

---

# 8. Cost & Footprint

The explorer surfaces namespace size, tier distribution, and embedding/index
footprint so operators can manage growth and retention.

---

# 9. Dependencies

- [`09-api/memory.md`](../09-api/memory.md)
- [`06-memory-engine/retrieval.md`](../06-memory-engine/retrieval.md)
- [`06-memory-engine/knowledge-graph.md`](../06-memory-engine/knowledge-graph.md)

---

# 10. Related Documents

- [`10-dashboard/overview.md`](overview.md)
- [`10-dashboard/agent-studio.md`](agent-studio.md)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Explorer specification |
