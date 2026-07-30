# Changelog

All notable changes to the Wovyr AI Platform are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from `0.3.0` onward (the workspace version was reconciled with the release tag by
RM-AIM-P1 DX-101; earlier milestones were tracked in `docs/18-roadmap/` without
bumping Cargo semver). Roadmap milestone docs remain the deep-dive source of truth —
each release links its own.

## [Unreleased]

### Fixed

- **The release pipeline's GHCR push is no longer denied.** The `container image
  (GHCR)` job built and uploaded every layer, then failed the manifest write with
  `denied: permission_denied: write_package` — pushing
  `ghcr.io/punarduttrajput/wovyr:0.3.2`. The workflow's `permissions: packages:
  write` was never the problem: the package had first been created by a local,
  label-less `docker push`, which makes it a *user*-owned package connected to no
  repository, and an unlinked package grants this repository's `GITHUB_TOKEN` no
  write access at all. Every image now carries
  `org.opencontainers.image.source` — the label GHCR uses to link a package to
  its repository — stamped both in
  [`deployment/docker/Dockerfile`](deployment/docker/Dockerfile) (so a manual
  local push links it the same way CI's does) and in
  [`.github/workflows/release.yml`](.github/workflows/release.yml)'s `labels:`
  (so it names the pushing repository rather than a hardcoded one, alongside
  per-build `revision`/`version`). Linking the *already-created* package is a
  one-time operator step that no label can do retroactively — package settings →
  "Manage Actions access" → add the repository with the **Write** role, or delete
  the package and let the next release recreate it auto-linked; both paths are
  documented in the workflow header and in
  [`DISTRIBUTION.md`](DISTRIBUTION.md).

## [0.3.2] — 2026-07-29

Build fix only — no behavior, API, or wire-format change.

### Fixed

- **macOS release binaries compile again.** `wovyr-tools`' native sandbox spelled
  `setrlimit`'s `resource` argument as `libc::__rlimit_resource_t` — a **glibc-only**
  type alias; musl and the BSDs (macOS included) use a plain `c_int`. The
  `#[cfg(unix)]` block therefore built fine on Linux and was never exercised on
  Windows, and only the `aarch64-apple-darwin` leg of the release pipeline failed,
  with `E0425: cannot find type __rlimit_resource_t in crate libc` — which took the
  whole v0.3.1 release with it, since the GitHub Release job needs all three binary
  jobs. The `set` helper is now a closure rather than a named `fn`, so it infers the
  argument type from the `libc::setrlimit` call itself and every Unix target gets its
  own with no `cfg` matrix to keep in sync
  ([`crates/wovyr-tools/src/sandbox/native.rs`](crates/wovyr-tools/src/sandbox/native.rs)).
  Type-checked for both `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`
  (the full workspace can't be cross-checked from Windows — `ring`'s build script
  needs a darwin C toolchain — so the affected function was checked in isolation
  against real `libc` headers for both targets).

## [0.3.1] — 2026-07-28

Open-source launch readiness. Everything below is either a security fix, a
correctness fix, or a docs-vs-reality correction; no new product surface.

Roadmap milestones **v1.4** (audit remediation), **v1.5** (design-system
unification), and **v1.6** (pentest remediation) all completed in this window —
see [docs/18-roadmap/v1.4-audit-remediation.md](docs/18-roadmap/v1.4-audit-remediation.md),
[v1.5-design-system-unification.md](docs/18-roadmap/v1.5-design-system-unification.md),
and [v1.6-pentest-remediation.md](docs/18-roadmap/v1.6-pentest-remediation.md).
(Milestone names are roadmap labels, not package versions — see the README's
"Two version numbers" section.)

### Security

- **RES-601 — `for_each` fan-out now accepts an aggregate cost/token ceiling.**
  `max_items` bounded item *count* only, and a per-item body may be a full
  `agent` activity, so one fan-out could expand into an unbounded number of
  billable model calls inside a single execution — the server's per-project
  budget is a *daily rate*, not a per-execution cap, and does not apply to CLI
  `--local` runs at all. `inputs.max_total_cost_usd` / `inputs.max_total_tokens`
  are enforced as items land: crossing one stops launching further items and
  fails the activity closed, while in-flight items still commit durably.
  Validated fail-closed at load (`0`/negative/non-finite is an error, not
  "unlimited"). Omitting them is behavior-identical to 0.3.0.
- **SBX-305 — privileged tools need an explicit opt-in under `--local`.**
  `wovyr agents run --local` and `wovyr workflows run --local` registered
  `shell`/`fs_write`/`code_execute` unconditionally, treating "the operator typed
  `--local`" as consent — indistinguishable between a trusted workstation and a
  shared or CI host. Now requires `--allow-privileged-tools` (or
  `WOVYR_LOCAL_PRIVILEGED=1`, which the `approve`/`signal`/`tick` resume paths
  also honor). A manifest or definition naming a privileged tool — including
  inside a `for_each` body — fails closed with an error naming the flag, instead
  of running with the tool silently absent. Documented in
  [security-isolation §5.2](docs/07-tool-runtime/security-isolation.md#52-privileged-builtins-need-an-explicit-opt-in-under---local).
- **VAL-401 — the agent manifest's unknown-field tolerance is documented** as
  the deliberate exception it is, alongside a statement that the manifest is
  therefore not a place to detect tampering
  ([agent-definition §5.1](docs/04-agent-framework/agent-definition.md)).

### Fixed

- **`cargo test --workspace` is deterministic.** Server tests built state via
  `AppState::from_env()`, which resolves durable stores under the developer's
  real `~/.wovyr` — so tests raced each other (and any prior `wovyr dev` run)
  through shared files, making `agents_are_isolated_per_tenant` fail
  intermittently with a name that read like a tenant-isolation breach.
  Added `AppState::for_test()` and routed every test call site through it.
- **Nested `cargo build` no longer inherits the outer cargo's jobserver.**
  `wovyr plugin build` passed `CARGO_MAKEFLAGS` (and host `RUSTFLAGS`, target,
  and target-dir settings) through to the `wasm32-wasip1` child build, which
  intermittently failed partway through compiling dependencies. Build failures
  now also report the exit status, stdout, and — when cargo exits without
  printing a compile error — that the process was terminated rather than the
  code failing to build.

### Changed

- **Package metadata:** the `repository` URL now matches the real repository
  name in all 15 places that carried the wrong casing (this reached crates.io,
  npm, and PyPI metadata for every published package), and
  `wovyr-server`'s crates.io description no longer claims "v0.1: agent runs".
- **README rewritten around one positioning line**, with a
  vs-LangChain/Temporal/E2B comparison table, a precise security-posture table
  that scopes each claim, a real (not illustrative) command transcript, and an
  explicit explanation of why the package version (`0.3.1`) and the roadmap
  milestone names (`v1.6`) differ.
- **`CONTRIBUTING.md` commands exist now.** It documented `make setup` and
  `make dev`, neither of which was a Makefile target; both are implemented, and
  the guide gained a "finding something to work on" section and the
  offline-cargo workaround.
- **`docs/19-implementation-guide/development-environment.md` rewritten** — it
  required a NestJS BFF, `pnpm`, `cargo nextest`, a devcontainer, Git hooks, a
  `.env.example`, `make run-svc`, `wovyr doctor`, and `sccache`, none of which
  exist in this repository.
- **`docs/03-workflow-engine/workflow-dsl.md` §13 rewritten** — it specified a
  `loop: {while, until, foreach}` block with a `collection:` key that was never
  implemented. It now documents the real `for_each`/`map` activity, the new
  aggregate ceilings, and a table making explicit that a `for_each` ceiling is
  the only per-execution budget that applies under `--local`.
- Internal go-to-market and landing-page requirement documents are no longer
  tracked in the repository.

### Earlier in this window (pre-0.3.1, previously unreleased)

Everything from here to the `[0.3.0]` heading below shipped after the `v0.3.0`
tag and had accumulated under `[Unreleased]`: v1.0 "GA hardening" (PRD-003),
v1.1 "AI Platform Maturity" (PRD-004), v1.2 "Generative UI Trust Runtime"
(PRD-005/ADR-0011), and v1.3 "MCP Connection Management" (PRD-006/ADR-0012) —
all complete. See [docs/18-roadmap/v1.0/](docs/18-roadmap/v1.0/),
[v1.1/](docs/18-roadmap/v1.1/),
[v1.2-generative-ui.md](docs/18-roadmap/v1.2-generative-ui.md), and
[v1.3-mcp-connections.md](docs/18-roadmap/v1.3-mcp-connections.md).

- **MCP Connection Management (RM-MCX, PRD-006/ADR-0012):** a persisted,
  API/dashboard-managed layer over the already-shipped, programmatic-only MCP
  client (`wovyr-tools::mcp`, ECO-301) — connect an external MCP server once,
  grant it to agents declaratively, see its tools in the existing tool picker.
  **Connection core:** a tenant-scoped, file-backed `McpConnectionStore` +
  bounded-idle-timeout client cache (`wovyr-tools/src/mcp_store.rs`/
  `mcp_cache.rs`); `POST/GET/DELETE /api/v1/mcp/connections[/{name}]` +
  `.../refresh` (`wovyr-server/src/mcp.rs`); a `Stdio`-transport connection
  (arbitrary local command execution) requires *both* the `mcp:admin` RBAC
  scope *and* the operator's `WOVYR_ENABLE_MCP_STDIO=1` opt-in (the
  `WOVYR_ENABLE_SHELL_TOOL` precedent) while `Http` reuses `http_get`'s SEC-304
  SSRF guard verbatim; a credential is always a `SecretRef`, never an inline
  value; a new `max_mcp_connections` quota dimension bounds the cache's warm
  process pool. **Agent wiring:** `AgentDefinition.spec.mcp_servers` is a
  declarative connection-name allow-list a run resolves into its
  `ToolRegistry` (CLI, server, and workflow `agent` activities alike, via the
  shared `McpClientCache::resolve_agent_mcp_tools`) — an agent naming no
  connection can't reach its tools even if the tenant has one configured;
  `GET /api/v1/tools` merges in the caller's tenant's live-discovered
  `mcp__<server>__<tool>` ids for an `mcp:read`-authorized caller, alongside
  built-ins. **SDK + dashboard:** the TypeScript SDK's `client.mcp` resource;
  a dashboard "MCP Servers" panel (compose → call → render, mirroring the
  Surfaces panel, hiding the `Stdio` option rather than offering-then-rejecting
  it when disabled); Agent Studio's tool picker surfaces MCP-sourced tools and
  auto-grants the underlying connection via `spec.mcp_servers` when one is
  picked. Verified live end to end in a real browser against a real server and
  a real spawned stdio MCP connection — PRD-006 §9's acceptance narrative, run
  for real. Fixed a real pre-existing bug found along the way: the dashboard's
  `agent.service.ts` `tools()` parsed a bare `{tools: [...]}` shape instead of
  the actual cursor-pagination envelope `GET /api/v1/tools` returns, so the
  live tool catalog silently never replaced the picker's hardcoded fallback.
- **Generative UI Trust Runtime — Beachhead & Embeddability (RM-GUI-P3, PRD-005):**
  standalone middleware mode (EMB-701) — `POST /api/v1/ui/present` +
  `GET/POST /api/v1/ui/decisions/{frame_id}` present, decide, and retrieve a
  trust-layer-governed frame with **zero workflow or agent adoption**,
  sharing the identical `judge_frame` policy path the workflow `ui` activity
  uses (`PendingFrame`'s `execution_id`/`activity_id` became `Option<String>`
  to model a standalone frame). A public, reusable conformance suite
  (EMB-704, `wovyr_ui_guard::conformance`) — must-allow/must-block/must-redact
  vectors any deployer can run against their own policy
  (`conformance_report(&policy)`), gated in this workspace's own `cargo test
  --workspace`. RDR-402's P2 cut ("React only") was revisited: `<wovyr-ui-frame>`
  (`@wovyr/ui-react/web-component`) is a framework-agnostic custom element
  wrapping `UiFrameView` via `react-dom/client`, dispatching a `decide`
  `CustomEvent` — proven with a React-free demo page
  (`examples/ui/checkout-demo/web-component.html`). A real dashboard Surfaces
  panel (ITS-601/602, `dashboard/src/app/features/surfaces/`) dogfoods the
  whole loop on Wovyr's own ops surface: an operator composes a real `UiFrame`,
  presents it through the dashboard's own `HttpClient` + tenant interceptor,
  renders it with `<wovyr-ui-frame>`, and decides it under their own
  RBAC-scoped session — including a live demonstration of the trust layer
  blocking a destructive action. A design-partner onboarding guide
  (`docs/01-product/design-partner-onboarding.md`, PRD-005 §8) ships with
  every quickstart command run against a real server. Cut, documented rather
  than silently dropped: an MCP server surface (EMB-702 — no MCP server
  subsystem exists in this codebase at all), the A2UI/MCP-Apps interop
  mapping (UIP-105/EMB-703), destructive-action auto-gating and
  saveable/shareable surfaces (ITS-603/604), and the design-partner program's
  actual execution (a business activity, not a code deliverable).
- **Generative UI Trust Runtime — Renderer & Interaction Loop (RM-GUI-P2, PRD-005):**
  `@wovyr/ui-react` (`sdks/ui-react`), a React renderer for the full frame
  vocabulary — themeable CSS-custom-property design tokens, an inert visible
  placeholder for unrecognized node types, and client-side frame-hash
  verification (canonical-JSON + Web Crypto SHA-256, cross-checked against a
  real frame from a live server and confirmed byte-for-byte identical to the
  Rust-computed hash — which also surfaced and fixed a real bug, see Fixed
  below). The base TS SDK (`sdks/typescript`) gained a `ui` resource
  (`frames.list/get`, `decisions.submit`) with an integration-test suite
  proving UC1/UC4 against a live server. The killer demo
  (`examples/workflows/ui-checkout-{approve,block}.yaml` +
  `examples/ui/checkout-demo`, a runnable Vite+React app) reproduces PRD-005
  §9 end to end in a real browser: the poisoned frame blocks and never
  renders; the safe frame renders, is approved with a real decision POST,
  and clears from the pending list. A scoped, explicitly non-durable
  HIL-304 landed too: the `ui_present` tool (`wovyr-tools`, opt-in only) +
  `UiInteraction` trait + `wovyr agents run --local --interactive-ui`'s
  stdin presenter, so a bare agent run can present a policy-checked frame
  outside a workflow. Deferred, documented in
  [the roadmap](docs/18-roadmap/v1.2-generative-ui.md): a web-component
  build, progressive/streaming frame rendering, durable frame timers and
  audit-chain integration for the bare-agent-run path, WASI-sandboxed
  validators, and signed UI templates.
- **Generative UI Trust Runtime — Protocol & Trust Core (RM-GUI-P1, PRD-005):**
  the `wovyr-ui` frame protocol (constrained component vocabulary — no raw
  HTML/script, no credential-input component; fail-closed parsing; semver'd
  schema version with newer-than-understood rejection; runtime-stamped
  provenance; canonical content hashes; typed decision validation) and the
  `wovyr-ui-guard` trust layer (declarative YAML `UiPolicy`:
  sensitive-input-name blocking, destructive-action gating, intent-mismatch
  deception checks, media-origin allow-lists, text redaction, tighten-only
  budgets; `hosted_floor` denies interactive frames when no policy exists;
  `WOVYR_UNRESTRICTED_UI=1` escape hatch). Server: the `ui` workflow activity
  (present a policy-checked frame → suspend durably → resume on a validated
  decision), a durable pending-frame store under `~/.wovyr/ui`,
  `GET /api/v1/ui/frames[/{id}]` + `POST /api/v1/ui/decisions/{id}`, every
  verdict and decision recorded in the tamper-evident audit chain paired with
  the frame hash, and a `RunEvent::UiFrame` SSE event. Proven end-to-end by
  UC1 (present → kill/restart → deterministic re-present → decide → resume)
  and UC4 (an injected credential-harvesting frame is blocked, never visible,
  audited).

### Fixed
- **A real lost-update race in the workflow engine's `resume`/`deliver`
  path** (`wovyr-workflow`): with no per-execution serialization, two
  same-process callers driving the same `execution_id` concurrently — the
  exact shape `wovyr-server`'s `submit_handler` produces (a fire-and-forget
  background `resume()` right after `start()`, racing an immediate
  `signal`/`approve`/`ui`-decide call) — could each read the same stale
  checkpoint and independently drive it; whichever finished its write *last*
  won, silently reverting an already-**completed** execution back to "still
  running". Found investigating an intermittent flake in the RM-GUI-P1
  `ui:` SDK test suite; reproduced deterministically (no wall-clock race,
  confirmed to fail pre-fix and pass post-fix) in
  `crates/wovyr-workflow/tests/engine.rs`'s
  `concurrent_resume_and_signal_do_not_lose_a_completed_state`. Fixed with
  an in-process per-execution async lock shared by `resume`/`deliver`
  (`signal_event`/`fire_timer`)/`run`. `Engine::cancel`'s racy-tolerant
  behavior is unrelated and deliberately untouched.
- **`UiFrame::content_hash` canonicalization (RM-GUI-P2):** hashed the
  struct's own field-declaration-order serialization instead of the
  alphabetically key-sorted `Value` form every real consumer (the server's
  JSON responses, the renderer) actually receives — silently breaking
  client-side integrity verification (RDR-403) the moment a struct's fields
  weren't already alphabetical. Found while building `@wovyr/ui-react`'s hash
  check and cross-verifying it against a live server, not by inspection.
- **Security floor (RM-GA-P1):** real request authentication (`WOVYR_AUTH_MODE` —
  HS256/RS256 JWT or hashed API keys) replacing trusted headers; TLS-or-refuse on
  non-loopback binds; per-principal rate limiting; CORS allow-list; HTTP limits;
  safe-by-default tool registry (`shell` opt-in); workspace-confined `fs_read`;
  SSRF-guarded `http_get`; trust-class-driven sandbox selection.
- **Durability & DR (RM-GA-P2):** crash-safe `atomic_write`/`fsync` for every store;
  cross-process `FileLock`s; durable timers/schedules/crash recovery driven by the
  server itself; real workflow cancellation; async agent runs (`Prefer:
  respond-async`); `wovyr admin backup|restore` (incl. S3-compatible destinations, a
  hand-rolled SigV4 signer); KMS root-key escrow drill; RPO/RTO targets validated by
  timed restore drills.
- **Scale & distribution (RM-GA-P3):** versioned schema migrations (`refinery`, one
  history table per Postgres-backed crate, `wovyr admin migrate`); CI feature matrix +
  service-container integration jobs.
- **Contract & operability (RM-GA-P4):** uniform cursor pagination + idempotency keys
  on every mutating route + request-id propagation; `snake_case` on every wire enum;
  RED metrics for every route; audit coverage for every state-changing handler;
  OpenAPI contract gate in CI (boots a real server against both SDKs); `cargo-deny`
  gate; `wovyr-runtime` (one shared activity executor for CLI/server/eval) and
  `wovyr-config` (one shared `~/.wovyr`/env/KMS/secrets construction).
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
- **v1.1 Phase 2 (RM-AIM-P2, in progress):** first-class `AnthropicProvider` against
  the native Messages API (PRV-201) — tool use via `tool_use`/`tool_result` blocks,
  system-prompt hoisting, prompt caching with cache-rate-aware `cost_usd`, real SSE
  streaming; wired into `Gateway::from_env()` (`ANTHROPIC_API_KEY`), class-based model
  resolution (haiku/sonnet/opus), current Claude prices in the `PriceBook`, and
  `wovyr agents run --local --provider anthropic`. Structured output + forced tool
  (PRV-202): `ChatRequest` gained `tool_choice` (auto/none/required/named) and
  `response_format` (`json_object`/`json_schema`), translated per provider
  (OpenAI `response_format` + `strict`, Anthropic `output_config.format` +
  `tool_choice`, mistral.rs `Constraint::JsonSchema` grammar + forced `Tool`);
  unsupported combinations fail closed as `Invalid`, and both fields joined the
  gateway's exact + semantic cache keys. Tool-schema normalization + surfaced
  arg-parse errors (PRV-203): opt-in `ToolSpec.strict` normalizes a tool's
  schema into the vendor strict-mode subset (unsupported keywords stripped,
  objects closed, all properties required) and flags it `strict` on the wire
  (OpenAI + Anthropic); the agent loop no longer swallows malformed tool
  arguments to `null` — the parse error is fed back to the model as a failed
  tool-result turn so it can correct the call (an empty argument string still
  invokes with `{}`).

## [0.3.0] — 2026-07-03

The extensibility + enterprise-controls milestone
([docs/18-roadmap/v0.3.md](docs/18-roadmap/v0.3.md)); tagged `v0.3.0`, with both
post-tag deferred slices (Postgres-backed marketplace registry, human review
workflow) also completed.

### Added
- **Plugin Engine** (`wovyr-plugin`): signed (`ed25519`) plugin packages, manifest
  validation, permission grants, dependency resolution, full lifecycle
  (install/enable/disable/upgrade/rollback/uninstall), WASM (WASI) capability
  runtime, durable catalog, SBOM + provenance policy.
- **Plugin Marketplace** (`wovyr-marketplace`): registry with signature-verified
  publish, discovery/search, downloads, reviews/ratings, static security scanning,
  human review workflow gating the verified badge, abuse reports + delisting;
  file-backed or Postgres-backed store.
- **Multi-tenancy** (`wovyr-tenancy` + server routes): Tenant → Org → Project model,
  default-deny RBAC, quotas enforced on the run path (concurrent runs + daily LLM
  spend), ETag optimistic concurrency.
- **Secret vault** (`wovyr-secrets`): reference-addressed, tenant-scoped, grant-gated
  secrets with masking + rotation; sandbox secret injection (`WOVYR_SECRET_*`).
- **Envelope-encryption KMS** (`wovyr-kms`): root → tenant keys → DEKs, rotation,
  crypto-shredding `destroy`; at-rest-encrypting secret/memory/webhook stores.
- **Tamper-evident audit log** (`wovyr-audit`): hash-chained, `fsync`ed, queryable
  over `GET /api/v1/audit`.
- **Domain events + webhooks** (`wovyr-events`): HMAC-signed deliveries, topic
  matching, backoff retry; `/v1` API hardening (pagination, idempotency, request-id).
- Sandbox spectrum: container (Docker/Podman), gVisor, Firecracker microVM, WASI
  backends; egress proxy + host-side `iptables`/`nsenter` lockdown; warm pooling +
  tenant-fair scheduling.
- The `wovyr-eval` deterministic evaluation harness (FUT-006 spike) and the
  workflow-orchestrated multi-agent prototype (FUT-001(b)).

## [0.2.0] — 2026-06-28

The intelligence + durability milestone
([docs/18-roadmap/v0.2.md](docs/18-roadmap/v0.2.md)); not tagged at the time.

### Added
- **Durable workflow engine** (`wovyr-workflow`): YAML DSL → validated DAG,
  event-sourced execution with per-step checkpoints and idempotent `resume`,
  retries, saga compensation, conditional branching, human-in-the-loop suspension,
  durable wall-clock timers, cron schedules, queries/visibility, child workflows,
  definition pinning, distributed workers over leases (in-memory/file/Postgres).
- **Memory engine** (`wovyr-memory`): hybrid retrieval (vector + keyword, RRF),
  weighted ranking, MMR diversification, ABAC scope filtering, compression;
  file-backed or tiered Postgres+Qdrant store; RAG-grounded agent runs.
- **Gateway resilience** (`wovyr-provider`): retry, failover, circuit breakers
  (local or Redis-shared), exact + semantic response caching (in-memory or Qdrant),
  request hedging, cost events; chaos + p95 perf gate tests.
- **Observability** (`wovyr-telemetry`): Prometheus/OpenMetrics metrics with
  exemplars, structured logging, OTLP traces/logs/metrics (opt-in).

## [0.1.0] — 2026-06-27

The runnable foundation slice ([docs/18-roadmap/v0.1.md](docs/18-roadmap/v0.1.md)).

### Added
- Cargo workspace skeleton (ADR-0001): shared `crates/`, thin `apps/`.
- `wovyr-common`: shared `Error`/`Result` + `Usage` accounting.
- `wovyr-provider`: vendor-neutral `AIProvider` trait, deterministic offline
  `MockProvider`, OpenAI-compatible provider, streaming, model-selector `Gateway`.
- `wovyr-tools`: `Tool` trait + registry, `echo`/`fs_read`/`http_get`/`shell`
  built-ins, native process sandbox (timeout + output cap + `setrlimit`).
- `wovyr-agent`: K8s-style YAML agent manifests and the model→tool→model run loop
  with step budgets and streamed events.
- `wovyr-server`: single-node Axum server — `/healthz`, `/metrics`, agent run/stream
  (SSE) + persistence routes, error envelope.
- `wovyr` CLI: `login`/`dev`/`agents run` (local embedded or remote server).
- The `docs/` specification tree (product → architecture → per-subsystem specs →
  ADRs → roadmap) that the codebase implements spec-first.

[Unreleased]: https://github.com/punarduttrajput/wovyr/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/punarduttrajput/wovyr/releases/tag/v0.3.2
[0.3.1]: https://github.com/punarduttrajput/wovyr/releases/tag/v0.3.1
[0.3.0]: https://github.com/punarduttrajput/wovyr/releases/tag/v0.3.0
[0.2.0]: https://github.com/punarduttrajput/wovyr/tree/v0.3.0/docs/18-roadmap/v0.2.md
[0.1.0]: https://github.com/punarduttrajput/wovyr/tree/v0.3.0/docs/18-roadmap/v0.1.md
