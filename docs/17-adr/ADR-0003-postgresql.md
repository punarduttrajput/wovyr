<!--
File: docs/17-adr/ADR-0003-postgresql.md
Document ID: ADR-0003
-->

# ADR-0003: PostgreSQL as the System of Record

**Status:** Accepted, **narrowed for GA** — see Current Status  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Current Status (added 2026-07-07)

This decision is real but was never universal. [ADR-0010](ADR-0010-ga-deployment-topology.md)
(2026-07-06) found that every control-plane catalog (tenancy, secrets, KMS,
plugins, webhooks, audit, agents) is **file-only under `~/.apex` by
default** — PostgreSQL is not the system of record for any of them today.
The only Postgres backend genuinely wired into a shipping binary is the
marketplace registry (`PostgresRegistryStore`, selected via
`APEX_MARKETPLACE_POSTGRES_URL`); the workflow store and tiered memory can
also opt into Postgres/Qdrant behind cargo features
(`postgres`/`tiered-memory`), but the GA default (Path A, single-node
appliance) is file-based. Promoting the remaining control-plane catalogs to
a shared Postgres backend is v1.1 "Scale-Out" scope (ticket **DIST-B3**,
[phase3-scale-distribution-tickets.md](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md)
Track B) — this ADR's rationale still holds for that future work, it just
wasn't the GA default it originally implied.

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
- [ADR-0010](ADR-0010-ga-deployment-topology.md) — Path A: file-based storage is the GA default

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-07 | Added a Current Status section: Postgres is real but opt-in per store, not the universal system of record this ADR implied — the GA (Path A) default is file-based; full control-plane promotion is tracked as v1.1 ticket DIST-B3. Found during a project-wide doc review |
| 1.0.0 | 2026-06-27 | Initial decision: PostgreSQL as the system of record |
