# Event-Driven Architecture

**Document ID:** ARCH-007
**Version:** 1.0.1
**Status:** Draft — the event *model* (immutable, append-only, versioned
domain events) genuinely describes the real `wovyr-events`/`wovyr-audit`
implementation. **Corrected 2026-07-07:** §5's "Preferred implementation:
NATS JetStream" was never built — there is no message broker at all;
`wovyr-events` is a custom in-process event/webhook/audit system, sufficient
for the single-node appliance [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)
ratified for GA. A real distributed event bus only becomes necessary once
multiple replicas exist (v1.1 "Scale-Out") — see
[ADR-0005](../17-adr/ADR-0005-nats.md)'s current-status note and the new
tracked ticket it links to.
**Owner:** Architecture Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document defines the event-driven architecture used throughout the Wovyr AI Platform.

It establishes the event model, event taxonomy, messaging patterns, delivery guarantees, event lifecycle, and integration principles.

The event architecture enables:

* Durable workflow execution
* Distributed coordination
* Loose coupling
* Horizontal scalability
* Replay and recovery
* Observability

---

# 2. Objectives

The event system should provide:

* Reliable delivery
* Event ordering where required
* Idempotent processing
* Replay capability
* High throughput
* Low latency
* Fault tolerance

---

# 3. Event Philosophy

Events represent **facts that have already occurred**.

Examples:

* WorkflowStarted
* GoalCompleted
* MemoryStored
* PluginInstalled

Events are immutable.

Events are append-only.

Events are versioned.

---

# 4. Event Categories

## Domain Events

Represent business state changes.

Examples:

* WorkflowCompleted
* ActivityFailed
* GoalStarted
* MemoryRetrieved

---

## Integration Events

Used for communication between bounded contexts.

Examples:

* UserProvisioned
* ProjectArchived
* PluginPublished

---

## Infrastructure Events

Represent operational system activity.

Examples:

* NodeJoined
* WorkerHeartbeat
* SchedulerTick
* StorageAvailable

---

# 5. Event Bus

The platform uses a centralized event bus abstraction.

Preferred implementation:

* NATS JetStream

Alternative implementations:

* Apache Kafka
* RabbitMQ

The business logic depends only on the event abstraction, not on a specific broker.

---

# 6. Event Lifecycle

```text id="l2ev1p"
Business Action
      │
      ▼
Domain Event Raised
      │
      ▼
Application Service
      │
      ▼
Event Publisher
      │
      ▼
Event Bus
      │
      ▼
Interested Subscribers
      │
      ▼
State Updates / Side Effects
```

---

# 7. Event Structure

Every event contains:

* Event ID
* Event Type
* Version
* Aggregate ID
* Aggregate Type
* Correlation ID
* Causation ID
* Tenant ID
* Timestamp
* Producer
* Payload
* Metadata

---

# 8. Event Metadata

Metadata should include:

* Trace ID
* Span ID
* User ID
* Organization ID
* Source Service
* Retry Count
* Schema Version

This supports observability and debugging.

---

# 9. Naming Conventions

Events use the **Past Tense**.

Examples:

* WorkflowStarted
* WorkflowCompleted
* ActivitySucceeded
* GoalFailed
* MemoryStored
* PluginEnabled

Commands and queries should not be published as events.

---

# 10. Delivery Guarantees

| Event Type     | Guarantee                   |
| -------------- | --------------------------- |
| Domain         | At least once               |
| Integration    | At least once               |
| Infrastructure | Best effort or configurable |

Consumers must be idempotent.

---

# 11. Event Ordering

Ordering is guaranteed only within a single aggregate.

Examples:

Workflow A:

1. Started
2. ActivityCompleted
3. WorkflowCompleted

Workflow B may execute independently.

Global ordering is not required.

---

# 12. Idempotency

Every consumer must tolerate duplicate delivery.

Strategies include:

* Event ID tracking
* Aggregate version checks
* Optimistic concurrency
* Deduplication tables

---

# 13. Event Versioning

Every event includes a schema version.

Rules:

* Additive changes are preferred.
* Breaking changes require a new version.
* Older versions remain supported according to the deprecation policy.

---

# 14. Correlation

Every workflow receives a Correlation ID.

Sub-events inherit the same Correlation ID.

Causation IDs identify the triggering event.

Example:

```text id="7zq8dn"
WorkflowStarted
       │
       ▼
GoalStarted
       │
       ▼
ToolInvoked
       │
       ▼
MemoryStored
```

All share the same Correlation ID while each references the preceding event as its Causation ID.

---

# 15. Event Topics

Illustrative topic hierarchy:

```text id="e3m6tf"
workflow.*
workflow.activity.*

agent.*

memory.*

plugin.*

tool.*

identity.*

project.*

audit.*

system.*
```

Topic names should remain stable and versioned when necessary.

---

# 16. Event Replay

Replay enables:

* Workflow recovery
* State reconstruction
* Testing
* Auditing

Replay should preserve event order for a given aggregate.

Side effects must be controlled during replay.

---

# 17. Dead Letter Handling

Failed events may be routed to a Dead Letter Queue (DLQ).

DLQ records should include:

* Original event
* Failure reason
* Processing attempts
* Timestamp

Operators should be able to inspect and reprocess DLQ entries.

---

# 18. Retry Strategy

Default retry policy:

* Exponential backoff
* Configurable maximum attempts
* Jitter
* Circuit breaker integration

Retries should not violate idempotency guarantees.

---

# 19. Event Consumers

Consumers include:

* Workflow Engine
* Agent Runtime
* Memory Engine
* Plugin Framework
* Notification Service
* Audit Service
* Monitoring Service

Consumers should subscribe only to events they require.

---

# 20. Event Producers

Producers include:

* Workflow Engine
* Agent Runtime
* Tool Runtime
* Memory Engine
* Identity
* Projects
* Plugin Framework

Producers own the schema for the events they publish.

---

# 21. Event Persistence

Critical domain events should be durably persisted before publication.

Persistence options include:

* Event store
* PostgreSQL
* Broker persistence (JetStream)

The chosen implementation should support recovery after process or node failures.

---

# 22. Security

Events should support:

* Authentication
* Authorization
* Encryption in transit
* Optional payload encryption
* Integrity validation

Sensitive data should not be broadcast unless explicitly required.

---

# 23. Observability

Every event should emit:

* Structured logs
* Metrics
* Distributed traces

Key metrics include:

* Publish latency
* Consumer latency
* Processing failures
* Retry counts
* Queue depth

---

# 24. Event Taxonomy

Examples:

| Domain   | Example Events                     |
| -------- | ---------------------------------- |
| Workflow | WorkflowStarted, WorkflowCompleted |
| Agent    | GoalStarted, GoalCompleted         |
| Memory   | MemoryStored, MemoryRetrieved      |
| Plugin   | PluginInstalled, PluginEnabled     |
| Identity | UserAuthenticated                  |
| Projects | ProjectCreated                     |
| Audit    | AuditRecordCreated                 |
| System   | NodeJoined, WorkerHeartbeat        |

---

# 25. Design Rules

Mandatory rules:

1. Events are immutable.
2. Events are versioned.
3. Consumers are idempotent.
4. Producers own their event schemas.
5. Business logic is not embedded in the event bus.
6. Cross-context communication occurs through events or explicit APIs.

---

# 26. Related Documents

* Domain-Driven Design
* Clean Architecture
* Workflow Engine Architecture
* C4 Component Diagram
* Deployment Architecture
* ADRs

---

# 27. Revision History

| Version | Date       | Description                                |
| ------- | ---------- | ------------------------------------------ |
| 1.0.1   | 2026-07-07 | Added a header note: the event model is real; NATS JetStream was never built — `wovyr-events` is a custom in-process system. Found during a project-wide doc review; no content changed |
| 1.0.0   | 2026-06-26 | Initial Event-Driven Architecture document |
