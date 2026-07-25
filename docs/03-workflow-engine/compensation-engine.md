# Compensation Engine Specification

**Document ID:** WF-009
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the Compensation Engine for the Wovyr Workflow Engine.

The Compensation Engine provides reliable rollback for long-running distributed workflows by executing business-defined compensation actions instead of relying on traditional ACID database transactions.

The engine supports:

- Saga Pattern
- Distributed transactions
- Rollback workflows
- Partial rollback
- Nested compensation
- Parallel compensation
- Compensation retry
- Compensation auditing
- Recovery after failures

---

# 2. Objectives

The Compensation Engine must provide:

- Deterministic rollback
- Durable execution
- Distributed recovery
- Replay compatibility
- Nested transaction support
- High availability
- Complete auditability

---

# 3. Design Principles

The engine follows these principles:

1. Every compensatable activity explicitly defines its compensation.
2. Compensation never assumes database rollback.
3. Compensation is idempotent.
4. Compensation is event sourced.
5. Compensation is checkpointed.
6. Compensation is deterministic.
7. Compensation is resumable after failures.

---

# 4. Architecture

```text
               Workflow Runtime
                      │
                      ▼
             Compensation Manager
                      │
        ┌─────────────┼──────────────┐
        ▼             ▼              ▼
  Compensation     Retry Engine   Event Bus
     Planner
        │
        ▼
 Compensation Scheduler
        │
        ▼
 Activity Workers
```

---

# 5. Compensation Lifecycle

```text
Workflow Running
       │
       ▼
Activity Failure
       │
       ▼
Failure Policy
       │
       ▼
Compensation Planned
       │
       ▼
Compensation Scheduled
       │
       ▼
Compensation Running
       │
 ┌─────┴─────────┐
 ▼               ▼
Completed     Failed
```

---

# 6. Compensation Model

Each activity may define a compensation activity.

Example:

```yaml
activities:

  reserve_inventory:
    type: function
    compensate: release_inventory

  charge_payment:
    type: payment
    compensate: refund_payment

  create_shipping:
    type: shipping
    compensate: cancel_shipping
```

---

# 7. Compensation Stack

Completed activities are pushed onto the compensation stack.

Example:

```text
Reserve Inventory
Charge Payment
Generate Invoice
Create Shipment

Compensation Stack

Create Shipment
Generate Invoice
Charge Payment
Reserve Inventory
```

Rollback executes in reverse order.

---

# 8. Compensation Order

Rollback order:

```text
Forward

A
B
C
D

Failure

Reverse

D
C
B
A
```

This guarantees consistency.

---

# 9. Compensation Policies

Supported policies:

| Policy | Description |
|---------|-------------|
| Always | Always compensate |
| On Failure | Only after failure |
| Manual | User initiated |
| Conditional | Expression based |
| Never | No compensation |

---

# 10. Partial Compensation

Only completed activities participate.

Example:

```text
A ✔
B ✔
C ❌
D Not Started

Rollback

B
A
```

Activities that never completed are ignored.

---

# 11. Nested Compensation

Sub-workflows maintain independent compensation stacks.

```text
Parent Workflow

 ├── Child Workflow A
 └── Child Workflow B
```

Each child compensates independently before the parent continues.

---

# 12. Parallel Compensation

Independent branches may compensate concurrently.

```text
         Failure
            │
     ┌──────┴──────┐
     ▼             ▼
 Refund       Release Stock
     │             │
     └──────┬──────┘
            ▼
        Notify User
```

Dependencies determine execution order.

---

# 13. Compensation Failure

If compensation fails:

```text
Rollback

↓

Failure

↓

Retry

↓

Escalation

↓

Manual Recovery
```

Policies determine subsequent actions.

---

# 14. Compensation Retry

Compensation uses the Retry Engine.

Supported strategies:

- Fixed
- Linear
- Exponential
- Exponential + Jitter

Retry history is persisted.

---

# 15. Compensation Events

Generated events include:

```text
CompensationStarted
CompensationCompleted
CompensationFailed
CompensationRetried
CompensationSkipped
```

All events are persisted.

---

# 16. State Machine

States:

```text
Pending
   │
   ▼
Scheduled
   │
   ▼
Running
   │
┌──┴─────────────┐
▼                ▼
Completed     Failed
                  │
                  ▼
             Retrying
```

---

# 17. Persistence

Stored metadata:

```yaml
compensationId:
workflowId:
executionId:
activityId:
compensationActivity:
state:
attempts:
workerId:
timestamp:
```

Persistence occurs after every state transition.

---

# 18. Recovery

Recovery steps:

1. Restore checkpoint.
2. Restore compensation stack.
3. Replay events.
4. Resume unfinished compensation.
5. Continue rollback.

Recovery must be deterministic.

---

# 19. Replay

Replay restores:

- Compensation stack
- Completed compensations
- Retry counts
- Pending compensations

Replay never duplicates completed compensation.

---

# 20. Compensation DSL

Example:

```yaml
activities:

  reserveInventory:
    compensate:
      activity: releaseInventory
      retry:
        attempts: 5

  chargePayment:
    compensate:
      activity: refundPayment
```

---

# 21. Scheduler Integration

The Scheduler treats compensation as standard activities with elevated priority.

Default priority:

```text
Critical
```

Compensation execution preempts non-critical background work.

---

# 22. Security

Compensation activities enforce:

- Authorization
- Tenant isolation
- Secret management
- Immutable audit trail
- Worker authentication

---

# 23. Observability

Metrics:

- Compensation count
- Rollback duration
- Failed compensations
- Retry attempts
- Average rollback latency
- Compensation queue depth

---

# 24. Logging

Every compensation event logs:

```yaml
workflowId:
executionId:
activityId:
compensationId:
workerId:
status:
attempt:
duration:
timestamp:
```

---

# 25. Rust API

```rust
pub trait CompensationEngine {
    fn register(
        &mut self,
        activity: ActivityId,
        compensation: ActivityId,
    );

    fn compensate(
        &mut self,
        execution: ExecutionId,
    ) -> Result<()>;
}
```

---

# 26. Module Organization

```text
engine-workflow/
└── compensation/
    ├── engine.rs
    ├── planner.rs
    ├── scheduler.rs
    ├── stack.rs
    ├── recovery.rs
    ├── replay.rs
    ├── retry.rs
    ├── persistence.rs
    ├── metrics.rs
    └── mod.rs
```

---

# 27. Testing Strategy

## Unit Tests

- Compensation ordering
- Stack management
- Policy evaluation
- Retry behavior

## Integration Tests

- Workflow rollback
- Nested workflows
- Parallel compensation
- Scheduler integration

## Performance Tests

- Large rollback chains
- Thousands of compensations
- Concurrent rollbacks

## Chaos Tests

- Worker crash
- Database outage
- Scheduler restart
- Network partition

---

# 28. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Compensation planning | < 5 ms |
| Rollback scheduling | < 20 ms |
| Replay correctness | 100% |
| Recovery correctness | 100% |
| Duplicate compensation | 0 |
| Audit completeness | 100% |

---

# 29. Related Documents

- Workflow Overview
- Workflow DSL
- Execution Model
- DAG Engine
- Scheduler
- State Machine
- Checkpointing
- Retry Engine
- Persistence
- Event Bus
- Distributed Execution

---

# 30. Future Enhancements

- Cross-region compensation
- Multi-cluster rollback
- AI-assisted recovery planning
- Automatic compensation optimization
- Compensation simulation mode
- Rollback cost estimation
- Interactive rollback dashboard

---

# 31. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Compensation Engine Specification |