# Changelog

All notable changes to the Apex AI Platform are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from `0.3.0` onward (the workspace version was reconciled with the release tag by
RM-AIM-P1 DX-101; earlier milestones were tracked in `docs/18-roadmap/` without
bumping Cargo semver). Roadmap milestone docs remain the deep-dive source of truth —
each release links its own.

## [Unreleased]

v1.0 "GA hardening" (PRD-003, complete) and v1.1 "AI Platform Maturity" Phase 1
(PRD-004, in progress) — see [docs/18-roadmap/v1.0/](docs/18-roadmap/v1.0/) and
[docs/18-roadmap/v1.1/](docs/18-roadmap/v1.1/).

### Added
- **Security floor (RM-GA-P1):** real request authentication (`APEX_AUTH_MODE` —
  HS256/RS256 JWT or hashed API keys) replacing trusted headers; TLS-or-refuse on
  non-loopback binds; per-principal rate limiting; CORS allow-list; HTTP limits;
  safe-by-default tool registry (`shell` opt-in); workspace-confined `fs_read`;
  SSRF-guarded `http_get`; trust-class-driven sandbox selection.
- **Durability & DR (RM-GA-P2):** crash-safe `atomic_write`/`fsync` for every store;
  cross-process `FileLock`s; durable timers/schedules/crash recovery driven by the
  server itself; real workflow cancellation; async agent runs (`Prefer:
  respond-async`); `apex admin backup|restore` (incl. S3-compatible destinations, a
  hand-rolled SigV4 signer); KMS root-key escrow drill; RPO/RTO targets validated by
  timed restore drills.
- **Scale & distribution (RM-GA-P3):** versioned schema migrations (`refinery`, one
  history table per Postgres-backed crate, `apex admin migrate`); CI feature matrix +
  service-container integration jobs.
- **Contract & operability (RM-GA-P4):** uniform cursor pagination + idempotency keys
  on every mutating route + request-id propagation; `snake_case` on every wire enum;
  RED metrics for every route; audit coverage for every state-changing handler;
  OpenAPI contract gate in CI (boots a real server against both SDKs); `cargo-deny`
  gate; `apex-runtime` (one shared activity executor for CLI/server/eval) and
  `apex-config` (one shared `~/.apex`/env/KMS/secrets construction).
- **Angular dashboard** covering agents, workflows, memory, plugins, marketplace,
  secrets + real login/session.
- **v1.1 Phase 1 (RM-AIM-P1, in progress):** real per-model LLM cost accounting
  (`PriceBook` → `cost_usd`, PRV-101); context-window compaction + a tokenizer
  abstraction (AIC-101); concurrent tool calls per turn (AIC-102); manifest
  `max_steps` honored (AIC-103); container/gVisor sandboxes activated on the run path
  (SBX-101) + Windows Job Object resource limits (SBX-102); graceful shutdown
  (SRV-101); durable async-run store (SRV-102); durable webhook outbox + DLQ
  (SRV-103); API-key expiry/rotation/revocation (SRV-104); Postgres connection pool +
  reconnect (WFL-101); sub-workflow recursion guard (WFL-102); TLS to remote Postgres,
  live-validated (WFL-103); race-free event sequencing (WFL-104, fencing pending);
  secrets encrypted-at-rest by default with automatic plaintext migration (SEC-101);
  version/CHANGELOG reconciliation (DX-101).

## [0.3.0] — 2026-07-03

The extensibility + enterprise-controls milestone
([docs/18-roadmap/v0.3.md](docs/18-roadmap/v0.3.md)); tagged `v0.3.0`, with both
post-tag deferred slices (Postgres-backed marketplace registry, human review
workflow) also completed.

### Added
- **Plugin Engine** (`apex-plugin`): signed (`ed25519`) plugin packages, manifest
  validation, permission grants, dependency resolution, full lifecycle
  (install/enable/disable/upgrade/rollback/uninstall), WASM (WASI) capability
  runtime, durable catalog, SBOM + provenance policy.
- **Plugin Marketplace** (`apex-marketplace`): registry with signature-verified
  publish, discovery/search, downloads, reviews/ratings, static security scanning,
  human review workflow gating the verified badge, abuse reports + delisting;
  file-backed or Postgres-backed store.
- **Multi-tenancy** (`apex-tenancy` + server routes): Tenant → Org → Project model,
  default-deny RBAC, quotas enforced on the run path (concurrent runs + daily LLM
  spend), ETag optimistic concurrency.
- **Secret vault** (`apex-secrets`): reference-addressed, tenant-scoped, grant-gated
  secrets with masking + rotation; sandbox secret injection (`APEX_SECRET_*`).
- **Envelope-encryption KMS** (`apex-kms`): root → tenant keys → DEKs, rotation,
  crypto-shredding `destroy`; at-rest-encrypting secret/memory/webhook stores.
- **Tamper-evident audit log** (`apex-audit`): hash-chained, `fsync`ed, queryable
  over `GET /api/v1/audit`.
- **Domain events + webhooks** (`apex-events`): HMAC-signed deliveries, topic
  matching, backoff retry; `/v1` API hardening (pagination, idempotency, request-id).
- Sandbox spectrum: container (Docker/Podman), gVisor, Firecracker microVM, WASI
  backends; egress proxy + host-side `iptables`/`nsenter` lockdown; warm pooling +
  tenant-fair scheduling.
- The `apex-eval` deterministic evaluation harness (FUT-006 spike) and the
  workflow-orchestrated multi-agent prototype (FUT-001(b)).

## [0.2.0] — 2026-06-28

The intelligence + durability milestone
([docs/18-roadmap/v0.2.md](docs/18-roadmap/v0.2.md)); not tagged at the time.

### Added
- **Durable workflow engine** (`apex-workflow`): YAML DSL → validated DAG,
  event-sourced execution with per-step checkpoints and idempotent `resume`,
  retries, saga compensation, conditional branching, human-in-the-loop suspension,
  durable wall-clock timers, cron schedules, queries/visibility, child workflows,
  definition pinning, distributed workers over leases (in-memory/file/Postgres).
- **Memory engine** (`apex-memory`): hybrid retrieval (vector + keyword, RRF),
  weighted ranking, MMR diversification, ABAC scope filtering, compression;
  file-backed or tiered Postgres+Qdrant store; RAG-grounded agent runs.
- **Gateway resilience** (`apex-provider`): retry, failover, circuit breakers
  (local or Redis-shared), exact + semantic response caching (in-memory or Qdrant),
  request hedging, cost events; chaos + p95 perf gate tests.
- **Observability** (`apex-telemetry`): Prometheus/OpenMetrics metrics with
  exemplars, structured logging, OTLP traces/logs/metrics (opt-in).

## [0.1.0] — 2026-06-27

The runnable foundation slice ([docs/18-roadmap/v0.1.md](docs/18-roadmap/v0.1.md)).

### Added
- Cargo workspace skeleton (ADR-0001): shared `crates/`, thin `apps/`.
- `apex-common`: shared `Error`/`Result` + `Usage` accounting.
- `apex-provider`: vendor-neutral `AIProvider` trait, deterministic offline
  `MockProvider`, OpenAI-compatible provider, streaming, model-selector `Gateway`.
- `apex-tools`: `Tool` trait + registry, `echo`/`fs_read`/`http_get`/`shell`
  built-ins, native process sandbox (timeout + output cap + `setrlimit`).
- `apex-agent`: K8s-style YAML agent manifests and the model→tool→model run loop
  with step budgets and streamed events.
- `apex-server`: single-node Axum server — `/healthz`, `/metrics`, agent run/stream
  (SSE) + persistence routes, error envelope.
- `apex` CLI: `login`/`dev`/`agents run` (local embedded or remote server).
- The `docs/` specification tree (product → architecture → per-subsystem specs →
  ADRs → roadmap) that the codebase implements spec-first.

[Unreleased]: https://github.com/punarduttrajput/Apex/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/punarduttrajput/Apex/releases/tag/v0.3.0
[0.2.0]: https://github.com/punarduttrajput/Apex/tree/v0.3.0/docs/18-roadmap/v0.2.md
[0.1.0]: https://github.com/punarduttrajput/Apex/tree/v0.3.0/docs/18-roadmap/v0.1.md
