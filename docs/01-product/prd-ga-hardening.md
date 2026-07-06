<!--
File: docs/01-product/prd-ga-hardening.md
Document ID: PRD-003
-->

# PRD: GA Hardening — Closing the Deployed-vs-Designed Gap

**Document ID:** PRD-003
**File Path:** `docs/01-product/prd-ga-hardening.md`
**Version:** 1.0.0
**Status:** Draft — planning input, not a commitment
**Owner:** Product Team
**Last Updated:** 2026-07-06

---

# 1. Purpose

[PRD-001](prd.md) defines the product through GA; [PRD-002](prd-future.md) scopes
what comes after. This PRD sits between them: it turns the findings of the
2026-07-06 solution-architecture review into committed, testable requirements
that must close **before** Apex can defensibly call itself GA.

The review examined four dimensions — architecture/coupling, security &
multi-tenancy, state/persistence/scale, and server/API/operability — and surfaced
21 pain points. This document is their remediation plan: each finding maps to one
or more numbered requirements with an owner workstream, exit criteria, and a
traceability entry (§13).

**This is a planning input, not a promise.** Requirements graduate to committed
work through the roadmap ([`18-roadmap/v1.0.md`](../18-roadmap/v1.0.md)) and, where
they change a boundary contract, through an [ADR](../17-adr/index.md).

---

# 2. Problem Statement

The review's one-sentence verdict:

> **Apex is a well-designed single-node appliance wearing the marketing of a
> distributed multi-tenant platform.**

The primitives are strong and cleanly abstracted — KMS envelope encryption,
hash-chained audit, trait-port storage, an RBAC/ABAC model, a sandbox spectrum,
durable workflow execution. But three structural gaps make the current surface
undefensible as GA:

1. **The security model has no floor.** Authentication does not exist; identity is
   a spoofable HTTP header. The RBAC, tenant-isolation, encryption, and audit
   machinery are all real, but they rest on an unauthenticated foundation and are
   not wired as the *enforced default* on the runtime path.
2. **"Distributed" is aspirational.** The Postgres/queue/lease/partition machinery
   exists as tested library code that **no shipping binary reaches**. As deployed,
   Apex is single-process by construction: two server replicas cannot coexist, and
   the server never drives workflows forward in the background.
3. **Durability is weaker than advertised.** No `fsync` anywhere, in-place rewrites
   of security-critical files (including the KMS root key), no cross-process
   locking despite the CLI and server sharing `~/.apex` by design, restart amnesia
   on in-memory stores, and no backup/restore tooling.

The danger is compounded by momentum: the Python SDK is already published to PyPI,
so every API-contract inconsistency shipped today converts into a *permanent
breaking-change cost* tomorrow.

The encouraging counterweight: because the abstractions are correct, most fixes
are **"wire the good primitive onto the default path,"** not "redesign."

---

# 3. Baseline (as of 2026-07-06)

- **Shipped (v0.1–v0.3, tagged):** agent runtime, workflow engine, memory engine,
  LLM gateway, tool runtime, plugin engine + marketplace, multi-tenancy,
  events/webhooks, audit, secrets, KMS.
- **In progress (v1.0):** DX/SDK, Security/KMS, Reliability — see
  [`v1.0.md`](../18-roadmap/v1.0.md).
- **The gap this PRD closes:** the difference between the primitives that exist and
  the enforced, deployed, crash-safe, authenticated, horizontally-scalable
  behavior GA requires.

---

# 4. Goals & Non-Goals

## 4.1 Goals

- Establish a **security floor**: no unauthenticated access to any mutating,
  secret, KMS, plugin, or admin surface.
- Make the **default deployment safe**: safe-by-default tool sandboxing, at-rest
  encryption, and transport security without opt-in flags.
- Make **durability real**: crash-safe writes, cross-process safety, no restart
  amnesia, and a backup/restore path.
- **Resolve the strategic fork** (§5) and deliver the storage/execution work its
  chosen path requires.
- **Freeze a consistent, contract-tested API** before the published SDKs acquire
  users.
- **Restore honesty** between docs/roadmap claims and shipped behavior.

## 4.2 Non-Goals

- Billions-of-memories / thousands-of-concurrent-runs capacity engineering at real
  scale (tracked in PRD-002 as post-GA Scale work).
- Multi-region residency/resilience (PRD-002).
- Marketplace monetization (PRD-002).
- A cloud-KMS/HSM-backed root key (may be deferred to a fast-follow; §5 Path A can
  ship with the local root escrowed).

---

# 5. Strategic Decision: Which Product Are We Shipping?

The review identified that the deployed system and the marketed system diverge,
and offered two honest paths. **This decision gates the scope of §8 (WS-5, WS-6)
and must be made before those workstreams are estimated.**

| | **Path A — Single-Node Appliance** | **Path B — Distributed Platform** |
|---|---|---|
| **Positioning** | Single-tenant or single-node deployment; honest about replica=1 | Multi-tenant, horizontally scalable, HA |
| **Scope** | WS-1..4, WS-7..10 (drop distributed claims) | Path A **plus** WS-5, WS-6 in full |
| **Effort** | Weeks | Months |
| **Risk** | Low; defensible quickly | High; real capacity engineering |
| **What we drop/keep** | Mark workflow-Postgres / Redis-breaker / Qdrant-cache as library-only in docs | Wire all of them onto the default path |

**Recommendation:** Adopt **Path A for the GA milestone** and schedule Path B as the
immediately-following major milestone. Path A is defensible in weeks, removes the
dishonest distributed claims, and every Path-A fix is also a prerequisite for Path
B — no throwaway work. This PRD is written so that **WS-5 and WS-6 are the only
workstreams whose scope depends on the decision**; all others are required
regardless.

> **DECIDED (2026-07-06): Path A.** Ratified in
> [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md) (Status: Accepted). GA ships
> as a single-node appliance; the distributed platform (Path B) is the v1.1 "Scale-Out"
> milestone, gated on GA shipping. This locks WS-5 scope: only its Track-A subset
> (migration framework + docs-honesty correction) is GA scope; the distributed-wiring
> tickets move to v1.1 — see [Phase 3](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md).

---

# 6. Requirement Conventions

- Requirements are `R-<workstream>.<n>`, testable, and each cite the pain-point IDs
  (`PP-##`, §13) they close.
- Priority: **P0** = GA blocker, **P1** = GA-gating, **P2** = required for GA
  quality bar, **P3** = fast-follow acceptable.
- "Done" for every requirement means: implemented, covered by an automated test
  that would fail on regression, and the linked `docs/` spec updated.

---

# 7. Workstreams — Security Floor (P0)

## WS-1: Authentication & Authorization

**Problem.** Identity is an unverified header; anonymous callers reach admin and
crypto-shred surfaces; plugin/marketplace mutation routes have no authorization at
all. (Closes **PP-01, PP-02, PP-03**.)

**Requirements.**

- **R-1.1 (P0)** — Every request's principal and tenant MUST be derived from a
  *verified* credential (signed JWT/OIDC, mTLS client cert, or an API key hashed in
  a store), never from an unauthenticated `X-Apex-Principal`/`X-Apex-Tenant` header.
  Header-asserted identity is rejected unless it carries a valid signature/token.
- **R-1.2 (P0)** — The anonymous-default-tenant bypass in `tenant_authorize`
  (`crates/apex-server/src/tenancy.rs:542-553`) MUST be removed from all mutating,
  secret, KMS, and audit routes. If retained for local dev, it MUST be behind an
  explicit `APEX_ALLOW_ANONYMOUS` flag that binds only to loopback and is refused on
  any non-loopback listener.
- **R-1.3 (P0)** — Plugin lifecycle routes (`install/enable/upgrade/trust/
  uninstall`) and marketplace moderation routes MUST call `tenant_authorize` with
  platform-admin scopes (`plugins:admin`, `marketplace:moderate`), matching the
  gating pattern already used by `kms.rs` and `secrets.rs`.
- **R-1.4 (P1)** — A negative-authorization test suite MUST assert that every
  mutating route returns `401`/`403` for an unauthenticated or under-scoped caller;
  this suite runs in CI.

**Exit criteria.** No route mutates state, reveals a secret, or performs a KMS
operation for an unauthenticated caller. `APEX_PLATFORM_ADMINS` membership is
meaningful only against a verified principal.

## WS-2: Web-Facing Transport & Resource Hardening

**Problem.** Plaintext HTTP only; no TLS, CORS, rate limiting, body-size limit,
request timeout, or concurrency cap; unbounded idempotency cache. (Closes
**PP-05**, and the idempotency-cache portion of **PP-07**.)

**Requirements.**

- **R-2.1 (P0)** — The server MUST support TLS termination (rustls) OR MUST refuse
  to bind a non-loopback address without a documented fronting-proxy declaration
  (`APEX_TLS_TERMINATED_UPSTREAM`).
- **R-2.2 (P0)** — Add `tower_http` layers for request timeout, `DefaultBodyLimit`,
  and a global concurrency limit, configurable via env/config.
- **R-2.3 (P1)** — Add a rate-limit layer (per-principal and per-IP) to blunt the
  cost-amplification and destroy-key DoS vectors.
- **R-2.4 (P1)** — Add a CORS layer with a configurable allow-list (default: same
  origin only) — see also WS-8/R-8.5 for the dashboard.
- **R-2.5 (P1)** — The idempotency store MUST be bounded with TTL-based eviction
  (`crates/apex-server/src/hardening.rs:88-94`), and persisted per WS-4/R-4.4.

**Exit criteria.** A default `apex` server exposed to a network enforces TLS,
bounded request size/time/concurrency, and rate limits; memory cannot be grown
without bound by fresh idempotency keys.

## WS-3: Safe-by-Default Tool Sandboxing

**Problem.** `shell` and `fs_read` register by default and run unsandboxed with full
host reach; permission grants default to unrestricted; `http_get` has no SSRF
protection; TrustClass floors are never enforced on the run path. (Closes
**PP-04**.)

**Requirements.**

- **R-3.1 (P0)** — In a server/hosted context, `with_builtins()` MUST NOT register
  `shell` by default; enabling it requires explicit operator opt-in.
- **R-3.2 (P0)** — `fs_read` MUST be confined to an allow-listed workspace root; the
  KMS root key, secrets file, and other `~/.apex` state MUST be unreachable via any
  builtin tool.
- **R-3.3 (P0)** — Permission grants MUST default to **deny** (empty grant set),
  requiring explicit per-tool opt-in in the agent manifest
  (`crates/apex-agent/src/runtime.rs:288`); `None` MUST no longer mean unrestricted
  in a hosted context.
- **R-3.4 (P1)** — `http_get` MUST enforce a per-tenant egress allow-list and block
  link-local/loopback/private ranges (incl. `169.254.169.254`) directly in the
  tool, independent of the container backend.
- **R-3.5 (P1)** — The agent run path MUST map manifest/plugin provenance to a real
  `TrustClass` and drive `select_backend` from it, so untrusted work cannot select
  the native backend.

**Exit criteria.** A default hosted agent run cannot read arbitrary host files,
execute arbitrary shell, reach cloud-metadata/internal endpoints, or run untrusted
code on the native sandbox.

---

# 8. Workstreams — Durability, Scale & Execution

## WS-4: Durable State & Crash-Safety (P0/P1)

**Problem.** No `fsync`; in-place file rewrites risk torn writes (incl. KMS root);
no cross-process locking though CLI+server share `~/.apex`; restart amnesia on
agents, workflow-owner index, idempotency keys, and quota windows. (Closes **PP-09,
PP-10, PP-11**, and the state portion of **PP-07**.)

**Requirements.**

- **R-4.1 (P0)** — A single shared `atomic_write(path, bytes)` utility (temp file +
  `fsync` + atomic rename + parent-dir fsync on Unix) MUST replace every direct
  `std::fs::write` in the 10 file stores. The KMS root/catalog write is the
  highest-priority instance (torn write = accidental crypto-shred).
- **R-4.2 (P0)** — Append paths for the workflow event log and audit chain MUST
  `sync_data` after append; checkpoint rename MUST fsync.
- **R-4.3 (P1)** — Cross-process safety: either advisory file locking (e.g.
  `fs2::FileExt::lock_exclusive`) per store directory, OR the CLI routes all
  mutations through the server API. The audit hash-chain tip MUST be re-read under
  the lock to prevent forked chains.
- **R-4.4 (P1)** — Eliminate restart amnesia: persist the `AgentStore` and the
  `workflow_owners` index beside the workflow store; move idempotency keys and quota
  accumulators to a durable store with TTLs. After restart, a tenant MUST still see
  (only) its own durable executions — closing the visibility-inversion regression.
- **R-4.5 (P2)** — Signal/approve MUST NOT require re-uploading the full manifest
  YAML; the pinned definition persists with the execution and is resolved by id.

**Exit criteria.** A power loss or crash mid-write never corrupts a store or loses
an acknowledged event; the CLI and server can safely operate on shared `~/.apex`;
no durable-adjacent state evaporates on restart.

## WS-5: Distributed Backend Promotion (P1 — **Path A ratified: only R-5.3 + R-5.5 are GA scope; the rest is v1.1**)

**Problem.** The Postgres workflow store, WorkQueue, Worker, leases, and sharded
partitions — plus Redis breakers and the Qdrant semantic cache — exist only as
library code; no binary wires them. Two replicas cannot coexist. (Closes **PP-06,
PP-08**, and the migration/scale items **PP-13, PP-14** where they touch Postgres.)

**Requirements.**

- **R-5.1 (P1, Path B)** — The server MUST select a Postgres-backed workflow store
  via env (mirroring `APEX_MARKETPLACE_POSTGRES_URL`), and route server-submitted
  workflows through the queue/worker/lease path for exactly-once, crash-recoverable
  execution.
- **R-5.2 (P1, Path B)** — Control-plane catalogs (tenancy, secrets, KMS, plugins,
  webhooks, audit) MUST support a shared backend so ≥2 replicas share one source of
  truth; the KMS root key MUST be injection-only (`APEX_KMS_ROOT_KEY`) in
  multi-replica mode (no per-pod generated key).
- **R-5.3 (P1)** — All Postgres backends MUST adopt a versioned migration framework
  (e.g. `refinery`/`sqlx migrate`) with a `schema_version` table; DDL is a separate
  step from `serve`, not run on every startup. *(Required for both paths if any
  Postgres backend ships.)*
- **R-5.4 (P2, Path B)** — Wire env-selected Redis breakers and the Qdrant semantic
  cache into the server gateway, or explicitly document them as library-only.
- **R-5.5 (P1, Path A)** — If Path A is chosen: docs and roadmap MUST be corrected
  to state the workflow-Postgres/Redis/Qdrant-fleet features are library-only and
  the deployment is single-node (replica=1 is a product statement, not a temporary
  limitation).

**Exit criteria (Path B).** Two server replicas share state, survive a replica
crash mid-workflow (another reclaims the lease and resumes), and schema evolves via
migrations. **Exit criteria (Path A).** No doc or roadmap claims a capability the
shipping binary lacks.

## WS-6: Server-Side Execution Driver (P0)

**Problem.** Workflows progress only via fire-and-forget `tokio::spawn`; no
`TimerDispatcher`/`ScheduleDispatcher` loop and no startup lease-reclaim; durable
timers/schedules fire only when a human runs the CLI `workflows tick` on the box; a
restart strands executions in `Running` forever. (Closes **PP-07** [execution
driver], **PP-15**.)

**Requirements.**

- **R-6.1 (P0)** — `serve()` MUST run background `TimerDispatcher::poll` and
  `ScheduleDispatcher::poll` loops so durable timers and schedules fire without CLI
  intervention. A `wait: {timer:}` workflow submitted over HTTP MUST resume on its
  own.
- **R-6.2 (P0)** — On startup, the server MUST rescan the workflow store and resume
  (or, Path B, re-lease) executions left in `Running`, so a restart does not strand
  in-flight work.
- **R-6.3 (P0)** — `DELETE /api/v1/workflows/{id}` MUST either implement
  `Engine::cancel` (write a `WorkflowCancelled` event, skip pending activities and
  transition terminally) or return `501 not_implemented` — it MUST NOT return `202`
  for a no-op. *(Closes **PP-15**.)*
- **R-6.4 (P1)** — `agents:run` MUST offer an asynchronous submit→poll resource for
  long-running work (mirroring workflow submit), so a wedged upstream cannot hold an
  HTTP connection and a run permit indefinitely.

**Exit criteria.** Timers, schedules, and crashed-run recovery work with no
human/CLI intervention on the server host; no API endpoint acknowledges an action
it does not perform.

---

# 9. Workstreams — Contract, Operability & Codebase Health

## WS-7: API Contract Stabilization (P1 — urgency rises with each SDK user)

**Problem.** Six route groups use ad-hoc list envelopes vs. the standard paginated
one; three serde casing idioms coexist; idempotency covers one route; OpenAPI is
hand-synced with no CI contract test; deprecation policy is prose with no
enforcement. (Closes **PP-16, PP-18**.)

**Requirements.**

- **R-7.1 (P1)** — All list endpoints MUST use the standard
  `{data, has_more, next_cursor, total_estimate}` envelope; the ad-hoc
  `{plugins,total}`-style envelopes are migrated in one pre-GA breaking pass.
- **R-7.2 (P1)** — One serde casing policy (`#[serde(rename_all = "snake_case")]`)
  MUST apply to all wire enums, eliminating the PascalCase-status-vs-lowercase-filter
  wart and the mixed `Debug`/hand-written-string idioms.
- **R-7.3 (P1)** — `Idempotency-Key` MUST be honored on all mutating routes, not
  just `agents:run`.
- **R-7.4 (P1)** — CI MUST boot `apex dev` and run the TypeScript + Python SDK
  integration suites and `redocly lint` as a contract gate on every PR.
- **R-7.5 (P2)** — The 90-day deprecation policy MUST be mechanically enforceable:
  `Deprecation`/`Sunset` headers emitted from a route-metadata table.

**Exit criteria.** The published SDKs are written against a stable, consistent,
contract-tested surface; a handler change that diverges from `openapi.yaml` fails
CI.

## WS-8: Observability & Operability (P2)

**Problem.** RED metrics cover 2 of ~50 routes despite the "per route" claim; no
shipped alert rules or dashboards; request id never reaches logs/traces/audit; the
dashboard has hardcoded identity, needs a dev proxy (no CORS), and ships nowhere.
(Closes **PP-17** [executor CI note lives in WS-9], **PP-19**, and the dashboard
part of **PP-11-server**.)

**Requirements.**

- **R-8.1 (P1)** — Replace per-handler metric calls with one metrics middleware
  layer (route template + status labels) so all routes emit RED metrics.
- **R-8.2 (P2)** — Record the `X-Request-Id` onto the handler `tracing` span and
  audit events (`AuditEvent.request_id`, which exists but is never set), so
  client-reported ids correlate to server logs/traces.
- **R-8.3 (P2)** — Ship a starter Prometheus alert-rule file and Grafana dashboard
  JSON under `deployment/`.
- **R-8.4 (P2)** — Extend audit coverage (`audit::record`) to every state-changing
  handler — agent runs, plugin lifecycle, tenancy mutations, marketplace
  publish/moderation, webhook subscription changes — not just secrets + KMS.
  *(Closes **PP-audit** / the review's audit-coverage finding.)*
- **R-8.5 (P2)** — The dashboard MUST replace compile-time `TENANT`/`PRINCIPAL`
  constants with a real login/session flow (tied to WS-1), work cross-origin via the
  WS-2 CORS layer, and be built into a deployment artifact (Docker stage).

**Exit criteria.** An on-call operator can page on route error/latency, correlate a
client request id end-to-end through logs/traces/audit, and every privileged
mutation is in the tamper-evident log.

## WS-9: Codebase Health & Test Coverage (P1/P2)

**Problem.** CI tests one point in a ~2⁹ feature matrix (feature-gated code never
linted, integration tests silently skip, a latent CLI panic hides); three
diverging `ActivityExecutor` impls; god modules; duplicated `~/.apex` bootstrap;
dependency hygiene; a vendor-coupling boundary leak. (Closes **PP-17, PP-16-exec,
PP-20, PP-21**, and the architecture-review hygiene items.)

**Requirements.**

- **R-9.1 (P1)** — CI MUST add `cargo hack check --each-feature` (so feature-gated
  code is linted) and at least one service-container integration job
  (Postgres/Qdrant/Redis) so capability-gated tests run. This is highest-leverage:
  it gates discovery of the other findings.
- **R-9.2 (P1)** — The three `ActivityExecutor` implementations (CLI
  `PlatformExecutor`, server `ServerExecutor`, `EvalWorkflowExecutor`) MUST be
  unified into one `PlatformActivityExecutor` parameterized over an `AgentResolver`
  trait, eliminating the retryable-vs-permanent and model-resolution divergences that
  make identical YAML behave differently locally vs. on the server.
- **R-9.3 (P1)** — Fix the latent CLI marketplace `spawn_blocking` panic
  (`apps/apex-cli/src/plugin.rs:654`) — the same bug the server already fixed —
  before shipping the `postgres` feature.
- **R-9.4 (P2)** — Extract an `apex-host`/`apex-config` crate owning `~/.apex`
  layout, env-var reading, and backend selection; both binaries consume it (removes
  the "agree by prose" risk).
- **R-9.5 (P2)** — Route model-invoking builtins (`image_generate`) through the
  `Gateway` and secrets vault instead of calling OpenAI directly; move shared
  external deps into `[workspace.dependencies]`; add `cargo-deny`; split the god
  modules (`lib.rs` 2,745 LOC, `sandbox.rs` 1,890 LOC) along the existing
  module-per-route/backend pattern.

**Exit criteria.** CI exercises the feature matrix and the fleet backends; one
executor drives all workflow paths identically; no known latent panic ships; the
shared-state bootstrap is code, not convention.

## WS-10: Backup, Restore & Disaster Recovery (P1)

**Problem.** No backup/restore tooling exists anywhere; a consistent snapshot of
`~/.apex` isn't even possible today (unlocked writers + in-place rewrites); losing
`~/.apex/kms` = permanent loss of all sealed data. (Closes **PP-12**.)

**Requirements.**

- **R-10.1 (P1)** — An `apex admin backup` command MUST quiesce writers (via the
  WS-4 locks), snapshot `~/.apex` atomically, and document `pg_dump` for the Postgres
  backends.
- **R-10.2 (P1)** — KMS root-key escrow MUST be a documented, mandatory install step;
  restore MUST be tested (backup → wipe → restore → decrypt a previously-sealed
  record).
- **R-10.3 (P2)** — Define and document RPO/RTO targets (currently
  "undefined/never") and validate them, per [v1.0 exit criteria](../18-roadmap/v1.0.md#5-exit-criteria).

**Exit criteria.** A documented, tested backup/restore path exists; RPO/RTO are
defined and met; loss of a single host is recoverable.

---

# 10. Prioritization & Sequencing

Phases are ordered by dependency, not calendar (dates per PRD-002 convention are
omitted). Within a phase, items are parallelizable.

- **Phase 0 — Decide (blocks §8 scope).** Ratify §5 Path A vs Path B via ADR.
- **Phase 1 — Security floor (P0).** WS-1, WS-2, WS-3. Nothing else ships to a
  network first. WS-1/R-1.1 is the single highest-leverage item — every other
  security fix is meaningless without it.
- **Phase 2 — Durability & execution (P0/P1).** WS-4 (crash-safety), WS-6 (execution
  driver + honest cancel), WS-10 (backup). WS-9/R-9.1 (CI matrix) runs alongside as
  it gates discovery of regressions in all other work.
- **Phase 3 — Scale/distribution (Path-dependent).** WS-5 in full (Path B) or the
  docs-correction subset (Path A). WS-5/R-5.3 (migrations) required either way if
  Postgres ships.
- **Phase 4 — Contract & operability (P1/P2).** WS-7 (freeze the API before SDK
  users accrete), WS-8 (observability, audit coverage, dashboard), WS-9 remainder.

**The trap to avoid:** shipping the API surface (and letting SDK adoption grow)
before WS-7 freezes it. Contract debt becomes permanent breaking-change debt the
day the first external consumer depends on the current shapes.

---

# 11. GA Exit Criteria (supersedes prose in v1.0 §5 for these dimensions)

GA is defensible when:

1. No unauthenticated access to any mutating/secret/KMS/plugin/admin route
   (WS-1); default deployment enforces TLS + resource limits (WS-2); default tools
   are safe (WS-3).
2. Crash/power-loss never corrupts state or loses acknowledged events (WS-4); a
   tested backup/restore path exists (WS-10).
3. The server drives its own timers/schedules and recovers crashed runs; no API
   lies about what it does (WS-6).
4. Docs/roadmap claim only what the shipping binary does (WS-5/R-5.5 for Path A, or
   WS-5 full for Path B).
5. The API surface is consistent and contract-tested in CI (WS-7); privileged
   mutations are audited and observable (WS-8).
6. CI exercises the feature matrix and fleet backends; the workflow executor is
   unified; no known latent panic ships (WS-9).
7. An external pen test passes against the hardened surface
   ([v1.0 §5](../18-roadmap/v1.0.md#5-exit-criteria)).

---

# 12. Risks & Assumptions

- **R:** WS-1 (real auth) touches every route and the dashboard — the largest blast
  radius. **Mitigation:** land the verification layer + negative-auth test suite
  first (R-1.4), migrate route-by-route behind it.
- **R:** Path B is scoped as "months" and may pressure teams to fake-complete it.
  **Mitigation:** §5 makes Path A a legitimate, shippable GA; distributed is a
  named follow-on, not a corner to cut.
- **R:** WS-7's breaking API pass affects the already-published PyPI SDK.
  **Mitigation:** do it *before* wider adoption; version the SDK in lockstep; use
  the deprecation policy (R-7.5) for anything that must soft-land.
- **A:** The trait-port architecture holds (review confirmed the spine is acyclic and
  one-directional), so backend promotion (WS-5) is wiring, not redesign.
- **A:** Single-node Path A is an acceptable GA positioning for the initial customer
  set — **must be validated with Product before Phase 0 closes.**

---

# 13. Traceability Matrix — Findings → Requirements

| PP | Review finding (abbrev.) | Tier | Requirement(s) |
|----|--------------------------|------|----------------|
| PP-01 | No authentication; identity is a spoofable header | 1 | R-1.1 |
| PP-02 | Anonymous-default-tenant bypass reaches crypto-shred | 1 | R-1.2 |
| PP-03 | Plugin/marketplace admin routes have zero authz | 1 | R-1.3 |
| PP-04 | Default tools unsandboxed; grants default unrestricted; SSRF | 1 | R-3.1..R-3.5 |
| PP-05 | No TLS/CORS/rate-limit/body-limit/timeout | 1 | R-2.1..R-2.4 |
| PP-06 | Distributed layer unreachable from binaries | 2 | R-5.1, R-5.4, R-5.5 |
| PP-07 | Server never drives workflows; restart amnesia; unbounded idempotency | 2 | R-6.1, R-6.2, R-4.4, R-2.5 |
| PP-08 | Two replicas cannot coexist | 2 | R-5.2 |
| PP-09 | File stores not crash-safe (no temp+rename); KMS torn-write | 3 | R-4.1 |
| PP-10 | Zero fsync anywhere | 3 | R-4.2 |
| PP-11 | No cross-process locking; CLI+server share `~/.apex` | 3 | R-4.3 |
| PP-12 | No backup/restore or DR tooling | 3 | R-10.1..R-10.3 |
| PP-13 | No Postgres migration story | 3 | R-5.3 |
| PP-14 | O(N)/O(N²) scale ceilings (memory scan, checkpoint growth, listing docs) | 3 | R-5.3 (schema); PRD-002 (real-scale) |
| PP-15 | `DELETE /workflows/{id}` is a no-op returning 202 | 4 | R-6.3 |
| PP-16 | Three diverging ActivityExecutor impls; API contract inconsistency | 4 | R-9.2, R-7.1..R-7.3 |
| PP-17 | CI tests one point in ~2⁹ feature matrix; latent CLI panic | 4 | R-9.1, R-9.3 |
| PP-18 | Hand-synced OpenAPI, no CI contract test, prose-only deprecation | 4 | R-7.4, R-7.5 |
| PP-19 | RED metrics on 2/50 routes; no alerts/dashboards; no request-id correlation | 4 | R-8.1..R-8.3 |
| PP-20 | God modules; duplicated `~/.apex` bootstrap; dep hygiene | 4 | R-9.4, R-9.5 |
| PP-21 | Boundary leak: builtin calls OpenAI directly | 4 | R-9.5 |
| PP-audit | Audit coverage limited to secrets + KMS | 1/4 | R-8.4 |

---

# 14. Related

- [PRD-001](prd.md) · [PRD-002](prd-future.md)
- [`18-roadmap/v1.0.md`](../18-roadmap/v1.0.md) — GA milestone (exit criteria this PRD sharpens)
- [`13-security/index.md`](../13-security/index.md) · [`13-security/encryption.md`](../13-security/encryption.md)
- [`12-deployment/index.md`](../12-deployment/index.md) · [`14-observability/index.md`](../14-observability/index.md)
- [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md) — the Path A/B decision (Accepted — Path A, 2026-07-06)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-06 | Initial GA-hardening PRD: 21 solution-architecture review findings mapped to 10 workstreams / testable requirements, with the Path A/B strategic decision, phased sequencing, sharpened GA exit criteria, and a findings→requirements traceability matrix |
