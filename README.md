# Wovyr — the trust layer for AI-generated interfaces

> **Self-hosted runtime, one Rust binary.** When an AI agent generates an
> interface for a human, Wovyr validates it against declarative policy
> (fail-closed), records it in a tamper-evident audit chain, and keeps the
> human's decision durable across crashes and restarts. Runs air-gapped.

![Version](https://img.shields.io/badge/version-0.4.1-blue)
![Rust](https://img.shields.io/badge/Rust-Edition%202024-orange)
![License](https://img.shields.io/badge/license-Apache%202.0-green)
[![Website](https://img.shields.io/badge/website-wovyr.com-black)](https://wovyr.com)

---

## Quickstart (5 minutes)

Everything below runs **offline against a deterministic mock provider — no API
key, no Docker, no cloud account.** You need Rust 1.85+ (edition 2024).

```bash
# Install the CLI from crates.io. Installs a `wovyr` binary.
cargo install wovyr-cli

# The example manifests live in the repo, so clone it for the run below.
git clone https://github.com/punarduttrajput/wovyr && cd wovyr

# Run your first agent.
wovyr agents run --local \
  -f examples/agents/hello.yaml --input '{"message":"Hi, who are you?"}' --stream
```

Working from a clone and would rather not install anything? Every `wovyr …`
command below is the same as `cargo run -p wovyr-cli -- …`.

Real output from that command, verbatim:

```text
INFO wovyr_provider::gateway: llm gateway: no API key set, using mock provider
start  · model: mock-chat-fast (provider: mock)
delta  · "Hello! I'm an Wo"
delta  · "vyr agent (mock "
delta  · "provider). You s"
…
done   · tokens: 82, cost_usd: 0.000041
```

Then start the server and talk to it over HTTP:

```bash
WOVYR_ALLOW_ANONYMOUS=1 wovyr dev   # → 127.0.0.1:8080
curl http://127.0.0.1:8080/healthz  # → {"status":"ok","version":"0.4.1"}
```

From there: set `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` for a real model,
`make dashboard-dev` for the Angular UI, or `pip install wovyr-sdk` /
`npm i @wovyr/sdk` to call the API from code. The full picture lives in
[docs/](docs/SUMMARY.md); runnable examples in [examples/](examples/).

---

## Why not just use X?

Wovyr overlaps three categories without being any of them. The honest
comparison:

| | **LangChain / LangGraph** | **Temporal** | **E2B / Firecracker sandboxes** | **Wovyr** |
|---|---|---|---|---|
| **Primary job** | Compose LLM calls and agents | Durable workflow execution | Run untrusted code in isolation | Agent runtime whose **human-facing output** is policy-checked and provable |
| **Durable human-in-the-loop** | Ad hoc / app's problem | Yes (generic signals) | n/a | Yes — a frame pends durably; the decision survives a kill -9 |
| **Agent-generated UI** | Renders whatever the model emits | n/a | n/a | Constrained component vocabulary; **no raw model-authored HTML/JS**, no credential inputs, fail-closed parse |
| **Tamper-evident audit** | No | Event history (not tamper-evident) | No | Keyed HMAC hash chain + head anchor; detects interior edits *and* tail truncation |
| **Tool sandboxing** | Trust the process | n/a | Yes — that's the product | Native → container → gVisor → microVM → WASI, selected by trust class, default-deny egress |
| **Deployment** | Library in your app | Server + workers + DB | Hosted service / VM host | **One binary**, file-backed state, air-gappable |
| **Encryption at rest** | n/a | Your DB's problem | n/a | Envelope encryption w/ per-tenant keys + crypto-shredding |

**Use LangChain** to build the agent. **Use Temporal** if you need
planet-scale generic workflows. **Use E2B** if isolated code execution is the
whole job. **Use Wovyr** when an agent's output reaches a human or an auditor
and someone will eventually ask *"prove what it showed them, and prove nothing
else could have happened."*

If you don't have that requirement, Wovyr is probably more machinery than you
need — and that's a fine answer.

---

## Security posture

Stated precisely, because vague security claims are worse than none. Each row
links the spec and is covered by tests in `cargo test --workspace`.

| Property | What actually holds |
|---|---|
| **Auth** | Enforced by default. JWT (HS256/RS256) or hashed API keys, verified *before* any handler; the verified principal overwrites client-supplied identity headers. Anonymous mode requires an explicit opt-in **and** refuses to bind a non-loopback address. |
| **Transport** | A non-loopback bind without TLS fails to start, unless you declare a terminating proxy. |
| **Authorization** | Default-deny RBAC over `domain:action` scopes, tenant-scoped on every route. Cross-tenant access returns 404, never 403 (no existence leak). |
| **Tool sandboxing** | Container/gVisor isolation for untrusted code with **L3 default-deny egress** (host-side `iptables`/`nsenter`, not just `HTTPS_PROXY`). The native backend is *scoped*: a Linux egress floor via unprivileged netns; elsewhere it runs only as an explicitly-acknowledged unsandboxed run, or fails closed. Filesystem confinement on the native path is a [documented gap](docs/07-tool-runtime/security-isolation.md#51-the-native-backends-isolation-is-scoped-not-universal-sec-404). |
| **Privileged local tools** | `shell`/`fs_write`/`code_execute` need an explicit per-run opt-in even locally ([§5.2](docs/07-tool-runtime/security-isolation.md#52-privileged-builtins-need-an-explicit-opt-in-under---local)). |
| **SSRF** | `http_get` and the MCP transport resolve and pin the target IP, reject loopback/link-local/private/CGNAT/metadata ranges (including IPv4-in-IPv6 encodings), and re-run the full guard on every redirect hop. |
| **Secrets at rest** | Encrypted by default via envelope encryption; values are non-serializable and masked in `Debug`/`Display`. Missing durable key material **fails startup** rather than silently using an ephemeral key. |
| **Audit** | Append-only, keyed-HMAC hash chain with a monotonic head anchor — an attacker with write access to the log cannot forge it or truncate its tail undetected. |
| **Generated UI** | No raw-HTML/script node and no credential-input component *exist* in the protocol — a structural guarantee, not a filter. Interactive frames are denied by default absent a policy. |
| **Crash safety** | Every single-document store uses atomic write + `fsync` + cross-process file locking; append-only logs `fsync` file *and* directory. |

Not yet done: no external penetration test, no SOC 2, no formal verification.
An internal red-team assessment (2026-07-27) found 0 Critical and 0 High
findings; all four lower-severity findings are fixed — see
[v1.6](docs/18-roadmap/v1.6-pentest-remediation.md). Report vulnerabilities via
[SECURITY.md](SECURITY.md), never a public issue.

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
- **Embeddable runtime** — an SSE/pull frame protocol, a React renderer SDK
  (`@wovyr/ui-react`) plus a framework-agnostic `<wovyr-ui-frame>` web
  component, and an MCP surface — adoptable as middleware by *any* agent stack;
  generative **enterprise internal tools** are the beachhead use case.
  Execution is phased in the
  [v1.2 roadmap](docs/18-roadmap/v1.2-generative-ui.md).

**The engine underneath.** That trust layer is only credible if the runtime
carrying it is real, so Wovyr also ships the platform an agent needs end to
end — and it is the platform, not just an LLM-orchestration library:

| Subsystem | Crate |
|---|---|
| Agent runtime (model + tool loop, context budgeting, guardrails) | `wovyr-agent` |
| Durable, event-sourced workflow engine (checkpoints, sagas, timers, cron) | `wovyr-workflow` |
| Tool execution + sandboxing (native → container → gVisor → microVM → WASI) | `wovyr-tools` |
| Memory engine (hybrid BM25 + vector retrieval, MMR, ABAC) | `wovyr-memory` |
| Multi-provider LLM gateway (retry, failover, breakers, caching, cost metering) | `wovyr-provider` |
| Plugin framework + signed marketplace | `wovyr-plugin`, `wovyr-marketplace` |
| Multi-tenancy, secrets, KMS, tamper-evident audit | `wovyr-tenancy`, `wovyr-secrets`, `wovyr-kms`, `wovyr-audit` |

Written in Rust for performance, memory safety, and a single deployable binary.

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
[tickets](docs/18-roadmap/v1.1/index.md)) is likewise complete:
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
session.

Since then:
**[v1.3 "MCP Connection Management"](docs/18-roadmap/v1.3-mcp-connections.md)**
([ADR-0012](docs/17-adr/ADR-0012-mcp-connection-trust-boundary.md)) — persisted,
API/dashboard-managed MCP connections with a real trust boundary (`Stdio`
transport needs both an RBAC scope and an operator opt-in) — **done**;
**[v1.5 "Design System Unification"](docs/18-roadmap/v1.5-design-system-unification.md)**
— one token system across all four UI surfaces, WCAG AA contrast gated in CI,
and a real Playwright + axe browser e2e harness — **done**;
**[v1.6 "Pentest Remediation"](docs/18-roadmap/v1.6-pentest-remediation.md)** —
all four findings of an internal red-team assessment (0 Critical, 0 High) —
**done**.

**[v1.4 "Audit Remediation"](docs/18-roadmap/v1.4-audit-remediation.md)** is
**partly done — 11 of its 20 tickets.** Shipped: the redirect-SSRF hole and the
encapsulated/CGNAT range gap, an org-level cross-tenant authz gap, keyed audit
MACs with a tail-truncation anchor, a native sandbox confinement floor,
fail-closed KMS key sourcing, bounded caches, fail-loud embeddings, and the CI
work (QA-401/402/403). **Still outstanding:** the version/maturity
reconciliation and claim-honesty pass (STR-501/502), and all of Phase 3 —
default-config retrieval quality, multimodal token counting, reasoning-model
parameter compatibility, load-time `${...}` reference validation, subsystem
freeze/experimental labeling, de-risking hand-rolled security code, and
wiring-or-cutting the built-but-unwired features (AIC-303/304/305, WFL-309,
STR-503/504/505). Each ticket in that document carries its own status marker.

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

## Two version numbers, and why

This trips people up, so plainly:

- **Crate/package version `0.4.1`** — the semver of the published artifacts
  (crates.io, npm, PyPI) and of the git release tag. Still `0.x`: the HTTP API
  and wire formats may change, and have.
- **Milestone names `v0.1`…`v1.6`** — the *roadmap* units in
  [`docs/18-roadmap/`](docs/18-roadmap/). These are planning labels, not package
  versions, and they are ahead of the semver number by design.

So "v1.6 complete" and "version 0.4.1" are both true and describe different
things. Release history lives in [`CHANGELOG.md`](CHANGELOG.md).

**Milestone status:** v0.1–v0.3 shipped and tagged. v1.0 (GA hardening —
engineering scope), v1.1 (AI platform maturity), v1.2 (generative UI trust
runtime), v1.3 (MCP connections), v1.5 (design-system unification), and v1.6
(pentest remediation) are **complete**.

Two tracks remain **open**: v1.4 (audit remediation) at 11 of 20 tickets, and
v1.0's Tier-A validation work (GA-001…GA-005 — scale/performance validation,
reliability, external security validation, marketplace economics, SDK
distribution), which is separate from the GA *engineering* scope above and was
never claimed complete.

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

- [`docs/18-roadmap/index.md`](docs/18-roadmap/index.md) — start here; the
  milestone-by-milestone status table
- [`v0.1.md`](docs/18-roadmap/v0.1.md) · [`v0.2.md`](docs/18-roadmap/v0.2.md) ·
  [`v0.3.md`](docs/18-roadmap/v0.3.md) — the foundation, workflow engine, and
  ecosystem milestones
- [`v1.0.md`](docs/18-roadmap/v1.0.md) (GA hardening, complete) ·
  [`v1.1/`](docs/18-roadmap/v1.1/index.md) (AI platform maturity, complete) ·
  [`v1.2`](docs/18-roadmap/v1.2-generative-ui.md) ·
  [`v1.3`](docs/18-roadmap/v1.3-mcp-connections.md) ·
  [`v1.4`](docs/18-roadmap/v1.4-audit-remediation.md) ·
  [`v1.5`](docs/18-roadmap/v1.5-design-system-unification.md) ·
  [`v1.6`](docs/18-roadmap/v1.6-pentest-remediation.md) — all complete
- [`docs/18-roadmap/future/`](docs/18-roadmap/future.md) — larger ideas that
  have not graduated to a milestone

Each ticket states the problem with file:line evidence, the change, acceptance
criteria, and a size estimate — so they double as the contributor backlog.

---

# Contributing

Contributions are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers the build,
where to find scoped work, the DCO sign-off requirement, and the "honest docs"
rule this project holds itself to.

Two things worth knowing before you start:

- `make lint` is exactly what CI gates on — run it before pushing.
- A fix ships with a test that fails against the pre-fix code. That's the house
  convention, not a formality.

Security issues go through [SECURITY.md](SECURITY.md), never a public issue.

---

# License

Apache License 2.0 — see [LICENSE](LICENSE).

---

# Acknowledgements

Wovyr is inspired by advances in distributed systems, workflow orchestration, cloud-native platforms, and modern AI agent architectures while being designed as an original, modular implementation focused on enterprise software engineering.
