<!--
File: docs/17-adr/ADR-0003-postgresql.md
Document ID: ADR-0003
-->

# ADR-0003: PostgreSQL as the System of Record

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

The platform needs a durable, transactional store for authoritative data: users,
projects, agent/workflow definitions, execution metadata, memory records, and ACLs.
It must support strong consistency, rich queries, and operational maturity.

---

# Decision

Use **PostgreSQL** as the **system of record** across the platform. Derived stores
(Qdrant, Redis, object storage) are rebuildable from it
([Memory storage](../06-memory-engine/storage-architecture.md#3-tiering-model)).

Rationale:
- ACID transactions for correctness (definitions, executions, grants).
- Rich querying: relational + JSONB + full-text (the keyword
  [retrieval](../06-memory-engine/retrieval.md) path) + arrays.
- Mature ecosystem: replication, PITR, partitioning, broad managed offerings.
- Extensible (e.g. `pgvector` is an option, though dedicated vector search is
  [ADR-0004](ADR-0004-qdrant.md)).

---

# Consequences

**Positive**
- One trustworthy source of truth; derived indexes are reconstructable.
- Strong consistency where it matters (auth, workflow state, billing).
- Operationally well-understood; easy to run managed
  ([Terraform](../12-deployment/terraform.md)).

**Negative**
- Horizontal write scaling requires partitioning/sharding strategy at very large
  scale (planned: partition by tenant/time).
- Not ideal for high-recall vector similarity at scale → delegated to Qdrant.

---

# Alternatives Considered

- **MySQL** — capable but weaker JSON/extension story for our needs. Rejected.
- **MongoDB** — flexible documents but weaker multi-row transactional guarantees for
  workflow/billing correctness. Rejected as system of record.
- **CockroachDB/Spanner** — strong distributed SQL; heavier/cost and unnecessary at
  current scale. Revisit if global write-scaling demands it.

---

# Related

- [`06-memory-engine/storage-architecture.md`](../06-memory-engine/storage-architecture.md)
- [ADR-0004](ADR-0004-qdrant.md)
