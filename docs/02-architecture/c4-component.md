# C4 Model – Level 3: Component Diagram

**Document ID:** ARCH-004
**Version:** 1.0.1
**Status:** Draft — Day-1 target-state component diagram, unrevised since
project inception. **Corrected 2026-07-07:** the components below are real
(as in-process Rust modules within one `apex-server` binary — see the
[README](../../README.md)'s architecture section and each crate's own doc
comments for the kept-current, per-crate description),
but the "Cross-container" communication row using gRPC does not exist (no
gRPC anywhere in the codebase) and "Event notification" is not NATS
JetStream (no message broker exists — `apex-events` is a custom in-process
system; see [ADR-0005](../17-adr/ADR-0005-nats.md)'s current-status note).
This is consistent with [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)'s
Path A single-node decision.
**Owner:** Architecture Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document describes the internal component architecture of the Apex AI Platform.

Where the Container Diagram defines deployable applications, this document defines the major software components inside those containers.

These components map directly to Rust workspace crates and domain boundaries.

---

# 2. Scope

This document defines:

* Internal components
* Component responsibilities
* Dependency rules
* Communication patterns
* Public interfaces
* Crate mappings
* Extension points

Implementation details are intentionally deferred to crate-specific documentation.

---

# 3. Component Philosophy

Components should satisfy the following principles:

* Single Responsibility
* High Cohesion
* Low Coupling
* Interface Segregation
* Dependency Inversion
* Replaceable Implementations
* Testability
* Stable Public Contracts

---

# 4. Component Hierarchy

```text
                           Apex AI Platform
                                   │
        ┌──────────────────────────┼──────────────────────────┐
        │                          │                          │
     Platform                 Runtime                    Infrastructure
        │                          │                          │
        ├──────────────┬───────────┴──────────────┬───────────┤
        ▼              ▼                          ▼
 Identity        Workflow Engine          Agent Runtime
 Projects         Scheduler               Planner
 Users            State Machine           Executor
 Config           DAG Engine              Reflection
 Audit            Retry Engine            Context Manager
                  Checkpoint              Goal Manager
        │
        ├─────────────────────────────────────────────────────┐
        ▼                                                     ▼
 Memory Engine                                       LLM Gateway
 Vector Search                                       Provider Registry
 Episodic Memory                                     Router
 Knowledge Graph                                     Streaming
 Embeddings                                          Token Manager
        │                                                     │
        └──────────────────────────┬──────────────────────────┘
                                   ▼
                            Tool Runtime
                          Plugin Framework
                          Event Bus
```

---

# 5. Platform Components

## Identity Component

Responsibilities:

* Authentication
* Authorization
* Token validation
* Session management
* RBAC

Public Interfaces:

* Auth Service
* User Context
* Permission Resolver

---

## Organization Component

Responsibilities:

* Organizations
* Teams
* Membership
* Tenant isolation

---

## Project Component

Responsibilities:

* Project lifecycle
* Environment configuration
* Secrets references
* Project metadata

---

## Configuration Component

Responsibilities:

* Global settings
* Environment settings
* Feature flags
* Runtime configuration

---

## Audit Component

Responsibilities:

* Audit events
* Compliance records
* Security logs

---

# 6. Workflow Components

## Workflow Definition

Responsibilities:

* Parse workflow DSL
* Validate definitions
* Version workflows
* Compile execution plans

---

## Scheduler

Responsibilities:

* Timers
* Delayed execution
* Cron schedules
* Queue management

---

## Execution Engine

Responsibilities:

* Execute workflow graph
* Parallel branches
* Conditional execution
* Synchronization

---

## State Machine

Responsibilities:

* Lifecycle transitions
* Durable state
* Recovery
* Replay

---

## Checkpoint Manager

Responsibilities:

* Save progress
* Restore execution
* Snapshot generation

---

## Retry Manager

Responsibilities:

* Retry policies
* Backoff strategies
* Failure handling

---

## Compensation Engine

Responsibilities:

* Rollback execution
* Saga orchestration
* Recovery actions

---

# 7. Agent Runtime Components

## Planner

Creates execution plans from user goals.

---

## Reasoner

Coordinates interaction with LLM providers and applies reasoning strategies.

---

## Executor

Executes workflow steps and tool calls.

---

## Reflection Engine

Evaluates previous outputs and determines whether additional reasoning or correction is required.

---

## Goal Manager

Tracks objectives, progress, priorities, and completion status.

---

## Context Manager

Aggregates runtime state, workflow context, memory retrievals, and tool outputs.

---

# 8. Memory Components

## Short-Term Memory

Maintains execution-specific context.

---

## Long-Term Memory

Stores persistent knowledge and historical interactions.

---

## Semantic Memory

Indexes and retrieves embeddings.

---

## Knowledge Graph

Maintains structured relationships between entities.

---

## Embedding Manager

Generates, stores, and updates embeddings.

---

## Retrieval Pipeline

Ranks, filters, and returns relevant context.

---

# 9. LLM Gateway Components

## Provider Registry

Maintains available model providers and capabilities.

---

## Router

Selects the appropriate provider based on policy.

---

## Streaming Engine

Supports incremental response delivery.

---

## Token Manager

Tracks token usage, quotas, and estimated cost.

---

## Failover Manager

Redirects requests when providers are unavailable or exceed policy thresholds.

---

# 10. Tool Runtime Components

## Tool Registry

Registers available tools and their metadata.

---

## Permission Manager

Evaluates tool execution permissions.

---

## Sandbox Manager

Executes tools within isolated environments.

---

## Invocation Engine

Coordinates tool execution, validation, and result handling.

---

# 11. Plugin Framework Components

## Plugin Registry

Discovers and indexes installed plugins.

---

## Lifecycle Manager

Handles installation, activation, upgrade, and removal.

---

## Compatibility Manager

Validates version compatibility and dependency constraints.

---

## Capability Registry

Publishes plugin-provided services to the platform.

---

# 12. Shared Components

These cross-cutting components are available across the platform:

* Event Bus
* Metrics
* Logging
* Distributed Tracing
* Configuration
* Secrets
* Cache
* Serialization
* Error Handling
* Health Monitoring

---

# 13. Dependency Rules

The following dependency rules are mandatory:

1. Components depend only on published interfaces.
2. Domain components do not access infrastructure directly.
3. Infrastructure implementations are replaceable.
4. Components communicate through explicit contracts.
5. Circular dependencies are prohibited.

---

# 14. Component Communication

Preferred communication mechanisms:

| Interaction        | Pattern                |
| ------------------ | ---------------------- |
| Same process       | Rust trait interfaces  |
| Cross-container    | gRPC                   |
| Event notification | NATS JetStream         |
| Client access      | REST / WebSocket       |
| Long-running work  | Asynchronous messaging |

---

# 15. Rust Workspace Mapping

Each major component corresponds to one or more Rust crates.

Illustrative mapping:

| Component           | Workspace Crate     |
| ------------------- | ------------------- |
| Workflow Engine     | `engine-workflow`   |
| Agent Runtime       | `engine-runtime`    |
| Memory Engine       | `engine-memory`     |
| LLM Gateway         | `engine-llm`        |
| Tool Runtime        | `engine-tools`      |
| Plugin Framework    | `engine-plugin`     |
| Identity            | `platform-identity` |
| Projects            | `platform-projects` |
| Configuration       | `platform-config`   |
| Audit               | `platform-audit`    |
| Common abstractions | `engine-core`       |

Additional crate organization is defined in the Rust workspace architecture.

---

# 16. Extension Points

Supported extension mechanisms include:

* Custom LLM providers
* Workflow activities
* Memory providers
* Storage backends
* Authentication providers
* Scheduling strategies
* Plugins
* Dashboard modules

Extension points must be documented with stable public interfaces.

---

# 17. Design Constraints

Components should:

* Remain independently testable.
* Avoid implementation leakage.
* Expose minimal public APIs.
* Maintain backward compatibility.
* Minimize compile-time dependencies.

---

# 18. Related Documents

* System Overview
* C4 Context
* C4 Container
* Domain-Driven Design
* Clean Architecture
* Rust Workspace Design
* Architecture Decision Records (ADRs)

---

# 19. Revision History

| Version | Date       | Description                            |
| ------- | ---------- | -------------------------------------- |
| 1.0.1   | 2026-07-07 | Added a header note: no gRPC or NATS exist anywhere in the codebase, contradicting the communication-pattern table. Found during a project-wide doc review; no content changed |
| 1.0.0   | 2026-06-26 | Initial C4 Level 3 – Component Diagram |
