# Wovyr — Generative UI Trust Runtime

> The infrastructure that lets AI agents render rich, interactive interfaces to
> humans **safely, auditable, and with durable human-in-the-loop decisions** —
> built on an enterprise AI Agent Operating System written in Rust.

![Version](https://img.shields.io/badge/version-0.3.0-blue)
![Rust](https://img.shields.io/badge/Rust-Edition%202024-orange)
![License](https://img.shields.io/badge/license-Apache%202.0-green)

---

## Quickstart (5 minutes)

Everything below runs offline with a deterministic mock provider — no API key
needed. You need Rust 1.85+ (edition 2024).

```bash
git clone https://github.com/punarduttrajput/Wovyr && cd Wovyr

# 1. Start the all-in-one local server (builds on first run).
WOVYR_ALLOW_ANONYMOUS=1 cargo run -p wovyr-cli -- dev
#    → listening on http://127.0.0.1:8080

# 2. In a second terminal: is it up?
curl http://127.0.0.1:8080/healthz
#    → {"status":"ok","version":"0.3.0"}

# 3. Run your first agent (mock provider answers deterministically).
cargo run -p wovyr-cli -- agents run --local \
  -f examples/agents/hello.yaml --input '{"message":"Hi"}' --stream
```

From there: set `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` for a real model,
`cd dashboard && npm ci && npx ng serve` for the UI (proxies to the server),
or `pip install wovyr-sdk` / `sdks/typescript` to call the API from code.
The full picture lives in [docs/](docs/SUMMARY.md); the runnable examples in
[examples/](examples/).

---

## Overview

**The product** ([PRD-005](docs/01-product/prd-generative-ui-runtime.md),
[ADR-0011](docs/17-adr/ADR-0011-generative-ui-repositioning.md)): software
interfaces are shifting from hard-coded pages to interfaces generated at runtime
by AI. That shift breaks the web's security assumptions — a generated form can be
a hallucinated phishing vector, prompt injection can manifest *as UI*, and nothing
can prove what an AI actually showed a user. Wovyr is the missing layer:

- **Trust & policy** — every agent-generated frame is validated against
  declarative policy (fail-closed), constrained to a safe component vocabulary
  (never raw model-authored HTML/JS), and recorded in a tamper-evident audit chain
  before a human sees it.
- **Durable interaction** — "agent shows an interface → human decides → agent
  continues" runs on an event-sourced workflow engine: the decision loop survives
  crashes, restarts, and time.
- **Embeddable runtime** — an SSE/pull frame protocol + a React renderer SDK
  (`@wovyr/ui-react`; a web-component build is a later slice) + MCP surface,
  adoptable as middleware by *any* agent stack; generative **enterprise
  internal tools** are the beachhead use case. Execution is phased in
  the [v1.2 roadmap](docs/18-roadmap/v1.2-generative-ui.md).

**The engine**: Wovyr is a next-generation AI Agent Operating System designed for building, deploying, and orchestrating intelligent autonomous agents at enterprise scale.

Unlike traditional AI frameworks that focus only on LLM orchestration, Wovyr provides a complete runtime platform featuring:

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
> ratified Path A): one `wovyr` binary, file-backed durable state by default,
> with Postgres/Qdrant as optional backends for specific subsystems (below).
> A distributed, multi-replica scheduler (shared queue/leases across
> instances) exists as tested library code but is **not wired into the
> shipping binary** — it's a v1.1 "Scale-Out" milestone, not a current
> capability. See
> [docs/18-roadmap/v1.0.md](docs/18-roadmap/v1.0.md) for what's shipped vs.
> deferred, workstream by workstream.

Wovyr is designed from the ground up using Rust to provide high performance, memory safety, and scalability.

---

# Vision

To be the trust layer of the generative-interface era: every interface an AI
shows a human is safe, provable, and accountable.

The platform underneath — everything required to build enterprise-grade AI
systems, modular, extensible, secure, and cloud-native — is the engine that makes
that product credible ([ADR-0011](docs/17-adr/ADR-0011-generative-ui-repositioning.md)).

---

# Mission

Enable organizations to let AI agents interact with humans through rich,
generated interfaces — without giving up security, auditability, or human
control — using modern software engineering principles rather than prompt
engineering alone.

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
      │   wovyr-server (one binary)      │
      ├─────────────────────────────────┤
      │ Agent Runtime (planner/executor/│
      │   tool-calling/memory loop)     │
      │ Workflow Engine (DAG, state     │
      │   machine, checkpoint, retry)   │
      └─────────────────────────────────┘
                      │
─────────────────────────────────────────────
 ~/.wovyr (local files) — the default, always-available durable store
 PostgreSQL — optional: marketplace registry (shipped); memory (CLI-only);
              workflow store exists as library code, not wired into the server
 Qdrant     — optional: memory vector index (CLI-only)

---

# Repository Structure

The actual Cargo workspace layout ([ADR-0001](docs/17-adr/ADR-0001-project-structure.md)):
shared logic lives in `crates/`, thin binaries in `apps/`.

```
wovyr/
  apps/
    wovyr-cli/            # the `wovyr` binary: login, dev server, agents/workflows/
                          # memory/kms/plugin/admin commands
  crates/
    wovyr-common/         # Error/Result, Usage, atomic_write/FileLock
    wovyr-provider/       # LLM gateway: chat/streaming/embeddings, resilience
    wovyr-agent/          # agent manifest + the model/tool run loop
    wovyr-tools/          # Tool trait, ToolRegistry, sandbox backends
    wovyr-workflow/       # durable, event-sourced workflow engine
    wovyr-memory/         # hybrid vector+keyword memory engine
    wovyr-telemetry/      # metrics + tracing/logging
    wovyr-tenancy/        # organizations/projects/RBAC/quota
    wovyr-events/         # domain events + outbound webhooks
    wovyr-secrets/        # the secret vault
    wovyr-audit/          # tamper-evident, hash-chained audit log
    wovyr-kms/            # envelope-encryption key management
    wovyr-plugin/         # plugin lifecycle (install/enable/upgrade/rollback)
    wovyr-marketplace/    # plugin marketplace registry
    wovyr-ui/             # the generative-UI frame protocol (PRD-005 UIP-1xx)
    wovyr-ui-guard/       # the UI trust layer: UiPolicy evaluation (GRD-2xx)
    wovyr-server/         # the Axum single-node server
    wovyr-eval/           # a deterministic AI-eval harness (prototype spike)
  dashboard/              # Angular SPA (direct to wovyr-server)
  deployment/             # systemd/Docker/Compose/Helm artifacts for what's actually built
  docs/                   # spec-driven documentation (source of truth)
  examples/               # runnable agent/workflow YAML manifests + examples/ui/ (the killer demo)
  sdks/                   # TypeScript + Python API clients, + ui-react/ (the generative-UI renderer, PRD-005 RDR-4xx)
```

Each crate's `lib.rs` doc comment links the `docs/` section it implements, and
every doc's status header says whether it describes shipped or target-state
behavior — `docs/` still contains future milestones not yet built; check the
status header before assuming a described feature exists.

## Where to start

It's a big workspace — but you don't need most of it to be productive. The
**flagship surface is the Generative UI Trust Runtime**; the rest of the crates
are the platform substrate it runs on and are safe to ignore on day one.

- **Just want to see it work?** Run the [Quickstart](#quickstart-5-minutes)
  above (offline, no API key), then click through the browser demo in
  [`examples/ui/checkout-demo`](examples/ui/) — present a frame → trust layer
  judges it → a human decides → the run resumes.
- **Want to understand the core idea?** Read, in order:
  [`wovyr-ui`](crates/wovyr-ui/) (the safe frame protocol) →
  [`wovyr-ui-guard`](crates/wovyr-ui-guard/) (the fail-closed policy layer) →
  `crates/wovyr-server/src/ui.rs` (where a frame is judged, audited, and
  rendered) → [`sdks/ui-react`](sdks/ui-react/) (the renderer). That's the whole
  loop; everything else (`wovyr-workflow`, `wovyr-memory`, `wovyr-kms`,
  `wovyr-marketplace`, …) is infrastructure you can treat as a black box until
  you need it.
- **Want to contribute?** Start with a doc/test/small-surface change in one of
  those four flagship crates before venturing into the platform internals, and
  see [CONTRIBUTING.md](CONTRIBUTING.md) (DCO sign-off required).

---

# Technology Stack

Backend

- Rust

API

- Axum (REST — this is what's actually shipped; there is no gRPC surface today)

Frontend

- Angular

Durable state (default)

- Local files under `~/.wovyr`, crash-safe via atomic writes + fsync'd
  append-only logs — no database required to run `wovyr dev`

Optional backends (env-var-selected; the default file-backed stores work
without any of these)

- **PostgreSQL** — marketplace registry (shipped, wired into both server and
  CLI); a `TieredStore` for the memory engine (CLI-only today, `wovyr memory`
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

- Bare-metal / systemd (real, working — [`deployment/install.sh`](deployment/install.sh) +
  [`deployment/systemd/`](deployment/systemd/), smoke-tested in CI; see
  [docs/12-deployment/systemd.md](docs/12-deployment/systemd.md))
- Docker, Docker Compose (real, working — [`deployment/docker-compose.yml`](deployment/docker-compose.yml))
- Kubernetes/Helm (a real single-replica chart exists at
  [`deployment/helm/wovyr/`](deployment/helm/wovyr/README.md); a multi-service,
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

- JWT (HS256/RS256) and API-key auth (`WOVYR_AUTH_MODE=jwt|apikey`) — no
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

**v0.1–v0.3 shipped and tagged; the v1.0 "GA hardening" effort
([PRD-003](docs/01-product/prd-ga-hardening.md)) is done across all four
phases** — security floor, durability & execution, scale & distribution
(single-node-appliance topology per
[ADR-0010](docs/17-adr/ADR-0010-ga-deployment-topology.md)), and contract &
operability. The **v1.1 "AI Platform Maturity" milestone**
([PRD-004](docs/01-product/prd-ai-platform-maturity.md),
[tickets](docs/18-roadmap/v1.1/index.md)) is in progress:
[Phase 1](docs/18-roadmap/v1.1/phase1-production-truth-tickets.md) (make
production claims true — real per-model cost, context-window management,
sandbox activation on the run path, graceful shutdown, durable async
runs/webhook outbox, API-key lifecycle, release automation) and
[Phase 2](docs/18-roadmap/v1.1/phase2-credible-ai-product-tickets.md)
(credible AI product — native Anthropic provider, structured output,
multimodal parts, RAG chunking/reranking/BM25, LLM-as-judge eval gates,
Redis-shared rate limiting, token quotas, content-safety guardrails, a
versioned prompt registry, per-tenant metrics) are **done**;
[Phase 3](docs/18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md)
(ecosystem & scale) is **done (2026-07-19), completing the v1.1 milestone** —
ecosystem (MCP client, plugin SDK + container loader, one-shot publish, an
OSV/CVE feed in the marketplace scanner), workflow-scale (WS-H), sandbox
(WS-E), server-hardening (WS-G) plus a request-scoped stdin secret channel
as the default plugin-secret path, incremental memory re-embedding
migration, dashboard (WS-K), DX/SDK (WS-J), and operability (WS-L: systemd
appliance, scrape-time operability gauges, end-to-end workflow traces + SLO
burn-rate rules, upgrade runbook + Helm TLS). Phase 3 was **re-scoped
through [PRD-005](docs/01-product/prd-generative-ui-runtime.md)** —
**[v1.2 "Generative UI Trust Runtime"](docs/18-roadmap/v1.2-generative-ui.md)**
([ADR-0011](docs/17-adr/ADR-0011-generative-ui-repositioning.md)) is **done,
all three phases (2026-07-15)**: the UI frame protocol (`wovyr-ui`), the
trust/policy engine (`wovyr-ui-guard`), the durable render→decide→resume
workflow interaction loop, the `@wovyr/ui-react` renderer SDK (cross-language
hash-verified against the real server) plus a framework-agnostic
`<wovyr-ui-frame>` web component, a killer-demo app you can run and click
through in a real browser (`examples/ui/checkout-demo`), a scoped
non-durable path for bare agent runs (`ui_present` tool +
`wovyr agents run --local --interactive-ui`), standalone middleware mode
(`POST /api/v1/ui/present` — present/decide/retrieve a governed frame with
**zero workflow or agent adoption**), a public conformance suite any
deployer can gate their own policy on (`wovyr_ui_guard::conformance`), a real
dashboard Surfaces panel dogfooding the whole loop under an operator's own
session, and a [design-partner onboarding
guide](docs/01-product/design-partner-onboarding.md) with its quickstart run
live end-to-end.

The implemented surface spans: an agent runtime with a real model/tool loop,
an LLM gateway (chat + streaming + embeddings, mock/OpenAI-compatible/native
Anthropic/local mistral.rs backends, with retry/failover/circuit-breaking/
caching), a tool
runtime with a sandbox spectrum (native/WASI/container/gVisor/microVM), a
**durable, event-sourced workflow engine** (checkpointing, retry, saga
compensation, durable timers/schedules, distributed worker leases as tested
library code), a **memory engine** (hybrid vector+keyword retrieval,
encryption, ABAC), a **secret vault**, **envelope-encryption KMS**,
**tamper-evident audit logging**, a **plugin engine + marketplace**,
**multi-tenancy** (RBAC + quota), a single-node Axum server, an Angular
dashboard, TypeScript/Python SDKs, and the `wovyr` CLI. The per-crate doc
comments and each `docs/` section's status header are the kept-current
description of what each crate actually does.

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
# provider; set OPENAI_API_KEY (and optionally WOVYR_OPENAI_BASE_URL) for a real model.
cargo run -p wovyr-cli -- agents run --local \
  -f examples/agents/hello.yaml \
  --input '{"message":"Hi, who are you?"}' --stream

# Or run against a single-node server:
cargo run -p wovyr-cli -- dev &                       # start the server
cargo run -p wovyr-cli -- agents run --server http://127.0.0.1:8080 \
  -f examples/agents/hello.yaml --input '{"message":"Hi"}'

# Run a durable workflow (event-sourced DAG with checkpoints + retry):
cargo run -p wovyr-cli -- workflows run --local -f examples/workflows/greet-and-fetch.yaml

# Saga rollback: a failing step triggers reverse-order compensation:
cargo run -p wovyr-cli -- workflows run --local -f examples/workflows/saga-order.yaml

# Store and query memory (hybrid retrieval, persisted under ~/.wovyr/memory):
cargo run -p wovyr-cli -- memory put --namespace kb --content "Refund window is 30 days." --importance 0.9
cargo run -p wovyr-cli -- memory query "refund policy" --namespace kb
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

Wovyr is inspired by advances in distributed systems, workflow orchestration, cloud-native platforms, and modern AI agent architectures while being designed as an original, modular implementation focused on enterprise software engineering.
