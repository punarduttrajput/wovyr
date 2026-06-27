# Domain-Driven Design (DDD)

**Document ID:** ARCH-005
**Version:** 1.0.0
**Status:** Draft
**Owner:** Architecture Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the Domain-Driven Design (DDD) model for the Apex AI Platform.

It identifies the platform's bounded contexts, aggregates, entities, value objects, domain events, repositories, and context relationships.

This document serves as the authoritative guide for:

* Rust workspace organization
* Service boundaries
* API ownership
* Database ownership
* Event ownership
* Team ownership

---

# 2. Architectural Philosophy

The platform is organized around **business capabilities**, not technologies.

Each bounded context:

* Owns its data
* Owns its business rules
* Owns its APIs
* Publishes domain events
* Maintains independent evolution

No bounded context may directly manipulate another context's internal state.

---

# 3. Strategic Domains

The platform is divided into the following strategic domains.

## Core Domain

These domains provide the unique competitive value of Apex.

* Agent Runtime
* Workflow Engine
* Memory Engine
* LLM Gateway

---

## Supporting Domains

These domains enable the core domains.

* Tool Runtime
* Plugin Framework
* Scheduler
* Event Bus

---

## Generic Domains

These domains provide common enterprise capabilities.

* Identity
* Projects
* Organizations
* Configuration
* Audit
* Notifications
* Observability

---

# 4. Context Map

```text
                           +---------------------+
                           |  Platform Kernel    |
                           |---------------------|
                           | Identity            |
                           | Config              |
                           | Events              |
                           | Audit               |
                           | Observability       |
                           +----------+----------+
                                      |
          ---------------------------------------------------------
          |            |            |            |                |
          ▼            ▼            ▼            ▼                ▼
 +---------------+ +---------------+ +---------------+ +---------------+ +---------------+
 | Agent Runtime | | Workflow      | | Memory Engine | | LLM Gateway   | | Tool Runtime  |
 |               | | Engine        | |               | |               | |               |
 +-------+-------+ +-------+-------+ +-------+-------+ +-------+-------+ +-------+-------+
         |                 |                 |                 |                 |
         ---------------------------------------------------------------
                                 |
                                 ▼
                        +------------------+
                        | Plugin Framework |
                        +------------------+
```

---

# 5. Bounded Contexts

## Platform Kernel

### Responsibilities

* Identity
* Configuration
* Secrets
* Logging
* Metrics
* Events
* Health
* Audit
* Feature flags

### Owns

* Users
* Roles
* Permissions
* Global configuration

---

## Agent Runtime

### Responsibilities

* Goal execution
* Planning
* Reflection
* Context management
* Multi-agent coordination

### Aggregate Roots

* Agent
* Goal
* Execution Session

### Domain Events

* AgentCreated
* GoalStarted
* GoalCompleted
* GoalFailed

---

## Workflow Engine

### Responsibilities

* Workflow lifecycle
* DAG execution
* Scheduling
* Retry
* Compensation
* Checkpointing

### Aggregate Roots

* Workflow
* WorkflowExecution
* Activity
* Schedule

### Domain Events

* WorkflowStarted
* WorkflowPaused
* WorkflowCompleted
* WorkflowFailed
* ActivityCompleted

---

## Memory Engine

### Responsibilities

* Semantic retrieval
* Episodic memory
* Long-term storage
* Embeddings

### Aggregate Roots

* Memory
* Collection
* KnowledgeGraph
* Embedding

### Domain Events

* MemoryStored
* MemoryRetrieved
* EmbeddingGenerated

---

## LLM Gateway

### Responsibilities

* Provider abstraction
* Routing
* Cost accounting
* Streaming
* Failover

### Aggregate Roots

* Provider
* Model
* CompletionRequest

### Domain Events

* ProviderSelected
* CompletionGenerated
* ProviderUnavailable

---

## Tool Runtime

### Responsibilities

* Tool registration
* Execution
* Sandboxing
* Permissions

### Aggregate Roots

* Tool
* Invocation
* Permission

### Domain Events

* ToolRegistered
* ToolInvoked
* ToolCompleted

---

## Plugin Framework

### Responsibilities

* Plugin discovery
* Lifecycle
* Version management
* Dependency resolution

### Aggregate Roots

* Plugin
* Extension
* Capability

### Domain Events

* PluginInstalled
* PluginEnabled
* PluginDisabled

---

# 6. Shared Kernel

The following concepts are shared across bounded contexts:

* UserId
* ProjectId
* OrganizationId
* WorkflowId
* ExecutionId
* CorrelationId
* TenantId
* Timestamp
* Version

These are represented as immutable value objects.

---

# 7. Value Objects

Examples include:

* Identifier
* Email
* ModelName
* Prompt
* TokenCount
* ExecutionStatus
* RetryPolicy
* ScheduleExpression
* Version
* ResourceLimit

Value objects are immutable and contain no identity beyond their values.

---

# 8. Domain Events

All bounded contexts communicate through domain events.

Examples:

* WorkflowCompleted
* GoalCompleted
* MemoryStored
* ToolInvoked
* PluginInstalled
* UserAuthenticated
* ProjectCreated

Events should be immutable, versioned, and idempotent where possible.

---

# 9. Repositories

Each aggregate root has a corresponding repository interface.

Examples:

* WorkflowRepository
* AgentRepository
* MemoryRepository
* PluginRepository
* UserRepository

Repository interfaces belong to the domain layer; implementations belong to the infrastructure layer.

---

# 10. Application Services

Application services coordinate use cases without containing business rules.

Examples:

* StartWorkflow
* ResumeWorkflow
* ExecuteGoal
* RetrieveContext
* RegisterPlugin
* InvokeTool

Business logic remains inside aggregates and domain services.

---

# 11. Domain Services

Domain services encapsulate business logic that spans multiple aggregates.

Examples:

* WorkflowPlanner
* RetryPolicyEvaluator
* MemoryRankingService
* ProviderSelectionStrategy
* PluginCompatibilityChecker

---

# 12. Anti-Corruption Layers

External systems are accessed through anti-corruption layers (ACLs).

Examples:

* OpenAI Adapter
* Anthropic Adapter
* Qdrant Adapter
* PostgreSQL Adapter
* NATS Adapter
* Keycloak Adapter

Adapters translate external models into internal domain concepts.

---

# 13. Dependency Rules

Mandatory rules:

1. Core domains must not depend on supporting or generic domains.
2. Supporting domains may depend on generic domains.
3. Infrastructure depends on domain interfaces.
4. Domain logic must not depend on infrastructure implementations.
5. Communication across contexts occurs through APIs or domain events.

---

# 14. Team Ownership

Each bounded context should have a clearly defined owning team.

| Domain           | Owning Team       |
| ---------------- | ----------------- |
| Platform Kernel  | Platform          |
| Agent Runtime    | AI Runtime        |
| Workflow Engine  | Workflow          |
| Memory Engine    | AI Infrastructure |
| LLM Gateway      | AI Platform       |
| Tool Runtime     | Runtime           |
| Plugin Framework | Ecosystem         |

---

# 15. Rust Workspace Mapping

Recommended crate structure:

```text
crates/
├── platform-kernel/
├── platform-identity/
├── platform-config/
├── platform-audit/
├── engine-runtime/
├── engine-workflow/
├── engine-memory/
├── engine-llm/
├── engine-tools/
├── engine-plugin/
├── sdk-core/
└── shared-types/
```

Each crate corresponds to one bounded context or a shared abstraction.

---

# 16. Evolution Strategy

New capabilities should be introduced as new bounded contexts when they represent distinct business domains.

Examples:

* Policy Engine
* Billing
* Marketplace
* Evaluation Engine
* Model Registry

Avoid expanding existing contexts beyond their core responsibilities.

---

# 17. Related Documents

* System Overview
* C4 Context
* C4 Container
* C4 Component
* Clean Architecture
* Event-Driven Architecture
* Rust Workspace Design
* ADRs

---

# 18. Revision History

| Version | Date       | Description                           |
| ------- | ---------- | ------------------------------------- |
| 1.0.0   | 2026-06-26 | Initial Domain-Driven Design document |
