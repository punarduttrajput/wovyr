# Workflow Execution Model

**Document ID:** WF-002
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the execution semantics of the Wovyr Workflow Engine.

It specifies:

* Workflow lifecycle
* Execution state transitions
* Activity dispatch
* Scheduling behavior
* Replay rules
* Checkpointing
* Concurrency model
* Failure handling
* Worker coordination

This document serves as the implementation contract for the runtime.

---

# 2. Objectives

The execution model must provide:

* Durable execution
* Deterministic behavior
* Horizontal scalability
* Replay capability
* Fault tolerance
* Observability
* High throughput

---

# 3. Execution Philosophy

The Workflow Engine executes workflows as:

> Durable, event-sourced state machines.

Execution state is reconstructed from events and checkpoints.

Workflow progress is never dependent on process memory alone.

---

# 4. Workflow Lifecycle

```text
Created
   │
   ▼
Validated
   │
   ▼
Scheduled
   │
   ▼
Running
   │
   ├────────► Waiting
   │             │
   │             ▼
   │          Resumed
   │
   ▼
Completed

Failed
Cancelled
Compensated
```

Every transition is persisted.

---

# 5. Execution Instance

Each workflow execution creates a unique instance.

Attributes:

```yaml
execution_id:
workflow_id:
workflow_version:
tenant_id:
correlation_id:
status:
started_at:
updated_at:
owner_worker:
variables:
metadata:
```

Instances are immutable except through state transitions.

---

# 6. Execution Context

The execution context contains runtime state.

```yaml
context:
  inputs:
  outputs:
  variables:
  secrets:
  metadata:
  correlation_id:
  retry_counts:
  activity_results:
```

The context is persisted throughout execution.

---

# 7. Event-Sourced Model

Workflow progress is represented by events.

Example:

```text
WorkflowCreated
WorkflowScheduled
ActivityStarted
ActivityCompleted
ActivityCompleted
WorkflowCompleted
```

State can be reconstructed by replaying events.

---

# 8. Checkpoint Model

To avoid replaying entire histories, periodic checkpoints are stored.

Checkpoint contents:

```yaml
checkpoint:
  execution_state:
  active_nodes:
  variables:
  completed_activities:
  timestamps:
```

Checkpoints accelerate recovery and startup.

---

# 9. Activity Execution

Activities are the smallest executable units.

Execution flow:

```text
Ready
 │
 ▼
Dispatched
 │
 ▼
Running
 │
 ├────► Retrying
 │
 ▼
Completed
 │
 └────► Failed
```

Activity execution is isolated from workflow orchestration.

---

# 10. Dispatch Algorithm

Workflow runtime identifies executable nodes.

Pseudo-flow:

```text
1. Load execution state
2. Identify ready activities
3. Publish work items
4. Workers claim activities
5. Execute activity
6. Persist result
7. Evaluate next transitions
```

Only activities whose dependencies are satisfied become eligible.

---

# 11. Worker Ownership

A workflow execution has an owning worker.

Responsibilities:

* Coordinate state transitions
* Schedule activities
* Persist progress

Ownership can transfer during failure recovery.

---

# 12. Activity Workers

Workers execute activity tasks.

Responsibilities:

* Claim task
* Execute activity
* Publish result
* Report failures

Workers remain stateless.

---

# 13. Concurrency Model

The engine supports:

* Sequential execution
* Parallel branches
* Dynamic fan-out
* Dynamic fan-in

Example:

```text
       Start
         │
    ┌────┴────┐
    ▼         ▼
 Task A    Task B
    └────┬────┘
         ▼
       Merge
```

Branch synchronization occurs at merge points.

---

# 14. Determinism Rules

Workflow definitions must remain deterministic.

Allowed:

* Workflow variables
* Stored activity outputs
* Persisted events

Disallowed:

* Current system time
* Random values
* External calls directly from orchestration logic

Non-deterministic work must occur inside activities.

---

# 15. Replay Model

Replay rebuilds execution state.

Process:

```text
Load Checkpoint
       │
       ▼
Load Events After Checkpoint
       │
       ▼
Reconstruct State
       │
       ▼
Resume Execution
```

Replay must produce identical workflow state.

---

# 16. Recovery Model

Recovery occurs when:

* Worker crashes
* Node fails
* Deployment restarts
* Infrastructure disruption occurs

Recovery steps:

1. Detect abandoned execution
2. Reassign ownership
3. Restore checkpoint
4. Replay events
5. Resume scheduling

No completed activity should be re-executed unless explicitly configured.

---

# 17. Event Handling

Events may trigger execution changes.

Examples:

* Human approval received
* Webhook callback
* Timer fired
* File uploaded

Workflow state changes only through validated events.

---

# 18. Waiting State

Execution may pause indefinitely.

Examples:

* Human approval
* External event
* Scheduled resume
* Long-running process

Waiting executions consume no worker resources.

---

# 19. Scheduling Semantics

Eligible activities are determined by:

* Dependency satisfaction
* Conditional evaluation
* Resource availability
* Policy constraints

Scheduling decisions are deterministic.

---

# 20. Resource Constraints

Execution policies may define:

```yaml
limits:
  max_parallelism:
  max_memory:
  timeout:
  retries:
```

Policies are evaluated before dispatch.

---

# 21. Timeout Handling

Timeout scopes:

* Activity timeout
* Workflow timeout
* Waiting timeout
* Schedule timeout

Timeouts generate workflow events and trigger configured policies.

---

# 22. Failure Model

Failures are categorized as:

## Activity Failure

Single activity fails.

## Workflow Failure

Entire workflow cannot proceed.

## Infrastructure Failure

Runtime or infrastructure issue.

## External Dependency Failure

Provider or service unavailable.

Each failure type may trigger different recovery behavior.

---

# 23. Retry Semantics

Retries are configurable.

Example:

```yaml
retry:
  attempts: 5
  strategy: exponential
  delay: 30s
```

Retries apply to activities, not orchestration logic.

---

# 24. Compensation Model

Compensation is used when completed work must be reversed.

Example:

```text
Reserve Inventory
Charge Payment
Create Shipment

Failure

Compensate:
Cancel Shipment
Refund Payment
Release Inventory
```

Compensation activities are explicit workflow steps.

---

# 25. Execution Events

Examples:

```text
WorkflowStarted
ActivityDispatched
ActivityStarted
ActivityCompleted
ActivityFailed
WorkflowPaused
WorkflowResumed
WorkflowCompleted
WorkflowFailed
```

These events drive observability and recovery.

---

# 26. Persistence Requirements

Persist after:

* State transition
* Activity completion
* Retry increment
* Event receipt
* Ownership change

Persistence must occur before acknowledging critical progress.

---

# 27. Scalability Model

Supports:

* Multiple schedulers
* Distributed workers
* Queue partitioning
* Horizontal scaling

Work distribution should avoid centralized bottlenecks.

---

# 28. Observability

Track:

* Workflow duration
* Activity duration
* Queue depth
* Retry count
* Failure rate
* Worker utilization

All executions must be traceable via Correlation ID.

---

# 29. Security

Execution runtime must enforce:

* Tenant isolation
* Activity permissions
* Secret access policies
* Audit logging

Security policies are evaluated before activity execution.

---

# 30. Related Documents

* Workflow Overview
* Workflow DSL
* DAG Engine
* Scheduler
* State Machine
* Checkpointing
* Retry Engine
* Compensation
* Distributed Execution
* Persistence
* Rust Crate Design

---

# 31. Revision History

| Version | Date       | Description                      |
| ------- | ---------- | -------------------------------- |
| 1.0.0   | 2026-06-26 | Initial Workflow Execution Model |
