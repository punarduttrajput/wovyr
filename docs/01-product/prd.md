**Document ID:** PRD-001
**Version:** 1.0.1
**Status:** Draft — this document is the **Day-1 product vision**, unrevised
since project inception. It has not been reconciled with
[ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md) (2026-07-06), which
ratified **Path A**: GA ships as a single-node appliance, not the
multi-service, horizontally-scaled platform implied below. **Current reality
vs. §2 "Primary Technologies"** (corrected 2026-07-07; see
[`CLAUDE.md`](../../CLAUDE.md) for the authoritative, kept-current
architecture description): Rust and Angular are real. **NestJS is not
built** — the dashboard SPA talks directly to `apex-server`; the NestJS BFF
is explicitly deferred until production auth needs it
([dashboard overview](../10-dashboard/overview.md)). **NATS is not
built** — there is no message broker; `apex-events` is a custom in-process
event/webhook system (see [ADR-0005](../17-adr/ADR-0005-nats.md)'s
current-status note). **PostgreSQL, Redis, and Qdrant are all real but
optional**, feature-gated backends (`postgres`/`redis`/`tiered-memory` cargo
features) — the shipping single-node binary's default is file-based storage
under `~/.apex`, not a hard dependency on any of the three. **Kubernetes**:
a real, offline-validated Helm chart exists for the single-binary topology
(`deployment/helm/apex/`, `replicas: 1` by product decision), not the
multi-replica HA cluster this vision implies, and it has never been applied
to a live cluster. **Docker** is real (`deployment/docker-compose.yml`).
Anything described here that isn't built and isn't yet tracked in
[`18-roadmap/v1.0/`](../18-roadmap/v1.0/index.md) (GA) or
[`18-roadmap/future/`](../18-roadmap/future/index.md) (post-GA) has been
added to the latter as of this revision — see §25 below.
**Owner:** Apex AI Platform Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This Product Requirements Document (PRD) defines the product vision, scope, objectives, high-level requirements, and release strategy for the Apex AI Platform.

It serves as the authoritative reference for product planning and aligns engineering, architecture, documentation, testing, and roadmap decisions.

Detailed functional specifications are maintained in companion documents.

---

# 2. Product Overview

## Product Name

Apex AI Platform

---

## Product Category

Enterprise AI Platform

---

## Product Type

Open-source infrastructure platform for building, deploying, and operating intelligent autonomous systems.

---

## Primary Technologies

* Rust
* Angular
* NestJS
* PostgreSQL
* Redis
* Qdrant
* NATS
* Kubernetes
* Docker

---

# 3. Vision

Enable developers and organizations to build production-ready AI applications using a unified platform rather than assembling numerous disconnected technologies.

---

# 4. Mission

Deliver a modular, secure, extensible, cloud-native platform for AI workflows, agents, memory, plugins, and enterprise operations.

---

# 5. Problem Statement

Organizations currently face several challenges when building AI-powered systems:

* Fragmented tooling
* Vendor lock-in
* Operational complexity
* Weak observability
* Limited workflow durability
* Inconsistent security
* Poor extensibility
* High maintenance costs

Apex AI Platform addresses these challenges through a cohesive, modular architecture.

---

# 6. Product Goals

The platform aims to provide:

* AI Runtime
* Workflow Engine
* Memory Engine
* LLM Gateway
* Tool Runtime
* Plugin SDK
* Dashboard
* CLI
* SDKs
* Enterprise APIs

---

# 7. Business Goals

The platform seeks to:

* Reduce AI infrastructure complexity.
* Increase developer productivity.
* Enable enterprise adoption.
* Foster a sustainable plugin ecosystem.
* Maintain provider independence.

Refer to `business-goals.md` for detailed objectives.

---

# 8. Target Users

Primary user groups include:

* Individual developers
* AI engineers
* Platform teams
* Startups
* Enterprises
* Researchers
* Systems integrators

Detailed personas are defined in `personas.md`.

---

# 9. Product Scope

## In Scope

* AI runtime
* Workflow orchestration
* Semantic memory
* Multi-provider LLM support
* Tool execution
* Plugin framework
* Visual dashboard
* REST APIs
* gRPC APIs
* CLI
* Cloud-native deployment

---

## Out of Scope

* Training foundation models
* Proprietary AI hosting
* Consumer chat applications
* Low-code website builders
* General-purpose database replacement

---

# 10. Product Pillars

The platform is organized into the following strategic pillars.

## AI Runtime

Responsible for reasoning, planning, execution, context management, and multi-agent orchestration.

---

## Workflow Engine

Responsible for durable execution, scheduling, checkpoints, retries, compensation, and event-driven workflows.

---

## Memory Engine

Responsible for semantic memory, episodic memory, retrieval, ranking, embeddings, and knowledge management.

---

## LLM Gateway

Provides a unified abstraction for multiple AI providers.

---

## Tool Runtime

Executes platform tools with controlled permissions and isolation.

---

## Plugin Framework

Allows extensions without modifying the platform core.

---

## Dashboard

Provides operational visibility and administrative capabilities.

---

# 11. Core Capabilities

The first major release targets:

* Agent creation
* Workflow authoring
* Workflow execution
* Tool execution
* Memory retrieval
* Multi-provider LLM routing
* User management
* Project management
* Plugin installation
* Observability

---

# 12. Functional Overview

High-level capabilities include:

* Create AI agents
* Execute workflows
* Invoke tools
* Store and retrieve memories
* Manage prompts
* Configure providers
* Monitor executions
* Review logs
* Configure plugins
* Manage users

Detailed requirements are documented in `functional-requirements.md`.

---

# 13. Non-Functional Requirements

The platform should emphasize:

* Performance
* Reliability
* Security
* Scalability
* Maintainability
* Extensibility
* Portability
* Testability
* Observability

Detailed requirements are maintained in `non-functional-requirements.md`.

---

# 14. Product Architecture

The platform consists of the following major domains:

* Runtime
* Workflow
* Memory
* LLM Gateway
* Tool Runtime
* Plugin SDK
* Dashboard
* API
* CLI
* Infrastructure

Detailed architecture is documented under `docs/02-architecture/`.

---

# 15. Deployment Targets

Supported environments:

* Local development
* Docker
* Kubernetes
* Private cloud
* Public cloud
* Hybrid cloud

---

# 16. Success Criteria

The product is considered successful when it:

* Supports production AI workloads.
* Demonstrates stable public APIs.
* Enables provider independence.
* Provides durable workflow execution.
* Encourages community contributions.
* Supports enterprise deployments.

Detailed KPIs are defined in `success-metrics.md`.

---

# 17. Release Strategy

## Phase 1

Documentation and architecture.

---

## Phase 2

Core runtime and workflow engine.

---

## Phase 3

Memory engine and LLM gateway.

---

## Phase 4

Plugin SDK and tool runtime.

---

## Phase 5

Dashboard and APIs.

---

## Phase 6

Distributed execution and enterprise features.

---

# 18. Risks

Major risks include:

* Rapid AI ecosystem evolution.
* Scope expansion.
* Integration complexity.
* Balancing flexibility with simplicity.
* Maintaining API stability.

Risk management details are maintained separately.

---

# 19. Assumptions

The PRD assumes:

* Rust remains the primary implementation language for core services.
* AI providers will continue evolving behind stable abstractions.
* Kubernetes remains a primary deployment target.
* Plugin ecosystems continue to grow.
* Enterprises require strong governance and observability.

---

# 20. Constraints

Current constraints include:

* Open-source licensing.
* Multi-platform compatibility.
* Cloud neutrality.
* Modular architecture.
* Stable public interfaces.

---

# 21. Dependencies

The platform depends on:

* Rust ecosystem
* PostgreSQL
* Redis
* Qdrant
* NATS
* Docker
* Kubernetes
* OpenTelemetry

Implementations should isolate these dependencies through abstraction layers wherever practical.

---

# 22. Companion Documents

This PRD references the following documents:

* Vision
* Mission
* Business Goals
* Success Metrics
* Product Scope
* User Personas
* User Stories
* Functional Requirements
* Non-Functional Requirements
* Acceptance Criteria
* Roadmap
* Architecture Overview
* ADRs

Together, these documents constitute the complete product specification.

---

# 23. Traceability

Every implementation artifact should be traceable to this PRD.

Traceability includes:

* Architecture decisions
* Rust crates
* APIs
* Database schemas
* UI features
* Test cases
* Documentation
* Release milestones

Maintaining traceability ensures that engineering work remains aligned with product objectives.

---

# 24. Approval

This document should be reviewed by:

* Product Management
* Solution Architecture
* Engineering Leadership
* Security
* Developer Experience
* Documentation Team

Approval indicates alignment on product direction prior to implementation.

---

# 25. Technology Gaps Tracked for Future Versions

Added 2026-07-07, alongside the header's divergence note. Three technologies
named in §2 have no implementation and, until now, no tracked future work
item either — captured here so they are deferred deliberately, not silently
dropped:

* **NATS-backed distributed event bus** — a real cross-replica message bus is
  only meaningful once the platform runs more than one replica (v1.1
  "Scale-Out," [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)).
  Tracked as ticket **DIST-B9** in
  [phase3-scale-distribution-tickets.md](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md)
  Track B.
* **gRPC / broader protocol interop** — the shipping API is REST/JSON + SSE
  only. Tracked under **FUT-005** ("Ecosystem & Interoperability"),
  [`18-roadmap/future/B5-ecosystem-interop.md`](../18-roadmap/future/B5-ecosystem-interop.md),
  alongside the existing MCP-gateway direction.
* **S3-compatible object storage** — nothing in the codebase uses object
  storage; plugin packages are local content-addressed files and
  `apex admin backup` writes to a local path. Tracked as a remote-destination
  option under **GA-002**,
  [`18-roadmap/v1.0/A2-reliability-ha-dr.md`](../18-roadmap/v1.0/A2-reliability-ha-dr.md)
  §4.1.

---

# 26. Revision History

| Version | Date       | Description        |
| ------- | ---------- | ------------------ |
| 1.0.1   | 2026-07-07 | Added a header divergence note correcting §2's tech-stack claims against ADR-0010/reality (NestJS and NATS were never built; Postgres/Redis/Qdrant are optional feature-gated backends, not primary dependencies), plus new §25 tracking the three technologies (NATS distributed event bus, gRPC, S3 object storage) that had no implementation *and* no tracked future work item until now. Found during a project-wide doc review; no product scope changed |
| 1.0.0   | 2026-06-26 | Initial master PRD |
