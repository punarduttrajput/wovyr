<!--
File: docs/17-adr/index.md
Document ID: ADR-INDEX-001
-->

# Architecture Decision Records (ADRs)

**Document ID:** ADR-INDEX-001  
**File Path:** `docs/17-adr/index.md`  
**Version:** 1.4.0  
**Status:** Active  
**Owner:** Architecture Team  
**Last Updated:** 2026-07-17

---

# 1. Purpose

This section records the **significant architectural decisions** for the Wovyr AI Platform — the *why* behind major choices — so future contributors understand the reasoning and the trade-offs that were accepted.

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
| [ADR-0003](ADR-0003-postgresql.md) | PostgreSQL as system of record | Accepted, narrowed for GA — opt-in, not universal (see ADR's Current Status) |
| [ADR-0004](ADR-0004-qdrant.md) | Qdrant for vector search | Accepted, opt-in for GA (see ADR's Current Status) |
| [ADR-0005](ADR-0005-nats.md) | NATS JetStream for the event bus | Accepted, **not implemented** — deferred to v1.1 (see ADR's Current Status) |
| [ADR-0006](ADR-0006-clean-architecture.md) | Clean Architecture + DDD | Accepted |
| [ADR-0007](ADR-0007-plugin-system.md) | Plugin-first extensibility | Accepted |
| [ADR-0008](ADR-0008-subworkflows.md) | Child workflows as activities (not inline expansion) | Accepted |
| [ADR-0009](ADR-0009-keyless-signing.md) | Wovyr-native keyless signing (Sigstore-shaped, offline-verifiable) | Accepted |
| [ADR-0010](ADR-0010-ga-deployment-topology.md) | GA as single-node appliance (Path A); distributed platform (Path B) as v1.1 follow-on | Accepted |
| [ADR-0011](ADR-0011-generative-ui-repositioning.md) | Reposition the product as the Generative UI Trust Runtime (platform becomes the engine; open UI shapes adopted, not invented; constrained component vocabulary; no browser) | Accepted |
| [ADR-0012](ADR-0012-mcp-connection-trust-boundary.md) | Trust boundary for user-managed MCP connections: `Stdio` transport gated like the `shell` tool (operator opt-in + `mcp:admin`); `Http` transport reuses the existing SSRF guard; credentials are vault references; no sandboxing of `Stdio` in v1 (stated residual risk) | Accepted |
| [ADR-0013](ADR-0013-client-sdk-languages.md) | First-party client SDKs stay TypeScript + Python; Go/Java are a documented non-goal — the generated `/openapi.json` (CI-gated ground truth) is the supported path for other languages, with a recorded revisit trigger | Accepted |

---

# 5. Relationship to Other Docs

ADRs record *decisions*; the [Architecture](../02-architecture/c4-context.md) docs
describe the resulting *design*. When they conflict, the latest accepted ADR wins
and the design docs should be updated.

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.4.0 | 2026-07-17 | Added ADR-0013 (client SDK languages: TS+Python only, OpenAPI generation as the path for others — closes DX-306) |
| 1.3.0 | 2026-07-15 | Added ADR-0012 (trust boundary for user-managed MCP connections) |
| 1.2.0 | 2026-07-14 | Added ADR-0011 (Generative UI Trust Runtime repositioning) |
| 1.1.0 | 2026-07-07 | Annotated ADR-0003/0004/0005's Status column: none of the three were fully executed as originally decided (Postgres/Qdrant are opt-in not universal; NATS was never implemented at all). Each ADR now has its own Current Status section with detail. Found during a project-wide doc review |
| 1.0.0 | 2026-06-27 | Initial ADR register |
