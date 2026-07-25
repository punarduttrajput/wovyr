<!--
File: docs/18-roadmap/v1.0/phase2-durability-execution-tickets.md
Document ID: RM-GA-P2
-->

# Phase 2 — Durability & Execution: Implementation Tickets

**Document ID:** RM-GA-P2
**File Path:** `docs/18-roadmap/v1.0/phase2-durability-execution-tickets.md`
**Version:** 1.1.0
**Status:** Done — all 12 tickets shipped (CI-901, DUR-401/402/403/404/405,
EXE-601/602/603/604, DR-1001/1002/1003). See `CLAUDE.md` for the implemented
behavior each ticket produced and the tests proving it.
**Owner:** Engineering (Platform / Workflow)
**Last Updated:** 2026-07-07

---

# Purpose

Phase 2 of [PRD-003 §10](../../01-product/prd-ga-hardening.md) — **durability &
execution** — broken into implementation tickets. Phase 2 makes durability *real*
(crash-safe writes, no restart amnesia) and makes the server actually drive its own
workflows, timers, and schedules. It runs after Phase 1 (the security floor,
[RM-GA-P1](phase1-security-floor-tickets.md)) but is largely independent of it.

Covers workstreams **WS-4** (durable state & crash-safety), **WS-6** (server-side
execution driver), **WS-10** (backup/restore/DR), and **WS-9/R-9.1** (the CI feature
matrix, which runs alongside because it gates discovery of regressions in all this
work).

Ticket format matches [RM-GA-P1](phase1-security-floor-tickets.md): problem with
file:line evidence, the change, acceptance criteria, files, dependencies, size
(S ≈ ≤2 days, M ≈ 3–5 days, L ≈ 1–2 weeks).

---

# Sequencing at a glance

```
DUR-401 (atomic_write util) ──┬─> DUR-403 (cross-process locking) ──> DR-1001 (backup)
                              └─> DUR-404 (persist in-mem stores)
DUR-402 (fsync append paths)  ─ independent (pairs with DUR-401)
DUR-405 (definition pinning)  ─ independent

EXE-601 (dispatcher loops)    ─┐
EXE-602 (startup reclaim)      ├─ land together (both in serve())
EXE-603 (Engine::cancel)       ─ independent (wovyr-workflow change)
EXE-604 (async run resource)   ─ independent

DR-1002 (KMS escrow + restore test) ── depends on DUR-401 (atomic KMS write)
DR-1003 (RPO/RTO targets)           ── depends on DR-1001, DR-1002

CI-901 (feature-matrix CI)    ─ independent, land FIRST (gates discovery)
```

**Land CI-901 first** — it costs a day and immediately surfaces whether any of this
work regresses a feature-gated build/test. **Critical path:** DUR-401 → DUR-403 →
DR-1001. WS-6 parallelizes entirely against WS-4.

---

# WS-4 — Durable State & Crash-Safety

## DUR-401 `[P0]` — Shared `atomic_write` utility across all file stores

**Problem.** Most single-document stores rewrite the live file in place with
`std::fs::write` — no temp+rename — so a crash mid-write truncates it. Confirmed
across `wovyr-tenancy/src/store.rs`, `wovyr-secrets/src/store.rs` +
`encrypted_store.rs`, `wovyr-kms/src/store.rs` + `root.rs`, `wovyr-events/src/store.rs`,
`wovyr-marketplace/src/store.rs`, `wovyr-server/src/plugins.rs`, and the CLI's
`plugin.rs`/`config.rs`. The worst case is `wovyr-kms/src/store.rs` / `root.rs`: a torn
write there is **accidental crypto-shredding of every tenant's sealed data**. (Only
the workflow checkpoint/timer/schedule saves and wovyr-memory's `delete` already use
temp+rename.) (PRD-003 R-4.1; closes PP-09.)

**Change.**
- Add one shared helper — `wovyr_common::fs::atomic_write(path, bytes)` — that writes
  to a temp file in the same directory, `fsync`s it, renames over the target, and
  (on Unix) `fsync`s the parent directory. Idempotent, `Send`-safe.
- Replace every direct `std::fs::write`/`tokio::fs::write` of a whole-document store
  file with it. Do the **KMS store + root key first** (highest blast radius).

**Acceptance criteria.**
- A fault-injection test (write interrupted before rename) leaves the previous file
  intact and parseable.
- `wovyr-kms` round-trips a sealed record after a simulated interrupted rotation.
- Grep shows no remaining direct whole-file `fs::write` in the 10 stores.

**Files.** `crates/wovyr-common/src/` (new `fs.rs`); all 10 file-store crates listed
above. **Size.** M. **Depends on:** none. **Blocks:** DUR-403, DUR-404, DR-1002.

---

## DUR-402 `[P0]` — `fsync` the workflow event log and audit chain append paths

**Problem.** No `sync_data`/`sync_all` anywhere in the workspace — "durable" means
page-cache-durable. The workflow event log appends and `flush()`es but never syncs
(`crates/wovyr-workflow/src/store.rs:188-189`); the checkpoint temp+rename never syncs
(`store.rs:214-216`); the audit log append doesn't sync (`crates/wovyr-audit/src/log.rs`).
A power loss can lose acknowledged workflow events (breaking resume) and silently drop
the tail of the tamper-evident audit chain. **Also:** the event append recomputes its
sequence by re-reading the entire file (`store.rs:191-193`
`load_lines(&path).len()`) — O(events) per append, O(N²) per execution. (PRD-003
R-4.2; closes PP-10, and PP-14's event-log growth.)

**Change.**
- `File::sync_data` after the event-log append and after the checkpoint rename
  (+ parent-dir fsync on Unix, reusing DUR-401's helper).
- Sync the audit-log append before returning success.
- While here: keep an in-handle monotonic sequence counter for the event log instead
  of re-reading the file, removing the O(N²) append cost.

**Acceptance criteria.**
- A crash-consistency test (kill after append returns) recovers all acknowledged
  events on `resume`.
- Event append latency is constant w.r.t. execution length (a micro-benchmark or
  assertion-style perf test, matching the existing perf-test convention).

**Files.** `crates/wovyr-workflow/src/store.rs`, `crates/wovyr-audit/src/log.rs`.
**Size.** M. **Depends on:** none (pairs with DUR-401).

---

## DUR-403 `[P1]` — Cross-process advisory locking for shared `~/.wovyr` stores

**Problem.** The CLI and server share every `~/.wovyr` directory by design, but locks
are process-local mutexes (e.g. `FileTenancyStore`'s `Mutex<()>`,
`wovyr-tenancy/src/store.rs`). Two writers (a CLI `memory put` racing a server request)
interleave file rewrites and lose data or collide on derived ids; the audit hash-chain
tip lives in a process-local `Mutex<ChainState>` (`wovyr-audit/src/log.rs`), so a second
writer forks the chain and `verify()` falsely reports tampering. (PRD-003 R-4.3; closes
PP-11.)

**Change.**
- Add advisory file locking (e.g. `fs2::FileExt::lock_exclusive`) per store directory,
  acquired around the read-modify-write of shared JSON files.
- For the audit log specifically: re-read the chain tip **under the lock** before
  appending, so concurrent appenders extend one chain.
- Alternative considered (documented, not chosen for GA): funnel all CLI mutations
  through the server API — larger scope, deferred.

**Acceptance criteria.**
- A test spawning two processes (or two `Store` handles with real file locks) doing
  concurrent writes shows no lost update and no corrupt file.
- Concurrent audit appends from two handles produce one chain that `verify()` accepts.

**Files.** the 10 file-store crates (lock wrapper), `crates/wovyr-audit/src/log.rs`.
New dep: `fs2` (or `fd-lock`). **Size.** L. **Depends on:** DUR-401. **Blocks:**
DR-1001.

---

## DUR-404 `[P1]` — Eliminate restart amnesia (agents, workflow owners, idempotency, quota)

**Problem.** Several stores are process-memory only while the data that references
them is durable: `AgentStore` is a `RwLock<BTreeMap>` ("durability … is a later slice",
`crates/wovyr-server/src/lib.rs:64-68`); `workflow_owners` is in-memory
(`lib.rs:131-135`) so after a restart every durable execution loses its tenant binding
— the owning tenant gets 404s while the anonymous `default` space can see them all (a
tenant-isolation **regression**, per `workflow_visible`); the idempotency store is an
unbounded in-memory map (bounded in Phase 1 by SEC-205, persisted here); `QuotaTracker`'s
daily window resets to $0 on restart (`tenancy.rs`), so a crash-loop bypasses daily
budgets. (PRD-003 R-4.4; closes PP-06/07 state portions and the restart-visibility
inversion.)

**Change.**
- Persist the `AgentStore` and the `workflow_owners` index beside the workflow store
  (`~/.wovyr/workflows`), using DUR-401's atomic write. On startup, load both.
- Persist idempotency keys and quota accumulators with TTLs (file-backed for Path A;
  the Path B milestone moves them to Postgres/Redis).
- After restart, a tenant MUST see (only) its own durable executions.

**Acceptance criteria.**
- Restart test: create an agent + submit a workflow as tenant T, restart the server,
  and confirm T (and only T) still sees its agent and execution; the `default` space
  does not.
- A daily-quota accumulator survives restart within the same UTC day.

**Files.** `crates/wovyr-server/src/lib.rs` (AgentStore, workflow_owners persistence),
`crates/wovyr-server/src/tenancy.rs` (QuotaTracker persistence),
`crates/wovyr-server/src/hardening.rs` (idempotency persistence). **Size.** L.
**Depends on:** DUR-401.

---

## DUR-405 `[P2]` — Resolve pinned definition by id on signal/approve (no manifest re-upload)

**Problem.** `signal_handler` and `approve_handler`
(`crates/wovyr-server/src/workflow_runner.rs:314-320,357-365`) require the client to
re-upload the **entire workflow definition YAML** on every call, because the server
never persists the definition by name. The engine already content-hashes and pins the
definition at `start` (G7); the server just doesn't resolve it back. (PRD-003 R-4.5;
closes PP-07 ergonomics.)

**Change.**
- Persist the pinned definition with the execution (or resolve it from the pinned hash
  in the checkpoint) so signal/approve/cancel need only the execution id.
- Keep accepting an optional manifest for back-compat, but make it unnecessary.

**Acceptance criteria.**
- `POST /workflows/{id}/signal` and `/approve` succeed with only the id + event/decision
  payload; a mismatched re-uploaded manifest is rejected (definition-drift guard, G7).

**Files.** `crates/wovyr-server/src/workflow_runner.rs`; possibly a small
`Engine`/store accessor for the pinned definition. **Size.** M. **Depends on:**
DUR-404 (execution-adjacent persistence) — soft.

---

# WS-6 — Server-Side Execution Driver

## EXE-601 `[P0]` — Background timer & schedule dispatcher loops in `serve()`

**Problem.** Grep for `TimerDispatcher`/`ScheduleDispatcher`/`fire_timer`/`tick` in
`crates/wovyr-server/src`: **zero matches**. The engine's G1 durable timers and G2
schedules only fire when an operator runs the CLI's `workflows tick` on the same host
against the same `~/.wovyr/workflows` dir. A `wait: {timer: {after: "30d"}}` workflow
submitted over HTTP will *never* resume. The primitives exist —
`TimerDispatcher::poll` (`crates/wovyr-workflow/src/timer.rs:257`) and
`ScheduleDispatcher::poll` (`schedule.rs:304`). (PRD-003 R-6.1; closes PP-07 execution
driver.)

**Change.**
- In `serve()` (`crates/wovyr-server/src/lib.rs:635`), spawn two background tasks that
  poll the timer and schedule dispatchers on a configurable interval
  (`WOVYR_DISPATCH_INTERVAL_SECS`, default e.g. 5s), driving due timers/schedules via
  the engine. Clock is read only at the dispatcher boundary (existing convention).
- Ensure graceful shutdown cancels the loops.

**Acceptance criteria.**
- An integration test submits a `wait: {timer: {after: <short>}}` workflow over HTTP
  and observes it resume and complete with **no** CLI invocation.
- A schedule fires on cadence via the server alone.

**Files.** `crates/wovyr-server/src/lib.rs` (serve + a `dispatch.rs` helper).
**Size.** M. **Depends on:** none (pairs with EXE-602).

---

## EXE-602 `[P0]` — Resume in-flight executions on startup

**Problem.** Workflows progress via fire-and-forget `tokio::spawn(engine.resume(...))`
(`crates/wovyr-server/src/workflow_runner.rs:300-302`). A server restart mid-run strands
the execution in `Running` forever — nothing rescans the store at startup. (PRD-003
R-6.2; closes PP-07/PP-15 recovery.)

**Change.**
- On startup, `Engine::list` executions in a non-terminal (`Running`/`Waiting`) state
  and re-drive them via the idempotent `resume` (Path A: in-process; Path B: re-lease
  via the WorkQueue — out of scope here).
- Guard against a thundering re-drive on a large store (bounded concurrency).

**Acceptance criteria.**
- Restart test: submit a multi-step workflow, kill the server mid-run, restart, and
  confirm it resumes from the last checkpoint and completes (no duplicated activity
  effects — relies on DUR-402 durability).

**Files.** `crates/wovyr-server/src/lib.rs` (serve startup), `workflow_runner.rs`.
**Size.** M. **Depends on:** none (benefits from DUR-402, DUR-404).

---

## EXE-603 `[P0]` — Implement `Engine::cancel`; stop returning `202` for a no-op

**Problem.** `DELETE /api/v1/workflows/{id}` (`workflow_runner.rs:409-437`) does auth
+ existence checks, then `tracing::info!("cancel requested (advisory)")` and returns
`202 Accepted` — the execution keeps running and never transitions. The doc comment
admits "A production implementation would add Engine::cancel". The SDKs and dashboard
present this as if it worked. (PRD-003 R-6.3; closes PP-15.)

**Change.**
- Add `Engine::cancel(execution_id)` to `wovyr-workflow`: write a `WorkflowCancelled`
  event, transition the execution to `Cancelled`, skip pending/waiting activities
  (in-flight activities complete; document that boundary), persist the checkpoint.
- Wire the handler to call it and return `200`/`202` only on real success. If the
  engine change slips, the handler returns `501 not_implemented` in the interim — it
  MUST NOT fake success.

**Acceptance criteria.**
- After `DELETE`, `GET /workflows/{id}` reports `Cancelled` and a `WorkflowCancelled`
  event is in the history; pending activities are `Skipped`.
- No code path returns `202` without a state transition.

**Files.** `crates/wovyr-workflow/src/engine.rs` (new `cancel`),
`crates/wovyr-server/src/workflow_runner.rs` (handler). **Size.** M. **Depends on:**
none.

---

## EXE-604 `[P1]` — Asynchronous submit→poll resource for `agents:run`

**Problem.** `agents:run` is unbounded synchronous request/response; `run_definition`
holds the HTTP connection and the project `RunPermit` for the whole agent loop, so a
wedged upstream holds both indefinitely (Phase 1's SEC-201 timeout bounds it but
doesn't give a good long-run UX). `overview.md`'s `/operations/{id}` polling resource
is unimplemented. (PRD-003 R-6.4; closes PP-04-adjacent run UX / PP-07.)

**Change.**
- Add an async run mode: `POST /api/v1/agents:run` with a `Prefer: respond-async` (or
  an explicit `:submit` route) returns a run id immediately; `GET
  /api/v1/agents/runs/{id}` polls status/result. Mirror the workflow submit shape.
- Runs are tracked in the (now-durable, DUR-404) run store; the permit is held by the
  background task, not the HTTP connection.

**Acceptance criteria.**
- A long run submitted async returns promptly with a run id; polling returns
  `running` then `completed` with the result.
- The synchronous path still works for short runs (back-compat).

**Files.** `crates/wovyr-server/src/lib.rs` (agents run handlers, run store).
**Size.** M. **Depends on:** DUR-404 (run persistence) — soft.

---

# WS-10 — Backup, Restore & Disaster Recovery

## DR-1001 `[P1]` — `wovyr admin backup` / `restore`

**Problem.** No backup/restore tooling exists anywhere — no CLI command, no pg_dump
hook, no `~/.wovyr` snapshot. A consistent snapshot isn't even *possible* today because
writers hold no lock and half the stores rewrite in place, so a naive `tar` of a live
directory captures torn JSON. Losing `~/.wovyr/kms` with no backup = permanent loss of
all sealed data. (PRD-003 R-10.1; closes PP-12.)

**Change.**
- Add `wovyr admin backup <dest>`: quiesce writers via the DUR-403 locks, snapshot
  `~/.wovyr` atomically (copy under the held locks, or use a consistent copy), and emit
  a manifest (versions, checksums). Document `pg_dump` for the Postgres-backed
  marketplace registry.
- Add `wovyr admin restore <src>`: validate the manifest, restore into a target
  `~/.wovyr`.

**Acceptance criteria.**
- backup → wipe `~/.wovyr` → restore → the server reads back all agents, secrets,
  memory, workflows, and tenancy unchanged.
- A backup taken during concurrent writes is internally consistent (no torn files).

**Files.** `apps/wovyr-cli/src/` (new `admin.rs`). **Size.** M. **Depends on:**
DUR-403 (quiesce via locks).

---

## DR-1002 `[P1]` — KMS root-key escrow (mandatory install step) + tested restore

**Problem.** The KMS root key at `~/.wovyr/kms/root.key` is generated-once and
`0600`-permissioned; if the host is lost and the key isn't escrowed, every sealed
secret and memory is permanently unrecoverable. There is no documented escrow step and
no restore test. (PRD-003 R-10.2; closes PP-12 for the crypto-critical path.)

**Change.**
- Make root-key escrow a documented, mandatory install step: `WOVYR_KMS_ROOT_KEY`
  (hex) injection is the supported production mode; the generated file is dev-only and
  logs a loud warning telling the operator to escrow it.
- Add a restore test: seal a record, back up + escrow, wipe, restore the key + data,
  decrypt.

**Acceptance criteria.**
- A CLI/server started with `WOVYR_KMS_ROOT_KEY` decrypts data sealed by another
  instance with the same key.
- Docs (`docs/13-security/encryption.md`, deployment) state escrow as mandatory.

**Files.** `crates/wovyr-kms/src/root.rs` (warning), docs, a restore integration test.
**Size.** S. **Depends on:** DUR-401 (atomic KMS write).

---

## DR-1003 `[P2]` — Define and validate RPO/RTO targets

**Problem.** RPO/RTO are currently "undefined/never" — a gap against the
[v1.0 exit criteria](../v1.0.md#5-exit-criteria). (PRD-003 R-10.3; closes PP-12
targets.)

**Change.**
- Define RPO (max acceptable data loss) and RTO (max acceptable recovery time) for the
  single-node appliance, document them, and validate via a timed backup→loss→restore
  drill.

**Acceptance criteria.**
- Documented RPO/RTO in `docs/12-deployment/`; a drill run confirms the restore path
  meets RTO and the backup cadence meets RPO.

**Files.** `docs/12-deployment/`, `docs/18-roadmap/v1.0/A2-reliability-ha-dr.md`.
**Size.** S. **Depends on:** DR-1001, DR-1002.

---

# WS-9 (parallel) — CI Feature Matrix

## CI-901 `[P1]` — `cargo hack --each-feature` + a service-container integration job

**Problem.** CI runs `cargo clippy/build/test --workspace` with **default features
only** (`.github/workflows/ci.yml:44-50`) — no `--features`/`--all-features` job. The
workspace has ~20 feature flags across 11 crates; feature-gated code is never
clippy-linted despite the `-D warnings` policy, and all capability-gated integration
tests (Postgres/Qdrant/Redis/WASI/sandbox) skip silently forever. This hides real
bugs (e.g. the latent CLI marketplace `spawn_blocking` panic, Phase-1-adjacent) and is
the single highest-leverage CI change — it gates discovery of regressions across all
of Phase 2. (PRD-003 R-9.1; closes PP-17.)

**Change.**
- Add a `cargo hack check --each-feature --workspace` job (installs `cargo-hack`) so
  every feature-gated code path is compiled and linted.
- Add one integration job with Postgres + Qdrant + Redis service containers that runs
  the capability-gated tests (`wovyr-workflow`/`wovyr-marketplace`/`wovyr-memory`
  Postgres, `wovyr-provider` Redis/Qdrant) with the env vars they gate on set.

**Acceptance criteria.**
- CI fails on a clippy warning in any feature-gated module.
- The Postgres/Qdrant/Redis integration tests run (not skip) in the new job and pass.

**Files.** `.github/workflows/ci.yml`. **Size.** S–M. **Depends on:** none. *(Land
first.)*

---

# Rollup

| Ticket | Title | Size | Priority | Depends on |
|--------|-------|------|----------|------------|
| CI-901 | Feature-matrix CI + service-container job | S–M | P1 | — |
| DUR-401 | Shared `atomic_write` utility | M | P0 | — |
| DUR-402 | fsync event log + audit; fix O(N²) seq | M | P0 | — |
| DUR-403 | Cross-process advisory locking | L | P1 | DUR-401 |
| DUR-404 | Persist agents/owners/idempotency/quota | L | P1 | DUR-401 |
| DUR-405 | Definition pinning on signal/approve | M | P2 | DUR-404 (soft) |
| EXE-601 | Dispatcher loops in serve() | M | P0 | — |
| EXE-602 | Resume in-flight runs on startup | M | P0 | — |
| EXE-603 | `Engine::cancel` + honest DELETE | M | P0 | — |
| EXE-604 | Async submit→poll for agents:run | M | P1 | DUR-404 (soft) |
| DR-1001 | `wovyr admin backup`/`restore` | M | P1 | DUR-403 |
| DR-1002 | KMS root-key escrow + restore test | S | P1 | DUR-401 |
| DR-1003 | RPO/RTO targets + drill | S | P2 | DR-1001, DR-1002 |

**Rough total:** 2 L + 7 M + (2–3) S ≈ 9–11 engineer-weeks, parallelizable to ~4–5
calendar weeks across 2–3 engineers (WS-4 and WS-6 are independent tracks). **Phase-2
exit** = PRD-003 §11 items 2 (crash/power-loss safe, tested backup/restore) and 3
(server drives timers/schedules + crashed-run recovery; no API lies).

**Path-note:** every Phase-2 ticket is topology-independent and required under both
ADR-0010 paths. DUR-404's file-backed idempotency/quota persistence and EXE-602's
in-process resume are the Path-A forms; Path B (v1.1) upgrades them to
Postgres/Redis and lease-based reclaim respectively — additive, not throwaway.

---

# Related

- [PRD-003](../../01-product/prd-ga-hardening.md) — parent PRD (WS-4/6/9/10, §10 phasing)
- [RM-GA-P1](phase1-security-floor-tickets.md) — Phase 1 (security floor) tickets
- [ADR-0010](../../17-adr/ADR-0010-ga-deployment-topology.md) — GA topology decision
- [`03-workflow-engine/temporal-gap-analysis.md`](../../03-workflow-engine/temporal-gap-analysis.md) — G1 timers / G7 pinning
- [GA-002](A2-reliability-ha-dr.md) — the Reliability delivery doc (HA/DR context)

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-07 | All tickets shipped. Marked Done in the header; see `CLAUDE.md`'s per-crate bullets (wovyr-common, wovyr-workflow, wovyr-kms, wovyr-audit, wovyr-server, apps/wovyr-cli) and [backup-and-restore.md](../../12-deployment/backup-and-restore.md) for what each ticket actually produced |
| 1.0.0 | 2026-07-06 | Initial Phase-2 (durability & execution) ticket breakdown: 13 tickets across WS-4/6/9/10 with dependencies, acceptance criteria, file targets, and sizing |
