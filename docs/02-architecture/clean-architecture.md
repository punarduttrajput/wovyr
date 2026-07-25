# Clean Architecture

**Document ID:** ARCH-006
**Version:** 1.0.1
**Status:** Draft — the layering *pattern* here (Domain → Application →
Interface → Infrastructure) genuinely describes how the real crates are
organized. **Corrected 2026-07-07:** the specific technology names used as
illustrations — gRPC services, a `NATSAdapter`, a `grpc/` module — do not
exist; the real infrastructure layer's adapters are HTTP/SSE (Axum) and
file-based/optional-Postgres/Redis/Qdrant stores, consistent with
[ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)'s Path A decision.
Treat this document's *structure* as accurate and its *named examples* as
illustrative, not current.
**Owner:** Architecture Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document defines the Clean Architecture principles used throughout the Wovyr AI Platform.

It establishes:

* Layer responsibilities
* Dependency rules
* Package organization
* Ports and adapters
* Dependency inversion
* Testing strategy

These principles apply to every Rust crate, backend service, SDK, and future platform extension.

---

# 2. Goals

The architecture aims to achieve:

* Framework independence
* Infrastructure independence
* Database independence
* Testability
* Long-term maintainability
* Replaceable implementations
* Stable public APIs

---

# 3. Architectural Model

The platform combines:

* Clean Architecture
* Hexagonal Architecture
* Domain-Driven Design
* Event-Driven Architecture

Each complements the others:

* **DDD** defines business boundaries.
* **Clean Architecture** defines dependencies.
* **Hexagonal Architecture** defines integration points.
* **Event-Driven Architecture** defines asynchronous communication.

---

# 4. Dependency Rule

The fundamental rule is:

> Source code dependencies always point inward.

Outer layers depend on inner layers.

Inner layers never depend on outer layers.

---

# 5. Layer Model

```text
                 ┌──────────────────────────────┐
                 │     Presentation Layer       │
                 │ REST • gRPC • CLI • UI       │
                 └──────────────┬───────────────┘
                                │
                 ┌──────────────▼───────────────┐
                 │      Application Layer       │
                 │ Use Cases • Commands         │
                 │ Queries • Orchestration      │
                 └──────────────┬───────────────┘
                                │
                 ┌──────────────▼───────────────┐
                 │        Domain Layer          │
                 │ Entities • Aggregates        │
                 │ Value Objects • Services     │
                 └──────────────┬───────────────┘
                                │
                 ┌──────────────▼───────────────┐
                 │    Infrastructure Layer      │
                 │ DB • Redis • LLM • NATS      │
                 │ Files • External APIs        │
                 └──────────────────────────────┘
```

---

# 6. Presentation Layer

## Responsibilities

* HTTP endpoints
* gRPC services
* CLI commands
* WebSocket endpoints
* Request validation
* Response formatting

The Presentation Layer must not contain business rules.

### Examples

* Axum handlers
* Actix handlers
* CLI commands
* Angular API endpoints
* Dashboard controllers

---

# 7. Application Layer

The Application Layer coordinates use cases.

Responsibilities include:

* Command handling
* Query handling
* Transaction boundaries
* Authorization checks
* Workflow orchestration
* Event publishing

Business decisions remain in the Domain Layer.

### Example Use Cases

* StartWorkflow
* ExecuteAgent
* RegisterPlugin
* StoreMemory
* InvokeTool

---

# 8. Domain Layer

The Domain Layer is the heart of the platform.

It contains:

* Entities
* Aggregates
* Value Objects
* Domain Services
* Repository interfaces
* Domain Events
* Business rules

The Domain Layer must not depend on:

* Databases
* Web frameworks
* AI providers
* Serialization libraries
* Infrastructure

---

# 9. Infrastructure Layer

The Infrastructure Layer implements the interfaces defined by the Domain Layer.

Responsibilities include:

* PostgreSQL repositories
* Redis cache
* Qdrant integration
* NATS messaging
* LLM providers
* File storage
* Metrics exporters
* Logging
* Secrets management

Infrastructure can change without affecting the Domain Layer.

---

# 10. Ports and Adapters

Every external dependency is accessed through a port.

## Input Ports

Examples:

* REST API
* CLI
* gRPC
* Scheduled jobs

These invoke application use cases.

---

## Output Ports

Examples:

* WorkflowRepository
* MemoryRepository
* ProviderClient
* EventPublisher
* ObjectStorage
* SecretStore

---

## Adapters

Adapters implement output ports.

Examples:

* PostgreSQLRepository
* RedisCache
* OpenAIAdapter
* AnthropicAdapter
* QdrantAdapter
* NATSAdapter

---

# 11. Example Dependency Flow

```text
Browser
   │
REST API
   │
Application Service
   │
Domain Service
   │
Repository Trait
   │
PostgreSQL Adapter
   │
Database
```

Only the adapter knows about PostgreSQL.

---

# 12. Rust Crate Structure

Each bounded context should follow a consistent internal layout.

```text
engine-workflow/
├── application/
│   ├── commands/
│   ├── queries/
│   └── services/
├── domain/
│   ├── aggregates/
│   ├── entities/
│   ├── events/
│   ├── repositories/
│   ├── services/
│   └── value_objects/
├── infrastructure/
│   ├── persistence/
│   ├── messaging/
│   ├── providers/
│   └── telemetry/
├── interfaces/
│   ├── rest/
│   ├── grpc/
│   └── cli/
└── lib.rs
```

Every engine crate should follow this structure unless there is a documented architectural exception.

---

# 13. Dependency Rules

Allowed dependencies:

* Presentation → Application
* Application → Domain
* Infrastructure → Domain
* Presentation → Shared

Forbidden dependencies:

* Domain → Infrastructure
* Domain → Presentation
* Application → Presentation
* Domain → External SDKs

Any exception requires an approved Architecture Decision Record (ADR).

---

# 14. Shared Kernel

The shared kernel contains reusable abstractions:

* IDs
* Errors
* Events
* Result types
* Time utilities
* Serialization helpers
* Configuration models

The shared kernel must remain stable and intentionally small.

---

# 15. Dependency Injection

Dependencies are provided through constructor injection.

Example pattern:

```rust
pub struct StartWorkflowHandler<R: WorkflowRepository> {
    repository: R,
}
```

Avoid global mutable state and service locators.

---

# 16. Error Handling

Errors are categorized into:

* Domain errors
* Validation errors
* Infrastructure errors
* Transport errors

The Domain Layer defines business errors; outer layers translate them into transport-specific responses.

---

# 17. Testing Strategy

## Domain

* Unit tests
* Property-based tests

## Application

* Use case tests
* Repository mocks

## Infrastructure

* Integration tests
* Contract tests

## Presentation

* API tests
* End-to-end tests

Business rules should be testable without databases or network access.

---

# 18. Observability

Observability concerns belong to the Infrastructure Layer.

Capabilities include:

* Structured logging
* Metrics
* Distributed tracing
* Health checks

Domain entities should not contain logging or telemetry logic.

---

# 19. Benefits

Applying this architecture provides:

* Easier testing
* Clear ownership
* Stable business logic
* Technology independence
* Easier refactoring
* Incremental evolution
* Reduced coupling

---

# 20. Related Documents

* Domain-Driven Design
* C4 Component Diagram
* Event-Driven Architecture
* Rust Workspace Design
* Architecture Decision Records (ADRs)

---

# 21. Revision History

| Version | Date       | Description                         |
| ------- | ---------- | ----------------------------------- |
| 1.0.1   | 2026-07-07 | Added a header note: the layering pattern is real, but its gRPC/NATS-named examples are illustrative, not implemented. Found during a project-wide doc review; no content changed |
| 1.0.0   | 2026-06-26 | Initial Clean Architecture document |
