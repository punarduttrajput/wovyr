# Workflow State Machine Specification

**Document ID:** WF-006  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Workflow Engine Team  
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the finite state machine (FSM) used by the Wovyr Workflow Engine.

The FSM governs:

- Workflow lifecycle
- Activity lifecycle
- State transitions
- Transition validation
- Recovery behavior
- Replay semantics
- Distributed execution consistency

Every workflow execution must transition only through valid states defined in this specification.

---

# 2. Design Goals

The state machine must provide:

- Deterministic execution
- Explicit lifecycle management
- Compile-time safety
- Replay compatibility
- Fault tolerance
- Idempotent transitions
- Distributed consistency
- Auditability

---

# 3. Design Principles

The state machine follows these principles:

1. Every transition is event-driven.
2. Every transition is persisted.
3. Invalid transitions are rejected.
4. State transitions are deterministic.
5. Terminal states are immutable.
6. Workflow state is reconstructed through replay.
7. Activity state is isolated from workflow state.
8. All transitions are auditable.

---

# 4. Workflow State Diagram

```text
                   +-----------+
                   |  Created  |
                   +-----+-----+
                         |
                         ▼
                   +-----------+
                   |Validated  |
                   +-----+-----+
                         |
                         ▼
                   +-----------+
                   |Scheduled  |
                   +-----+-----+
                         |
                         ▼
                   +-----------+
                   | Running   |
                   +--+--+--+--+
                      |  |  |
      +---------------+  |  +----------------+
      ▼                  ▼                   ▼
   Waiting          Cancelled            Failed
      |                                    |
      ▼                                    ▼
   Resumed                           Compensating
      |                                    |
      +----------------+-------------------+
                       ▼
                  Completed
```

---

# 5. Workflow States

| State | Description | Terminal |
|--------|-------------|----------|
| Created | Workflow instance created | No |
| Validated | Workflow definition validated | No |
| Scheduled | Ready for execution | No |
| Running | Currently executing | No |
| Waiting | Waiting for event, timer, or human | No |
| Resumed | Execution resumed after waiting | No |
| Compensating | Executing rollback logic | No |
| Completed | Successfully finished | Yes |
| Failed | Execution failed | Yes |
| Cancelled | Execution cancelled | Yes |

---

# 6. Activity State Diagram

```text
Created
   │
   ▼
Ready
   │
   ▼
Scheduled
   │
   ▼
Running
   │
┌──┴──────────────┐
▼                 ▼
Completed      Failed
                  │
                  ▼
             Retrying
                  │
                  ▼
              Scheduled
```

---

# 7. Activity States

| State | Description |
|--------|-------------|
| Created | Activity instantiated |
| Ready | Dependencies satisfied |
| Scheduled | Assigned to scheduler |
| Running | Executing on worker |
| Completed | Successfully finished |
| Failed | Execution failed |
| Retrying | Waiting for retry |

---

# 8. Workflow Transition Matrix

| From | To | Allowed |
|------|----|----------|
| Created | Validated | ✔ |
| Validated | Scheduled | ✔ |
| Scheduled | Running | ✔ |
| Running | Waiting | ✔ |
| Waiting | Resumed | ✔ |
| Resumed | Running | ✔ |
| Running | Completed | ✔ |
| Running | Failed | ✔ |
| Running | Cancelled | ✔ |
| Failed | Compensating | ✔ |
| Compensating | Completed | ✔ |
| Completed | Running | ✘ |
| Failed | Running | ✘ |
| Cancelled | Running | ✘ |

---

# 9. Activity Transition Matrix

| From | To | Allowed |
|------|----|----------|
| Created | Ready | ✔ |
| Ready | Scheduled | ✔ |
| Scheduled | Running | ✔ |
| Running | Completed | ✔ |
| Running | Failed | ✔ |
| Failed | Retrying | ✔ |
| Retrying | Scheduled | ✔ |
| Completed | Running | ✘ |

---

# 10. Transition Guards

Before a transition is committed, the runtime validates:

- Workflow version
- Dependency completion
- Activity ownership
- Worker lease validity
- Security permissions
- Resource availability
- Timeout constraints
- Tenant isolation

If any validation fails, the transition is rejected.

---

# 11. Transition Events

Every successful transition produces an immutable event.

Examples:

```text
WorkflowCreated
WorkflowValidated
WorkflowScheduled
WorkflowStarted
WorkflowPaused
WorkflowResumed
WorkflowCompleted
WorkflowFailed
WorkflowCancelled

ActivityReady
ActivityScheduled
ActivityStarted
ActivityCompleted
ActivityFailed
ActivityRetried
```

These events are published to the Event Bus and persisted.

---

# 12. State Persistence

Each transition persists:

```yaml
workflowId:
executionId:
previousState:
currentState:
transitionTime:
workerId:
correlationId:
causationId:
version:
reason:
```

Persistence occurs before acknowledging completion.

---

# 13. State Versioning

Each workflow instance maintains a monotonically increasing version.

Example:

```text
Version 1 -> Created
Version 2 -> Validated
Version 3 -> Scheduled
Version 4 -> Running
Version 5 -> Waiting
Version 6 -> Running
Version 7 -> Completed
```

Version numbers enable optimistic concurrency control.

---

# 14. Replay Semantics

Replay reconstructs workflow state using:

1. Latest checkpoint
2. Transition events
3. Activity history
4. Runtime metadata

Replay must produce the same final state regardless of worker assignment.

---

# 15. Waiting State

Workflows may pause indefinitely.

Supported waiting reasons:

- Human approval
- External webhook
- Message queue event
- Timer
- Cron trigger
- File upload
- Child workflow completion

Waiting workflows consume no execution thread.

---

# 16. Compensation State

Compensation executes only after configured failures.

Example:

```text
Reserve Inventory
        │
        ▼
Charge Payment
        │
        ▼
Create Shipment
        │
        ▼
Failure
        │
        ▼
Compensating
        │
        ▼
Refund Payment
        │
        ▼
Release Inventory
        │
        ▼
Completed
```

Compensation itself is managed by the state machine.

---

# 17. Cancellation

Cancellation may originate from:

- User
- Administrator
- Timeout
- Policy Engine
- Scheduler
- Parent workflow

Cancellation modes:

- Immediate
- Graceful
- Compensated

---

# 18. Timeout Handling

Timeout scopes:

| Timeout | Description |
|----------|-------------|
| Activity | Activity exceeded execution limit |
| Workflow | Workflow exceeded maximum duration |
| Waiting | Waiting period expired |
| Lease | Worker lease expired |

Timeouts generate events before state transitions.

---

# 19. Concurrency Rules

Only one workflow state transition may commit at a time.

Concurrency is enforced using:

- Version numbers
- Optimistic locking
- Lease ownership
- Atomic persistence

Conflicting updates must be rejected.

---

# 20. Distributed Execution

When execution moves between workers:

1. Current worker releases lease.
2. Scheduler assigns new worker.
3. State restored from persistence.
4. Replay validates consistency.
5. Execution resumes.

No transition may be skipped during migration.

---

# 21. Failure Recovery

Recovery procedure:

1. Detect failed worker.
2. Expire lease.
3. Restore checkpoint.
4. Replay transition history.
5. Resume execution.
6. Publish recovery event.

Recovery must preserve deterministic behavior.

---

# 22. Rust State Model

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowState {
    Created,
    Validated,
    Scheduled,
    Running,
    Waiting,
    Resumed,
    Compensating,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Created,
    Ready,
    Scheduled,
    Running,
    Completed,
    Failed,
    Retrying,
}
```

---

# 23. Transition API

Recommended interface:

```rust
pub trait StateMachine {
    fn current_state(&self) -> WorkflowState;

    fn transition(
        &mut self,
        event: TransitionEvent,
    ) -> Result<(), StateTransitionError>;

    fn validate(
        &self,
        event: &TransitionEvent,
    ) -> bool;
}
```

Implementations must guarantee deterministic transitions.

---

# 24. Validation Rules

The runtime validates:

- No invalid transitions
- Terminal state immutability
- Valid worker ownership
- Dependency satisfaction
- Activity completion ordering
- Replay consistency
- Version monotonicity
- Security policies

---

# 25. Testing Strategy

Required tests:

### Unit Tests

- Transition validation
- Guard validation
- Invalid transitions
- Timeout handling

### Integration Tests

- Scheduler interaction
- Replay
- Worker migration
- Compensation

### Property-Based Tests

- Random transition sequences
- Concurrency validation
- Replay determinism

### Chaos Tests

- Worker crash
- Lease expiration
- Network partition
- Database restart

---

# 26. Observability

Metrics:

- Active workflows
- Running workflows
- Waiting workflows
- State transition latency
- Failed transitions
- Compensation count
- Cancellation count
- Replay count

Logs must include:

- Workflow ID
- Execution ID
- Correlation ID
- Worker ID
- Previous state
- Current state

---

# 27. Security

The state machine enforces:

- Tenant isolation
- Authorization
- Immutable audit history
- Transition validation
- Secret protection
- Worker authentication

No unauthorized component may mutate workflow state.

---

# 28. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Transition latency | < 5 ms |
| Replay accuracy | 100% |
| Invalid transition detection | 100% |
| Recovery correctness | 100% |
| State persistence durability | No data loss |
| Concurrent transition conflicts | Automatically detected |

---

# 29. Related Documents

- Workflow Overview
- Workflow Execution Model
- Workflow DSL
- DAG Engine
- Scheduler
- Checkpointing
- Retry Engine
- Compensation
- Distributed Execution
- Persistence
- Rust Crate Design

---

# 30. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Workflow State Machine Specification |