<!--
File: docs/17-adr/ADR-0001-project-structure.md
Document ID: ADR-0001
-->

# ADR-0001: Monorepo with Crate and Service Structure

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

Wovyr comprises many components — services (API Gateway, Agent Runtime, Workflow
Engine, LLM Gateway, Memory Engine, Tool Runtime, Plugin Engine, Dashboard) and
shared libraries (provider SDK, tool SDK, common types). We must decide how to
organize the source: one repository or many, and how code is shared and built.

---

# Decision

Use a **single monorepo** with a Cargo workspace of focused **crates** for shared
libraries and **per-service binaries**, plus the dashboard app. This matches the
[repository structure](../../README.md) (`crates/`, `apps/`, `sdk/`, `plugins/`,
`deployment/`, `docs/`).

- Shared logic lives in libraries (`common`, `provider-sdk`, `tool-sdk`).
- Each deployable is a thin binary crate over those libraries.
- One version, one CI, atomic cross-cutting changes.

---

# Consequences

**Positive**
- Atomic refactors across services; no version-skew between internal libraries.
- Single CI/build/test pipeline; shared tooling and standards.
- Easy code reuse; consistent dependencies.

**Negative**
- Larger checkout/build; needs build caching and selective test runs
  (`cargo nextest`, affected-crate detection).
- Requires discipline on crate boundaries to avoid a tangled dependency graph.

---

# Alternatives Considered

- **Polyrepo (repo per service)** — better isolation but painful cross-cutting
  changes, version skew, and duplicated tooling. Rejected for a tightly-integrated
  platform at this stage.
- **Single crate** — too coarse; prevents independent service binaries and clean
  boundaries. Rejected.

---

# Related

- [`README.md`](../../README.md) · [`02-architecture/clean-architecture.md`](../02-architecture/clean-architecture.md)
- [ADR-0006](ADR-0006-clean-architecture.md)
