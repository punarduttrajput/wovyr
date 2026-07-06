<!--
File: docs/18-roadmap/v1.0/phase3-scale-distribution-tickets.md
Document ID: RM-GA-P3
-->

# Phase 3 — Scale & Distribution: Implementation Tickets

**Document ID:** RM-GA-P3
**File Path:** `docs/18-roadmap/v1.0/phase3-scale-distribution-tickets.md`
**Version:** 1.0.0
**Status:** Track A ready for grooming (GA); Track B deferred to v1.1 (ADR-0010 Path A ratified 2026-07-06)
**Owner:** Engineering (Platform / Workflow)
**Last Updated:** 2026-07-06

---

# Purpose

Phase 3 of [PRD-003 §10](../../01-product/prd-ga-hardening.md) — **scale &
distribution** (workstream **WS-5**). This is the one phase whose scope **forked on
the [ADR-0010](../../17-adr/ADR-0010-ga-deployment-topology.md) decision** — **ratified
2026-07-06 as Path A** (single-node appliance for GA). The tickets are split into two
tracks accordingly:

- **Track A — the entire Phase-3 GA scope (Path A ratified).** Two tickets: a migration
  framework (because a Postgres-backed surface already ships) and an honesty correction
  to the docs/roadmap.
- **Track B — the v1.1 "Scale-Out" milestone.** Wiring the distributed machinery that
  today exists solely as tested library code onto the default path, plus multi-replica
  correctness. **Deferred out of GA by the ADR-0010 Path-A decision; scheduled as v1.1,
  gated on GA shipping. Do not start until then.**

Every Track-A ticket is also a Track-B prerequisite — no throwaway work, per ADR-0010.

**Scope boundary:** real-scale *capacity engineering* (billions of memories, thousands
of concurrent runs, the O(N) memory scan and checkpoint-growth ceilings — PP-14) is a
[PRD-003 §4.2 non-goal](../../01-product/prd-ga-hardening.md) tracked as
[GA-001](A1-scale-performance.md) / PRD-002 Scale work. Phase 3 delivers the *wiring
and correctness* that capacity engineering later builds on; it does not itself chase
throughput numbers. One bridging design ticket (SCALE-B8) captures the
encryption-defeats-pushdown decision because it is a design choice, not a tuning knob.

Ticket format matches [RM-GA-P1](phase1-security-floor-tickets.md)/
[RM-GA-P2](phase2-durability-execution-tickets.md).

---

# Sequencing at a glance

```
TRACK A (GA — both paths)
  MIG-A1 (migration framework) ─ independent; required if any Postgres backend ships
  DOC-A2 (honesty correction)  ─ independent; required at GA under Path A

TRACK B (v1.1 Scale-Out — deferred out of GA, after GA ships)
  DIST-B1 (env-select workflow Postgres) ──> DIST-B2 (queue/worker path) ──> DIST-B6 (multi-replica test)
  DIST-B3 (control-plane shared backend) ──┬─> DIST-B6
  DIST-B4 (KMS root injection-only) ───────┘
  DIST-B5 (gateway fleet resilience wiring) ─ independent
  DIST-B7 (Helm replicas>1 topology) ── depends on DIST-B3, DIST-B6
  SCALE-B8 (encryption vs. pushdown design) ─ independent; bridges to PRD-002 scale
```

**Critical path (Track B):** DIST-B1 → DIST-B2 → DIST-B6, with DIST-B3/B4 as a
parallel track that also feeds DIST-B6. All Track-B tickets depend on MIG-A1 (they
introduce/evolve Postgres schema).

---

# Track A — GA scope (required under both ADR-0010 paths)

## MIG-A1 `[P1]` — Versioned migration framework for all Postgres backends

**Problem.** All three Postgres backends run inline `CREATE TABLE IF NOT EXISTS` +
ad-hoc `ALTER TABLE ADD COLUMN IF NOT EXISTS` at connect time —
`crates/apex-workflow/src/postgres.rs:63-79`, `crates/apex-memory/src/backends.rs:78-93`,
`crates/apex-marketplace/src/postgres.rs:44`. No migration framework, no
`schema_version` table, no down-path, and every binary runs DDL on startup (requiring
DDL privileges in prod). Additive columns work; any rename/type change/index rebuild or
old-binary-vs-new-schema rollback is undefined behavior. The marketplace Postgres backend
**already ships**, so this is GA-relevant even under Path A. (PRD-003 R-5.3; closes
PP-13, and the schema portion of PP-14.)

**Change.**
- Adopt a versioned migration tool (e.g. `refinery` or `sqlx migrate` — pick one; both
  work with the workspace's existing Postgres crates). Add a `schema_version` (or
  tool-managed `_migrations`) table.
- Move each backend's DDL into numbered migration files; `connect` verifies the schema
  is at the expected version and refuses to run against an unmigrated/newer schema
  (fail-closed), rather than silently `CREATE TABLE IF NOT EXISTS`.
- Separate `apex admin migrate` (or a startup flag) from `serve`, so the serving path
  needs no DDL privilege.

**Acceptance criteria.**
- A fresh database is brought up by the migrate step; `serve` against an unmigrated DB
  fails with a clear "run migrations" error, not a partial auto-DDL.
- The capability-gated Postgres integration tests (workflow/memory/marketplace) run
  against a migrated schema in the CI service-container job (CI-901 from Phase 2).
- A version-skew test: an old binary refuses a newer schema rather than corrupting it.

**Files.** `crates/apex-workflow/src/postgres.rs`, `crates/apex-memory/src/backends.rs`,
`crates/apex-marketplace/src/postgres.rs`; migration files under each crate;
`apps/apex-cli/src/admin.rs` (migrate command). **Size.** L. **Depends on:** Phase-2
CI-901 (to actually exercise the migrated backends). **Blocks:** all Track-B tickets.

---

## DOC-A2 `[P1]` — Correct docs/roadmap/README to match the shipping binary

**Problem.** The distributed-execution machinery (workflow `PostgresStore`, `WorkQueue`,
`Worker`, leases, sharded partitions), Redis breakers, and the Qdrant semantic cache
are positioned in docs/roadmap as platform capabilities, but **no shipping binary wires
them** — `default_workflows_engine` (`crates/apex-server/src/lib.rs:496-524`) hardwires
`FileStore`, and `with_redis_breakers`/`with_qdrant_semantic_cache`
(`crates/apex-provider/src/gateway.rs:146,186`) have zero references in `apex-server` or
`apps/`. `docs/12-deployment/{kubernetes,helm}.md` describe an unbuilt multi-service
split. Shipping GA while docs claim an unwired capability is itself a defect. (PRD-003
R-5.5; closes PP-06/PP-08 honesty.)

**Change.**
- Under Path A: update the README, `docs/12-deployment/*`, and the roadmap to state
  plainly that (a) the deployment is **single-node** (`replicas: 1` is a product
  statement — see the Helm chart), and (b) the workflow-Postgres / Redis-breaker /
  Qdrant-cache code is **library-only, not wired into the shipping binary**.
- Cross-reference ADR-0010 as the decision of record. Mark the multi-service
  `kubernetes.md`/`helm.md` topology explicitly aspirational.
- If ADR-0010 ratifies Path B instead, this ticket becomes "update docs to describe the
  now-wired distributed topology" — done as part of Track B rather than standalone.

**Acceptance criteria.**
- A doc-lint / review pass confirms no doc claims a capability the shipping binary
  lacks; each such feature is labeled library-only with a pointer to its follow-on.
- The Helm chart's single-replica constraint is documented as intent, not a TODO.

**Files.** `README.md`, `docs/12-deployment/*.md`, `docs/18-roadmap/v1.0.md`,
`docs/03-workflow-engine/distributed-execution.md`. **Size.** S. **Depends on:**
ADR-0010 ratified.

---

# Track B — v1.1 "Scale-Out" (after GA)

> **Gate:** ADR-0010 ratified **Path A**, so Track B is confirmed **out of GA scope**
> and scheduled as the v1.1 "Scale-Out" milestone. Do not start until the GA milestone
> (Phases 1–2 + Phase-3 Track A) has shipped. (Reopening Track B into GA would require a
> new ADR superseding ADR-0010.)

## DIST-B1 `[P1]` — Env-select a Postgres-backed workflow store in the server

**Problem.** `default_workflows_engine` unconditionally builds a `FileStore` (or
in-memory fallback); the `APEX_WORKFLOW_POSTGRES_URL` env var is referenced only in
tests (`crates/apex-workflow/tests/postgres_store.rs`), never in a binary. The
`PostgresStore` (`crates/apex-workflow/src/postgres.rs`, `connect`/`with_partitions`)
exists and is tested but unreachable. (PRD-003 R-5.1; closes PP-06.)

**Change.**
- Mirror the marketplace pattern (`crates/apex-server/src/marketplace.rs:76`): when
  built with the `postgres` feature and `APEX_WORKFLOW_POSTGRES_URL` is set, back the
  engine with `PostgresStore`; else the file store. Forward the feature through the
  server's `Cargo.toml` (`postgres = [..., "apex-workflow/postgres"]`).
- Respect MIG-A1: connect verifies schema version, does not auto-DDL.

**Acceptance criteria.**
- With the env var set, workflow events/checkpoints persist to Postgres (verified via
  the integration job); without it, behavior is unchanged (file store).

**Files.** `crates/apex-server/src/lib.rs` (`default_workflows_engine`),
`crates/apex-server/Cargo.toml`. **Size.** M. **Depends on:** MIG-A1. **Blocks:**
DIST-B2.

---

## DIST-B2 `[P1]` — Route server-submitted workflows through the queue/worker/lease path

**Problem.** The server drives executions with a fire-and-forget
`tokio::spawn(engine.resume(...))` (`crates/apex-server/src/workflow_runner.rs:300-302`)
— no lease guards it, so two replicas would double-drive one execution. The
`WorkQueue`/`Worker` machinery (`crates/apex-workflow/src/queue.rs`, `worker.rs` —
`Worker::new`/`with_partitions`, `PostgresStore` `FOR UPDATE SKIP LOCKED` leasing)
exists but the server doesn't use it. This is what makes "exactly-once with horizontal
scaling" real. (PRD-003 R-5.1; closes PP-06.)

**Change.**
- On submit, `Engine::start` durably creates the execution and enqueues it; a pool of
  in-process `Worker`s (or a separate worker binary) leases and drives executions via
  the idempotent `resume`, with lease-expiry reclaim.
- Builds on Phase-2 EXE-602 (startup reclaim) — under Path B, startup reclaim becomes
  lease-reclaim rather than in-process re-drive.

**Acceptance criteria.**
- Two server replicas against one Postgres run each execution exactly once (no
  duplicated activity effects); a killed worker's lease expires and another reclaims and
  completes the run.

**Files.** `crates/apex-server/src/workflow_runner.rs`, `lib.rs` (worker pool in
`serve`). **Size.** L. **Depends on:** DIST-B1. **Blocks:** DIST-B6.

---

## DIST-B3 `[P1]` — Promote control-plane catalogs to a shared backend

**Problem.** Every control-plane catalog (tenancy, secrets, KMS, plugins, webhooks,
audit, agents) is file-only under `~/.apex`. Two replicas cannot share them, so
tenancy/quotas/agents diverge across replicas. (PRD-003 R-5.2; closes PP-08.)

**Change.**
- Behind the existing trait ports (`TenancyStore`, `SecretStore`, `KmsStore`,
  `WebhookStore`, `AuditSink`, and the persisted agent store from Phase-2 DUR-404),
  add Postgres-backed implementations selected by env (one `APEX_*_POSTGRES_URL` per
  catalog, or a shared connection). Reuse MIG-A1's migration framework.
- Prioritize the catalogs whose divergence is most harmful (tenancy, agents, audit).

**Acceptance criteria.**
- Two replicas against one Postgres see the same tenancy/agents/audit state; a write on
  replica A is immediately visible on replica B.

**Files.** `crates/apex-tenancy`, `apex-secrets`, `apex-kms`, `apex-events`,
`apex-audit` (Postgres impls); `crates/apex-server/src/lib.rs` (selection). **Size.**
L. **Depends on:** MIG-A1, Phase-2 DUR-404. **Blocks:** DIST-B6, DIST-B7.

---

## DIST-B4 `[P1]` — KMS root key injection-only in multi-replica mode

**Problem.** The KMS root key is generated-and-persisted per host at `~/.apex/kms/root.key`
if `APEX_KMS_ROOT_KEY` is unset (`crates/apex-kms/src/root.rs`, `default_kms` in
`crates/apex-server/src/lib.rs`). With per-pod volumes, replica 2 generates its **own**
root key and cannot decrypt replica 1's sealed data — a silent, catastrophic split.
(PRD-003 R-5.2; closes PP-08 for the crypto path.)

**Change.**
- In multi-replica mode (detected via config, e.g. `APEX_REPLICAS>1` or an explicit
  `APEX_KMS_REQUIRE_INJECTED_ROOT=1`), refuse to start without `APEX_KMS_ROOT_KEY`;
  never auto-generate. Builds on Phase-2 DR-1002 (escrow made mandatory).

**Acceptance criteria.**
- Two replicas started with the same injected `APEX_KMS_ROOT_KEY` cross-decrypt each
  other's sealed records; a replica with no injected key in multi-replica mode refuses
  to start.

**Files.** `crates/apex-kms/src/root.rs`, `crates/apex-server/src/lib.rs`. **Size.** S.
**Depends on:** Phase-2 DR-1002. **Blocks:** DIST-B6.

---

## DIST-B5 `[P2]` — Wire Redis breakers and Qdrant semantic cache into the gateway

**Problem.** `Gateway::with_redis_breakers` and `with_qdrant_semantic_cache`
(`crates/apex-provider/src/gateway.rs:146,186`) provide fleet-shared circuit-breaker
state and a shared semantic cache, but `default_gateway` never calls them. A fleet of
gateways can't trip/recover together or share cache entries. (PRD-003 R-5.4; closes
PP-06 for the gateway path.)

**Change.**
- In `default_gateway`, when `APEX_REDIS_URL` / `APEX_QDRANT_URL` (+ the relevant cargo
  features) are set, attach shared breakers and the Qdrant semantic cache; else the
  in-process defaults. KV/cache errors already fail open/degrade per the crate design.

**Acceptance criteria.**
- With Redis configured, two gateways share breaker state (one tripping is visible to
  the other); with Qdrant configured, a cache entry written by one is served to the
  other.

**Files.** `crates/apex-server/src/lib.rs` (`default_gateway`), server `Cargo.toml`
feature forwarding. **Size.** M. **Depends on:** none (independent Track-B item).

---

## DIST-B6 `[P1]` — Multi-replica correctness test suite

**Problem.** Nothing proves ≥2 replicas behave correctly against shared state — the
core claim Path B exists to deliver. (PRD-003 R-5.1/R-5.2 verification; closes PP-08.)

**Change.**
- An integration test (docker-compose or a two-process harness) runs two `apex`
  replicas against one Postgres (+ Redis/Qdrant), and asserts: shared tenancy/agents/
  audit, exactly-once workflow execution under concurrent submit, replica-crash
  mid-workflow reclaim-and-complete, and cross-replica KMS decryption.

**Acceptance criteria.**
- The suite passes in CI's service-container job; killing one replica mid-workflow does
  not lose or duplicate work.

**Files.** `crates/apex-server/tests/multi_replica.rs` (new); CI job. **Size.** L.
**Depends on:** DIST-B2, DIST-B3, DIST-B4.

---

## DIST-B7 `[P2]` — Helm chart multi-replica topology

**Problem.** The Helm chart pins `replicas: 1`
(`deployment/helm/apex/templates/apex-statefulset.yaml`) because durable state is
local files. Once DIST-B3 makes state shared, the chart can scale out. (PRD-003 R-5.2
deployment; closes PP-08 deployment.)

**Change.**
- Add a `Deployment` (or scalable `StatefulSet`) topology gated on shared-backend
  config: `replicas > 1` requires the Postgres/Redis/Qdrant URLs and injected KMS root
  (DIST-B4). Validate offline with `helm lint`/`kubeconform` (the existing chart's
  precedent).

**Acceptance criteria.**
- `helm template` with a multi-replica values file renders a valid, schema-checked
  manifest that wires the shared backends and injected KMS root; single-replica remains
  the default.

**Files.** `deployment/helm/apex/`. **Size.** M. **Depends on:** DIST-B3, DIST-B6.

---

## SCALE-B8 `[P2]` — Design: preserve retrieval pushdown under memory encryption

**Problem.** The server always wraps memory in `EncryptingMemoryStore`, which
**unconditionally reports `supports_pushdown() = false`** (documented design), so even a
Qdrant-backed tiered store degrades to a full `all()` scan + in-process decrypt +
O(N) cosine (`crates/apex-memory/src/engine.rs:203-205`). This caps practical retrieval
at ~10³–10⁴ records/namespace — a design choice (correct-but-slow) that becomes the
binding constraint the moment PRD-002's real-scale work starts. This ticket **decides
the design**, it does not chase throughput. (Bridges PP-08/PP-14 to
[GA-001](A1-scale-performance.md)/PRD-002.)

**Change.**
- Evaluate options and record a decision (short ADR): encrypt `content` but leave
  `embedding`s plaintext so the index can score them (with a documented threat-model
  note), vs. a searchable-encryption scheme, vs. accepting the scan ceiling for the
  single-node appliance. Whichever is chosen, the O(N) `put` seq re-read
  (`crates/apex-memory/src/store.rs`) is fixed with a per-namespace counter.

**Acceptance criteria.**
- A decision ADR exists with the threat-model trade-off stated; if "embeddings
  plaintext" is chosen, pushdown survives encryption and a perf test shows sub-linear
  retrieval on a Qdrant-backed encrypted namespace.

**Files.** new ADR under `docs/17-adr/`; `crates/apex-memory/src/encrypting_store.rs`,
`engine.rs`, `store.rs`. **Size.** M (design) + follow-on. **Depends on:** none.
*(Hand-off point to PRD-002 Scale / GA-001.)*

---

# Rollup

| Ticket | Track | Title | Size | Priority | Depends on |
|--------|-------|-------|------|----------|------------|
| MIG-A1 | A (GA) | Postgres migration framework | L | P1 | Phase-2 CI-901 |
| DOC-A2 | A (GA) | Docs/roadmap honesty correction | S | P1 | ADR-0010 |
| DIST-B1 | B (v1.1) | Env-select workflow Postgres store | M | P1 | MIG-A1 |
| DIST-B2 | B (v1.1) | Queue/worker/lease execution path | L | P1 | DIST-B1 |
| DIST-B3 | B (v1.1) | Control-plane shared backend | L | P1 | MIG-A1, DUR-404 |
| DIST-B4 | B (v1.1) | KMS root injection-only (multi-replica) | S | P1 | DR-1002 |
| DIST-B5 | B (v1.1) | Redis breakers + Qdrant cache wiring | M | P2 | — |
| DIST-B6 | B (v1.1) | Multi-replica correctness tests | L | P1 | B2, B3, B4 |
| DIST-B7 | B (v1.1) | Helm multi-replica topology | M | P2 | B3, B6 |
| SCALE-B8 | B (bridge) | Encryption-vs-pushdown design ADR | M | P2 | — |

**Track A total (GA):** 1 L + 1 S ≈ 1.5–2 engineer-weeks. **This is all Phase 3 needs
for a Path-A GA.**
**Track B total (v1.1):** 4 L + 2 M + 1 S + SCALE-B8 ≈ 10–13 engineer-weeks,
parallelizable to ~5–6 calendar weeks (workflow track vs. control-plane track are
independent until DIST-B6).

**Phase-3 exit (GA / Path A)** = PRD-003 §11 item 4: no doc or roadmap claims a
capability the shipping binary lacks, and any shipped Postgres surface has real
migrations. **Phase-3 exit (v1.1 / Path B)** = two replicas share state and survive a
replica crash mid-workflow with exactly-once effects.

---

# Related

- [PRD-003](../../01-product/prd-ga-hardening.md) — parent PRD (WS-5, §5 the Path fork, §10 phasing)
- [ADR-0010](../../17-adr/ADR-0010-ga-deployment-topology.md) — the Path A/B decision that gates Track B
- [RM-GA-P1](phase1-security-floor-tickets.md) · [RM-GA-P2](phase2-durability-execution-tickets.md) — earlier phases
- [`03-workflow-engine/distributed-execution.md`](../../03-workflow-engine/distributed-execution.md) — scaling envelope + leases/partitions
- [GA-001](A1-scale-performance.md) — real-scale capacity engineering (PRD-002 Scale; the hand-off from SCALE-B8)

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-06 | ADR-0010 ratified **Path A**: Track A confirmed as the entire Phase-3 GA scope; Track B confirmed deferred to the v1.1 "Scale-Out" milestone (status/gate wording updated accordingly) |
| 1.0.0 | 2026-07-06 | Initial Phase-3 (scale & distribution) ticket breakdown: 2 Track-A (GA, both paths) + 8 Track-B (v1.1 Scale-Out, Path B only) tickets, gated on the ADR-0010 decision, with the real-scale-capacity boundary handed off to PRD-002/GA-001 |
