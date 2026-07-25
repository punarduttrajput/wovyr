# Event Bus Specification

**Document ID:** WF-010
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the Event Bus architecture for the Wovyr Workflow Engine.

The Event Bus is responsible for delivering immutable events between internal engine components and external systems.

It enables:

- Event-driven workflows
- Loose coupling
- Distributed execution
- Durable messaging
- Workflow replay
- Event sourcing
- External integrations
- Real-time notifications
- Agent communication

The Event Bus is one of the core infrastructure components of the workflow platform.

---

# 2. Objectives

The Event Bus must provide:

- Durable delivery
- At-least-once delivery
- Ordered delivery within a stream
- Horizontal scalability
- Multi-tenant isolation
- Replay capability
- Backpressure handling
- High throughput

---

# 3. Design Principles

1. Events are immutable.
2. Events are append-only.
3. Every event has a globally unique identifier.
4. Event payloads are versioned.
5. Consumers are independent.
6. Event processing is idempotent.
7. Event ordering is preserved within a workflow.
8. Events are replayable.

---

# 4. Architecture

```text
                 Workflow Runtime
                        │
        ┌───────────────┼────────────────┐
        ▼               ▼                ▼
 Scheduler        State Machine      Retry Engine
        │               │                │
        └───────────────┼────────────────┘
                        ▼
                 Event Publisher
                        │
                        ▼
                 Event Bus Core
                        │
      ┌─────────────────┼──────────────────┐
      ▼                 ▼                  ▼
 Event Store      Internal Topics    External Topics
      │                 │                  │
      ▼                 ▼                  ▼
 Replay Engine      Subscribers       Webhooks/Kafka/NATS
```

---

# 5. Event Lifecycle

```text
Event Created
      │
      ▼
Validated
      │
      ▼
Persisted
      │
      ▼
Published
      │
      ▼
Consumed
      │
      ▼
Acknowledged
```

Events are persisted before publication.

---

# 6. Event Structure

```yaml
eventId:
eventType:
eventVersion:
workflowId:
executionId:
tenantId:
correlationId:
causationId:
timestamp:
producer:
payload:
metadata:
```

---

# 7. Event Types

## Workflow Events

- WorkflowCreated
- WorkflowValidated
- WorkflowStarted
- WorkflowPaused
- WorkflowResumed
- WorkflowCompleted
- WorkflowCancelled
- WorkflowFailed

---

## Activity Events

- ActivityScheduled
- ActivityStarted
- ActivityCompleted
- ActivityFailed
- ActivityRetried
- ActivityTimedOut

---

## Scheduler Events

- LeaseGranted
- LeaseExpired
- WorkerRegistered
- WorkerDisconnected

---

## Retry Events

- RetryScheduled
- RetryStarted
- RetryCompleted
- RetryExhausted

---

## Compensation Events

- CompensationStarted
- CompensationCompleted
- CompensationFailed

---

## System Events

- CheckpointCreated
- SnapshotRestored
- RecoveryStarted
- RecoveryCompleted

---

# 8. Event Categories

| Category | Description |
|-----------|-------------|
| Domain | Business events |
| Workflow | Engine lifecycle |
| System | Infrastructure |
| Audit | Security and compliance |
| Metrics | Operational telemetry |

---

# 9. Event Streams

Each workflow execution owns an independent event stream.

```text
Workflow A

1
2
3
4
5

Workflow B

1
2
3
4
```

Ordering is guaranteed within a stream but not across different workflows.

---

# 10. Event Persistence

Events are stored in an append-only log.

Properties:

- Immutable
- Sequential
- Durable
- Versioned
- Replayable

---

# 11. Event Ordering

Ordering guarantees:

- Workflow-local ordering
- Activity-local ordering
- Checkpoint ordering
- Compensation ordering

Global ordering is not required.

---

# 12. Event Delivery

Delivery guarantees:

- At least once
- Ordered per stream
- Durable
- Retryable

Consumers must implement idempotency.

---

# 13. Publisher API

```rust
pub trait EventPublisher {
    fn publish(
        &self,
        event: WorkflowEvent,
    ) -> Result<EventId>;
}
```

---

# 14. Subscriber API

```rust
pub trait EventSubscriber {
    fn handle(
        &self,
        event: WorkflowEvent,
    ) -> Result<()>;
}
```

---

# 15. Event Replay

Replay reconstructs runtime state.

Process:

```text
Load Stream
     │
     ▼
Read Events
     │
     ▼
Apply Events
     │
     ▼
Rebuild State
```

Replay is deterministic.

---

# 16. Event Versioning

Each event contains:

```yaml
eventVersion:
schemaVersion:
producerVersion:
```

Older versions remain readable.

---

# 17. Topic Organization

Example:

```text
workflow.created
workflow.completed
workflow.failed

activity.started
activity.completed
activity.failed

scheduler.worker.registered
scheduler.lease.expired

system.checkpoint.created

audit.security
```

---

# 18. Filtering

Consumers may subscribe by:

- Workflow
- Tenant
- Event Type
- Topic
- Labels
- Tags

Filtering occurs before delivery.

---

# 19. Dead Letter Queue

Undeliverable events are moved to the DLQ.

Stored information:

- Event
- Consumer
- Failure reason
- Retry count
- Timestamp

Operators may replay events manually.

---

# 20. External Integrations

Supported transports:

- Kafka
- NATS
- RabbitMQ
- Redis Streams
- Apache Pulsar
- AWS SNS
- AWS SQS
- Azure Service Bus
- Google Pub/Sub
- Webhooks

Transport implementations are pluggable.

---

# 21. Security

The Event Bus enforces:

- Authentication
- Authorization
- TLS
- Tenant isolation
- Payload encryption
- Audit logging

Sensitive payloads may be encrypted.

---

# 22. Observability

Metrics:

- Published events
- Consumed events
- Processing latency
- Queue depth
- Failed deliveries
- Replay count
- DLQ size

---

# 23. Logging

Every published event logs:

```yaml
eventId:
workflowId:
executionId:
tenantId:
eventType:
producer:
timestamp:
```

---

# 24. Performance Targets

| Metric | Target |
|----------|--------|
| Publish latency | < 5 ms |
| Delivery latency | < 20 ms |
| Replay throughput | 100K events/sec |
| Ordering correctness | 100% |
| Delivery durability | 100% |

---

# 25. Rust Crate Organization

```text
engine-workflow/
└── eventbus/
    ├── bus.rs
    ├── publisher.rs
    ├── subscriber.rs
    ├── stream.rs
    ├── event.rs
    ├── serializer.rs
    ├── replay.rs
    ├── router.rs
    ├── dlq.rs
    ├── metrics.rs
    └── mod.rs
```

---

# 26. Testing Strategy

## Unit Tests

- Event serialization
- Routing
- Filtering
- Ordering

## Integration Tests

- Replay
- Multi-worker delivery
- Scheduler integration
- Retry integration

## Performance Tests

- Million-event streams
- Concurrent publishers
- Large payloads
- High-throughput replay

## Chaos Tests

- Broker failure
- Duplicate delivery
- Consumer crash
- Network partition

---

# 27. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Event durability | 100% |
| Ordering accuracy | 100% |
| Duplicate handling | Idempotent |
| Replay correctness | 100% |
| Horizontal scalability | Unlimited through partitioning |

---

# 28. Related Documents

- Workflow Overview
- Execution Model
- Scheduler
- State Machine
- Checkpointing
- Retry Engine
- Compensation Engine
- Persistence
- Distributed Execution
- Agent Runtime
- Rust Crate Design

---

# 29. Future Enhancements

- Event compression
- Cross-region replication
- Event snapshots
- GraphQL subscriptions
- Event schema registry
- Event transformation pipelines
- AI event analytics

---

# 30. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Event Bus Specification |