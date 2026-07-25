**Document ID:** ARCH-001
**Version:** 1.0.1
**Status:** Draft — Day-1 target-state architecture, unrevised since project
inception; not reconciled with [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)
(Path A, 2026-07-06: GA ships as a single-node appliance). **Corrected
2026-07-07** — what actually ships today: one Rust binary (`wovyr-server`)
containing every domain in §6 (Agent Runtime, Workflow Engine, Memory Engine,
LLM Gateway, Tool Runtime, Plugin Framework, Platform Services) as in-process
crates, not independently deployable/scalable services; a REST/JSON + SSE
API (no gRPC, no WebSocket transport); an Angular SPA talking to it directly
(no NestJS Gateway/BFF layer — [dashboard overview](../10-dashboard/overview.md));
no message broker (no NATS — `wovyr-events` is a custom in-process event
system); and file-based storage under `~/.wovyr` by default, with PostgreSQL/
Redis/Qdrant as optional, feature-gated backends rather than the "primary
storage" §10 implies. See the [README](../../README.md)'s architecture
section for the kept-current architecture. Technologies named below with no implementation
and no tracked future work are now tracked — see
[`prd.md` §25](prd.md#25-technology-gaps-tracked-for-future-versions).
**Positioning note (2026-07-15):** this overview also predates
[ADR-0011](../17-adr/ADR-0011-generative-ui-repositioning.md)'s repositioning
(the product is now the Generative UI Trust Runtime,
[PRD-005](prd-generative-ui-runtime.md), with the platform as its engine) and
the since-shipped generative-UI runtime (`wovyr-ui`/`wovyr-ui-guard`, the server's
`ui` activity + frame routes) and MCP connection layer
([PRD-006](prd-mcp-connections.md)), which don't appear in its domain map.
**Owner:** Architecture Team
**Last Updated:** 2026-07-15

---

# 1. Purpose

This document provides a high-level overview of the Wovyr AI Platform architecture.

It defines the major architectural domains, runtime layers, system boundaries, deployment model, and interactions between the platform's core components.

This document serves as the entry point for all architecture documentation.

---

# 2. Scope

This document covers:

* Overall platform architecture
* Major subsystems
* Architectural principles
* Runtime layers
* Deployment overview
* Data flow
* Component relationships

Detailed design specifications are maintained in companion architecture documents.

---

# 3. Architectural Vision

The Wovyr AI Platform is designed as a modular, cloud-native, AI-first platform that enables developers to build intelligent autonomous systems using reusable infrastructure components.

The architecture emphasizes:

* Modularity
* Extensibility
* Provider independence
* Cloud portability
* Enterprise readiness
* Operational simplicity

---

# 4. Architecture Principles

The platform is built upon the following principles.

## Modular Design

Every major capability is implemented as an independent module with clearly defined interfaces.

---

## Domain-Driven Design

Business capabilities are organized into bounded contexts.

Examples include:

* Workflow
* Runtime
* Memory
* Identity
* Projects
* Plugins

---

## API First

All platform functionality is exposed through stable APIs.

Supported protocols:

* REST
* gRPC
* WebSocket
* CLI

---

## Plugin First

Core functionality remains intentionally small.

Additional capabilities are implemented through plugins and extensions.

---

## Cloud Native

Every component should support:

* Containers
* Kubernetes
* Horizontal scaling
* Health probes
* Metrics
* Distributed deployments

---

# 5. Platform Layers

The platform is organized into six logical layers.

```text
┌──────────────────────────────────────────┐
│               User Layer                 │
│ Dashboard • CLI • SDK • API Clients      │
└──────────────────────────────────────────┘
                    │
┌──────────────────────────────────────────┐
│              Gateway Layer               │
│ REST • gRPC • WebSocket • Auth           │
└──────────────────────────────────────────┘
                    │
┌──────────────────────────────────────────┐
│             Runtime Layer                │
│ Agent Runtime • Workflow Engine          │
│ Memory Engine • Tool Runtime             │
└──────────────────────────────────────────┘
                    │
┌──────────────────────────────────────────┐
│             Platform Layer               │
│ Identity • Projects • Plugins            │
│ Scheduler • Event Bus                    │
└──────────────────────────────────────────┘
                    │
┌──────────────────────────────────────────┐
│             Storage Layer                │
│ PostgreSQL • Redis • Qdrant • Object     │
│ Storage                                 │
└──────────────────────────────────────────┘
                    │
┌──────────────────────────────────────────┐
│        Infrastructure Layer              │
│ Docker • Kubernetes • Observability      │
│ Secrets • Networking                     │
└──────────────────────────────────────────┘
```

---

# 6. Core Domains

The platform consists of the following bounded contexts.

## Agent Runtime

Responsibilities:

* Planning
* Reasoning
* Reflection
* Context management
* Tool invocation
* Goal execution

---

## Workflow Engine

Responsibilities:

* DAG execution
* Scheduling
* State management
* Checkpointing
* Retries
* Compensation
* Human tasks

---

## Memory Engine

Responsibilities:

* Semantic memory
* Episodic memory
* Embeddings
* Retrieval
* Context compression

---

## LLM Gateway

Responsibilities:

* Provider abstraction
* Routing
* Streaming
* Failover
* Token accounting

---

## Tool Runtime

Responsibilities:

* Tool registration
* Tool execution
* Permission enforcement
* Sandboxing

---

## Plugin Framework

Responsibilities:

* Extension discovery
* Lifecycle management
* Version compatibility
* Marketplace integration

---

## Platform Services

Responsibilities:

* Authentication
* Authorization
* Projects
* Users
* Organizations
* Configuration
* Licensing (optional)
* Audit logging

---

# 7. High-Level Component Diagram

```text
                        Users
                           │
        ┌──────────────────┴──────────────────┐
        │ Dashboard │ CLI │ SDK │ REST Clients│
        └──────────────────┬──────────────────┘
                           │
                    API Gateway
                           │
        ┌──────────────────┴──────────────────┐
        │ Auth │ Rate Limit │ Routing │ Events│
        └──────────────────┬──────────────────┘
                           │
      ┌────────────────────┼────────────────────┐
      │                    │                    │
Agent Runtime      Workflow Engine      Platform Services
      │                    │                    │
      ├──────────────┬─────┴───────┬────────────┤
      │              │             │            │
 Memory Engine   Tool Runtime  Plugin SDK  LLM Gateway
      │              │             │            │
      └──────────────┴─────────────┴────────────┘
                           │
                 Event Bus / Scheduler
                           │
          PostgreSQL • Redis • Qdrant • Object Storage
```

---

# 8. Runtime Flow

Typical execution sequence:

1. Client submits a request.
2. API Gateway authenticates the request.
3. Agent Runtime receives the task.
4. Workflow Engine creates or resumes execution.
5. Memory Engine retrieves relevant context.
6. LLM Gateway invokes the configured AI provider.
7. Tool Runtime executes required tools.
8. Workflow Engine records state transitions.
9. Results are persisted.
10. Response is returned to the client.

---

# 9. Deployment Model

Supported deployment topologies:

## Local Development

Single process with embedded services.

---

## Modular Monolith

Multiple crates running in a single executable.

---

## Distributed Services

Independent services communicating through APIs and the event bus.

---

## Kubernetes Cluster

Highly available deployment with horizontal scaling.

---

# 10. Data Storage

Primary storage technologies:

| Purpose         | Technology            |
| --------------- | --------------------- |
| Relational Data | PostgreSQL            |
| Cache           | Redis                 |
| Vector Search   | Qdrant                |
| Object Storage  | S3-compatible storage |

Each storage implementation is accessed through abstraction layers to preserve portability.

---

# 11. Communication Patterns

The platform supports multiple communication styles:

* Synchronous REST
* gRPC
* WebSocket
* Event-driven messaging
* Internal asynchronous task execution

---

# 12. Cross-Cutting Concerns

The following concerns apply across all domains:

* Authentication
* Authorization
* Configuration
* Logging
* Metrics
* Distributed tracing
* Audit logging
* Error handling
* Secrets management

These capabilities should be implemented consistently across the platform.

---

# 13. Scalability Strategy

The architecture supports scaling by:

* Stateless services
* Horizontal replication
* Event-driven processing
* Distributed scheduling
* Storage abstraction
* Provider abstraction

Individual domains may scale independently based on workload characteristics.

---

# 14. Security Overview

Security principles include:

* Zero Trust
* Least privilege
* Encrypted communication
* Secret isolation
* Signed plugins
* Audit logging
* Role-based access control (RBAC)

Detailed security requirements are documented separately.

---

# 15. Observability

Every service should expose:

* Health endpoints
* Metrics
* Structured logs
* Distributed traces

Operational dashboards should provide visibility into workflows, agents, plugins, and infrastructure.

---

# 16. Design Decisions

Key architectural decisions include:

* Rust for core runtime components
* Modular crate-based design
* Provider abstraction layers
* Event-driven workflows
* Plugin-first extensibility
* API-first interfaces

Each decision is documented in an Architecture Decision Record (ADR).

---

# 17. Companion Documents

This overview is supported by:

* C4 Context Diagram
* C4 Container Diagram
* C4 Component Diagram
* Domain-Driven Design
* Clean Architecture
* Event-Driven Architecture
* Deployment Architecture
* ADRs

These documents provide progressively deeper technical detail.

---

# 18. Revision History

| Version | Date       | Description             |
| ------- | ---------- | ----------------------- |
| 1.0.2   | 2026-07-15 | Added a positioning note: predates ADR-0011's repositioning and the shipped generative-UI runtime (v1.2) + MCP connection layer (v1.3), absent from the domain map; no content changed |
| 1.0.1   | 2026-07-07 | Added a header divergence note: this doc's six-layer, multi-service, NATS/gRPC/NestJS topology was never built and diverges from ADR-0010's Path A decision. Found during a project-wide doc review; no content changed |
| 1.0.0   | 2026-06-26 | Initial system overview |
