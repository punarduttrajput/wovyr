<!--
File: docs/06-memory-engine/storage-architecture.md
Document ID: MEM-003
-->

# Memory Engine Storage Architecture

**Document ID:** MEM-003  
**File Path:** `docs/06-memory-engine/storage-architecture.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the Memory Engine maps logical memories onto physical storage backends, how data is tiered, and how the stores are kept consistent.

The Engine deliberately uses **multiple specialized stores** rather than one database, because memory has several access patterns — point lookups, vector similarity, graph traversal, and bulk archive — that no single backend serves well.

---

# 2. Backends

| Backend | Role | Stores |
|---------|------|--------|
| PostgreSQL | System of record | Memory records, metadata, versions, ACLs |
| Qdrant | Vector index | Embeddings + payload for similarity search |
| Redis | Hot tier / cache | Working memory, query cache, hot records |
| Object storage | Cold / archive | Aged records, large payloads, snapshots |
| Graph store | Knowledge graph | Entities + relationships (see [knowledge-graph.md](knowledge-graph.md)) |

The graph may be implemented inside PostgreSQL (adjacency/recursive CTEs) initially
and migrated to a dedicated graph database if traversal volume requires it.

---

# 3. Tiering Model

```text
            access frequency / recency
   high ┌──────────────────────────────────────┐ low
        │ HOT        WARM         COLD   ARCHIVE │
        │ Redis      PG+Qdrant    PG+Qdrant  S3  │
        └──────────────────────────────────────┘
```

| Tier | Backend | Typical contents | Latency |
|------|---------|------------------|---------|
| Hot | Redis | Working memory, active conversation, cached queries | sub-ms |
| Warm | PostgreSQL + Qdrant | Recent conversation/workflow/episodic | < 30 ms |
| Cold | PostgreSQL + Qdrant | Semantic/organizational knowledge | < 50 ms |
| Archive | Object storage | Aged, low-importance records | seconds |

The **PostgreSQL record is authoritative**; Qdrant, Redis, and the archive are
derived and rebuildable from it.

---

# 4. Record Storage (PostgreSQL)

The canonical record table (simplified):

```sql
CREATE TABLE memory (
  id           TEXT PRIMARY KEY,
  tenant       TEXT NOT NULL,
  scope        TEXT NOT NULL,
  project      TEXT,
  agent        TEXT,
  type         TEXT NOT NULL,
  title        TEXT,
  content      TEXT NOT NULL,
  tags         TEXT[],
  labels       JSONB,
  metadata     JSONB,
  importance   REAL DEFAULT 0,
  version      INT  NOT NULL DEFAULT 1,
  tier         TEXT NOT NULL DEFAULT 'warm',
  embedding_id TEXT,            -- pointer into Qdrant
  deleted_at   TIMESTAMPTZ,     -- soft delete tombstone
  created_at   TIMESTAMPTZ NOT NULL,
  updated_at   TIMESTAMPTZ NOT NULL
);

CREATE TABLE memory_version (
  id        TEXT,
  version   INT,
  content   TEXT,
  labels    JSONB,
  created_at TIMESTAMPTZ,
  PRIMARY KEY (id, version)
);
```

Indexes: `(tenant, scope, project)`, GIN on `tags`/`labels`, and a full-text index
on `content` for the keyword retrieval path.

---

# 5. Vector Storage (Qdrant)

Each embedded memory has a Qdrant point:

```json
{
  "id": "mem_01H...",
  "vector": [0.01, -0.02, "..."],
  "payload": {
    "tenant": "acme",
    "scope": "project",
    "project": "support-bot",
    "type": "semantic",
    "tags": ["refunds"],
    "importance": 0.8,
    "created_at": 1750000000
  }
}
```

- **Collections are namespaced per tenant** (or per tenant+type) to guarantee
  isolation and bound search space.
- Payload fields mirror the filters in the [Memory API query](memory-api.md#6-query-request)
  so metadata filtering happens inside the vector search.
- HNSW parameters (`m`, `ef_construct`, `ef`) are tuned per collection size.

---

# 6. Hot Tier (Redis)

Redis holds:

- Working memory (TTL = execution lifetime)
- Hot record cache (recently read records)
- Query result cache (short TTL, keyed by normalized query + scope)
- Distributed locks for ingestion and reaper coordination

Redis is a **cache and ephemeral store** — losing it degrades latency, not durability.

---

# 7. Archive (Object Storage)

Aged or low-importance records are serialized and moved to object storage:

```text
s3://apex-memory/{tenant}/{year}/{month}/{id}.json.zst
```

The PostgreSQL row is retained as a lightweight stub (`tier = 'archive'`) pointing
to the object, so the record is still discoverable; full content is rehydrated on
demand.

---

# 8. Write Path & Consistency

```text
1. Begin: write canonical row to PostgreSQL  (durable)
2. Request embedding from LLM Gateway
3. Upsert vector + payload to Qdrant
4. Populate Redis hot cache
5. Emit memory.created event (Event Bus)
```

Consistency model:

- PostgreSQL commit is the **durability point**; the API may return after step 1
  with `embedded:false` and complete steps 2–4 asynchronously.
- Qdrant/Redis are **eventually consistent** with PostgreSQL; a reconciliation job
  rebuilds drifted index entries from the canonical rows.
- Deletes tombstone in PostgreSQL first, then purge Qdrant/Redis.

---

# 9. Reindex & Recovery

Because PostgreSQL is authoritative, the Engine can fully rebuild derived stores:

- **Reindex** — re-embed and re-upsert all vectors (e.g. after changing the
  embedding model); runs as a throttled background job, dual-writing old+new
  collections and swapping atomically.
- **Recovery** — on Qdrant loss, rebuild collections from PostgreSQL; on Redis
  loss, simply repopulate on demand.

---

# 10. Tenant Isolation

- Every query is constrained by `tenant` at the SQL layer and via per-tenant
  Qdrant collections.
- Object storage prefixes are per-tenant with bucket policies.
- No cross-tenant index is ever shared. Isolation is verified in tests as a hard
  requirement (zero leakage).

---

# 11. Encryption & Security

- Encryption at rest on all backends; TLS/mTLS in transit.
- Sensitive fields may be encrypted at the application layer with per-tenant keys.
- Archived objects use server-side encryption with key references, never inline keys.

See [Overview §12](overview.md#12-security).

---

# 12. Capacity & Scaling

| Backend | Scaling approach |
|---------|------------------|
| PostgreSQL | Primary/replica; partition `memory` by tenant/time |
| Qdrant | Sharded/distributed collections; replicas for read |
| Redis | Cluster mode; eviction by LRU on hot cache |
| Object storage | Effectively unlimited |

Target scale: **billions of records** with sub-50 ms warm retrieval.

---

# 13. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Canonical write commit | < 15 ms p95 |
| Vector upsert | < 25 ms p95 |
| Index/record drift | reconciled < 60 s |
| Rebuild throughput | 10k+ vectors/sec |

---

# 14. Dependencies

- [`03-workflow-engine/persistence-layer.md`](../03-workflow-engine/persistence-layer.md)
- [`05-llm-gateway/index.md`](../05-llm-gateway/index.md)
- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)

---

# 15. Related Documents

- [`06-memory-engine/overview.md`](overview.md)
- [`06-memory-engine/retrieval.md`](retrieval.md)
- [`06-memory-engine/semantic-memory.md`](semantic-memory.md)
- [`06-memory-engine/knowledge-graph.md`](knowledge-graph.md)

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Storage Architecture |
