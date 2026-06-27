<!--
File: docs/17-adr/ADR-0004-qdrant.md
Document ID: ADR-0004
-->

# ADR-0004: Qdrant for Vector Search

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

Semantic memory and retrieval require fast approximate nearest-neighbor (ANN)
search over embeddings at large scale, with metadata filtering and per-tenant
isolation ([Memory Engine](../06-memory-engine/index.md)).

---

# Decision

Use **Qdrant** as the dedicated **vector database** for embeddings and similarity
search. PostgreSQL ([ADR-0003](ADR-0003-postgresql.md)) remains authoritative;
Qdrant is a rebuildable derived index.

Rationale:
- High-performance HNSW ANN with tunable parameters per collection.
- **Payload filtering** inside the search (tenant/scope/tags) — critical for
  [retrieval](../06-memory-engine/retrieval.md#6-metadata-filtering) and isolation.
- Per-collection namespacing for hard
  [tenant isolation](../06-memory-engine/storage-architecture.md#10-tenant-isolation).
- Horizontal scaling/replication for read-heavy retrieval.
- Rust-friendly, cloud-native, self-hostable or managed.

---

# Consequences

**Positive**
- Sub-50 ms warm retrieval at large scale; tunable recall/latency.
- Filtering pushed into the index avoids post-filter top-K distortion.
- Rebuildable from PostgreSQL → not a second source of truth.

**Negative**
- Another stateful system to operate, back up, and scale.
- Embedding-model changes require a controlled
  [reindex](../06-memory-engine/storage-architecture.md#9-reindex--recovery).

---

# Alternatives Considered

- **pgvector (in PostgreSQL)** — simpler stack, no extra system; but at our target
  scale and filter complexity, a dedicated engine offers better performance and
  isolation. May still be used in small/embedded deployments.
- **Milvus / Weaviate / Pinecone** — all viable; Qdrant chosen for Rust ergonomics,
  filtering model, and self-host + managed flexibility. Pinecone (managed-only) was
  rejected as a hard dependency.

---

# Related

- [`06-memory-engine/storage-architecture.md`](../06-memory-engine/storage-architecture.md)
- [`06-memory-engine/semantic-memory.md`](../06-memory-engine/semantic-memory.md)
- [ADR-0003](ADR-0003-postgresql.md)
