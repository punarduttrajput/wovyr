# Distributed Execution Specification

**Document ID:** WF-012
**Version:** 1.1.0
**Status:** Draft. **The `WorkQueue`/`Worker`/lease/partition machinery this
document describes is real, tested library code in `wovyr-workflow`
(§33's measured baselines are real runs, not projections) — but it is
**not wired into the shipping `wovyr-server` binary**.
`default_workflows_engine` hardwires a single-process `FileStore`; there is
no queue, no lease, no worker pool in the running server today.
[ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md) ratified a
single-node-appliance GA (Path A) specifically because of this gap — wiring
this machinery onto the default path (env-selecting a `PostgresStore`,
routing submitted workflows through the queue/lease path, a multi-replica
correctness suite) is the v1.1 "Scale-Out" milestone — Track B of
[the Phase-3 ticket doc](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md),
gated on GA shipping first. Read this document as "what the library can
do today, and what wiring it up will require," not as a description of the
running platform.**
**Owner:** Workflow Engine Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document defines the Distributed Execution architecture for the Wovyr Workflow Engine.

Distributed Execution enables workflows to execute across multiple worker nodes while maintaining deterministic execution, fault tolerance, scalability, and consistency.

The subsystem supports:

- Horizontal scaling
- Worker clustering
- Dynamic scheduling
- Load balancing
- Fault recovery
- Lease management
- Multi-region deployments
- High availability

---

# 2. Objectives

The Distributed Execution subsystem must provide:

- Unlimited horizontal scaling
- Fault tolerance
- Worker independence
- Deterministic execution
- Automatic failover
- Efficient resource utilization
- Low scheduling latency
- Replay compatibility

---

# 3. Design Principles

1. Workers are stateless.
2. Workflow state is never stored in worker memory permanently.
3. Workers communicate only through infrastructure services.
4. All execution state is durable.
5. Failed workers never corrupt workflow state.
6. Every workflow has a single active owner.
7. Workers can join or leave the cluster at any time.

---

# 4. High-Level Architecture

```text
                     API Gateway
                          │
                          ▼
                 Workflow API Service
                          │
                          ▼
                 Workflow Scheduler
                          │
        ┌─────────────────┼─────────────────┐
        ▼                 ▼                 ▼
   Worker Node A     Worker Node B     Worker Node C
        │                 │                 │
        └─────────────────┼─────────────────┘
                          ▼
                    Event Bus
                          │
        ┌─────────────────┼─────────────────┐
        ▼                 ▼                 ▼
  Persistence        Checkpointing     Metrics
```

---

# 5. Cluster Architecture

```text
                    Cluster

        +-------------------------------+

          Scheduler Leader

        +-------------------------------+

             Worker Pool

    Worker-1
    Worker-2
    Worker-3
    Worker-4
    Worker-N
```

Workers are homogeneous.

---

# 6. Worker Responsibilities

Each worker is responsible for:

- Executing workflow activities
- Reporting heartbeats
- Creating checkpoints
- Publishing events
- Managing activity lifecycle
- Recovering interrupted executions

Workers never directly communicate with each other.

---

# 7. Worker Registration

Startup sequence:

```text
Start Worker

↓

Authenticate

↓

Register

↓

Receive Worker ID

↓

Advertise Capabilities

↓

Ready
```

---

# 8. Worker Metadata

Each worker publishes:

```yaml
workerId:
hostname:
version:
cpu:
memory:
architecture:
region:
zone:
supportedActivities:
status:
heartbeatInterval:
```

---

# 9. Worker States

```text
Offline

↓

Starting

↓

Registering

↓

Idle

↓

Busy

↓

Draining

↓

Stopped
```

---

# 10. Heartbeats

Workers periodically send heartbeats.

Heartbeat includes:

```yaml
workerId:
timestamp:
activeExecutions:
cpuUsage:
memoryUsage:
queueLength:
leaseCount:
```

Missed heartbeats initiate failover.

---

# 11. Lease Model

A workflow execution is protected by a lease.

```text
Scheduler

↓

Lease Granted

↓

Worker Executes

↓

Heartbeat

↓

Lease Renewed
```

Only the lease owner may execute a workflow.

---

# 12. Lease Expiration

If heartbeats stop:

```text
Worker Crash

↓

Lease Timeout

↓

Lease Expired

↓

Workflow Recovered

↓

New Lease Granted
```

---

# 13. Scheduling

Scheduler considers:

- Worker load
- Queue depth
- Activity affinity
- Region
- Available memory
- CPU utilization
- Tenant limits

Scheduling is deterministic.

---

# 14. Worker Failover

Recovery sequence:

```text
Worker Failure

↓

Lease Expiration

↓

Load Checkpoint

↓

Replay Events

↓

Assign New Worker

↓

Resume Workflow
```

---

# 15. Load Balancing

Supported algorithms:

- Round Robin
- Least Loaded
- Least Active Executions
- Resource Aware
- Priority Aware
- Region Aware

Default:

```text
Least Loaded
```

---

# 16. Activity Affinity

Activities may request affinity.

Example:

```yaml
activity:
  affinity:
    gpu: true
    region: us-east
    memory: high
```

Scheduler attempts to honor affinity.

---

# 17. Cluster Scaling

Horizontal scaling:

```text
High Queue Length

↓

Provision Worker

↓

Register Worker

↓

Accept New Work
```

Scale-down uses graceful draining.

---

# 18. Worker Draining

Draining procedure:

```text
Busy

↓

Draining

↓

Reject New Activities

↓

Finish Existing Work

↓

Shutdown
```

No workflow interruption occurs.

---

# 19. Multi-Region Deployment

```text
Region A

Scheduler

Workers

────────────

Region B

Workers

────────────

Region C

Workers
```

Workflows may execute within preferred regions.

---

# 20. Distributed Locks

Locks protect:

- Workflow execution
- Checkpoint creation
- State transitions
- Compensation
- Retry scheduling

Lock ownership follows lease ownership.

---

# 21. Consistency Model

Consistency guarantees:

- Single workflow owner
- Ordered state transitions
- Deterministic replay
- Atomic checkpoint creation
- Optimistic concurrency

---

# 22. Recovery

Recovery process:

1. Detect failure.
2. Expire lease.
3. Load latest checkpoint.
4. Replay missing events.
5. Acquire new lease.
6. Resume execution.

Recovery must not duplicate completed work.

---

# 23. Event Coordination

Workers communicate using events only.

Examples:

```text
ActivityStarted

ActivityCompleted

WorkflowPaused

CheckpointCreated

RetryScheduled

LeaseExpired
```

---

# 24. Security

Workers authenticate using:

- mTLS
- JWT
- API Keys
- Certificate-based identity

Authorization is enforced before lease assignment.

---

# 25. Observability

Metrics:

- Active workers
- Idle workers
- Busy workers
- Failed workers
- Queue length
- Scheduling latency
- Lease renewals
- Recovery count

---

# 26. Logging

Each worker logs:

```yaml
workerId:
executionId:
workflowId:
activityId:
leaseId:
event:
timestamp:
```

---

# 27. Performance Targets

| Metric | Target |
|---------|--------|
| Worker registration | < 500 ms |
| Lease acquisition | < 20 ms |
| Scheduling latency | < 50 ms |
| Failover detection | < 10 sec |
| Workflow recovery | < 500 ms |
| Heartbeat interval | 5 sec |

---

# 28. Rust Interfaces

```rust
pub trait Worker {
    fn register(&self) -> Result<WorkerId>;

    fn heartbeat(&self) -> Result<()>;

    fn execute(
        &self,
        activity: ActivityExecution,
    ) -> Result<ActivityResult>;
}

pub trait LeaseManager {
    fn acquire(
        &self,
        workflow: WorkflowId,
        worker: WorkerId,
    ) -> Result<Lease>;

    fn renew(
        &self,
        lease: LeaseId,
    ) -> Result<()>;

    fn release(
        &self,
        lease: LeaseId,
    ) -> Result<()>;
}
```

---

# 29. Module Organization

```text
engine-distributed/
├── cluster.rs
├── worker.rs
├── worker_registry.rs
├── lease_manager.rs
├── heartbeat.rs
├── scheduler_client.rs
├── load_balancer.rs
├── failover.rs
├── recovery.rs
├── affinity.rs
├── metrics.rs
└── mod.rs
```

---

# 30. Testing Strategy

## Unit Tests

- Lease acquisition
- Lease renewal
- Worker registration
- Heartbeat validation

## Integration Tests

- Multi-worker execution
- Failover recovery
- Load balancing
- Distributed scheduling

## Performance Tests

- 10,000 workers
- 1M concurrent workflows
- Large cluster recovery
- Scheduling throughput

## Chaos Tests

- Worker crash
- Network partition
- Scheduler restart
- Region outage
- Heartbeat loss
- Database failure

---

# 31. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Availability | 99.99% |
| Horizontal scalability | Unlimited |
| Duplicate execution | 0 |
| Recovery correctness | 100% |
| Lease consistency | 100% |
| Replay correctness | 100% |

---

# 32. Related Documents

- Workflow Overview
- Execution Model
- Scheduler
- State Machine
- Checkpointing
- Retry Engine
- Compensation Engine
- Event Bus
- Persistence Layer
- Agent Runtime
- Rust Crate Design

---

# 33. Scaling Envelope (G6)

This section states the **current, honest scaling envelope** of the implemented
distributed runtime — the leased `WorkQueue` + `Worker` model — and how to scale it
out, rather than overstating it. It closes
[gap-closure item G6](temporal-gap-analysis.md#g6--horizontal-scaling-story-honest-tiering).

## 33.1 Model

Executions are durably created by `Engine::start`, enqueued, and leased to one
`Worker` at a time via a time-bounded lease; a crashed worker's lease expires and
another reclaims it (exactly-once activity effects via idempotent `resume`). Two
queue backends:

- **`InMemoryWorkQueue`** — single process. Entries live in a `BTreeMap` keyed by
  execution id; a lease takes the first ready entry by an early-exiting ordered scan
  (no per-call sort), so lease/remove is near-O(1) when leases are removed promptly.
- **`PostgresStore`** as a `WorkQueue` — cross-process/node. Uses `FOR UPDATE SKIP
  LOCKED` so concurrent workers claim disjoint rows without blocking.

## 33.2 Partitioning (removing pool contention)

To let **multiple worker pools** scale horizontally without contending on one hot
row range, the queue is **sharded**. Each execution is assigned a partition
`shard_of(id, total)` (a stable FNV-1a hash mod `total`). A pool serves a
`PartitionAssignment` (`PartitionAssignment::for_pool(index, pool_count, total)`),
and `WorkQueue::lease_sharded` only considers executions in the pool's owned
partitions — so pools on disjoint partitions never lock the same rows. For the
Postgres queue the shard is a column populated at enqueue (`PostgresStore::with_partitions`,
indexed), so the `SKIP LOCKED` claim is filtered server-side (`WHERE shard = ANY(...)`).
A `Worker::with_partitions(assignment)` joins one pool.

Correctness is covered by `queue::tests::sharded_pools_lease_disjoint_executions`
(N pools over P partitions lease every execution exactly once, each only from its own
partitions).

## 33.3 Measured baselines

Assertion-style baselines from
[`crates/wovyr-workflow/tests/perf.rs`](../../crates/wovyr-workflow/tests/perf.rs),
trivial single-activity workflow, in-memory stores, **single core, debug build** on a
developer machine (2026-06-29) — a software-overhead ceiling, *not* a distributed
figure:

| Metric | Measured | Notes |
|--------|----------|-------|
| Engine throughput | ~23,600 executions/sec | start → drive → complete, one core |
| Lease + remove | ~299,000 ops/sec | in-memory queue primitive |

Release builds and real activity work shift these; the test asserts a conservative
floor and prints the live number so regressions surface.

## 33.4 Honest ceiling & migration path

- The single leased queue + `SKIP LOCKED` scales to **many workers on one Postgres
  queue** for moderate load. **Partitioning** (§33.2) extends this to **multiple
  pools** by removing cross-pool lock contention — the recommended first scale step.
- Beyond that, the bottleneck becomes the single Postgres queue table itself.
  Splitting matching from history (a Temporal-style **matching/history service**
  tier) is **out of scope** by design — see
  [ADR rationale in the gap analysis](temporal-gap-analysis.md#6-explicitly-not-doing).
  The documented path is: shard the queue across pools → if needed, shard the queue
  table/Postgres → only then consider a dedicated matching tier. We publish the
  envelope rather than imply web-scale we have not built.

---

# 34. Future Enhancements

- Multi-cluster federation
- Cross-cloud execution
- Geo-aware scheduling
- Spot instance optimization
- Predictive workload placement
- AI-driven scheduling
- Autonomous cluster healing

---

# 35. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Distributed Execution Specification |
| 1.1.0 | 2026-06-29 | Added §33 Scaling Envelope (G6): partitioning + measured baselines |
| 1.2.0 | 2026-07-07 | RM-GA-P3 DOC-A2: added a top-level status note clarifying this machinery is tested library code not wired into the shipping `wovyr-server` binary — wiring it is the v1.1 "Scale-Out" milestone per ADR-0010 |