<!--
File: docs/06-memory-engine/index.md
Document ID: MEM-INDEX-001
-->

# Memory Engine Index

**Document ID:** MEM-INDEX-001  
**File Path:** `docs/06-memory-engine/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for the Memory Engine in the Wovyr AI Platform.

The Memory Engine is the deployable service that stores, indexes, retrieves, and governs all agent memory. It operates the memory abstraction defined for agents in the [Memory System](../04-agent-framework/memory-system.md) and exposes it as a shared, multi-tenant platform container backed by PostgreSQL, Qdrant, Redis, and object storage.

---

# 2. Engine vs. Memory System

As with the [LLM Gateway](../05-llm-gateway/index.md) and the Provider SDK, the
platform separates the **abstraction** from the **operated service**.

| Concern | Memory System (`04-agent-framework`) | Memory Engine (`06-memory-engine`) |
|---------|--------------------------------------|------------------------------------|
| Form | In-agent abstraction / library view | Deployable service / container |
| Audience | Agent Runtime authors | Any service over REST / gRPC |
| Scope | A single agent's view of memory | All tenants, agents, and workflows |
| State | Describes record shapes & strategies | Owns the durable stores and indexes |
| Governance | Conceptual permissions | Enforced isolation, RBAC/ABAC, audit |
| Storage | Storage-agnostic | Concrete PostgreSQL / Qdrant / Redis / object store |

The Memory System defines *what* a memory is and *how* agents reason about it.
The Memory Engine is *where* memories actually live and how they are served at
scale. See [C4 Container §4.4](../02-architecture/c4-container.md).

---

# 3. Engine Subsystems

```text
Memory Engine
│
├── Memory API          (store / retrieve / update / delete / query)
├── Ingestion Pipeline  (validate → embed → index → persist)
├── Retrieval Engine    (vector + keyword + graph hybrid search)
├── Ranking Engine      (relevance, recency, importance scoring)
├── Knowledge Graph     (entities + relationships)
├── Compression Engine  (summarization, dedup, token optimization)
├── Storage Layer       (Postgres, Qdrant, Redis, object store)
└── Governance          (tenant isolation, RBAC/ABAC, audit, retention)
```

---

# 4. Request Lifecycle (High Level)

```text
Caller (Agent Runtime / Workflow / Service)
        │  REST / gRPC
        ▼
   Memory API ──► AuthN/Z + tenant resolution
        │
        ├── write ──► Ingestion (validate → embed → index → persist)
        │
        └── read  ──► Retrieval (hybrid search)
                          │
                          ▼
                    Ranking (score + filter by policy)
                          │
                          ▼
                    Compression (fit token budget)
                          │
                          ▼
                    Return ranked memory set
```

A detailed lifecycle appears in [Overview §6](overview.md).

---

# 5. Document Map

| Document | Responsibility |
|----------|----------------|
| [overview.md](overview.md) | Service responsibilities, architecture, lifecycle, NFRs |
| [memory-api.md](memory-api.md) | External store/retrieve/query contract (REST + gRPC) |
| [storage-architecture.md](storage-architecture.md) | Tiered storage across Postgres/Qdrant/Redis/object store |
| [retrieval.md](retrieval.md) | Hybrid retrieval pipeline and strategies |
| [ranking.md](ranking.md) | Relevance, recency, and importance scoring |
| [semantic-memory.md](semantic-memory.md) | Embeddings and semantic memory |
| [knowledge-graph.md](knowledge-graph.md) | Entity/relationship graph and traversal |
| [compression.md](compression.md) | Summarization, deduplication, token optimization |

---

# 6. Design Principles

1. **One memory plane** — all subsystems read/write through the Engine.
2. **Storage-tiered** — hot, warm, and cold data live in the right backend.
3. **Hybrid retrieval** — vector, keyword, and graph combine for recall + precision.
4. **Governed by default** — isolation, RBAC/ABAC, and audit on every access.
5. **Deterministic & versioned** — every memory is versioned and reproducible.
6. **Token-aware** — retrieved context is compressed to fit model budgets.
7. **Observable** — every read/write emits logs, metrics, and traces.

---

# 7. Dependencies

- [`04-agent-framework/memory-system.md`](../04-agent-framework/memory-system.md) — memory abstraction the Engine operates
- [`05-llm-gateway/index.md`](../05-llm-gateway/index.md) — embedding generation via the Gateway
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md) — access governance
- [`03-workflow-engine/persistence-layer.md`](../03-workflow-engine/persistence-layer.md) — shared persistence patterns
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md) — memory change events

---

# 8. Related Documents

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)
- [`02-architecture/c4-component.md`](../02-architecture/c4-component.md)
- [`04-agent-framework/context-manager.md`](../04-agent-framework/context-manager.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Index |
