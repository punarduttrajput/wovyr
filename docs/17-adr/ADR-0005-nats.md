<!--
File: docs/17-adr/ADR-0005-nats.md
Document ID: ADR-0005
-->

# ADR-0005: NATS JetStream for the Event Bus

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

The platform is **event-driven** ([Event-Driven Architecture](../02-architecture/event-driven-architecture.md)):
services communicate asynchronously via domain events (`workflow.*`, `tool.*`,
`plugin.*`, cost/usage, audit). We need a messaging backbone with durability,
at-least-once delivery, and good operational characteristics.

---

# Decision

Use **NATS with JetStream** as the [Event Bus](../03-workflow-engine/event-bus.md)
and async messaging backbone.

Rationale:
- Lightweight, high-throughput, low-latency core.
- **JetStream** adds durable streams, at-least-once delivery, replay, and consumer
  acks — needed for workflow events, cost events, and audit.
- Subject-based routing fits our topic taxonomy (`plugin.*`, `execution.*`).
- Simple to operate and cluster; small footprint
  ([deployment](../12-deployment/kubernetes.md)).

---

# Consequences

**Positive**
- Durable, replayable events decouple services and enable
  [event-driven workflows](../03-workflow-engine/workflow-dsl.md#17-event-wait).
- Streams support cost/usage and audit pipelines
  ([token cost events](../05-llm-gateway/token-management.md#9-cost-events),
  [audit](../13-security/audit.md#5-pipeline)).
- Lightweight vs. heavier brokers; easy clustering.

**Negative**
- Another stateful cluster to run; JetStream storage must be sized/monitored.
- Exactly-once is not native — consumers must **deduplicate** (by `request_id` /
  event id), which the design already requires.

---

# Alternatives Considered

- **Apache Kafka** — battle-tested, huge throughput, but heavier to operate and
  more than needed; revisit for very high-volume analytics streaming.
- **RabbitMQ** — solid queuing but less suited to high-throughput streaming + replay.
- **Cloud-native (SQS/SNS, PubSub)** — viable in managed deployments but creates a
  hard cloud dependency; NATS keeps the platform portable/self-hostable.

---

# Related

- [`02-architecture/event-driven-architecture.md`](../02-architecture/event-driven-architecture.md)
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md)
