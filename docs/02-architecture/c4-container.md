# C4 Model – Level 2: Container Diagram

**Document ID:** ARCH-003
**Version:** 1.0.1
**Status:** Draft — Day-1 target-state container diagram, unrevised since
project inception; not reconciled with
[ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md) (Path A, 2026-07-06).
**Corrected 2026-07-07 — none of §3's independently-deployable/-scalable
containers exist as separate processes.** The real topology is one Rust
binary (`apex-server`) containing Agent Runtime, Workflow Engine, Memory
Engine, LLM Gateway, Tool Runtime, and Plugin Engine as in-process crates —
none is horizontally scaled independently (§8's per-container scaling
strategy is aspirational). Specific corrections to §4/§5/§6/§11: **API
Gateway** interface is REST + SSE only, no gRPC/WebSocket. **Dashboard
Backend is NestJS in name only — it was never built**; the Angular SPA
talks directly to `apex-server`
([dashboard overview](../10-dashboard/overview.md)). **Event Bus is not
NATS JetStream — no message broker exists**; `apex-events` is a custom
in-process event/webhook system (current-status note on
[ADR-0005](../17-adr/ADR-0005-nats.md)). **PostgreSQL, Redis, and Qdrant are
real but optional**, feature-gated backends, not always-on shared
infrastructure — the default is file-based storage under `~/.apex`. **Object
Storage does not exist at all** — plugin packages are local
content-addressed files. **mTLS between containers** is moot since there are
no separate containers; the real security floor is `apex-server`'s own
JWT/API-key auth (RM-GA-P1). Gaps with no implementation and no tracked
future work (NATS, gRPC, object storage) are now tracked — see
[`prd.md` §25](../01-product/prd.md#25-technology-gaps-tracked-for-future-versions).
**Owner:** Architecture Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document describes the major deployable containers that make up the Apex AI Platform.

A *container* in the C4 Model represents a deployable application or data store—not necessarily a Docker container.

This document defines:

* Deployable services
* Primary responsibilities
* Technology stack
* Communication protocols
* Deployment strategy
* Container boundaries

---

# 2. Objectives

The container architecture aims to:

* Support modular development
* Enable independent scaling
* Preserve domain boundaries
* Simplify deployment
* Support cloud-native operations
* Minimize coupling

---

# 3. Container Overview

```text
                        Users
                           │
        Browser / CLI / SDK / External APIs
                           │
                           ▼
                    +----------------+
                    |  API Gateway   |
                    +----------------+
                           │
     ┌─────────────────────┼─────────────────────┐
     ▼                     ▼                     ▼
+-------------+    +----------------+    +----------------+
|Agent Runtime|    |Workflow Engine |    |Platform Services|
+-------------+    +----------------+    +----------------+
      │                    │                    │
      ├────────────┬───────┴─────────────┬──────┤
      ▼            ▼                     ▼      ▼
+-----------+ +-------------+ +----------------+ +-------------+
|LLM Gateway| |Memory Engine| |Tool Runtime    | |Plugin Engine|
+-----------+ +-------------+ +----------------+ +-------------+
      │              │                 │               │
      └──────────────┴─────────────────┴───────────────┘
                           │
                     Event Bus / Scheduler
                           │
        PostgreSQL • Redis • Qdrant • Object Storage
```

---

# 4. Container Catalog

## 4.1 API Gateway

### Responsibilities

* Request routing
* Authentication
* Authorization
* Rate limiting
* API versioning
* Request validation

### Interfaces

* REST
* gRPC
* WebSocket

### Technology

* Rust (Axum)
* Tower middleware

---

## 4.2 Agent Runtime

### Responsibilities

* Goal execution
* Planning
* Reasoning
* Reflection
* Context management
* Multi-agent coordination

### Depends On

* Workflow Engine
* Memory Engine
* Tool Runtime
* LLM Gateway

---

## 4.3 Workflow Engine

### Responsibilities

* Durable execution
* Scheduling
* DAG processing
* State machine
* Retry
* Compensation
* Checkpointing

### Depends On

* Scheduler
* Event Bus
* PostgreSQL

---

## 4.4 Memory Engine

### Responsibilities

* Context retrieval
* Semantic search
* Episodic memory
* Embedding management
* Knowledge graph integration

### Storage

* PostgreSQL
* Qdrant
* Redis

---

## 4.5 LLM Gateway

### Responsibilities

* Provider abstraction
* Routing
* Failover
* Streaming
* Token accounting
* Cost tracking

### Supported Providers

* OpenAI
* Anthropic
* Gemini
* Ollama
* Azure OpenAI
* Local models

---

## 4.6 Tool Runtime

### Responsibilities

* Tool discovery
* Registration
* Execution
* Permission enforcement
* Sandboxing
* Resource limits

---

## 4.7 Plugin Engine

### Responsibilities

* Plugin lifecycle
* Dependency resolution
* Version compatibility
* Marketplace integration
* Capability registration

---

## 4.8 Platform Services

### Responsibilities

* Users
* Organizations
* Projects
* Authentication
* Authorization
* Configuration
* Audit logging
* Licensing (optional)

---

## 4.9 Dashboard Backend

### Responsibilities

* UI APIs
* Monitoring APIs
* Workflow management
* Administration

---

## 4.10 CLI Service

### Responsibilities

* Local workflow execution
* Project scaffolding
* Deployment commands
* Diagnostics

---

# 5. Shared Infrastructure

## PostgreSQL

Stores:

* Users
* Projects
* Workflow definitions
* Execution metadata
* Configuration

---

## Redis

Stores:

* Session cache
* Distributed locks
* Temporary execution state
* Rate limiting counters

---

## Qdrant

Stores:

* Vector embeddings
* Semantic memory
* Retrieval indexes

---

## Object Storage

Stores:

* Workflow artifacts
* Plugin packages
* Logs
* Attachments
* Snapshots

---

## Event Bus

Responsibilities:

* Asynchronous communication
* Workflow events
* Notifications
* Scheduling events

Preferred implementation:

* NATS JetStream

---

# 6. Communication Matrix

| Source      | Destination     | Protocol              |
| ----------- | --------------- | --------------------- |
| Client      | API Gateway     | HTTPS                 |
| API Gateway | Runtime         | Internal API          |
| Runtime     | Workflow Engine | Rust Interface / gRPC |
| Runtime     | Memory Engine   | Internal API          |
| Runtime     | LLM Gateway     | Internal API          |
| Runtime     | Tool Runtime    | Internal API          |
| Services    | Event Bus       | NATS                  |
| Services    | Database        | SQL                   |
| Services    | Redis           | RESP                  |
| Services    | Qdrant          | HTTP/gRPC             |

---

# 7. Deployment Models

## Development

Single executable composed of all Rust crates.

Advantages:

* Fast startup
* Simple debugging
* Minimal infrastructure

---

## Team Deployment

Modular monolith with external databases.

---

## Enterprise

Independent containers:

* API Gateway
* Runtime
* Workflow Engine
* Memory Engine
* LLM Gateway
* Dashboard Backend

Scaled independently.

---

## Kubernetes

Each container deployed independently with:

* Horizontal Pod Autoscaler
* Health probes
* Rolling updates
* Service discovery

---

# 8. Scaling Strategy

| Container       | Scaling Method     |
| --------------- | ------------------ |
| API Gateway     | Horizontal         |
| Agent Runtime   | Horizontal         |
| Workflow Engine | Horizontal         |
| Memory Engine   | Read-heavy scaling |
| LLM Gateway     | Horizontal         |
| Dashboard       | Horizontal         |
| Event Bus       | Clustered          |
| PostgreSQL      | Primary/Replica    |
| Redis           | Cluster            |
| Qdrant          | Distributed        |

---

# 9. Security Boundaries

All inter-container communication should use:

* Mutual TLS (mTLS)
* Service authentication
* Authorization policies
* Structured audit logging

Tool Runtime and Plugin Engine should execute untrusted code within isolated sandboxes.

---

# 10. Observability

Every container exposes:

* `/health`
* `/ready`
* `/metrics`

All services emit:

* Structured logs
* OpenTelemetry traces
* Prometheus metrics

---

# 11. Technology Mapping

| Container         | Primary Technology |
| ----------------- | ------------------ |
| API Gateway       | Rust + Axum        |
| Agent Runtime     | Rust               |
| Workflow Engine   | Rust               |
| Memory Engine     | Rust               |
| LLM Gateway       | Rust               |
| Tool Runtime      | Rust               |
| Plugin Engine     | Rust               |
| Dashboard Backend | NestJS             |
| Dashboard UI      | Angular            |
| CLI               | Rust               |
| PostgreSQL        | PostgreSQL         |
| Redis             | Redis              |
| Qdrant            | Qdrant             |
| Event Bus         | NATS JetStream     |

---

# 12. Future Containers

The architecture allows additional containers without disrupting existing deployments:

* MCP Gateway
* Model Registry
* Workflow Marketplace
* Prompt Registry
* Policy Engine
* Billing Service
* AI Evaluation Service
* Distributed Worker Pool

---

# 13. Related Documents

* System Overview
* C4 System Context
* C4 Component Diagram
* Domain-Driven Design
* Clean Architecture
* Deployment Architecture
* ADRs

---

# 14. Revision History

| Version | Date       | Description                            |
| ------- | ---------- | -------------------------------------- |
| 1.0.1   | 2026-07-07 | Added a header note correcting the container/technology mapping against reality: no separate containers, no NestJS BFF, no NATS event bus, no gRPC, no object storage; Postgres/Redis/Qdrant are optional feature-gated backends. Found during a project-wide doc review; no content changed |
| 1.0.0   | 2026-06-26 | Initial C4 Level 2 – Container Diagram |
