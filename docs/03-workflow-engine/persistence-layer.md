# Persistence Layer Specification

**Document ID:** WF-011
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the Persistence Layer architecture for the Apex Workflow Engine.

The Persistence Layer is responsible for durable storage of all workflow runtime data while remaining independent of the underlying database technology.

The persistence subsystem stores:

- Workflow definitions
- Workflow executions
- Activity executions
- Events
- Checkpoints
- Scheduler state
- Retry state
- Compensation state
- Worker leases
- Audit records
- Metrics metadata

The persistence layer is the single source of durable runtime state.

---

# 2. Objectives

The Persistence Layer must provide:

- ACID transactions where required
- Horizontal scalability
- Storage abstraction
- High availability
- Encryption
- Optimistic concurrency
- Multi-tenant isolation
- Schema evolution

---

# 3. Design Principles

1. Storage implementation must be pluggable.
2. Runtime components never access databases directly.
3. All persistence occurs through repositories.
4. Data models are versioned.
5. Writes are atomic.
6. Reads are strongly consistent where required.
7. Storage providers are interchangeable.

---

# 4. Architecture

```text
                 Workflow Runtime
                        │
                        ▼
                Repository Layer
                        │
        ┌───────────────┼────────────────┐
        ▼               ▼                ▼
 Workflow Repo    Event Repo     Checkpoint Repo
        │               │                │
        └───────────────┼────────────────┘
                        ▼
              Persistence Provider
                        │
        ┌───────────────┼────────────────────┐
        ▼               ▼                    ▼
    PostgreSQL     CockroachDB         FoundationDB
        │               │                    │
        └───────────────┼────────────────────┘
                        ▼
                  Object Storage
```

---

# 5. Persisted Entities

The Persistence Layer stores:

- WorkflowDefinition
- WorkflowExecution
- ActivityExecution
- WorkflowEvent
- WorkflowCheckpoint
- WorkerLease
- RetryRecord
- CompensationRecord
- Schedule
- AuditLog
- MetricsSnapshot

---

# 6. Repository Pattern

Every entity is managed through a repository.

Example:

```text
WorkflowRuntime

↓

WorkflowRepository

↓

Persistence Provider

↓

Database
```

Repositories isolate the runtime from storage technology.

---

# 7. Storage Providers

Supported providers:

| Provider | Purpose |
|-----------|----------|
| PostgreSQL | Default production |
| CockroachDB | Distributed SQL |
| FoundationDB | Enterprise |
| SQLite | Development |
| RocksDB | Embedded |
| DynamoDB | Cloud |
| MongoDB | Metadata storage (optional) |

Additional providers may be implemented through adapters.

---

# 8. Data Consistency

Consistency guarantees:

- Atomic workflow transitions
- Atomic event persistence
- Atomic checkpoint creation
- Atomic lease updates

Distributed consistency uses optimistic concurrency.

---

# 9. Transaction Model

Transactional operations include:

- Workflow creation
- State transitions
- Activity completion
- Checkpoint creation
- Event persistence
- Lease assignment

Transactions should remain short-lived.

---

# 10. Optimistic Concurrency

Each persisted object contains:

```yaml
id:
version:
updatedAt:
updatedBy:
```

Updates succeed only when versions match.

Conflicts require retry.

---

# 11. Schema Versioning

Each table/document stores:

```yaml
schemaVersion:
entityVersion:
serializationVersion:
```

Migration is handled through version-aware readers.

---

# 12. Workflow Repository

Responsibilities:

- Store workflow definitions
- Retrieve workflow versions
- Validate uniqueness
- Archive obsolete definitions

---

# 13. Execution Repository

Stores:

- Runtime state
- Variables
- Activity status
- Current workflow state
- Metadata

Execution records are mutable through validated transitions only.

---

# 14. Event Repository

Stores:

- Immutable events
- Event metadata
- Correlation IDs
- Causation IDs

Events are append-only.

---

# 15. Checkpoint Repository

Stores:

- Full snapshots
- Incremental snapshots
- Checkpoint metadata
- Retention information

Supports efficient lookup of the latest checkpoint.

---

# 16. Lease Repository

Stores:

- Active leases
- Lease expiration
- Worker ownership
- Heartbeat information

Used by the Scheduler.

---

# 17. Retry Repository

Stores:

- Retry attempts
- Retry delays
- Failure reasons
- Retry policies

Supports replay and recovery.

---

# 18. Compensation Repository

Stores:

- Compensation stack
- Compensation state
- Retry history
- Rollback metadata

Supports resumable rollback.

---

# 19. Audit Repository

Stores immutable audit records.

Examples:

- Workflow created
- User approval
- Workflow cancelled
- Secret access
- Configuration changes

Audit records are never modified.

---

# 20. Encryption

Sensitive fields are encrypted.

Examples:

- Secrets
- Credentials
- API tokens
- AI prompts
- Personally identifiable information

Encryption uses AES-256-GCM.

---

# 21. Compression

Large objects may be compressed.

Supported:

- Checkpoints
- Event payloads
- Large variables
- AI responses

Default algorithm:

```text
Zstandard (Zstd)
```

---

# 22. Backup Strategy

Backup types:

- Full backup
- Incremental backup
- Continuous WAL archiving
- Point-in-time recovery

Recovery objectives:

| Metric | Target |
|---------|--------|
| RPO | < 1 minute |
| RTO | < 5 minutes |

---

# 23. Recovery

Recovery procedure:

1. Restore database.
2. Validate schema versions.
3. Restore checkpoints.
4. Replay events.
5. Resume workers.

Recovery must preserve deterministic execution.

---

# 24. Multi-Tenant Isolation

Every persisted entity contains:

```yaml
tenantId:
```

Queries are automatically scoped by tenant.

Cross-tenant access is prohibited.

---

# 25. Security

Persistence enforces:

- Encryption at rest
- TLS
- RBAC
- Audit logging
- Secret isolation
- Key rotation

---

# 26. Observability

Metrics:

- Read latency
- Write latency
- Transaction failures
- Lock conflicts
- Storage usage
- Query throughput
- Replication lag

---

# 27. Rust Repository Traits

```rust
pub trait Repository<T> {
    fn insert(&self, entity: T) -> Result<()>;

    fn update(&self, entity: T) -> Result<()>;

    fn delete(&self, id: Id) -> Result<()>;

    fn find(&self, id: Id) -> Result<Option<T>>;
}
```

---

# 28. Crate Organization

```text
engine-storage/
├── provider/
│   ├── postgres.rs
│   ├── sqlite.rs
│   ├── cockroach.rs
│   ├── foundationdb.rs
│   └── mod.rs
│
├── repository/
│   ├── workflow.rs
│   ├── execution.rs
│   ├── activity.rs
│   ├── event.rs
│   ├── checkpoint.rs
│   ├── retry.rs
│   ├── compensation.rs
│   ├── lease.rs
│   ├── audit.rs
│   └── mod.rs
│
├── migration/
├── encryption/
├── compression/
├── transaction/
├── schema/
└── mod.rs
```

---

# 29. Testing Strategy

## Unit Tests

- CRUD operations
- Serialization
- Encryption
- Compression

## Integration Tests

- Transaction rollback
- Multi-provider support
- Optimistic concurrency
- Schema migration

## Performance Tests

- One million workflow executions
- High write throughput
- Large checkpoints
- Massive event streams

## Chaos Tests

- Database failure
- Storage corruption
- Replication lag
- Partial writes

---

# 30. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Read latency | < 5 ms |
| Write latency | < 10 ms |
| Transaction latency | < 20 ms |
| Availability | 99.99% |
| Durability | No committed data loss |
| Horizontal scaling | Supported |

---

# 31. Related Documents

- Workflow Overview
- Execution Model
- Scheduler
- State Machine
- Checkpointing
- Retry Engine
- Compensation Engine
- Event Bus
- Distributed Execution
- Rust Crate Design

---

# 32. Future Enhancements

- Multi-region replication
- Automatic sharding
- Columnar analytics storage
- Tiered storage
- Hot/cold data management
- Transparent database failover
- AI-assisted query optimization

---

# 33. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Persistence Layer Specification |