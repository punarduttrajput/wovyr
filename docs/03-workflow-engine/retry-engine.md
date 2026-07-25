# Retry Engine Specification

**Document ID:** WF-008
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the Retry Engine used by the Wovyr Workflow Engine.

The Retry Engine is responsible for recovering from transient failures while maintaining deterministic workflow execution.

It provides:

- Configurable retry policies
- Exponential backoff
- Linear retry
- Fixed interval retry
- Jitter support
- Circuit breaker integration
- Retry budgeting
- Dead-letter handling
- Failure classification

The Retry Engine operates independently from the Scheduler and Activity Workers.

---

# 2. Objectives

The Retry Engine must provide:

- Deterministic retries
- Configurable retry policies
- High reliability
- Fault tolerance
- Replay compatibility
- Retry observability
- Resource protection

---

# 3. Design Principles

The Retry Engine follows these principles:

1. Retry only transient failures.
2. Permanent failures must not be retried.
3. Retry decisions are deterministic.
4. Every retry attempt is persisted.
5. Retry history is immutable.
6. Retries must survive worker failures.
7. Retry policies are versioned.

---

# 4. Architecture

```text
                Activity Failure
                       │
                       ▼
              Failure Classifier
                       │
          ┌────────────┴────────────┐
          ▼                         ▼
Permanent Failure           Retry Eligible
          │                         │
          ▼                         ▼
 Workflow Failure            Retry Planner
                                      │
                                      ▼
                              Backoff Calculator
                                      │
                                      ▼
                              Scheduler Delay Queue
                                      │
                                      ▼
                                Activity Worker
```

---

# 5. Retry Lifecycle

```text
Running
    │
    ▼
Failure
    │
    ▼
Retry Evaluation
    │
 ┌──┴─────────────┐
 ▼                ▼
Retry         Permanent Failure
 │
 ▼
Delayed
 │
 ▼
Scheduled
 │
 ▼
Running
```

---

# 6. Retry Policy

Global example:

```yaml
retry:
  enabled: true
  maxAttempts: 5
  strategy: exponential
  initialDelay: 2s
  maxDelay: 2m
  multiplier: 2.0
  jitter: true
```

Activities may override the global policy.

---

# 7. Retry Strategies

Supported strategies:

### Fixed

```text
5s
5s
5s
5s
```

---

### Linear

```text
5s
10s
15s
20s
```

---

### Exponential

```text
2s
4s
8s
16s
32s
```

---

### Exponential with Jitter

```text
2.1s
3.8s
8.6s
15.4s
31.2s
```

Recommended for distributed deployments.

---

# 8. Failure Classification

Failures are categorized before retry.

| Type | Retry |
|--------|-------|
| Network timeout | Yes |
| Temporary database outage | Yes |
| Rate limiting | Yes |
| HTTP 429 | Yes |
| HTTP 503 | Yes |
| Worker crash | Yes |
| Invalid input | No |
| Validation failure | No |
| Permission denied | No |
| Schema error | No |

Custom classifiers may be registered.

---

# 9. Retry Budget

Each workflow maintains a retry budget.

Example:

```yaml
retryBudget:
  maxAttempts: 100
  maxDuration: 2h
```

When exhausted:

- Retries stop.
- Workflow failure policy is invoked.

---

# 10. Retry State

The runtime stores:

```yaml
retry:
  attempts:
  lastAttempt:
  nextAttempt:
  strategy:
  delay:
  reason:
```

Retry state is checkpointed.

---

# 11. Delay Queue

Retryable activities enter the Delay Queue.

```text
Failure
   │
   ▼
Delay Queue
   │
   ▼
Scheduler
   │
   ▼
Worker
```

The Delay Queue is durable and survives restarts.

---

# 12. Retry Scheduling

Retry scheduling considers:

- Retry delay
- Queue priority
- Worker availability
- Tenant limits
- Rate limits

Retry scheduling is deterministic.

---

# 13. Maximum Attempts

When maximum attempts are reached:

```text
Attempt 1
Attempt 2
Attempt 3
Attempt 4
Attempt 5

↓

Failure Policy
```

No further retries occur.

---

# 14. Timeout Integration

Retry policies interact with:

- Activity timeout
- Workflow timeout
- Lease timeout

Retries never extend workflow timeout unless explicitly configured.

---

# 15. Circuit Breaker Integration

Circuit breakers prevent repeated failures.

States:

```text
Closed
   │
   ▼
Open
   │
   ▼
Half Open
   │
   ▼
Closed
```

While open, retries are skipped.

---

# 16. Dead Letter Queue

Activities exceeding retry limits may be moved to a Dead Letter Queue.

Stored information:

- Workflow ID
- Activity ID
- Failure reason
- Retry history
- Stack trace
- Metadata

Operators may inspect or replay failed activities.

---

# 17. Persistence

Retry metadata is persisted after:

- Every failure
- Every retry
- Delay calculation
- Retry completion
- Retry exhaustion

Persistence guarantees recovery.

---

# 18. Replay

Replay restores:

- Retry count
- Delay state
- Failure history
- Pending retry

Replay never duplicates completed retries.

---

# 19. Worker Recovery

If a worker crashes during retry:

1. Lease expires.
2. Retry state restored.
3. Scheduler requeues activity.
4. Retry continues.

No retry attempts are lost.

---

# 20. Metrics

Expose:

- Retry attempts
- Retry success rate
- Retry failure rate
- Retry latency
- Retry queue size
- Average retry delay
- Retry budget usage

---

# 21. Logging

Every retry event logs:

```yaml
workflowId:
executionId:
activityId:
attempt:
strategy:
delay:
failureReason:
workerId:
timestamp:
```

---

# 22. Security

Retry operations enforce:

- Tenant isolation
- Worker authorization
- Immutable audit logs
- Secure persistence
- Replay validation

---

# 23. Rust API

```rust
pub trait RetryStrategy {
    fn next_delay(
        &self,
        attempt: u32,
    ) -> Duration;

    fn should_retry(
        &self,
        error: &WorkflowError,
    ) -> bool;
}
```

---

# 24. Crate Organization

```text
engine-workflow/
└── retry/
    ├── engine.rs
    ├── strategy.rs
    ├── classifier.rs
    ├── delay_queue.rs
    ├── budget.rs
    ├── circuit_breaker.rs
    ├── dead_letter.rs
    ├── persistence.rs
    ├── metrics.rs
    └── mod.rs
```

---

# 25. Testing Strategy

## Unit Tests

- Retry calculation
- Strategy selection
- Failure classification
- Budget enforcement

## Integration Tests

- Scheduler integration
- Checkpoint recovery
- Delay queue persistence
- Circuit breaker behavior

## Performance Tests

- Millions of retries
- Large delay queues
- High concurrency
- Distributed scheduling

## Chaos Tests

- Worker failure
- Database outage
- Queue corruption
- Network partition

---

# 26. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Retry calculation | < 1 ms |
| Delay queue lookup | < 5 ms |
| Retry persistence | < 10 ms |
| Replay correctness | 100% |
| Duplicate retries | 0 |
| Recovery correctness | 100% |

---

# 27. Related Documents

- Workflow Overview
- Execution Model
- Scheduler
- State Machine
- Checkpointing
- Compensation
- Persistence
- Distributed Execution
- Event Bus
- Rust Crate Design

---

# 28. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Retry Engine Specification |