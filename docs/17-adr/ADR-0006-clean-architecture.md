<!--
File: docs/17-adr/ADR-0006-clean-architecture.md
Document ID: ADR-0006
-->

# ADR-0006: Clean Architecture + Domain-Driven Design

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

The platform spans many domains (agents, workflows, memory, tools, plugins, billing)
and must remain testable, swappable (datastores, providers), and maintainable as it
grows. We need a consistent internal structure across services.

---

# Decision

Adopt **Clean Architecture** principles with **Domain-Driven Design (DDD)**, as
described in [Clean Architecture](../02-architecture/clean-architecture.md) and
[Domain-Driven Design](../02-architecture/domain-driven-design.md).

- **Dependencies point inward**: domain core has no infrastructure dependencies;
  databases, providers, and transports are outer-layer adapters behind traits.
- **Ports & adapters**: e.g. `MemoryProvider`, `AIProvider`, tool traits are ports;
  Qdrant/PostgreSQL/OpenAI are adapters.
- **Bounded contexts** map to subsystems/crates
  ([ADR-0001](ADR-0001-project-structure.md)).

---

# Consequences

**Positive**
- Infrastructure is swappable (e.g. vector store, provider) without touching domain
  logic — directly enabled the [Provider SDK](../04-agent-framework/provider-sdk.md)
  and pluggable memory backends.
- Domain logic is unit-testable in isolation with fakes
  ([unit testing](../15-testing/unit-tests.md)).
- Clear boundaries reduce coupling across a large codebase.

**Negative**
- More upfront structure/boilerplate (traits, mapping at boundaries).
- Risk of over-abstraction; requires judgment on where ports add value.

---

# Alternatives Considered

- **Layered/transaction-script** — simpler initially but tends toward tight coupling
  to the database and hard-to-test logic at this scale. Rejected.
- **Pure hexagonal without DDD** — similar benefits; DDD adds the domain/bounded-
  context vocabulary that fits a multi-domain platform. Adopted together.

---

# Related

- [`02-architecture/clean-architecture.md`](../02-architecture/clean-architecture.md)
- [`02-architecture/domain-driven-design.md`](../02-architecture/domain-driven-design.md)
- [ADR-0001](ADR-0001-project-structure.md)
