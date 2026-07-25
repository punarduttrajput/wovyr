<!--
File: docs/17-adr/ADR-0005-nats.md
Document ID: ADR-0005
-->

# ADR-0005: NATS JetStream for the Event Bus

**Status:** Accepted (decision), **not implemented** — see Current Status  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Current Status (added 2026-07-07)

This decision was never executed. The shipped `wovyr-events` crate is a
**custom in-process event/webhook/audit system** (domain events, HMAC-signed
webhook deliveries with retry/backoff, a tamper-evident audit chain) — no
NATS, no JetStream, no message broker of any kind exists anywhere in the
codebase. This is not a defect: [ADR-0010](ADR-0010-ga-deployment-topology.md)
(2026-07-06) ratified a single-node appliance for GA, and a single process
has no cross-replica event-distribution problem for an in-process bus to
solve. A real message bus becomes relevant only once multiple replicas need
to coordinate — the v1.1 "Scale-Out" milestone. That future need is now
tracked explicitly as ticket **DIST-B9** in
[phase3-scale-distribution-tickets.md](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md)
Track B, rather than left as a dangling, unexecuted "Accepted" decision with
no owner. This ADR's original rationale below still stands as the candidate
design *if and when* that ticket graduates — it is not superseded, just
deferred.

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
- [ADR-0010](ADR-0010-ga-deployment-topology.md) — Path A decision this ADR's deferral follows from
- [phase3-scale-distribution-tickets.md](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md) — DIST-B9, the tracked future ticket

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-07 | Added a Current Status section: this decision was never implemented (`wovyr-events` is a custom in-process system, no NATS/JetStream anywhere); deferred, not superseded, and now tracked as ticket DIST-B9 for the v1.1 Scale-Out milestone. Found during a project-wide doc review |
| 1.0.0 | 2026-06-27 | Initial decision: NATS with JetStream as the event bus |
