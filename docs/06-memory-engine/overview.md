<!--
File: docs/06-memory-engine/overview.md
Document ID: MEM-001
-->

# Memory Engine Overview

**Document ID:** MEM-001  
**File Path:** `docs/06-memory-engine/overview.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **Memory Engine**, the deployable service that durably stores and serves all agent memory in the Wovyr AI Platform.

The Engine centralizes everything that should not be re-implemented per agent: durable storage, embedding/indexing, hybrid retrieval, ranking, knowledge-graph maintenance, compression, retention, and access governance. It is the operational counterpart to the [Memory System](../04-agent-framework/memory-system.md) abstraction.

---

# 2. Scope

The Memory Engine is responsible for:

- A network API for memory read/write (REST + gRPC)
- Ingestion: validation, embedding, indexing, persistence
- Hybrid retrieval (vector + keyword + graph)
- Relevance ranking and policy filtering
- Knowledge graph maintenance and traversal
- Context compression to fit token budgets
- Versioning, retention, and archival
- Tenant isolation, RBAC/ABAC, and audit

The Memory Engine is **not** responsible for:

- Prompt assembly — see [Context Manager](../04-agent-framework/context-manager.md)
- Generating embeddings itself — it delegates to the [LLM Gateway](../05-llm-gateway/index.md)
- Deciding *what* to remember — that is the Agent Runtime's job

---

# 3. Position in the Platform

```text
 Agent Runtime ─┐
 Workflow Engine├──► Memory Engine ──► PostgreSQL  (records, metadata)
 Tool Runtime   │        │          ──► Qdrant      (vectors)
 Dashboard      ┘        │          ──► Redis       (working/cache)
                         │          ──► Object Store (large/archive)
                         └── embeddings ──► LLM Gateway
                         └── change events ──► Event Bus
```

The Engine is horizontally scalable. Read replicas and the vector store scale
independently from the write path. See
[C4 Container §4.4](../02-architecture/c4-container.md).

---

# 4. Memory Tiers

The Engine maps the conceptual [memory types](../04-agent-framework/memory-system.md#6-memory-types)
onto physical tiers:

| Tier | Memory types | Backend | Latency |
|------|--------------|---------|---------|
| Hot | Working, active conversation | Redis | sub-ms |
| Warm | Conversation, workflow, recent episodic | PostgreSQL + Qdrant | < 30 ms |
| Cold | Semantic, organizational knowledge | PostgreSQL + Qdrant | < 50 ms |
| Archive | Aged/low-importance records | Object storage | seconds |

Records migrate between tiers based on age, access frequency, and importance
(see [Lifecycle](#7-memory-lifecycle) and
[storage-architecture.md](storage-architecture.md)).

---

# 5. Core Responsibilities

## 5.1 Ingestion

Writes flow through a pipeline: schema validation → embedding (via the
[LLM Gateway](../05-llm-gateway/index.md)) → indexing (vector + keyword +
metadata) → durable persistence → change event.

## 5.2 Retrieval

Reads combine vector similarity, keyword/BM25, metadata filters, and graph
traversal into a single ranked result. See [Retrieval](retrieval.md).

## 5.3 Ranking

Candidates are scored by relevance, recency, and importance, then filtered by
policy. See [Ranking](ranking.md).

## 5.4 Knowledge Graph

Entities and relationships extracted from memories form a graph enabling
multi-hop reasoning. See [Knowledge Graph](knowledge-graph.md).

## 5.5 Compression

Result sets are summarized/deduplicated to fit the caller's token budget. See
[Compression](compression.md).

## 5.6 Governance

Every access is authenticated, authorized (RBAC/ABAC), tenant-isolated, and
audited, per the [Policy Engine](../04-agent-framework/policy-engine.md).

---

# 6. Request Lifecycle

```text
WRITE
1. Receive memory record       (REST / gRPC)
2. Authenticate + resolve tenant
3. Validate schema + policy
4. Generate embedding          (LLM Gateway)
5. Index (vector + keyword + metadata)
6. Persist (tier-appropriate backend)
7. Emit memory.created/updated event
8. Return memory id + version

READ
1. Receive query               (REST / gRPC)
2. Authenticate + resolve tenant
3. Embed query                 (LLM Gateway, if semantic)
4. Hybrid search across tiers
5. Rank candidates
6. Apply policy filter (drop unauthorized)
7. Compress to token budget
8. Return ranked memory set + scores
```

---

# 7. Memory Lifecycle

```text
Created → Indexed → Stored → Retrieved → Updated(+version) → Aged → Archived → Expired
```

Lifecycle transitions are driven by retention policy and access patterns.
Versioning preserves history (see
[Memory System §21](../04-agent-framework/memory-system.md#21-memory-versioning)).

---

# 8. Retention & Archival

| Memory | Default retention |
|--------|-------------------|
| Working | Execution only (Redis TTL) |
| Conversation | Configurable per tenant |
| Workflow | Permanent (with workflow) |
| Episodic | Permanent, archivable |
| Semantic / Organizational | Permanent |
| Archive | Configurable cold storage |

Retention is enforced by a background reaper that demotes, archives, or expires
records and reclaims index space.

---

# 9. Module Organization

```text
service-memory-engine/
├── api/            # REST + gRPC handlers
├── ingestion/      # validate, embed, index, persist
├── retrieval/      # hybrid search
├── ranking/        # scoring + policy filtering
├── graph/          # knowledge graph
├── compression/    # summarization, dedup
├── storage/        # postgres, qdrant, redis, object-store adapters
├── retention/      # lifecycle reaper, archival
├── governance/     # isolation, RBAC/ABAC, audit
├── telemetry/      # logs, metrics, traces
└── main.rs
```

---

# 10. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Hot read (Redis) | < 2 ms p95 |
| Warm retrieval (vector + rank) | < 30 ms p95 |
| Write (incl. embedding) | < 60 ms p95 (embedding-dependent) |
| Ingestion throughput | 10k+ records/sec/instance |
| Availability | 99.99% |
| Scale | Billions of memories |
| Cross-tenant leakage | 0 (hard isolation) |

---

# 11. Failure Behavior

| Failure | Behavior |
|---------|----------|
| Embedding (Gateway) unavailable | Queue write; persist record, embed asynchronously |
| Qdrant unavailable | Degrade to keyword/metadata retrieval |
| Redis unavailable | Bypass hot cache; serve from warm tier |
| Object store unavailable | Archive operations deferred; reads of hot/warm unaffected |
| PostgreSQL primary down | Reads from replica; writes fail until failover |

Degraded retrieval is reported in the response so callers know recall may be
reduced.

---

# 12. Security

- Encryption at rest (all backends) and in transit (mTLS).
- Tenant isolation enforced at query construction and storage namespace.
- RBAC + ABAC on every memory access; sensitive memories require elevated scope.
- PII masking before logging.
- Full audit trail of reads and writes.

See [Memory System §23](../04-agent-framework/memory-system.md#23-memory-security)
and the planned `13-security/` section.

---

# 13. Observability

Every operation emits logs, metrics (retrieval latency, recall proxy, cache hit
ratio, index size, tier distribution), and OpenTelemetry traces. Memory change
events publish to the [Event Bus](../03-workflow-engine/event-bus.md).

---

# 14. Dependencies

- [`04-agent-framework/memory-system.md`](../04-agent-framework/memory-system.md)
- [`05-llm-gateway/index.md`](../05-llm-gateway/index.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`03-workflow-engine/persistence-layer.md`](../03-workflow-engine/persistence-layer.md)

---

# 15. Related Documents

- [`06-memory-engine/memory-api.md`](memory-api.md)
- [`06-memory-engine/storage-architecture.md`](storage-architecture.md)
- [`06-memory-engine/retrieval.md`](retrieval.md)
- [`06-memory-engine/ranking.md`](ranking.md)
- [`06-memory-engine/semantic-memory.md`](semantic-memory.md)
- [`06-memory-engine/knowledge-graph.md`](knowledge-graph.md)
- [`06-memory-engine/compression.md`](compression.md)

---

# 16. Future Enhancements

- Memory federation across regions
- Autonomous pruning and confidence scoring
- AI-generated knowledge graphs
- Multi-modal memory (image/audio)
- Time-travel queries over version history

---

# 17. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Overview |
