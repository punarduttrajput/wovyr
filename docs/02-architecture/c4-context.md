# C4 Model – Level 1: System Context

**Document ID:** ARCH-002
**Version:** 1.0.1
**Status:** Draft — Day-1 target-state context diagram, unrevised since
project inception. **Corrected 2026-07-07:** the "Databases"/"Messaging
Systems" lists below are illustrative *categories* of external system this
architecture could integrate with, not a description of what's wired today.
The shipping single-node binary (`apex-server`, [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)
Path A) uses file-based storage by default; PostgreSQL/Redis/Qdrant are real
but optional, feature-gated backends; there is no messaging system of any
kind (NATS/Kafka/RabbitMQ) — see
[`prd.md` §25](../01-product/prd.md#25-technology-gaps-tracked-for-future-versions)
for where the NATS gap is now tracked.
**Owner:** Architecture Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document describes the highest level of the Apex AI Platform architecture using the C4 Model.

The System Context view answers the following questions:

* What is the Apex AI Platform?
* Who uses it?
* Which external systems interact with it?
* What are the platform boundaries?
* What responsibilities belong inside and outside the platform?

This document provides a common understanding for business stakeholders, architects, developers, operations teams, and security teams.

---

# 2. Scope

This document covers:

* Platform boundary
* Primary users
* External systems
* High-level integrations
* Trust boundaries
* Primary data flows
* Architectural assumptions

It intentionally omits internal implementation details, which are covered in the Container and Component architecture documents.

---

# 3. System Context

The Apex AI Platform is an enterprise-grade platform for building, deploying, orchestrating, and operating intelligent AI systems.

It provides reusable infrastructure for:

* AI agent execution
* Workflow orchestration
* Memory management
* Tool execution
* Plugin management
* Multi-provider LLM integration
* Operational monitoring
* API access
* Enterprise governance

The platform exposes these capabilities through APIs, SDKs, a web dashboard, and a CLI.

---

# 4. System Boundary

The following capabilities are considered part of the Apex AI Platform:

* API Gateway
* Agent Runtime
* Workflow Engine
* Memory Engine
* Tool Runtime
* LLM Gateway
* Plugin Framework
* Dashboard Backend
* CLI Backend
* Authentication
* Authorization
* Scheduler
* Event Bus
* Configuration
* Observability
* Audit Services

The following capabilities are external to the platform:

* Foundation model providers
* External databases
* Identity providers
* Source control systems
* Cloud infrastructure
* Enterprise messaging systems
* Third-party APIs

---

# 5. Primary Actors

## Developer

Responsibilities:

* Create AI applications
* Build workflows
* Develop plugins
* Configure deployments
* Debug executions

Primary Interfaces:

* CLI
* Dashboard
* REST API
* SDK

---

## Platform Administrator

Responsibilities:

* Manage users
* Configure providers
* Manage deployments
* Monitor runtime health
* Configure policies

---

## Enterprise User

Responsibilities:

* Execute approved workflows
* Review results
* Manage projects
* Collaborate with teams

---

## Plugin Developer

Responsibilities:

* Build reusable extensions
* Publish plugins
* Maintain compatibility
* Test integrations

---

## DevOps Engineer

Responsibilities:

* Deploy infrastructure
* Configure Kubernetes
* Scale workloads
* Monitor services
* Manage upgrades

---

# 6. External Systems

## Identity Provider

Examples:

* Microsoft Entra ID
* Keycloak
* Okta
* Auth0

Responsibilities:

* Authentication
* Single Sign-On (SSO)
* Identity federation

---

## LLM Providers

Examples:

* OpenAI
* Anthropic
* Google Gemini
* Ollama
* Azure OpenAI
* Local inference servers

Responsibilities:

* Model inference
* Embeddings
* Streaming responses

---

## Databases

Examples:

* PostgreSQL
* Redis
* Qdrant
* S3-compatible object storage

Responsibilities:

* Persistence
* Caching
* Vector search
* Artifact storage

---

## Messaging Systems

Examples:

* NATS
* Apache Kafka
* RabbitMQ

Responsibilities:

* Event distribution
* Asynchronous communication

---

## Monitoring Systems

Examples:

* Prometheus
* Grafana
* Jaeger
* OpenTelemetry collectors

Responsibilities:

* Metrics
* Dashboards
* Tracing
* Alerting

---

## Source Control

Examples:

* GitHub
* GitLab
* Azure DevOps

Responsibilities:

* Repository management
* CI/CD integration
* Workflow storage

---

# 7. System Context Diagram

```text
                        +------------------------------+
                        |         Developers           |
                        +--------------+---------------+
                                       |
                                       |
                     CLI / SDK / Dashboard / REST API
                                       |
                                       v
+--------------------------------------------------------------------------+
|                         Apex AI Platform                                 |
|--------------------------------------------------------------------------|
| API Gateway                                                              |
| Agent Runtime                                                            |
| Workflow Engine                                                          |
| Memory Engine                                                            |
| LLM Gateway                                                              |
| Tool Runtime                                                             |
| Plugin Framework                                                         |
| Dashboard Backend                                                        |
| Scheduler                                                                |
| Event Bus                                                                |
| Identity & Access                                                        |
| Observability                                                            |
+--------------------------------------------------------------------------+
     |            |             |            |            |             |
     |            |             |            |            |             |
     v            v             v            v            v             v
 Identity     LLM Providers  Databases   Messaging   Monitoring   Source Control
 Provider                    Storage      Systems      Systems        Systems
```

---

# 8. Trust Boundaries

The architecture defines the following trust zones:

## External Zone

Untrusted clients and internet-facing systems.

Examples:

* Browsers
* CLI users
* SDK clients

---

## Platform Zone

Trusted application services.

Examples:

* Runtime
* Workflow
* Memory
* Scheduler
* APIs

---

## Infrastructure Zone

Managed infrastructure.

Examples:

* Databases
* Object storage
* Message brokers

---

## Third-Party Zone

External services outside platform control.

Examples:

* LLM providers
* Identity providers
* External APIs

Communication across trust boundaries should use authenticated and encrypted channels.

---

# 9. High-Level Interaction Flow

1. A developer or application submits a request.
2. The API Gateway authenticates and authorizes the request.
3. The Agent Runtime receives the task.
4. The Workflow Engine creates or resumes execution.
5. The Memory Engine retrieves contextual information.
6. The LLM Gateway invokes one or more model providers.
7. The Tool Runtime executes permitted tools when required.
8. Workflow state is persisted.
9. Observability services record metrics, logs, and traces.
10. The response is returned to the caller.

---

# 10. External Interfaces

The platform provides the following public interfaces:

* REST API
* gRPC API
* WebSocket API
* Command Line Interface
* Software Development Kits
* Dashboard UI

Future interfaces may include GraphQL and Model Context Protocol (MCP) adapters.

---

# 11. Security Considerations

All external requests should pass through:

* Authentication
* Authorization
* Rate limiting
* Audit logging
* Input validation
* TLS encryption

Plugins and tool execution should operate within defined permission boundaries.

---

# 12. Assumptions

This architecture assumes:

* Multi-cloud deployment support
* Provider-neutral integrations
* Stateless service design where appropriate
* Durable workflow execution
* Horizontal scalability
* Extensible plugin ecosystem

---

# 13. Constraints

The platform must:

* Remain cloud-agnostic
* Avoid mandatory vendor lock-in
* Support modular deployment
* Maintain stable public APIs
* Preserve backward compatibility for supported releases

---

# 14. Related Documents

* System Overview
* C4 Container Diagram
* C4 Component Diagram
* Domain-Driven Design
* Clean Architecture
* Deployment Architecture
* Security Architecture
* Architecture Decision Records (ADRs)

---

# 15. Revision History

| Version | Date       | Description                                  |
| ------- | ---------- | -------------------------------------------- |
| 1.0.1   | 2026-07-07 | Added a header note clarifying the "Databases"/"Messaging Systems" lists are illustrative categories, not current integrations; ADR-0010's Path A reality has no messaging system at all. Found during a project-wide doc review; no content changed |
| 1.0.0   | 2026-06-26 | Initial C4 Level 1 – System Context document |
