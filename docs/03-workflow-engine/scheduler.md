# Scheduler Specification

**Document ID:** WF-005
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

The Scheduler coordinates execution of workflow activities across one or more workers.

It is responsible for:

* Activity scheduling
* Worker coordination
* Queue management
* Load balancing
* Priority handling
* Delayed execution
* Cron scheduling
* Worker leasing
* Failure recovery

The Scheduler never executes activities directly; it assigns work to workers.

---

# 2. Objectives

The Scheduler must provide:

* Deterministic scheduling
* Horizontal scalability
* Fair work distribution
* High availability
* Low scheduling latency
* Automatic recovery
* Distributed coordination

---

# 3. Scheduler Architecture

```text id="7b1qzm"
                 DAG Engine
                     │
                     ▼
              Ready Queue Manager
                     │
                     ▼
            Distributed Scheduler
                     │
      ┌──────────────┼──────────────┐
      ▼              ▼              ▼
   Worker A       Worker B       Worker C
      │              │              │
      └──────────────┼──────────────┘
                     ▼
              Activity Execution
```

---

# 4. Responsibilities

The Scheduler:

* Receives ready nodes
* Applies scheduling policies
* Assigns activities to workers
* Tracks leases
* Detects failures
* Requeues abandoned work
* Maintains execution fairness

---

# 5. Scheduling Lifecycle

```text id="m4hvbp"
Ready
  │
  ▼
Queued
  │
  ▼
Leased
  │
  ▼
Executing
  │
 ┌┴──────────────┐
 ▼               ▼
Completed     Failed
```

Each state transition is persisted.

---

# 6. Worker Registration

Workers register on startup.

Worker metadata:

```yaml id="1v5b2f"
workerId:
hostname:
capabilities:
maxConcurrency:
labels:
version:
heartbeatInterval:
```

Workers periodically renew their registration.

---

# 7. Worker Heartbeats

Workers send heartbeats containing:

* Current workload
* Memory usage
* CPU utilization
* Queue depth
* Last completed activity
* Lease status

Missed heartbeats trigger failure detection.

---

# 8. Ready Queue

The Ready Queue stores executable activities.

Ordering considers:

* Priority
* Workflow deadline
* Creation time
* Fairness score

The queue must support efficient insertion and removal.

---

# 9. Scheduling Policies

Supported policies:

* FIFO
* Priority
* Deadline-first
* Weighted fair scheduling
* Tenant-aware scheduling
* Custom policy plugins

Policies are configurable per deployment.

---

# 10. Priority Model

Priority levels:

| Level    | Description           |
| -------- | --------------------- |
| Critical | Immediate execution   |
| High     | Elevated importance   |
| Normal   | Default               |
| Low      | Background processing |

Priorities influence dispatch but do not bypass security or dependency rules.

---

# 11. Lease-Based Execution

Workers obtain leases before executing activities.

Lease contents:

```yaml id="0ckyb5"
leaseId:
workerId:
activityId:
expiresAt:
renewable:
```

A lease grants exclusive execution rights.

---

# 12. Lease Renewal

Long-running activities periodically renew leases.

If renewal fails:

* Lease expires.
* Activity becomes eligible for reassignment.
* Recovery logic determines whether execution resumes or restarts.

---

# 13. Failure Detection

Failure conditions include:

* Missed heartbeat
* Expired lease
* Worker shutdown
* Infrastructure failure

The Scheduler marks affected work as recoverable.

---

# 14. Requeueing

Abandoned activities are returned to the Ready Queue.

Rules:

* Preserve retry counts.
* Avoid duplicate execution.
* Maintain workflow consistency.

Idempotent activities simplify safe reassignment.

---

# 15. Load Balancing

Supported strategies:

* Least loaded
* Round robin
* Capability-aware
* Resource-aware
* Locality-aware

Workers advertise capabilities (e.g., GPU, AI provider access).

---

# 16. Work Stealing

Idle workers may request work from overloaded workers.

Rules:

* Respect active leases.
* Preserve workflow ordering.
* Avoid starvation.

Work stealing is optional and configurable.

---

# 17. Delayed Execution

The Scheduler supports:

* Relative delays
* Absolute timestamps
* Time zones
* Calendar schedules

Delayed activities remain dormant until eligible.

---

# 18. Cron Scheduling

Cron expressions trigger recurring workflows.

Example:

```yaml id="6r39lj"
schedule:
  cron: "0 */6 * * *"
```

The Scheduler validates cron expressions before registration.

---

# 19. Rate Limiting

Policies may define:

```yaml id="m8nwyc"
limits:
  maxActivitiesPerSecond:
  maxConcurrentActivities:
  maxTenantConcurrency:
```

Rate limiting prevents resource exhaustion.

---

# 20. Backpressure

When downstream systems become saturated, the Scheduler may:

* Slow dispatch
* Queue additional work
* Reject new workflow starts
* Notify operators

Backpressure policies should be configurable.

---

# 21. Queue Partitioning

Queues may be partitioned by:

* Tenant
* Workflow type
* Priority
* Region
* Capability

Partitioning improves scalability and isolation.

---

# 22. Persistence

Scheduler state includes:

* Ready Queue
* Active leases
* Worker registry
* Delayed tasks
* Cron schedules

State must survive process and node failures.

---

# 23. Recovery

Recovery process:

1. Restore scheduler state.
2. Rebuild worker registry.
3. Expire stale leases.
4. Requeue abandoned activities.
5. Resume dispatch.

Recovery must not violate determinism.

---

# 24. Security

The Scheduler enforces:

* Worker authentication
* Mutual TLS
* Tenant isolation
* Capability validation
* Authorization for activity types

Unauthorized workers cannot claim leases.

---

# 25. Observability

Metrics:

* Queue depth
* Scheduling latency
* Lease renewals
* Expired leases
* Worker utilization
* Dispatch rate
* Requeue count

Logs and traces should include the workflow Correlation ID.

---

# 26. Performance Targets

| Metric                    | Target   |
| ------------------------- | -------- |
| Activity dispatch latency | < 20 ms  |
| Worker registration       | < 100 ms |
| Lease renewal             | < 10 ms  |
| Queue insertion           | O(log N) |
| Queue removal             | O(log N) |
| Scheduler recovery        | < 5 s    |

Targets apply to standard production deployments.

---

# 27. Rust Crate Mapping

Recommended module structure:

```text id="5y6u3r"
engine-workflow/
└── scheduler/
    ├── dispatcher.rs
    ├── queue.rs
    ├── worker_registry.rs
    ├── lease.rs
    ├── cron.rs
    ├── timer.rs
    ├── load_balancer.rs
    ├── rate_limiter.rs
    ├── recovery.rs
    └── mod.rs
```

---

# 28. Design Constraints

* Scheduling decisions must be deterministic.
* Workers remain stateless where possible.
* Lease ownership is exclusive.
* Queue operations should be efficient.
* Recovery must preserve workflow correctness.

---

# 29. Related Documents

* Workflow Overview
* Execution Model
* Workflow DSL
* DAG Engine
* State Machine
* Checkpointing
* Retry Engine
* Distributed Execution
* Persistence
* Rust Crate Design
* [Temporal Gap Closure (next phase)](temporal-gap-analysis.md) — durable timers
  (G1) and schedules/cron (G2) extend this scheduler.

---

# 30. Revision History

| Version | Date       | Description                     |
| ------- | ---------- | ------------------------------- |
| 1.0.0   | 2026-06-26 | Initial Scheduler Specification |
