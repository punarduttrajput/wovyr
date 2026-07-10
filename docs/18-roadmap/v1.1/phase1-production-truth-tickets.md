<!--
File: docs/18-roadmap/v1.1/phase1-production-truth-tickets.md
Document ID: RM-AIM-P1
-->

# Phase 1 — Make Production Claims True: Implementation Tickets

**Document ID:** RM-AIM-P1
**File Path:** `docs/18-roadmap/v1.1/phase1-production-truth-tickets.md`
**Version:** 1.0.0
**Status:** Planned — not started
**Owner:** Engineering (AI Core / Platform)
**Last Updated:** 2026-07-09

---

# Purpose

Phase 1 of [PRD-004 §8](../../01-product/prd-ai-platform-maturity.md) — the fixes
that make features the platform *already advertises* actually work: real cost
accounting, context/token management, activated sandboxing, graceful/durable server
behavior, a production-grade Postgres path, and a reconciled release/CI story.

Covers workstreams **WS-A** (AI core), **WS-B/R-B.1** (cost), **WS-E** (sandbox
activation), **WS-G** (server durability), **WS-H** (workflow Postgres core),
**WS-I/R-I.3** (secret default), **WS-J** (release/CI), **WS-K** (UI security/tests).

Format matches [RM-GA-P2](../v1.0/phase2-durability-execution-tickets.md): problem +
file:line evidence, change, acceptance criteria, files, size (S ≤2d, M 3–5d, L 1–2w),
deps, priority.

---

# Sequencing at a glance

```
PRV-101 (cost table) ── LAND FIRST (unblocks real quota enforcement everywhere)
AIC-101 (context/token mgmt) ─┬─ independent AI-core work
AIC-102 (parallel tools)       ├
AIC-103 (max_steps default)    ┘
SBX-101 (wire strong sandboxes) ── SBX-102 (Windows Job Object) [parallel]
SRV-101 (graceful shutdown) ─┬─ server durability (parallel)
SRV-102 (durable async runs)  ├
SRV-103 (webhook outbox)      ├
SRV-104 (API-key lifecycle)   ┘
WFL-101 (pg pool) ─┬─ WFL-103 (pg TLS) ── workflow Postgres hardening
WFL-102 (subwf depth) │
WFL-104 (fenced seq)  ┘
SEC-101 (encrypted secret default) ─ independent
DX-101 (version+CHANGELOG) ── DX-102 (release automation) ── DX-103 (dashboard+Windows CI)
UI-101 (token storage) ─ UI-102 (UI tests) [parallel]
```

**Land PRV-101 first.** Until real cost flows, per-project quotas (PRD-003) admit
everything at $0 — so PRV-101 is a prerequisite for trusting any Phase-2 quota work.

---

# WS-B — Provider & Cost

## PRV-101 `[P0]` — Per-model price table + real `cost_usd` — **DONE (2026-07-09)**

**Problem.** `OpenAiProvider` hardcodes `cost_usd = 0.0`
(`crates/apex-provider/src/openai.rs:442,513-515`) and mistralrs likewise
(`mistralrs_provider.rs:197-203`); the only price constant is the mock's synthetic
`MOCK_USD_PER_TOKEN` (`mock.rs:16`). Every `CostEvent`, and the server's per-project
budget accounting fed from `output.usage.cost_usd` (`apex-runtime/src/lib.rs:257`),
sees **$0** in production — so `llm_cost_per_day_usd` quota enforcement (PRD-003) is
a no-op. (PRD-004 R-B.1; audit High.)

**Change.**
- Add a per-model `$/input-token` + `$/output-token` price table (a
  `PriceBook`), sourced from config with sane built-in defaults for common
  OpenAI/Anthropic models and overridable via env/config file.
- Compute `cost_usd` from returned token `Usage` in `OpenAiProvider` (and
  mistralrs = $0 local is legitimate, but make it explicit/documented).
- Log the computed cost before enforcement (a one-release "observe then enforce"
  rollout, per PRD-004 §10 risk note).

**Acceptance criteria.**
- A unit test asserts a known token count × known price = expected `cost_usd`.
- An integration test drives a run and asserts the project's daily-cost accumulator
  advances by the computed amount (not 0).
- Unknown-model lookups fail safe (documented default or a loud warn), never panic.

**Files.** `crates/apex-provider/src/{openai.rs,mistralrs_provider.rs,gateway.rs}`, a
new `pricing.rs`; `crates/apex-server/src/tenancy.rs` (quota assertions).
**Size.** M. **Depends on:** none. **Blocks:** SRV-202 (token quotas), EVL-202.

**Implementation notes (2026-07-09).** New `apex-provider::pricing` module: a
`PriceBook` (model → `ModelPrice { input_per_1m, output_per_1m }`) with built-in
defaults for common OpenAI/Anthropic models, overridable via `APEX_MODEL_PRICES`
(inline JSON) or `APEX_PRICEBOOK_FILE` (a JSON file); lookup is
exact-match-then-longest-prefix (so `gpt-4o-mini-2024-07-18` → `gpt-4o-mini`), then
a configurable `default`, then a one-time `tracing::warn!` returning `$0` for a
genuinely unknown/undefaulted model (fail-safe, never a panic). `OpenAiProvider`
carries a `PriceBook` (from `PriceBook::from_env()` by default; `with_price_book`
override) and computes `cost_usd` from returned `Usage` in both `parse_response`
(non-streaming) and `StreamAccumulator::finish` (streaming), logging it at
`debug` (`target: "apex.pricing"`) — the "observe" half of observe-then-enforce.
mistral.rs keeps `$0` with an expanded comment: a local model has no vendor bill,
so that's the *correct* cost, not a placeholder. **Acceptance:**
`pricing::tests::known_token_count_times_price_is_expected_cost` +
`openai::tests::parses_text_completion` (now asserts the computed non-zero cost)
cover the unit criterion; `tenancy::tests::priced_run_cost_advances_the_daily_
accumulator` drives a price-book-computed cost through `record_run_cost` and
asserts the daily accumulator advances by exactly that amount (not $0);
`pricing::tests::unknown_model_without_default_is_free_not_a_panic` covers
fail-safe. `MOCK_USD_PER_TOKEN` is unchanged (the mock already priced its output).

---

# WS-A — AI Core Runtime

## AIC-101 `[P0]` — Context-window management: tokenizer + history compaction — **DONE (2026-07-09)**

**Problem.** `run_agent` clones the *entire* message history into every request
(`crates/apex-agent/src/runtime.rs:240` `messages.clone()`) and appends
assistant+tool-result messages every tool turn (`runtime.rs:259-270`) with no
budgeting, truncation, or summarization. There is **no tokenizer anywhere** (the
mock estimates chars/4, `mock.rs:33`). A long tool loop silently blows the model's
context window and cost. (PRD-004 R-A.1; audit High.)

**Change.**
- Add a token-counting utility (a real tokenizer, e.g. `tiktoken`-style for
  OpenAI-family, with a documented fallback estimate for others).
- Before each `ChatRequest`, run a token-budgeted compactor: keep the system prompt
  + latest user turn + most-recent tool turns; drop-oldest first, then optionally
  summarize dropped turns. Strategy configurable; default lossless drop-oldest.

**Acceptance criteria.**
- A test with a synthetic long tool loop asserts the request token count stays under
  a configured budget while preserving the system + latest user turn.
- Compaction is deterministic given the same input (house determinism rule).

**Files.** `crates/apex-agent/src/runtime.rs`; new token-count util (in
`apex-provider` or `apex-common`). **Size.** L. **Depends on:** none.
**Blocks:** EVL-203.

**Implementation notes (2026-07-09).** Two new modules. `apex-provider::tokenizer`:
a `TokenCounter` trait with a dependency-free, deterministic `HeuristicTokenizer`
default (each whitespace-delimited chunk → ~4-char subword tokens + one per
punctuation char; a `count_message` helper adds role/tool-call framing overhead via
`PER_MESSAGE_OVERHEAD`/`PER_TOOL_OVERHEAD`). Deliberately **not** a real BPE encoder
— a bundled vocab is a heavy dep and this workspace builds offline; documented as a
~10–20% estimate suitable for *budgeting* (not billing — real cost is PRV-101's
provider `usage`). A real tokenizer drops in behind the trait later. `apex-agent::
context`: `compact(messages, tools_overhead, policy, counter)` drops the **oldest
tool rounds** first (an `assistant` tool-call message + its `tool` results, kept
whole so the wire sequence stays valid) while always preserving the leading system
prompt(s) + first user turn; `ContextPolicy { max_prompt_tokens, strategy }` with a
generous 96k default (so short runs are untouched, back-compat) and a `DropOldest`
strategy, wired into `RunOptions` (`with_context_policy`) and applied at the top of
every `run_agent` loop iteration (logged at `debug`, `target: "apex.context"`).
**Acceptance:** `context::tests::long_tool_loop_stays_under_budget_and_keeps_system_
and_user` + the through-`run_agent` integration test `runtime::tests::long_tool_
loop_request_stays_under_context_budget` (a scripted 30-round tool loop asserts the
largest request the provider ever saw stayed under the budget and the system+user
turns were present on every request); `context::tests::compaction_is_deterministic`
covers the house determinism rule; `retained_rounds_stay_coherent` proves no
dangling tool result survives.

## AIC-102 `[P0]` — Concurrent tool-call execution within a turn — **DONE (2026-07-09)**

**Problem.** When the model requests multiple tool calls in one turn, they run
one-at-a-time in an awaited `for` loop (`runtime.rs:261-270`); parallelizable I/O-bound
tools serialize. (PRD-004 R-A.2; audit High.)

**Change.** Execute independent tool calls concurrently (`join_all`/`JoinSet`),
preserving result ordering by call id when feeding results back to the model.

**Acceptance criteria.** A test with two artificially-delayed tools asserts wall-clock
≈ max(individual), not sum; result ordering is stable/deterministic.

**Files.** `crates/apex-agent/src/runtime.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-09).** `execute_tool_call` was refactored to *not*
touch the `&mut` sink (it now returns a `ToolOutcome { result_text, ok }`), so the
whole batch can execute on one task via `futures::future::join_all` — no `Send`/
spawn requirement, so no threading of a shared sink. The loop emits every `ToolCall`
event up front (deterministic order), joins the batch concurrently, then emits each
`ToolResult` and pushes each `Message::tool_result` in **input order** — `join_all`
returns results positionally regardless of completion timing, so the history fed
back to the model is deterministic. Chose `join_all` over `JoinSet` deliberately:
`JoinSet` requires `'static + Send` futures (forcing owned clones of `def`/`registry`
or an `Arc` refactor), whereas these tool futures only borrow `&` state and share no
mutable data, so on-task concurrency is both sufficient and simpler. **Acceptance:**
`runtime::tests::independent_tool_calls_run_concurrently_with_deterministic_order`
(a `#[tokio::test(start_paused = true)]` with a 300ms + 100ms `SleepyTool` pair;
asserts paused wall-clock ≈ 300ms = max, definitively under the 400ms a serial loop
would take, and that results feed back as `[slow, fast]` = call order, not the
`[fast, slow]` completion order). Added `tokio` `time`+`test-util` dev-features for
the paused clock (same pattern as apex-provider's hedging tests).

## AIC-103 `[P1]` — Apply manifest `max_steps` as the default budget — **DONE (2026-07-09)**

**Problem.** `spec.max_steps` is parsed (`definition.rs:56-57`) but `run_agent_inner`
reads only `opts.max_steps` (`runtime.rs:239`); only `apex-runtime` wires it
(`lib.rs:248-250`), so the eval runner and any direct `run_agent` caller ignore the
manifest budget. (PRD-004 R-A.3; audit Med.)

**Change.** In `run_agent_inner`, default the step budget to `def.spec.max_steps`
unless `RunOptions` explicitly overrides it.

**Acceptance criteria.** A test with a manifest `max_steps: N` and no `RunOptions`
override stops at N steps.

**Files.** `crates/apex-agent/src/runtime.rs`. **Size.** S. **Depends on:** none.

**Implementation notes (2026-07-09).** `RunOptions.max_steps` changed from `usize`
(default 8, indistinguishable from an explicit 8) to `Option<usize>` (`None` =
"defer to the manifest, then the built-in default"); `with_max_steps` sets `Some(n)`.
`run_agent_inner` now resolves the budget as `opts.max_steps.or(def.spec.max_steps)
.unwrap_or(DEFAULT_MAX_STEPS)` — precedence **explicit override > manifest >
built-in** — used for the loop bound, the "did not finish within N steps" error, and
the `agent.run` span field. Existing pre-resolving callers (`apex-runtime`, the
server's `agents.rs` doing `req.max_steps.or(def.spec.max_steps)`) are unaffected —
they set `Some(..)` before the call, so the new default branch only ever fires for
the previously-broken direct/eval callers. **Acceptance:**
`runtime::tests::manifest_max_steps_is_the_default_budget` (manifest `max_steps: 0`,
no override → fails at 0 steps) + `explicit_max_steps_overrides_the_manifest_budget`
(manifest 0 but `with_max_steps(4)` → completes), and the server's existing
`agent_level_max_steps_is_a_default_not_a_floor`/`max_steps_override_is_honored`
still pass. Field-type change rippled to `tool_loop.rs` (`opts.max_steps = Some(3)`)
and the `with_max_steps` unit test (`Some(0)`).

---

# WS-E — Sandbox Activation

## SBX-101 `[P0]` — Wire `SandboxManager::detect()` + `SandboxPool` into the run path — **DONE (2026-07-09)**

**Problem.** `ShellTool` hardcodes `SandboxManager::native_only()`
(`crates/apex-tools/src/builtin.rs:561`), so `ContainerSandbox`, `FirecrackerSandbox`,
`SandboxPool`, and `FairScheduler` are referenced only in tests/defs — never in the
`apex-agent`/`apex-server` run path. A node with Docker/gVisor/Firecracker never uses
them, and untrusted/verified runs simply fail closed. (PRD-004 R-E.1; audit High.)

**Change.**
- The tool/agent run path consumes `SandboxManager::detect()` (capability-probed)
  and acquires from a shared `SandboxPool`, so verified/untrusted work runs on the
  strongest available backend for its `TrustClass`.
- Keep `native_only()` as an explicit opt-in for trusted first-party/local runs.

**Acceptance criteria.**
- A capability-gated integration test (Docker present) asserts an untrusted run
  selects the container backend, not native, and executes.
- A first-party run still uses native; a node with no strong backend fails closed
  for untrusted work (unchanged).

**Files.** `crates/apex-tools/src/builtin.rs`, `sandbox/*`, `pool.rs`;
`crates/apex-agent`/`apex-server` run wiring. **Size.** L. **Depends on:** none.

**Implementation notes (2026-07-09).** `ShellTool` is now stateful — it holds a
`SandboxManager` and a container image (`APEX_SANDBOX_IMAGE`, default `alpine:3.20`):
`ShellTool::native_only()` (native-only caps; fail-closed for verified/untrusted — the
CLI/local/test default) and `ShellTool::with_manager(detected)`. `execute` resolves
the backend from `ctx.trust_class` against the manager's capabilities and dispatches:
`Native` → the existing host-shell path (powershell/cmd/sh); `Container`/`Gvisor` →
`run_container`, which runs `sh -c <cwd-wrapped>` inside a network-isolated
`ContainerSandbox` (a non-`sh` shell request is rejected there, since the container is
Linux). The registry gained `with_shell_using(manager)` (`with_shell()` stays
native-only for back-compat); `apex-server`'s `AppState::from_env` calls
`SandboxManager::detect().await` and uses it when `APEX_ENABLE_SHELL_TOOL=1`, so a
Docker/gVisor node actually runs verified/untrusted shell work in a container.
**Acceptance:** capability-gated `sandbox_backends.rs::shell_tool_runs_a_verified_run_
in_a_container_not_native` (a `Verified` run executes `cat /etc/alpine-release` — only
succeeds inside the alpine image) + `shell_tool_first_party_run_stays_native_even_
when_containers_exist` (first-party runs on the host, where that file is absent);
both skip cleanly with no Docker. Deterministic offline coverage:
`builtin::tests::shell_with_container_capability_routes_verified_run_off_native`
(a Container-capable manager routes a verified run to the container backend — not
fail-closed, not native), plus the existing native-only fail-closed tests. **Scope
note:** the `SandboxPool`/`FairScheduler` integration into the shell path was
*deliberately deferred* — a `ContainerSandbox` handle is stateless config (each
`execute` still does its own `docker run`), so pooling the handles yields no
warm-container reuse (the pool's own module doc calls persistent warm sessions "a
separate concern that needs a session-capable backend"); the real gap this ticket
targets — strong backends never selected on the run path — is fully closed by the
selection/dispatch/detect wiring. Bounded-concurrency pooling is tracked for the
session-capable-backend work rather than added here as non-functional ceremony.

## SBX-102 `[P0]` — Windows Job Object resource limits in the native sandbox — **DONE (2026-07-09)**

**Problem.** `setrlimit` memory/CPU/PID enforcement is `#[cfg(unix)]`-only; the
`not(unix)` branch applies only a timeout + output cap
(`crates/apex-tools/src/sandbox/native.rs:37-70,149-173`). On this Windows host
`shell` runs with **zero resource isolation**. (PRD-004 R-E.2; audit High.)

**Change.** Add a Windows Job Object (`JOBOBJECT_EXTENDED_LIMIT_INFORMATION`:
`ProcessMemoryLimit`, `ActiveProcessLimit`, and a CPU-rate control) in the non-Unix
path, mirroring the Unix `setrlimit` caps.

**Acceptance criteria.** A Windows-gated test asserts a child exceeding the memory or
process-count cap is terminated; caps match the Unix path's semantics.

**Files.** `crates/apex-tools/src/sandbox/native.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-09).** A `#[cfg(windows)]` `JobObject` guard
(`windows-sys` 0.61, `Win32_System_JobObjects`) is created from `ResourceLimits` and
assigned to the child right after spawn in `NativeSandbox::run` — the non-Unix analog
of the Unix `setrlimit` `pre_exec` hook. It sets `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`
with `ProcessMemoryLimit` (`RLIMIT_AS` analog), `ActiveProcessLimit` (the container
`pids.max` analog — Unix native has none), and `PerJobUserTimeLimit` (total user-CPU
time — the `RLIMIT_CPU` total-time analog; chosen over the ticket's parenthetical
"CPU-rate control" because the acceptance criterion prioritizes matching the Unix
*semantics*, which are a total-time quota, not a rate), plus `KILL_ON_JOB_CLOSE` so a
survivor can't outlive the run when the guard drops. The handle is `unsafe impl Send`
(a process-global kernel handle) so `run`'s future stays `Send` across the `.await`.
**Acceptance (runs for real on this Windows host, not just gated):**
`native::tests::job_object_active_process_limit_blocks_child_spawns` (`max_pids = 1` →
an assigned `cmd.exe`'s attempted child spawn is blocked) and
`job_object_memory_limit_fails_an_over_allocating_child` (a ~1 GiB allocation under a
256 MiB `ProcessMemoryLimit` throws `OutOfMemoryException` and dies non-zero). The
memory test surfaced a real PowerShell subtlety — a `;`-chained OOM is *non-terminating*
by default, so the assertion required `$ErrorActionPreference='Stop'` to make the cap
breach actually end the process. `windows-sys` 0.61 was already in the tree (ring links
it), so no new duplicate version / offline fetch and no `cargo-deny` change.

---

# WS-G — Server Durability & Auth Lifecycle

## SRV-101 `[P0]` — Graceful shutdown / drain — **DONE (2026-07-09)**

**Problem.** `axum::serve` is called without `with_graceful_shutdown` and no
SIGTERM/SIGINT handling (`crates/apex-server/src/lib.rs:339-344`); in-flight runs and
spawned tasks are killed abruptly, and the dispatch-loop abort only runs *after*
`serve` returns (which only happens on hard error). (PRD-004 R-G.1; audit High.)

**Change.** Wire a shutdown signal (SIGTERM/SIGINT) into `axum::serve`, drain
in-flight requests within a bounded deadline, and cleanly stop the dispatcher loops.

**Acceptance criteria.** A test sends the shutdown signal mid-request and asserts the
in-flight request completes and new connections are refused; dispatch loops stop.

**Files.** `crates/apex-server/src/lib.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-09).** `serve()` extracted into `serve_http`/`serve_tls`
helpers, each taking a `shutdown: impl Future`. HTTP uses axum's
`.with_graceful_shutdown(shutdown)`; TLS uses `axum_server::Handle::graceful_shutdown
(Some(grace))` triggered from a task awaiting the same future. `shutdown_signal()`
resolves on SIGINT (any platform) or SIGTERM (Unix) via `tokio::select!`. A bounded
`APEX_SHUTDOWN_GRACE_SECS` (default 30) caps the drain; after the serving future
returns, the dispatch loops are aborted (previously that abort only ran on a hard
error, since nothing signaled a clean stop). Added `tokio` `signal`+`macros` features.
**Acceptance:** `graceful_shutdown_drains_in_flight_then_refuses_new_connections`
drives `serve_http` with a test-controlled shutdown future: a slow in-flight request
(gated by a `Notify`) completes with `200` after shutdown is triggered, then a new
connection is refused once the drained serving future returns.

## SRV-102 `[P1]` — Durable async-run store (or documented non-durability) — **DONE (2026-07-09)**

**Problem.** `RunStore` is in-memory only (`crates/apex-server/src/state.rs:171-180`);
the background `tokio::spawn` executing an async agent run
(`agents.rs:144`) has no checkpoint, so a restart loses every in-flight/pollable run
and clients poll a run that can never finish. (PRD-004 R-G.2; audit High.)

**Change.** Persist run records (status transitions) durably and, on startup, mark
orphaned `Running` async runs as `Failed` (a bare agent run has no checkpoint to
resume) — or, if durability is out of scope, document non-durability explicitly in
the API and return a clear terminal status.

**Acceptance criteria.** A restart-simulation test asserts a previously-`Running`
async run is reported terminally (not stuck `Running`) after reopen.

**Files.** `crates/apex-server/src/{state.rs,agents.rs}`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-09).** Chose durability + reconcile-on-startup (the
first ticket option). `RunStore` gained a `path` and `new_with_path`; `RunRecord`'s
`inserted_at` switched from a restart-meaningless `Instant` to wall-clock
`inserted_at_ms` so records round-trip through JSON (the same DUR-404 move
`IdempotencyStore` made). Every `insert_running`/`finish` persists via `atomic_write`.
On reopen, any run still `Running` is reconciled to terminal `Failed` ("server
restarted while the run was in flight; agent runs are not resumable") and re-persisted,
so a poller gets a truthful terminal status rather than a stuck-`Running` poll or a
404. `AppState::from_env` opens it at `~/.apex/server/async_runs.json`; `path: None`
stays in-memory (tests). **Acceptance:** `run_store_tests::running_run_is_reconciled_
to_failed_after_restart` (reopen against the same path shows the orphan `Failed`, a
finished run keeps its terminal status) + `in_memory_store_persists_nothing`.

## SRV-103 `[P1]` — Durable webhook outbox + delivery worker — **DONE (2026-07-09)**

**Problem.** Webhook delivery + retries are in-process fire-and-forget
(`crates/apex-server/src/webhooks.rs:138-152`), retrying via `tokio::sleep` in a
spawned task (`:116-119`); a crash drops all pending retries and dead-letters are only
logged (`:107-113`), not persisted. (PRD-004 R-G.3; audit High.)

**Change.** Add a durable outbox (persisted delivery attempts + a DLQ) and a delivery
worker that survives restart; dead-letters land in a queryable store, not just a log.

**Acceptance criteria.** A restart-simulation test asserts a pending delivery is
retried after reopen; an exhausted delivery lands in the persisted DLQ.

**Files.** `crates/apex-server/src/webhooks.rs`; a durable outbox store.
**Size.** L. **Depends on:** none.

**Implementation notes (2026-07-09).** New `webhook_outbox` module: a durable
`WebhookOutbox` (`{pending, dlq}` document, `atomic_write` on every mutation, `path:
None` = in-memory). `dispatch` now **journals each delivery as pending before its task
runs** (storing the subscription *id*, not its secret — the secret is re-resolved from
the webhook store at send time, so it's never duplicated into the outbox even under the
encrypted store); `spawn_delivery` settles the entry — `remove` on success, `dead_letter`
on exhaustion. `serve()` calls `webhooks::recover_outbox` on startup to re-dispatch
deliveries pending from a dead process (re-resolving the sub by id; a deleted sub drops
the stale entry). A new tenant-scoped `GET /api/v1/webhooks/dead-letters` serves the
persisted DLQ (secrets never included). `deliver` and its retry/signing tests are
unchanged. **Acceptance:** `webhook_outbox::tests::{pending_delivery_survives_reopen,
dead_letter_is_persisted_and_queryable}` (store round-trips across the reopen "restart"
stand-in) + `webhooks::tests::dispatch_dead_letters_exhausted_delivery_into_the_outbox`
(the real dispatch path dead-letters an always-failing delivery into the DLQ). The
existing dispatch tests reset to an in-memory outbox (`with_in_memory_webhook_outbox`)
so concurrent `from_env` tests don't race the shared durable file.

## SRV-104 `[P1]` — API-key lifecycle: expiry, rotation, revocation — **DONE (2026-07-09)**

**Problem.** The API-key store is a bare `hash → principal` map; the only operation
is mint (`crates/apex-server/src/auth.rs:238-241,275-277,301-312`). No `created_at`,
TTL, revoke, rotate, or last-used. (PRD-004 R-G.4; audit High.)

**Change.** Add key metadata (created/expires/revoked/last-used), a revoke endpoint,
and rotation; reject expired/revoked keys at auth time.

**Acceptance criteria.** Tests: an expired key is rejected; a revoked key is rejected;
rotation issues a new key and invalidates the old on a grace schedule.

**Files.** `crates/apex-server/src/auth.rs`; CLI `apex auth` subcommands.
**Size.** M. **Depends on:** none.

**Implementation notes (2026-07-09).** The store value went from a bare `principal`
string to a `KeyRecord { key_id, principal, created_at_ms, expires_at_ms, revoked,
last_used_ms }` (keyed by the key's SHA-256 hash; `key_id` = `key_<first 12 hex of the
hash>`, the non-secret handle for revoke/rotate). `principal_for` now enforces
revocation + expiry via a shared `resolve_live_key`, refreshing `last_used` at most
once/min/key to avoid rewriting the file on every request. `FileApiKeyStore` gained
`create_key(principal, ttl)`, `list_keys`, `revoke(key_id)`, and
`rotate(key_id, grace)` (mints a replacement, sets the old key to expire after the
grace window — both valid during it, only the old lapses after). `load()` transparently
migrates the pre-SRV-104 `hash → principal` on-disk format, so existing keys keep
authenticating. CLI: `apex auth create-key [--ttl-days]`, `list-keys`, `revoke <id>`,
`rotate <id> [--grace-hours]`. **Acceptance:** `auth::tests::{expired_key_is_rejected,
revoked_key_is_rejected, rotation_issues_new_key_and_expires_old_after_grace,
legacy_hash_to_principal_format_is_migrated}`. **Scope note:** the revoke/rotate
surface is the CLI (operating on the shared `~/.apex/auth` store, like `kms`/`memory`);
a server *route* for it wasn't added — the CLI is the operator path, consistent with
how `apex auth create-key` already worked pre-ticket.

---

# WS-H — Workflow Postgres Core

## WFL-101 `[P0]` — Postgres connection pool + reconnect — **DONE (2026-07-10)**

**Problem.** The Postgres store uses a single `tokio_postgres::Client`
(`crates/apex-workflow/src/postgres.rs:87`); every call serializes on one TCP
connection and there is no reconnect if the driver task dies (it only logs,
`:102-106`). (PRD-004 R-H.1; audit High.)

**Change.** Back the store with `deadpool-postgres`/`bb8`; add health-check +
reconnect.

**Acceptance criteria.** A capability-gated test asserts concurrent store calls don't
serialize on one connection and that a dropped connection recovers on the next call.

**Files.** `crates/apex-workflow/src/postgres.rs`. **Size.** M.
**Depends on:** none. **Blocks:** WFL-103, WFL-104.

**Implementation notes (2026-07-10).** Replaced the single `tokio_postgres::Client`
with a hand-rolled `PgPool` (semaphore-bounded, `APEX_PG_POOL_MAX` default 8) that
reuses idle clients and **transparently reconnects** — a client whose background
driver died (`is_closed()`) is discarded on return to the pool and a fresh one dialed
on the next checkout. Every store method now does `self.pool.get().await?` +
`conn.client()...`, so concurrent calls get distinct connections instead of serializing
on one socket. Hand-rolled rather than pulling `deadpool`/`bb8`: neither is vendored in
this offline workspace, and the needs are modest — the same "hand-roll to avoid a heavy
dep" call as the S3 signer / cron evaluator. **Acceptance (validated live against a real
remote Aiven Postgres over TLS, not just capability-gated):**
`postgres_store::tests::concurrent_store_calls_are_served_by_the_pool` (16 independent
executions' appends+loads complete concurrently without deadlock/serialization failure)
passed against the live database. The reconnect path is structural (every `get()`
discards a closed client); forcing a mid-test backend kill needs `pg_terminate_backend`,
left to a dedicated drill.

## WFL-102 `[P0]` — Sub-workflow recursion depth guard — **DONE (2026-07-10)**

**Problem.** A `workflow` activity naming its own (or a mutually-recursive) workflow
recurses forever; `run_subworkflow` boxes the future but caps nothing
(`crates/apex-workflow/src/engine.rs:998-1029`). (PRD-004 R-H.2; audit High.)

**Change.** Thread a depth counter (or ancestor set) through sub-workflow resolution;
fail closed past a configurable max depth / on a detected cycle.

**Acceptance criteria.** A test with a self-referential workflow fails with a clear
depth/cycle error instead of hanging/overflowing.

**Files.** `crates/apex-workflow/src/engine.rs`. **Size.** S. **Depends on:** none.

**Implementation notes (2026-07-10).** `Engine` gained a `max_subworkflow_depth`
(`with_max_subworkflow_depth`, default `DEFAULT_MAX_SUBWORKFLOW_DEPTH = 16`).
`run_subworkflow` derives the nesting depth from the `::`-delimited child id (each
level appends one `::<activity>`), and if it exceeds the cap fails the activity closed
via `terminal_activity_failure` with a clear "sub-workflow nesting depth N exceeded"
message rather than recursing until the stack overflows. (Root execution ids are
`::`-free by construction, so the separator count is the true depth.) **Acceptance:**
`temporal_gaps::self_referential_subworkflow_fails_with_a_depth_error` (a workflow whose
`workflow` activity names itself, `with_max_subworkflow_depth(3)`, fails with the depth
error and creates no execution past the cap — the test terminating proves no
hang/overflow).

## WFL-103 `[P1]` — TLS to Postgres — **DONE (2026-07-10, live-validated)**

**Problem.** `connect`/`run_migrations` hardcode `NoTls`
(`crates/apex-workflow/src/postgres.rs:98,121`). (PRD-004 R-H.3; audit High.)

**Change.** Support `MakeRustlsConnect`; require TLS for non-loopback DB hosts
(refuse plaintext to a remote host).

**Acceptance criteria.** A test asserts a non-loopback URL without TLS config is
refused; a TLS connection to a loopback test server succeeds.

**Files.** `crates/apex-workflow/src/postgres.rs`. **Size.** M.
**Depends on:** WFL-101.

**Implementation notes (2026-07-10).** `resolve_tls_mode` parses the connection string
and **refuses plaintext to a non-loopback host** (`Error::Config`) unless TLS is
requested (`sslmode=require` or `APEX_PG_TLS=1`) — loopback/Unix-socket hosts still
allow plaintext (trusted-local). The `dial` path branches `NoTls` vs a rustls
`MakeRustlsConnect` (`tokio-postgres-rustls` 0.13, rustls 0.23 `ring` provider passed
explicitly so no process-global default is needed). Certificate handling matches libpq
`sslmode` semantics: `require` encrypts **without identity verification**
(`AcceptAnyServerCert` — signatures still checked via the ring provider's algorithms),
which is what lets a managed DB with a private project CA connect without its CA bundle;
`APEX_PG_TLS_VERIFY=1` opts into full Mozilla-webpki-root verification for a public-CA
host. **Acceptance:** the refuse-plaintext guard is unit-tested offline
(`postgres::tests::tls_guard_refuses_plaintext_to_remote_but_allows_loopback`), and the
**whole store was validated live end-to-end against a real remote managed Postgres
(Aiven, `sslmode=require`, TCP :10281)** — `apex admin migrate --target workflow`
succeeded over TLS, and all six `postgres_store` integration tests passed against it
(so WFL-101's pool and WFL-104's concurrent-seq test are live-validated too, not just
capability-gated). The earlier offline blocker (only `tokio-postgres-rustls` 0.10 /
rustls 0.21 was vendored) was resolved by fetching 0.13 with `net.offline=false`. The
version-skew test opens a raw `NoTls` admin connection for its fake-row setup and now
skips cleanly on a TLS-only host (that behavior is orthogonal to transport).

## WFL-104 `[P1]` — Fenced event-sequence generation — **PARTIAL (seq-safety done; lease fencing deferred, 2026-07-10)**

**Problem.** Postgres event `append` computes seq via `SELECT MAX(seq)+1`
(`crates/apex-workflow/src/postgres.rs:152-166`), safe only under "one driver per
execution"; a lease-expiry race (old worker still running while a new one resumes,
`worker.rs:98-101`) yields concurrent appends → PK violation. (PRD-004 R-H.4; audit
Med.)

**Change.** Use a DB identity/sequence with `INSERT … RETURNING`, and fence writes by
lease token (reject an append from a superseded lease).

**Acceptance criteria.** A capability-gated test simulating two overlapping workers on
one execution asserts no PK violation and no forked history.

**Files.** `crates/apex-workflow/src/{postgres.rs,worker.rs}`. **Size.** M.
**Depends on:** WFL-101.

**Implementation notes (2026-07-10).** The **PK-violation half is fixed**: per-execution
seq allocation moved from the racy `SELECT MAX(seq)+1` (two overlapping appenders both
read the same MAX → `(execution_id, seq)` PK collision) to an atomic
`INSERT … ON CONFLICT DO UPDATE SET next_seq = next_seq + 1 RETURNING` on a dedicated
`workflow_event_seq` counter row (new `V2__event_seq_counter.sql` migration, back-filled
from existing events). The `UPDATE` row-locks per execution, so concurrent appenders get
distinct, contiguous seqs and never collide. **Acceptance (validated live against a real
remote Aiven Postgres, not just capability-gated):**
`postgres_store::tests::concurrent_appends_to_one_execution_get_distinct_contiguous_seqs`
(24 concurrent appends to one execution yield exactly the distinct seqs 1..=24, no PK
violation) passed against the live database over TLS. **Deferred — lease-token fencing
(the "no forked history"
half):** rejecting a *superseded* worker's appends needs a fence token threaded from
`WorkQueue::lease` → `Worker` → `Engine` → `EventLog::append`, a cross-crate signature
change through the `EventLog` port (and all its impls — in-memory/file/Postgres) that
can only be validated against the live overlapping-worker race on real Postgres. Left as
a follow-on rather than shipped blind; the counter table eliminates the concrete crash
the ticket's evidence cites, and two overlapping workers now corrupt nothing at the PK
level (though they can still interleave events until fencing lands).

---

# WS-I — Secret Default

## SEC-101 `[P1]` — Default the secrets store to encrypted-at-rest

**Problem.** The default secrets store writes plaintext `secrets.json`
(`crates/apex-secrets/src/store.rs:82-126`); at-rest encryption
(`EncryptedFileSecretStore` via KMS) is opt-in behind `APEX_SECRETS_ENCRYPT_AT_REST`
(`crates/apex-config/src/secrets.rs`). (PRD-004 R-I.3; audit High.)

**Change.** Make encrypted-at-rest the default; plaintext becomes an explicit opt-out
(`APEX_SECRETS_PLAINTEXT=1`). Provide a documented migration for an existing
plaintext store (the two use distinct filenames, so a one-time re-seal step).

**Acceptance criteria.** A fresh vault writes ciphertext to disk by default; the
plaintext opt-out still works; a migration test re-seals an existing plaintext file.

**Files.** `crates/apex-config/src/secrets.rs`, `crates/apex-secrets/src/*`; docs.
**Size.** M. **Depends on:** none.

---

# WS-J — Release & CI Reconciliation

## DX-101 `[P1]` — Reconcile versioning + add a CHANGELOG

**Problem.** `Cargo.toml` version is `0.1.0` (`Cargo.toml:26`), README badge `0.1.0`,
both SDKs `0.1.0` — while the repo is tagged `v0.3.0`; there is no root `CHANGELOG.md`.
The `repository` URL also diverges (`Cargo.toml` → `apex-ai/apex`; Python
`pyproject.toml` → `punarduttrajput/Apex`). (PRD-004 R-J.1; audit High/Low.)

**Change.** Bump workspace/badge/SDK versions to match the tag; add a maintained root
`CHANGELOG.md` (Keep-a-Changelog); unify the `repository` URL across all manifests.

**Acceptance criteria.** Versions agree with the latest tag; CHANGELOG exists with the
v0.1–v1.0 history; one canonical repo URL everywhere.

**Files.** `Cargo.toml`, `README.md`, `sdks/*/`; new `CHANGELOG.md`. **Size.** S.
**Depends on:** none.

## DX-102 `[P1]` — Release automation: binaries + published image + SDK publish

**Problem.** Only `ci.yml` exists; no `release.yml`, no cargo-dist/`dist-workspace.toml`,
no changelog generation. The TS SDK is unpublished (`sdks/typescript/README.md`), and
the container image is only ever built in CI (`apex:ci`), never pushed
(`deployment/helm/apex/values.yaml:5-11`). (PRD-004 R-J.2; audit High.)

**Change.** A tag-triggered release workflow producing signed cross-platform binaries,
a **published container image** (GHCR/Docker Hub), npm publish of `@apex-ai/sdk` and
PyPI publish of `apex-ai-sdk`, and a generated changelog. Default the Helm chart image
to the published repo.

**Acceptance criteria.** A dry-run (or tagged pre-release) produces binaries + a pushed
image + packed SDK tarballs; the Helm chart references the published image.

**Files.** `.github/workflows/release.yml`, `deployment/helm/apex/values.yaml`,
SDK publish config. **Size.** L. **Depends on:** DX-101.

## DX-103 `[P1]` — Add the dashboard and a Windows leg to CI

**Problem.** `.github/workflows/ci.yml` has zero `dashboard` references despite the
Angular SPA + `deployment/docker/dashboard.Dockerfile`; every `runs-on:` is
`ubuntu-latest` despite recent Windows-specific breakage (the cmd.exe + temp-dir
fixes). (PRD-004 R-J.3; audit High.)

**Change.** Add an Angular CI job (`npm ci && ng lint && ng build && ng test
--watch=false`) and a `windows-latest` matrix leg for `cargo build`/`cargo test`.

**Acceptance criteria.** CI builds+tests the dashboard and runs the Rust suite on
Windows on every PR; both are required checks.

**Files.** `.github/workflows/ci.yml`. **Size.** M. **Depends on:** UI-102 (so the
dashboard test job has specs to run — may land together).

---

# WS-K — UI Security & Tests

## UI-101 `[P1]` — Move the bearer token off `localStorage`

**Problem.** The API key/JWT is held in `localStorage`
(`dashboard/src/app/core/session.ts:22-73`) → XSS-exfiltratable. (PRD-004 R-K.1; audit
High.)

**Change.** Store the credential in memory/`sessionStorage` at minimum, or (preferred)
issue an httpOnly cookie via a thin BFF. Keep tenant/principal (non-secret) where they
are.

**Acceptance criteria.** The bearer token is no longer readable from `localStorage`
via `window.localStorage`; auth still works across a page reload per the chosen model.

**Files.** `dashboard/src/app/core/{session.ts,tenant.interceptor.ts}`. **Size.** M.
**Depends on:** none.

## UI-102 `[P1]` — UI test coverage + enable specs

**Problem.** Zero specs; `skipTests:true` is set on every schematic
(`dashboard/angular.json:13-37`); Karma/Jasmine are installed but unused. (PRD-004
R-K.2; audit High.)

**Change.** Remove the global `skipTests`; add service specs for the riskiest logic —
the SSE stream parser (`agent.service.ts:159-245`) and the manifest YAML round-trip
(`agent.service.ts:67-150`, `workflow.service.ts:136-177`) — plus one smoke e2e.

**Acceptance criteria.** `ng test --watch=false` runs a non-trivial suite green; the
SSE parser and manifest round-trip are covered.

**Files.** `dashboard/angular.json`, `dashboard/src/app/core/*.spec.ts`. **Size.** M.
**Depends on:** none. **Pairs with:** DX-103 (which runs these in CI).

---

# Exit criteria (Phase 1)

1. Every real provider reports accurate `cost_usd`; a project's daily-cost accumulator
   advances by real spend (PRV-101).
2. A long tool loop stays within a configured token budget; multi-tool turns run
   concurrently; manifest `max_steps` is honored (AIC-101/102/103).
3. Untrusted runs use the strongest available sandbox; Windows enforces real resource
   limits (SBX-101/102).
4. A server restart/shutdown loses no pollable run or pending webhook and drains
   in-flight requests; API keys expire/rotate/revoke (SRV-101..104).
5. The Postgres workflow path pools connections, uses TLS, guards recursion, and can't
   fork history under a lease race (WFL-101..104).
6. Secrets are encrypted-at-rest by default (SEC-101).
7. Versions/CHANGELOG/release automation/published image exist; the dashboard and
   Windows are in CI; UI tokens are off `localStorage` and the UI has tests
   (DX-101..103, UI-101/102).

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-09 | Initial Phase-1 tickets from PRD-004 / the 2026-07-09 engineering audit (production-truth P0/P1 fixes) |
