<!--
File: docs/17-adr/index.md
Document ID: ADR-INDEX-001
-->

# Architecture Decision Records (ADRs)

**Document ID:** ADR-INDEX-001  
**File Path:** `docs/17-adr/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** Architecture Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This section records the **significant architectural decisions** for the Apex AI Platform — the *why* behind major choices — so future contributors understand the reasoning and the trade-offs that were accepted.

---

# 2. What an ADR Is

An ADR captures a single decision with its context and consequences. ADRs are
**immutable once accepted**: to change a decision, write a new ADR that supersedes
the old one (the old one stays, marked `Superseded`).

---

# 3. ADR Template

```text
Title · Status · Context · Decision · Consequences · Alternatives Considered
```

Status is one of: `Proposed`, `Accepted`, `Superseded by ADR-XXXX`, `Deprecated`.

---

# 4. Register

| ADR | Decision | Status |
|-----|----------|--------|
| [ADR-0001](ADR-0001-project-structure.md) | Monorepo + crate/service structure | Accepted |
| [ADR-0002](ADR-0002-rust.md) | Rust as the implementation language | Accepted |
| [ADR-0003](ADR-0003-postgresql.md) | PostgreSQL as system of record | Accepted |
| [ADR-0004](ADR-0004-qdrant.md) | Qdrant for vector search | Accepted |
| [ADR-0005](ADR-0005-nats.md) | NATS JetStream for the event bus | Accepted |
| [ADR-0006](ADR-0006-clean-architecture.md) | Clean Architecture + DDD | Accepted |
| [ADR-0007](ADR-0007-plugin-system.md) | Plugin-first extensibility | Accepted |
| [ADR-0008](ADR-0008-subworkflows.md) | Child workflows as activities (not inline expansion) | Accepted |

---

# 5. Relationship to Other Docs

ADRs record *decisions*; the [Architecture](../02-architecture/c4-context.md) docs
describe the resulting *design*. When they conflict, the latest accepted ADR wins
and the design docs should be updated.

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial ADR register |
