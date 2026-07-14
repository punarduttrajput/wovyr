# Apex AI Platform — Documentation Index

> Apex — an enterprise AI agent platform built with Rust

Version: **0.1.0**
Status: **Planning / Documentation phase**

---

## How to read this index

* **Available** documents link directly to the file on disk.
* **Planned** documents are scoped but not yet written.
* Section numbers match the folder layout under `docs/` (`00-` through `05-` exist today).

New contributors should follow the [Reading Order](#reading-order) at the bottom.

---

# 00 Executive — `00-executive/`

| Document | Status |
|----------|--------|
| [Vision](00-executive/vision.md) | Available |
| [Mission](00-executive/mission.md) | Available |
| [Business Goals](00-executive/business-goals.md) | Available |
| [Success Metrics](00-executive/success-metrics.md) | Available |

---

# 01 Product — `01-product/`

| Document | Status |
|----------|--------|
| [Product Requirements Document](01-product/prd.md) | Available |
| [Future Directions PRD (Beyond & Completing 1.0)](01-product/prd-future.md) | Available |
| [GA Hardening PRD (Closing the Deployed-vs-Designed Gap)](01-product/prd-ga-hardening.md) | Available |
| [AI Platform Maturity PRD (Post-GA capability & operability)](01-product/prd-ai-platform-maturity.md) | Available |
| [Generative UI Trust Runtime PRD (the product repositioning — PRD-005)](01-product/prd-generative-ui-runtime.md) | Available |
| [System Overview](01-product/system-overview.md) | Available |
| [Personas](01-product/personas.md) | Available |
| [User Stories](01-product/user-stories.md) | Available |
| [Functional Requirements](01-product/functional-requirements.md) | Available |
| [Non-Functional Requirements](01-product/non-functional-requirements.md) | Available |
| [Acceptance Criteria](01-product/acceptance-criteria.md) | Available |

---

# 02 Architecture — `02-architecture/`

| Document | Status |
|----------|--------|
| [C4 — System Context](02-architecture/c4-context.md) | Available |
| [C4 — Container](02-architecture/c4-container.md) | Available |
| [C4 — Component](02-architecture/c4-component.md) | Available |
| [Domain-Driven Design](02-architecture/domain-driven-design.md) | Available |
| [Clean Architecture](02-architecture/clean-architecture.md) | Available |
| [Event-Driven Architecture](02-architecture/event-driven-architecture.md) | Available |
| [Deployment Architecture](02-architecture/deployment-architecture.md) | Available |

---

# 03 Workflow Engine — `03-workflow-engine/`

| Document | Status |
|----------|--------|
| [Overview](03-workflow-engine/overview.md) | Available |
| [Execution Model](03-workflow-engine/execution-model.md) | Available |
| [Workflow DSL](03-workflow-engine/workflow-dsl.md) | Available |
| [DAG Engine](03-workflow-engine/dag-engine.md) | Available |
| [Scheduler](03-workflow-engine/scheduler.md) | Available |
| [State Machine](03-workflow-engine/state-machine.md) | Available |
| [Checkpointing Specification](03-workflow-engine/checkpointing-specification.md) | Available |
| [Retry Engine](03-workflow-engine/retry-engine.md) | Available |
| [Compensation Engine](03-workflow-engine/compensation-engine.md) | Available |
| [Event Bus](03-workflow-engine/event-bus.md) | Available |
| [Persistence Layer](03-workflow-engine/persistence-layer.md) | Available |
| [Distributed Execution](03-workflow-engine/distributed-execution.md) | Available |
| [Agent Runtime (in-workflow)](03-workflow-engine/agent-runtime.md) | Available |
| [Temporal Gap Closure (next phase)](03-workflow-engine/temporal-gap-analysis.md) | Planned |

---

# 04 Agent Framework — `04-agent-framework/`

| Document | Status |
|----------|--------|
| [Index](04-agent-framework/index.md) | Available |
| [Agent Definition](04-agent-framework/agent-definition.md) | Available |
| [Agent Runtime Protocol](04-agent-framework/agent-runtime-protocol.md) | Available |
| [Planning Engine](04-agent-framework/planning-engine.md) | Available |
| [Context Manager](04-agent-framework/context-manager.md) | Available |
| [Tool Framework](04-agent-framework/tool-framework.md) | Available |
| [Provider SDK](04-agent-framework/provider-sdk.md) | Available |
| [Memory System](04-agent-framework/memory-system.md) | Available |
| [Policy Engine](04-agent-framework/policy-engine.md) | Available |
| [Multi-Agent Coordination](04-agent-framework/multi-agent-coordination.md) | Available |

---

# 05 LLM Gateway — `05-llm-gateway/`

The deployable service that fronts all model providers. Builds on the in-process
[Provider SDK](04-agent-framework/provider-sdk.md) and exposes it as a shared,
governed platform container (see [C4 Container §4.5](02-architecture/c4-container.md)).

| Document | Status |
|----------|--------|
| [Index](05-llm-gateway/index.md) | Available |
| [Overview](05-llm-gateway/overview.md) | Available |
| [Provider API](05-llm-gateway/provider-api.md) | Available |
| [Routing](05-llm-gateway/routing.md) | Available |
| [Resilience & Failover](05-llm-gateway/resilience.md) | Available |
| [Streaming](05-llm-gateway/streaming.md) | Available |
| [Token Management & Cost](05-llm-gateway/token-management.md) | Available |
| [Caching](05-llm-gateway/caching.md) | Available |

---

# 06 Memory Engine — `06-memory-engine/`

The deployable service that stores, indexes, retrieves, and governs all agent
memory. Operates the [Memory System](04-agent-framework/memory-system.md)
abstraction as a multi-tenant platform container
(see [C4 Container §4.4](02-architecture/c4-container.md)).

| Document | Status |
|----------|--------|
| [Index](06-memory-engine/index.md) | Available |
| [Overview](06-memory-engine/overview.md) | Available |
| [Memory API](06-memory-engine/memory-api.md) | Available |
| [Storage Architecture](06-memory-engine/storage-architecture.md) | Available |
| [Retrieval](06-memory-engine/retrieval.md) | Available |
| [Ranking](06-memory-engine/ranking.md) | Available |
| [Semantic Memory](06-memory-engine/semantic-memory.md) | Available |
| [Knowledge Graph](06-memory-engine/knowledge-graph.md) | Available |
| [Compression](06-memory-engine/compression.md) | Available |

---

# 07 Tool Runtime — `07-tool-runtime/`

The deployable service that executes tools with isolation, resource limits, and
governance. Operationalizes the [Tool Framework](04-agent-framework/tool-framework.md)
model as a multi-tenant platform container
(see [C4 Container §4.6](02-architecture/c4-container.md)).

| Document | Status |
|----------|--------|
| [Index](07-tool-runtime/index.md) | Available |
| [Overview](07-tool-runtime/overview.md) | Available |
| [Execution API](07-tool-runtime/execution-api.md) | Available |
| [Sandbox Runtime](07-tool-runtime/sandbox-runtime.md) | Available |
| [Worker Pool](07-tool-runtime/worker-pool.md) | Available |
| [Security & Isolation](07-tool-runtime/security-isolation.md) | Available |
| [Observability & Ops](07-tool-runtime/observability-ops.md) | Available |
| [E2B Gap Closure (next phase)](07-tool-runtime/e2b-gap-analysis.md) | Planned |

Built-in tool catalog:

| Tool | Status |
|------|--------|
| [Filesystem](07-tool-runtime/filesystem.md) | Available |
| [Shell](07-tool-runtime/shell.md) | Available |
| [Database](07-tool-runtime/database.md) | Available |
| [Git](07-tool-runtime/git.md) | Available |
| [Docker](07-tool-runtime/docker.md) | Available |
| [Kubernetes](07-tool-runtime/kubernetes.md) | Available |
| [HTTP](07-tool-runtime/http.md) | Available |
| [Browser](07-tool-runtime/browser.md) | Available |

---

# 08 Plugin SDK — `08-plugin-sdk/`

The extension model for the **Plugin First** platform: the SDK for authoring
plugins and the Plugin Engine that installs, versions, and governs them
(see [C4 Container §4.7](02-architecture/c4-container.md)).

| Document | Status |
|----------|--------|
| [Index](08-plugin-sdk/index.md) | Available |
| [Overview](08-plugin-sdk/overview.md) | Available |
| [Plugin API & SDK](08-plugin-sdk/plugin-api.md) | Available |
| [Permissions](08-plugin-sdk/permissions.md) | Available |
| [Sandbox & Loading](08-plugin-sdk/sandbox.md) | Available |
| [Versioning & Lifecycle](08-plugin-sdk/versioning.md) | Available |
| [Distribution](08-plugin-sdk/distribution.md) | Available |
| [Marketplace](08-plugin-sdk/marketplace.md) | Available |

---

# 09 API — `09-api/`

The external REST/gRPC surface fronted by the API Gateway
(see [C4 Container §4.1](02-architecture/c4-container.md)). This is the
control-plane/management API; high-throughput data-plane contracts live in their
own sections (LLM Gateway, Memory Engine, Tool Runtime).

| Document | Status |
|----------|--------|
| [Index](09-api/index.md) | Available |
| [Overview & Conventions](09-api/overview.md) | Available |
| [Authentication & Authorization](09-api/authentication.md) | Available |
| [Agents](09-api/agents.md) | Available |
| [Workflows](09-api/workflows.md) | Available |
| [Memory (Management)](09-api/memory.md) | Available |
| [Tools](09-api/tools.md) | Available |
| [Plugins](09-api/plugins.md) | Available |
| [Projects](09-api/projects.md) | Available |
| [Users](09-api/users.md) | Available |

---

# 10 Dashboard — `10-dashboard/`

The web application (Angular UI + NestJS backend-for-frontend) for designing,
operating, and monitoring the platform. A client of the [Platform API](09-api/index.md)
(see [C4 Container §4.9](02-architecture/c4-container.md)).

| Document | Status |
|----------|--------|
| [Index](10-dashboard/index.md) | Available |
| [Overview & Architecture](10-dashboard/overview.md) | Available |
| [Workflow Builder](10-dashboard/workflow-builder.md) | Available |
| [Agent Studio](10-dashboard/agent-studio.md) | Available |
| [Memory Explorer](10-dashboard/memory-explorer.md) | Available |
| [Marketplace UI](10-dashboard/marketplace.md) | Available |
| [Monitoring & Cost](10-dashboard/monitoring.md) | Available |
| [Settings & Administration](10-dashboard/settings.md) | Available |

---

# 11 CLI — `11-cli/`

The `apex` command-line interface — a client of the [Platform API](09-api/index.md)
and a local development toolchain
(see [C4 Container §4.10](02-architecture/c4-container.md)).

| Document | Status |
|----------|--------|
| [Index](11-cli/index.md) | Available |
| [Installation](11-cli/installation.md) | Available |
| [Configuration](11-cli/configuration.md) | Available |
| [Command Reference](11-cli/commands.md) | Available |
| [Examples & Recipes](11-cli/examples.md) | Available |

---

# 12 Deployment — `12-deployment/`

Practical, artifact-level deployment guides that operationalize the
[Deployment Architecture](02-architecture/deployment-architecture.md).

| Document | Status |
|----------|--------|
| [Index](12-deployment/index.md) | Available |
| [Docker](12-deployment/docker.md) | Available |
| [Docker Compose](12-deployment/docker-compose.md) | Available |
| [Kubernetes](12-deployment/kubernetes.md) | Available |
| [Helm](12-deployment/helm.md) | Available |
| [Terraform](12-deployment/terraform.md) | Available |

---

# 13 Security — `13-security/`

The cross-cutting security reference (Secure by Default). Consolidates mechanisms
implemented across the platform and centered on the
[Policy Engine](04-agent-framework/policy-engine.md).

| Document | Status |
|----------|--------|
| [Index](13-security/index.md) | Available |
| [Authentication](13-security/authentication.md) | Available |
| [Authorization](13-security/authorization.md) | Available |
| [RBAC & ABAC](13-security/rbac.md) | Available |
| [Encryption](13-security/encryption.md) | Available |
| [Secret Management](13-security/secret-management.md) | Available |
| [Audit Logging](13-security/audit.md) | Available |

---

# 14 Observability — `14-observability/`

The platform-wide observability reference (Observable by Default): logs, metrics,
traces, dashboards, alerting. Distinct from security [audit](13-security/audit.md).

| Document | Status |
|----------|--------|
| [Index](14-observability/index.md) | Available |
| [Logging](14-observability/logging.md) | Available |
| [Metrics](14-observability/metrics.md) | Available |
| [Tracing](14-observability/tracing.md) | Available |
| [Dashboards](14-observability/dashboards.md) | Available |
| [Alerting & SLOs](14-observability/alerting.md) | Available |

---

# 15 Testing — `15-testing/`

The testing strategy across the pyramid: unit → integration → workflow/agent →
performance → chaos → security.

| Document | Status |
|----------|--------|
| [Index](15-testing/index.md) | Available |
| [Unit Testing](15-testing/unit-tests.md) | Available |
| [Integration Testing](15-testing/integration-tests.md) | Available |
| [Workflow & Agent Testing](15-testing/workflow-tests.md) | Available |
| [Performance Testing](15-testing/performance-tests.md) | Available |
| [Chaos Testing](15-testing/chaos-testing.md) | Available |
| [Security Testing](15-testing/security-testing.md) | Available |

---

# 16 Examples — `16-examples/`

Worked, runnable examples combining agents, tools, memory, workflows, and plugins.

| Document | Status |
|----------|--------|
| [Index](16-examples/index.md) | Available |
| [Hello Agent](16-examples/hello-agent.md) | Available |
| [RAG Agent](16-examples/rag-agent.md) | Available |
| [Code Agent](16-examples/code-agent.md) | Available |
| [Customer Support Workflow](16-examples/customer-support.md) | Available |
| [VPN Operations Agent](16-examples/vpn-agent.md) | Available |

---

# 17 Architecture Decision Records — `17-adr/`

The *why* behind major architectural choices, in standard ADR format.

| Document | Status |
|----------|--------|
| [Index / Register](17-adr/index.md) | Available |
| [ADR-0001 — Project structure](17-adr/ADR-0001-project-structure.md) | Accepted |
| [ADR-0002 — Rust](17-adr/ADR-0002-rust.md) | Accepted |
| [ADR-0003 — PostgreSQL](17-adr/ADR-0003-postgresql.md) | Accepted |
| [ADR-0004 — Qdrant](17-adr/ADR-0004-qdrant.md) | Accepted |
| [ADR-0005 — NATS](17-adr/ADR-0005-nats.md) | Accepted, not implemented |
| [ADR-0006 — Clean Architecture + DDD](17-adr/ADR-0006-clean-architecture.md) | Accepted |
| [ADR-0007 — Plugin system](17-adr/ADR-0007-plugin-system.md) | Accepted |
| [ADR-0011 — Generative UI Trust Runtime repositioning](17-adr/ADR-0011-generative-ui-repositioning.md) | Accepted |

---

# 18 Roadmap — `18-roadmap/`

Directional, per-release evolution of the platform.

| Document | Status |
|----------|--------|
| [Index](18-roadmap/index.md) | Available |
| [v0.1 — Foundations](18-roadmap/v0.1.md) | Available |
| [v0.2 — Memory, Tools & Gateway](18-roadmap/v0.2.md) | Available |
| [v0.3 — Plugins, Dashboard & Multi-Tenancy](18-roadmap/v0.3.md) | Available |
| [v1.0 — General Availability](18-roadmap/v1.0.md) | Available |
| [GA-Completion Work (Tier A) — Index](18-roadmap/v1.0/index.md) | Available |
| [GA-001 — Scale & Performance Validation](18-roadmap/v1.0/A1-scale-performance.md) | Planned |
| [GA-002 — Reliability: HA, DR & Deployment](18-roadmap/v1.0/A2-reliability-ha-dr.md) | In progress |
| [GA-003 — Security: Root-of-Trust, PII & External Validation](18-roadmap/v1.0/A3-security-completion.md) | In progress |
| [GA-004 — Marketplace Economics & Safety](18-roadmap/v1.0/A4-marketplace-economics.md) | Planned |
| [GA-005 — SDK Distribution & Migration Guides](18-roadmap/v1.0/A5-sdk-distribution.md) | In progress |
| [v1.2 — Generative UI Trust Runtime](18-roadmap/v1.2-generative-ui.md) | Planned |
| [Future — Beyond 1.0](18-roadmap/future.md) | Available |
| [Future Research Bets (Tier B) — Index](18-roadmap/future/index.md) | Available |
| [FUT-001 — Autonomous Multi-Agent Systems](18-roadmap/future/B1-multi-agent-systems.md) | Exploratory |
| [FUT-002 — Self-Optimizing Platform](18-roadmap/future/B2-self-optimizing-platform.md) | Exploratory |
| [FUT-003 — Advanced Memory](18-roadmap/future/B3-advanced-memory.md) | Exploratory |
| [FUT-004 — Execution Frontiers](18-roadmap/future/B4-execution-frontiers.md) | Exploratory |
| [FUT-005 — Ecosystem & Interoperability](18-roadmap/future/B5-ecosystem-interop.md) | Exploratory |
| [FUT-006 — Trust & Evaluation](18-roadmap/future/B6-trust-evaluation.md) | Exploratory |

---

# 19 Implementation Guide — `19-implementation-guide/`

The contributor's handbook: environment, build, standards, releases, contributing.

| Document | Status |
|----------|--------|
| [Index](19-implementation-guide/index.md) | Available |
| [Development Environment](19-implementation-guide/development-environment.md) | Available |
| [Build System & SDK](19-implementation-guide/build-system.md) | Available |
| [Coding Standards](19-implementation-guide/coding-standards.md) | Available |
| [Release Process](19-implementation-guide/release-process.md) | Available |
| [Contributing](19-implementation-guide/contributing.md) | Available |

---

# Reading Order

New contributors should read the documentation in the following sequence:

1. [README](../README.md)
2. [Vision](00-executive/vision.md)
3. [Mission](00-executive/mission.md)
4. [PRD](01-product/prd.md)
5. [System Overview](01-product/system-overview.md)
6. [Architecture — C4 Context](02-architecture/c4-context.md) → [Container](02-architecture/c4-container.md) → [Component](02-architecture/c4-component.md)
7. [Workflow Engine — Overview](03-workflow-engine/overview.md)
8. [Agent Framework — Index](04-agent-framework/index.md)
9. [LLM Gateway — Index](05-llm-gateway/index.md)
10. [Memory Engine — Index](06-memory-engine/index.md)
11. [Tool Runtime — Index](07-tool-runtime/index.md)
12. [Plugin SDK — Index](08-plugin-sdk/index.md)
13. [Platform API — Index](09-api/index.md)
14. [Dashboard — Index](10-dashboard/index.md)
15. [CLI — Index](11-cli/index.md)
16. [Deployment — Index](12-deployment/index.md)
17. [Security — Index](13-security/index.md)
18. [Observability — Index](14-observability/index.md)
19. [Testing — Index](15-testing/index.md)
20. [Examples — Index](16-examples/index.md)
21. [ADRs — Register](17-adr/index.md)
22. [Roadmap — Index](18-roadmap/index.md)
23. [Implementation Guide — Index](19-implementation-guide/index.md)

---

# Documentation Conventions

* Every document begins with metadata (Document ID, Version, Status, Owner, Last Updated).
* Use Mermaid, PlantUML, or ASCII diagrams for architecture.
* Cross-reference related documents using relative links.
* Keep implementation details separate from product requirements.
* Record major architectural decisions as ADRs (section 17).
* Update the roadmap and ADRs whenever significant design changes occur.
* The canonical product name is **Apex AI Platform** (short form: **Apex**).

---

# Revision History

| Version | Date       | Description                                              |
| ------- | ---------- | ------------------------------------------------------- |
| 1.16.0  | 2026-07-14 | Added PRD-005 (Generative UI Trust Runtime), ADR-0011, and the v1.2 roadmap milestone |
| 1.15.0  | 2026-06-27 | Added 19 Implementation Guide section (6 docs) — all sections 00–19 now Available |
| 1.14.0  | 2026-06-27 | Added 18 Roadmap section (6 documents)                   |
| 1.13.0  | 2026-06-27 | Added 17 ADR section (8 documents)                       |
| 1.12.0  | 2026-06-27 | Added 16 Examples section (6 documents)                  |
| 1.11.0  | 2026-06-27 | Added 15 Testing section (7 documents)                   |
| 1.10.0  | 2026-06-27 | Added 14 Observability section (6 documents)             |
| 1.9.0   | 2026-06-27 | Added 13 Security section (7 documents)                  |
| 1.8.0   | 2026-06-27 | Added 12 Deployment section (6 documents)                |
| 1.7.0   | 2026-06-27 | Added 11 CLI section (5 documents)                       |
| 1.6.0   | 2026-06-27 | Added 10 Dashboard section (8 documents)                 |
| 1.5.0   | 2026-06-27 | Added 09 API section (10 documents)                      |
| 1.4.0   | 2026-06-27 | Added 08 Plugin SDK section (8 documents)                |
| 1.3.0   | 2026-06-27 | Added 07 Tool Runtime section (7 documents)              |
| 1.2.0   | 2026-06-27 | Added 06 Memory Engine section (9 documents)             |
| 1.1.0   | 2026-06-27 | Reconciled index with on-disk structure; added 05 LLM Gateway; standardized naming to Apex AI Platform |
| 1.0.0   | 2026-06-26 | Initial documentation index                             |
