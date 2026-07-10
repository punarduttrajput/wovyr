# Apex AI Platform

> An Enterprise AI Agent Operating System written in Rust.

![Version](https://img.shields.io/badge/version-0.3.0-blue)
![Rust](https://img.shields.io/badge/Rust-Edition%202024-orange)
![License](https://img.shields.io/badge/license-Apache%202.0-green)

---

## Overview

Apex is a next-generation AI Agent Operating System designed for building, deploying, and orchestrating intelligent autonomous agents at enterprise scale.

Unlike traditional AI frameworks that focus only on LLM orchestration, Apex provides a complete runtime platform featuring:

- AI Agent Runtime
- Durable Workflow Engine
- Tool Execution Engine
- Memory Engine
- Plugin Framework
- Multi-LLM Gateway
- Visual Workflow Studio
- Enterprise Security
- Cloud Native Deployment

> **GA ships as a single-node appliance** ([ADR-0010](docs/17-adr/ADR-0010-ga-deployment-topology.md),
> ratified Path A): one `apex` binary, file-backed durable state by default,
> with Postgres/Qdrant as optional backends for specific subsystems (below).
> A distributed, multi-replica scheduler (shared queue/leases across
> instances) exists as tested library code but is **not wired into the
> shipping binary** — it's a v1.1 "Scale-Out" milestone, not a current
> capability. See
> [docs/18-roadmap/v1.0.md](docs/18-roadmap/v1.0.md) for what's shipped vs.
> deferred, workstream by workstream.

Apex is designed from the ground up using Rust to provide high performance, memory safety, and scalability.

---

# Vision

To become the Linux of AI Agents.

Apex provides everything required to build enterprise-grade AI systems while remaining modular, extensible, secure, and cloud-native.

---

# Mission

Enable developers to build intelligent autonomous software using modern software engineering principles rather than prompt engineering alone.

---

# Core Principles

- Rust First
- API First
- Plugin First
- Event Driven
- Cloud Native
- AI Native
- Secure by Default
- Distributed by Design
- Observable by Default
- Developer Friendly

---

# Project Goals

The project aims to provide:

- AI Agent Framework
- Distributed Workflow Engine
- Long-Term Memory
- Semantic Search
- Tool Execution Framework
- Multi-Agent Collaboration
- Human-in-the-loop Workflows
- Visual Workflow Builder
- Enterprise Dashboard
- SDKs
- CLI
- Marketplace

---

# High-Level Architecture

                    Users
                      │
               Angular Dashboard
                      │
                   REST API
                      │
      ┌─────────────────────────────────┐
      │   apex-server (one binary)      │
      ├─────────────────────────────────┤
      │ Agent Runtime (planner/executor/│
      │   tool-calling/memory loop)     │
      │ Workflow Engine (DAG, state     │
      │   machine, checkpoint, retry)   │
      └─────────────────────────────────┘
                      │
─────────────────────────────────────────────
 ~/.apex (local files) — the default, always-available durable store
 PostgreSQL — optional: marketplace registry (shipped); memory (CLI-only);
              workflow store exists as library code, not wired into the server
 Qdrant     — optional: memory vector index (CLI-only)

---

# Repository Structure

The actual Cargo workspace layout ([ADR-0001](docs/17-adr/ADR-0001-project-structure.md)):
shared logic lives in `crates/`, thin binaries in `apps/`.

```
apex/
  apps/
    apex-cli/            # the `apex` binary: login, dev server, agents/workflows/
                          # memory/kms/plugin/admin commands
  crates/
    apex-common/         # Error/Result, Usage, atomic_write/FileLock
    apex-provider/       # LLM gateway: chat/streaming/embeddings, resilience
    apex-agent/          # agent manifest + the model/tool run loop
    apex-tools/          # Tool trait, ToolRegistry, sandbox backends
    apex-workflow/       # durable, event-sourced workflow engine
    apex-memory/         # hybrid vector+keyword memory engine
    apex-telemetry/      # metrics + tracing/logging
    apex-tenancy/        # organizations/projects/RBAC/quota
    apex-events/         # domain events + outbound webhooks
    apex-secrets/        # the secret vault
    apex-audit/          # tamper-evident, hash-chained audit log
    apex-kms/            # envelope-encryption key management
    apex-plugin/         # plugin lifecycle (install/enable/upgrade/rollback)
    apex-marketplace/    # plugin marketplace registry
    apex-server/         # the Axum single-node server
    apex-eval/           # a deterministic AI-eval harness (prototype spike)
  dashboard/              # Angular SPA (direct to apex-server)
  deployment/             # Docker/Compose/Helm artifacts for what's actually built
  docs/                   # spec-driven documentation (source of truth)
  examples/               # runnable agent/workflow YAML manifests
  sdks/                   # TypeScript + Python API clients
```

See [`CLAUDE.md`](CLAUDE.md) for what each crate actually implements today —
`docs/` still describes future milestones not yet built, and `CLAUDE.md` is
kept in sync with the shipping code, not the aspiration.

---

# Technology Stack

Backend

- Rust

API

- Axum (REST — this is what's actually shipped; there is no gRPC surface today)

Frontend

- Angular

Durable state (default)

- Local files under `~/.apex`, crash-safe via atomic writes + fsync'd
  append-only logs — no database required to run `apex dev`

Optional backends (env-var-selected; the default file-backed stores work
without any of these)

- **PostgreSQL** — marketplace registry (shipped, wired into both server and
  CLI); a `TieredStore` for the memory engine (CLI-only today, `apex memory`
  commands); a workflow-engine `PostgresStore` exists as tested library code
  but is **not wired into the server** (v1.1 "Scale-Out" milestone)
- **Qdrant** — vector ANN backend for the memory engine's `TieredStore`
  (same CLI-only scope as above), and a `Gateway::with_qdrant_semantic_cache`
  option that exists as library code but **isn't attached by any shipping
  binary**
- **Redis** — a `Gateway::with_redis_breakers` option for fleet-shared
  circuit-breaker state; exists as library code, **not attached by any
  shipping binary**

There is no NATS/message-broker dependency anywhere in this workspace.

Observability

- OpenTelemetry (opt-in, `otlp` cargo feature)
- Prometheus (`/metrics`, always on)
- Grafana (bring-your-own, scrapes the above)

Deployment

- Docker, Docker Compose (real, working — [`deployment/docker-compose.yml`](deployment/docker-compose.yml))
- Kubernetes/Helm (a real single-replica chart exists at
  [`deployment/helm/apex/`](deployment/helm/apex/README.md); a multi-service,
  multi-replica topology is documented as aspirational, v1.1+)

---

# Key Features

## AI Agent Runtime

Supports

- Planning
- Reflection
- Tool Calling
- Context Management
- Goal Tracking
- Multi-Agent Collaboration

---

## Workflow Engine

Supports

- DAG
- State Machine
- Retry
- Compensation
- Long Running Workflows
- Event Driven Workflows
- Human Approval
- Parallel Execution

---

## Memory Engine

Supports

- Long-Term Memory (hybrid vector + keyword retrieval, recency/importance
  ranking, MMR diversification, ABAC filtering, compression)
- Knowledge Graph — **deferred**, not yet implemented (tagged for v1)

---

## Plugin Framework

Supports

- Dynamic Loading
- Versioning
- Permissions
- Sandboxed Execution

---

## Security

Supports

- JWT (HS256/RS256) and API-key auth (`APEX_AUTH_MODE=jwt|apikey`) — no
  OAuth2 authorization flow is implemented
- RBAC (organization/project roles, default-deny)
- Secrets Management (a reference-addressed vault, tenant-scoped)
- Encryption (envelope encryption via a KMS; opt-in at-rest encryption for
  secrets/memory/webhooks)
- Audit Logs (tamper-evident, hash-chained)

---

# Documentation

Full documentation lives in [`docs/`](docs/), indexed by [`docs/SUMMARY.md`](docs/SUMMARY.md).

The documentation is organized into the following sections:

00 Executive

01 Product

02 Architecture

03 Workflow Engine

04 Agent Framework

05 LLM Gateway

06 Memory Engine

07 Tool Runtime

08 Plugin SDK

09 API

10 Dashboard

11 CLI

12 Deployment

13 Security

14 Observability

15 Testing

16 Examples

17 ADR

18 Roadmap

19 Implementation Guide

---

# Development Status

Current Phase

**v0.1–v0.3 shipped and tagged.** The v1.0 "GA hardening" effort
([PRD-003](docs/01-product/prd-ga-hardening.md)) is now in progress, phased:
[Phase 1](docs/18-roadmap/v1.0/phase1-security-floor-tickets.md) (security
floor) and [Phase 2](docs/18-roadmap/v1.0/phase2-durability-execution-tickets.md)
(durability & execution) are **done** — crash-safe atomic writes everywhere,
cross-process locking, no restart amnesia, the server drives its own
timers/schedules/crash-recovery, real workflow cancellation, `apex admin
backup`/`restore`, and KMS root-key escrow with a proven restore drill.
[Phase 3](docs/18-roadmap/v1.0/phase3-scale-distribution-tickets.md) (scale &
distribution) is **in progress** — [ADR-0010](docs/17-adr/ADR-0010-ga-deployment-topology.md)
ratified a single-node-appliance GA topology, deferring the distributed
multi-replica scheduler to a v1.1 "Scale-Out" milestone.
[Phase 4](docs/18-roadmap/v1.0/phase4-contract-operability-tickets.md)
(contract & operability) has not started.

The implemented surface spans: an agent runtime with a real model/tool loop,
an LLM gateway (chat + streaming + embeddings, mock/OpenAI-compatible/local
mistral.rs backends, with retry/failover/circuit-breaking/caching), a tool
runtime with a sandbox spectrum (native/WASI/container/gVisor/microVM), a
**durable, event-sourced workflow engine** (checkpointing, retry, saga
compensation, durable timers/schedules, distributed worker leases as tested
library code), a **memory engine** (hybrid vector+keyword retrieval,
encryption, ABAC), a **secret vault**, **envelope-encryption KMS**,
**tamper-evident audit logging**, a **plugin engine + marketplace**,
**multi-tenancy** (RBAC + quota), a single-node Axum server, an Angular
dashboard, TypeScript/Python SDKs, and the `apex` CLI. See
[`CLAUDE.md`](CLAUDE.md) for the authoritative, kept-current description of
what each crate actually does.

Current Version

Crate version `0.3.0`, in lockstep with the latest release tag (`v0.3.0`) —
reconciled by RM-AIM-P1 DX-101 after drifting since inception. Release history
lives in [`CHANGELOG.md`](CHANGELOG.md); the roadmap milestones ("v0.1"…"v1.1")
are tracked in [`docs/18-roadmap/`](docs/18-roadmap/).

## Quickstart (code)

```bash
# Build, lint, test
cargo build --workspace
cargo test --workspace

# Run the hello agent locally. With no API key it uses a deterministic mock
# provider; set OPENAI_API_KEY (and optionally APEX_OPENAI_BASE_URL) for a real model.
cargo run -p apex-cli -- agents run --local \
  -f examples/agents/hello.yaml \
  --input '{"message":"Hi, who are you?"}' --stream

# Or run against a single-node server:
cargo run -p apex-cli -- dev &                       # start the server
cargo run -p apex-cli -- agents run --server http://127.0.0.1:8080 \
  -f examples/agents/hello.yaml --input '{"message":"Hi"}'

# Run a durable workflow (event-sourced DAG with checkpoints + retry):
cargo run -p apex-cli -- workflows run --local -f examples/workflows/greet-and-fetch.yaml

# Saga rollback: a failing step triggers reverse-order compensation:
cargo run -p apex-cli -- workflows run --local -f examples/workflows/saga-order.yaml

# Store and query memory (hybrid retrieval, persisted under ~/.apex/memory):
cargo run -p apex-cli -- memory put --namespace kb --content "Refund window is 30 days." --importance 0.9
cargo run -p apex-cli -- memory query "refund policy" --namespace kb
```

See [`docs/16-examples/hello-agent.md`](docs/16-examples/hello-agent.md) and
[`docs/18-roadmap/v0.1.md`](docs/18-roadmap/v0.1.md).

---

# Roadmap

The real, actively-maintained roadmap lives in
[`docs/18-roadmap/`](docs/18-roadmap/), not here — this section previously
listed a generic 8-phase plan that no longer matched how the project is
actually tracked and has been removed to avoid two conflicting sources of
truth. See:

- [`docs/18-roadmap/v0.1.md`](docs/18-roadmap/v0.1.md) ·
  [`v0.2.md`](docs/18-roadmap/v0.2.md) ·
  [`v0.3.md`](docs/18-roadmap/v0.3.md) — shipped milestones
- [`docs/18-roadmap/v1.0.md`](docs/18-roadmap/v1.0.md) — the GA hardening
  effort in progress now, with per-phase ticket docs under
  [`docs/18-roadmap/v1.0/`](docs/18-roadmap/v1.0/)

---

# Contributing

See CONTRIBUTING.md

---

# License

Apache License 2.0

---

# Acknowledgements

Apex is inspired by advances in distributed systems, workflow orchestration, cloud-native platforms, and modern AI agent architectures while being designed as an original, modular implementation focused on enterprise software engineering.
