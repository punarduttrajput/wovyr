<!--
File: docs/18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md
Document ID: RM-AIM-P3
-->

# Phase 3 — Ecosystem & Scale: Implementation Tickets

**Document ID:** RM-AIM-P3
**File Path:** `docs/18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md`
**Version:** 1.2.0
**Status:** In progress — ECO-301, ECO-302 done; everything else planned
**Owner:** Engineering (Ecosystem / Platform / DX / Frontend)
**Last Updated:** 2026-07-14

---

# Purpose

Phase 3 of [PRD-004 §8](../../01-product/prd-ai-platform-maturity.md) — the broad,
largely-parallel tail: ecosystem connectivity (MCP, plugin SDK, richer tools),
workflow expressiveness + scale hardening, SDK/DX/docs parity, UI maturity
(component system, audit viewer, responsive, a11y), and operability
(systemd/runbooks/gauges).

Covers **WS-E/R-E.3..6**, **WS-F**, **WS-H/R-H.5..8**, **WS-G/R-G.8**, **WS-I/R-I.4/5**,
**WS-C/R-C.6**, **WS-J/R-J.4..7**, **WS-K/R-K.3..8**, **WS-L/R-L.2..5**.

Format matches [RM-GA-P2](../v1.0/phase2-durability-execution-tickets.md). These are
mostly independent; group by team and land in any order after Phases 1–2.

---

# WS-F — Ecosystem & Extensibility

## ECO-301 `[P1]` — MCP (Model Context Protocol) client tool-source — **DONE (2026-07-14)**

**Problem.** No MCP or external tool-server client exists; the `RemoteWorker` sandbox
backend is an unimplemented enum variant (`crates/apex-tools/src/sandbox/types.rs:19`);
MCP appears only under `docs/`. There is no way to connect external tools. (PRD-004
R-F.1; audit High.)

**Change.** Add an MCP client (stdio + HTTP transports) that discovers a remote
server's tools and proxies them into `ToolRegistry` as `Tool` impls, respecting the
permission model.

**Acceptance criteria.** A test connects to a mock MCP server, lists its tools, and
invokes one through `ToolRegistry`; permissions are enforced.

**Files.** `crates/apex-tools/src/` (new `mcp.rs`). **Size.** L. **Depends on:** none.

**Implementation notes (2026-07-14).** New `crates/apex-tools/src/mcp.rs`, zero
new dependencies (reqwest/tokio/serde were already in the crate). An
`McpTransport` trait (JSON-RPC 2.0 `request`/`notify`) with two impls:
**`StdioTransport`** — spawns a child process (`kill_on_drop`), speaks
newline-delimited JSON-RPC over its stdin/stdout, serialized behind a lock;
unsolicited server *notifications* are skipped and server→client *requests*
(sampling etc.) are answered `-32601 method not found` so a compliant server
isn't left hanging; built generically over `AsyncRead`/`AsyncWrite`
(`from_streams`), so the framing is unit-tested against an in-memory
`tokio::io::duplex` pair with no real process — and **`HttpTransport`** —
JSON-RPC POSTs in the streamable-HTTP *plain-JSON* mode, capturing the
server-assigned `Mcp-Session-Id` from any response and echoing it on every
subsequent request; a notification's empty-body `202` ack is accepted; a
server answering `text/event-stream` (the spec's optional SSE mode,
deliberately out of scope) gets a clear "not supported" error, never a hang
or a mangled parse. **`McpClient::connect`** runs the `initialize` handshake
(protocol `2025-03-26`) + `notifications/initialized`; the server name is
validated `[A-Za-z0-9_-]+` fail-closed since it namespaces ids and
permissions. `list_tools` drains `tools/list` cursor pagination (capped at 64
pages — a server that keeps returning a cursor is refused rather than spun
on); `call_tool` passes the MCP result object through as the payload with
`isError: true` mapped to an *unsuccessful* `ToolResponse` — a tool-level
failure the model should see, distinct from a JSON-RPC protocol error, which
maps onto the platform's standard categories (`-32600`/`-32602` →
permanent `Validation`; anything else → retryable `Internal`; transport
failures → `Network`) so workflow retry classification (RUN-201) applies
sensibly to remote tools. Every request is bounded by a timeout
(`with_timeout`, default 30 s). **Registry integration:**
`discover_tools`/`register_into` proxy each discovered tool as an `McpTool`
with id `mcp__<server>__<tool>` (sanitized to model-callable chars; the
namespace means a remote tool can never silently shadow a built-in),
category `mcp`, the server's declared version, and declared permissions
`["mcp:<server>"]` by default (`with_tool_permissions` overrides) — enforced
by the existing `ToolRegistry::execute` fail-closed path, no new enforcement
code. Proven by 9 unit tests (duplex-scripted stdio server: handshake +
paginated list + call round-trip, noise-skipping, error mapping, `isError`
mapping, name validation, pagination guard, id sanitization) and the
acceptance integration test `crates/apex-tools/tests/mcp_tools.rs` — a
hand-rolled HTTP/1.1 mock MCP server over a real `TcpListener` (the S3-signer
stance: no test-framework dependency) that *rejects any post-initialize
request missing the session id*, against which the client discovers, lists,
and invokes through `ToolRegistry` with the grant present, and — the
fail-closed half — an ungranted call is denied with the mock's request log
proving **no `tools/call` ever reached the wire**. Programmatic only (same
stance as SAF-201/202): no agent-manifest/server/CLI configuration surface
for MCP connections yet, and the `RemoteWorker` sandbox variant is untouched
— follow-ons.

## ECO-302 `[P1]` — Plugin authoring SDK + `apex plugin new` scaffold — **DONE (2026-07-14)**

**Problem.** No authoring SDK/scaffold — only format docs; authors hand-write
`plugin.yaml`, compile `wasm32-wasi`, and hand-embed `sha256:` digests
(`apps/apex-cli/src/plugin.rs`; sample is hand-authored `.wat`). (PRD-004 R-F.2; audit
High.)

**Change.** Ship an `apex-plugin-sdk` crate (typed capability entry points) + an
`apex plugin new` scaffold generating a manifest, a buildable wasm project, and a
build step that computes digests.

**Acceptance criteria.** `apex plugin new foo` → `cargo build` → `apex plugin install`
round-trips with no hand-edited digests.

**Files.** new `crates/apex-plugin-sdk`, `apps/apex-cli/src/plugin.rs`. **Size.** L.
**Depends on:** none.

**Implementation notes (2026-07-14).** Two halves. **`crates/apex-plugin-sdk`**
(new workspace crate, deliberately tiny — serde + serde_json only, no
apex-common, so a plugin author's dependency tree stays clean and everything
compiles to `wasm32-wasip1`): `run_tool(handler)` is the typed entry point
wrapping the platform's capability ABI (request JSON on stdin → typed
handler → response JSON on stdout; a handler error prints to stderr and
exits non-zero, which the WASI loader surfaces as the tool failure's
detail), built on a pure `respond(input, handler)` core so handlers
unit-test on the host with no wasm build or stdin pipe; `secret(name)` /
`secret_env_var(name)` read platform-injected secrets with the exact
`APEX_SECRET_<UPPER_SNAKE>` mangling `apex-plugin`'s `resolve_secret_env`
uses (mirrored in a test). **The scaffold + build step**
(`apps/apex-cli/src/scaffold.rs`, wired as `apex plugin new` / `apex plugin
build`): `new` generates a buildable project — `Cargo.toml` (SDK dependency;
`--sdk-path` emits a local `path` dep, needed until the SDK is published to
crates.io), a typed greeter `src/main.rs`, a valid `plugin.yaml` whose
`artifacts` list is deliberately **empty** (digests are computed, never
hand-edited), `.gitignore`, and a README walking sign → trust → install →
enable → run; it fails closed on an existing directory and validates the
name `[a-z][a-z0-9_-]*` (it doubles as the crate name and capability-id
prefix). `build` compiles the project (`cargo build --release --target
wasm32-wasip1`, explicit `--target-dir` so a caller's `CARGO_TARGET_DIR`
can't hide the artifact; a missing-target failure gets a `rustup target add`
hint), locates the module (package-name parse with a single-`.wasm`-glob
fallback), and stages `dist/`: the module beside a rewritten `plugin.yaml`
carrying the computed `sha256:` digest — exactly one distinct wasm entry
per project is supported (shared entries across capabilities are fine),
fail-closed otherwise. The existing supply chain then applies unchanged:
`sign` → `trust` → `install`. **The acceptance round trip runs for real**,
not mocked (`scaffold::tests::scaffolded_project_builds_signs_and_installs_
with_no_hand_edited_digests`): scaffold → real nested `cargo build` to wasm
(offline-safe: warm registry cache + path SDK dep) → `keygen_cmd`/`sign_cmd`
(the real CLI signing commands) → `read_package_dir` + `PluginEngine::
install` (the same verify-signature → verify-digest → stage → register core
`install_cmd` runs), against scratch directories so the test never touches
the real `~/.apex`; the staged digest is asserted equal to a recomputed
digest of the staged module. Under `--features plugin-wasi` the same test
additionally enables the plugin and executes it through `ToolRegistry`,
proving the scaffolded, SDK-built module really answers (`{"greeting":
"Hello, Apex!"}` through a real Wasmtime run — verified locally, 24 s).
Skips cleanly when the wasm target/cargo are unavailable (the established
capability-gated pattern); CI's rust job now installs `wasm32-wasip1` via
the toolchain action so the round trip runs on every PR (the Windows leg
skips — no double-build cost). Not done here (later slices): `apex plugin
publish` one-shotting sign+digest+trust output (ECO-304, which depends on
this), SBOM/provenance auto-fill in `build`, and publishing the SDK crate
to crates.io.

## ECO-303 `[P2]` — Container capability loader

**Problem.** Only `WasiCapabilityRuntime` exists; without the `wasi` feature the
default `NotLoadedRuntime` errors on call (`crates/apex-plugin/src/{lib.rs:31-34,
runtime.rs:20-33}`). (PRD-004 R-F.3; audit Med.)

**Change.** Add a container capability loader reusing `ContainerSandbox` (SBX-101).

**Acceptance criteria.** A gated test runs a container-backed capability end to end.

**Files.** `crates/apex-plugin/src/runtime.rs`. **Size.** M. **Depends on:** SBX-101.

## ECO-304 `[P2]` — One-shot `apex plugin publish`

**Problem.** Publishing is multi-step and manual: `keygen` → `sign` → operator
`trust`, with manual digest embedding (`apps/apex-cli/src/plugin.rs:163-208`). (PRD-004
R-F.4; audit Med.)

**Change.** A one-shot `apex plugin publish` that signs, fills digests, and emits the
trust snippet.

**Acceptance criteria.** One command produces a signed, digest-complete package + the
trust line an operator pastes.

**Files.** `apps/apex-cli/src/plugin.rs`. **Size.** M. **Depends on:** ECO-302.

## ECO-305 `[P3]` — Marketplace OSV/CVE feed

**Problem.** The scanner is static-only (manifest/digest/SBOM-deny-list/wildcard-perm),
with a manually-maintained deny-list (`crates/apex-marketplace/src/scan.rs:14,79-172`).
(PRD-004 R-F.5; audit Med.)

**Change.** Integrate an OSV/CVE feed keyed on SBOM `name@version`; optionally add
wasm import-analysis for undeclared syscalls.

**Acceptance criteria.** A test flags a known-vulnerable SBOM component via the feed.

**Files.** `crates/apex-marketplace/src/scan.rs`. **Size.** M. **Depends on:** none.

---

# WS-E — Richer Tools

## SBX-301 `[P2]` — Confined `fs_write` builtin

**Problem.** No write tool; write access explicitly deferred
(`crates/apex-tools/src/builtin.rs:5-8`). (PRD-004 R-E.3; audit Med.)

**Change.** Add an `fs_write` builtin confined via the existing `confine_path`
(the same canonicalize-and-prefix guard `fs_read` uses), opt-in like `shell`.

**Acceptance criteria.** A test writes inside the workspace root and is denied outside
it (symlink-escape included).

**Files.** `crates/apex-tools/src/builtin.rs`. **Size.** M. **Depends on:** none.

## SBX-302 `[P2]` — Sandboxed code-execution tool

**Problem.** No code-exec/python tool. (PRD-004 R-E.4; audit Med.)

**Change.** A code-execution tool routed through the sandbox (SBX-101), language
runtime configurable, resource-limited and egress-controlled.

**Acceptance criteria.** A gated test runs a snippet in the sandbox and captures
stdout/exit; resource limits apply.

**Files.** `crates/apex-tools/src/builtin.rs` + sandbox wiring. **Size.** L.
**Depends on:** SBX-101.

## SBX-303 `[P2]` — `#[derive(Tool)]` / schemars ergonomics

**Problem.** Authors hand-write JSON Schema as `json!` literals and parse params with
`.get().and_then()` (`crates/apex-tools/src/tool.rs:177-190`; e.g.
`builtin.rs:120-124`). (PRD-004 R-E.5; audit Med.)

**Change.** A proc-macro / `schemars`-based derive generating schema + typed param
deserialization from a struct.

**Acceptance criteria.** A tool defined via the derive round-trips schema + typed args
with no hand-written JSON.

**Files.** new derive crate + `crates/apex-tools/src/tool.rs`. **Size.** M.
**Depends on:** none.

## SBX-304 `[P2]` — Egress platform matrix + fail-closed

**Problem.** `egress_lockdown` is Linux/Docker-only; on Windows/macOS the L3 egress
protection silently doesn't exist (`crates/apex-tools/src/lib.rs:14-18`,
`sandbox/types.rs:243-259`). (PRD-004 R-E.6; audit Med.)

**Change.** Document the platform matrix; **fail closed** (refuse a non-empty
`NetworkPolicy`) when lockdown is unavailable, rather than silently allowing egress.

**Acceptance criteria.** A test asserts a network-policy run on a non-lockdown platform
is refused, not silently unrestricted.

**Files.** `crates/apex-tools/src/{lib.rs,sandbox/*}`. **Size.** S. **Depends on:** none.

---

# WS-H — Workflow Expressiveness & Scale

## WFL-301 `[P1]` — Loop / for-each activity

**Problem.** The DAG is strictly acyclic with a static activity list
(`crates/apex-workflow/src/definition.rs:62,236`); no map-over-collection. (PRD-004
R-H.5; audit High.)

**Change.** A `map`/`for_each` activity that expands over a runtime collection into a
bounded sub-DAG (concurrency-capped), results collected in order.

**Acceptance criteria.** A workflow maps an activity over an N-element input and joins
N results deterministically.

**Files.** `crates/apex-workflow/src/{definition.rs,engine.rs}`. **Size.** L.
**Depends on:** none.

## WFL-302 `[P1]` — Dynamic (data-driven) fan-out

**Problem.** The concurrent `ready_batch` is only over statically-declared activities
(`engine.rs:686,1099-1121`); K can't be derived from data. (PRD-004 R-H.6; audit High.)

**Change.** Support spawning K instances of one activity keyed by an input array
(the runtime companion to WFL-301).

**Acceptance criteria.** A workflow fans out to a data-determined K and joins.

**Files.** `crates/apex-workflow/src/engine.rs`. **Size.** L. **Depends on:** WFL-301.

## WFL-303 `[P2]` — Checkpoint size cap + out-of-line large outputs

**Problem.** Every step re-serializes and upserts the entire `ExecutionState`; activity
outputs merge into `variables` unbounded with no cap (`engine.rs:843,1387`;
`postgres.rs:186`). (PRD-004 R-H.7; audit Med.)

**Change.** Cap serialized snapshot size (fail-closed with a clear error) and/or store
large activity outputs out-of-line (blob ref).

**Acceptance criteria.** A test asserts an over-cap output is rejected or externalized,
not silently bloating every checkpoint.

**Files.** `crates/apex-workflow/src/{engine.rs,postgres.rs,store.rs}`. **Size.** M.
**Depends on:** none.

## WFL-304 `[P2]` — Event-log compaction + paged load

**Problem.** Append-only with no retention; `load`/`history` deserialize every event
(`store.rs:225`; `postgres.rs:169`). (PRD-004 R-H.7; audit Med.)

**Change.** Add event compaction/retention and a bounded/paged `load`.

**Acceptance criteria.** A test asserts `history` pages and that recovery doesn't read
the full log for a long execution.

**Files.** `crates/apex-workflow/src/{store.rs,postgres.rs}`. **Size.** M.
**Depends on:** none.

## WFL-305 `[P2]` — Indexed `list()` columns + SQL-side filtering

**Problem.** `list()` scans and decodes every checkpoint, filtering in Rust
(`postgres.rs:216-238`; `store.rs:259-278`). (PRD-004 R-H.7; audit Med.)

**Change.** Promote `workflow_name`/`status` to indexed columns; push filtering +
pagination into SQL.

**Acceptance criteria.** A test asserts filtered `list()` doesn't load non-matching
rows; a migration adds the columns/indexes.

**Files.** `crates/apex-workflow/src/postgres.rs` + migration. **Size.** M.
**Depends on:** none.

## WFL-306 `[P2]` — `fire_at`-indexed timers + adaptive dispatch

**Problem.** Dispatch accuracy is bounded by the poll interval (default 5s) and each
poll is O(N) — `due()`/schedule `poll` load all pending timers/schedules
(`crates/apex-workflow/src/{timer.rs:207-222,schedule.rs:307}`; interval
`lib.rs:313`). (PRD-004 R-H.7; audit Med.)

**Change.** Index timers by `fire_at`; sleep until the next deadline instead of a
fixed interval.

**Acceptance criteria.** A test asserts due-timer lookup is bounded (not full-scan) and
a near-deadline timer fires promptly.

**Files.** `crates/apex-workflow/src/{timer.rs,schedule.rs,lib.rs}`. **Size.** M.
**Depends on:** none.

## WFL-307 `[P3]` — Activity progress events

**Problem.** `ActivityExecutor::execute` returns a single `Value`; no progress channel
(`crates/apex-workflow/src/executor.rs:69-72`; `event.rs:27-83`). (PRD-004 R-H.8; audit
Low.)

**Change.** Add an optional progress sink to `ActivityContext` + an `ActivityProgress`
event.

**Acceptance criteria.** A test asserts a long activity emits progress events.

**Files.** `crates/apex-workflow/src/{executor.rs,event.rs}`. **Size.** M.
**Depends on:** none.

## WFL-308 `[P3]` — Event-enum schema versioning

**Problem.** The event enum wire format has no version tag; a rename breaks the on-disk
log (`crates/apex-workflow/src/event.rs:18-24`). (PRD-004 R-H.8; audit Low.)

**Change.** Add a schema-version tag + a migration path before any future rename.

**Acceptance criteria.** A test round-trips a versioned event and rejects an unknown
future version cleanly.

**Files.** `crates/apex-workflow/src/event.rs`. **Size.** S. **Depends on:** none.

---

# WS-G — Server Health (R-G.8 cluster)

## SRV-302 `[P2]` — Cache `FileApiKeyStore` in memory

**Problem.** `principal_for` calls `self.load()` per authenticated request — disk I/O +
full deserialize on the hot auth path (`crates/apex-server/src/auth.rs:315-318,291-296`).
(PRD-004 R-G.8; audit Med.)

**Change.** Cache the key map in memory with file-watch/invalidation; O(1) lookup.

**Acceptance criteria.** A test asserts no per-request file read after warm-up and that
an external key change is picked up.

**Files.** `crates/apex-server/src/auth.rs`. **Size.** S. **Depends on:** SRV-104.

## SRV-303 `[P2]` — Serve a generated OpenAPI spec

**Problem.** No `utoipa`/OpenAPI generation; `openapi.yaml` is hand-synced
(audit grep). (PRD-004 R-G.8; audit Med.)

**Change.** Generate the OpenAPI doc from route/handler types and serve it at
`/openapi.json`; keep the CI contract gate (redocly) against the generated doc.

**Acceptance criteria.** The served spec matches the handlers; the contract gate runs
against it.

**Files.** `crates/apex-server/src/*` + `docs/09-api/openapi.yaml` pipeline.
**Size.** L. **Depends on:** none.

## SRV-304 `[P2]` — Extract the inline `lib.rs` test suite

**Problem.** `lib.rs` is ~86% inline test code (~2,260 of 2,618 lines,
`crates/apex-server/src/lib.rs:356`→EOF). (PRD-004 R-G.8; audit Med.)

**Change.** Move the suite to `tests/` or a `tests.rs` submodule (widening the few
`pub(crate)` fields the tests reach only as needed).

**Acceptance criteria.** `lib.rs` production module is navigable; the suite still runs.

**Files.** `crates/apex-server/src/lib.rs` (+ new test module). **Size.** M.
**Depends on:** none.

## SRV-305 `[P2]` — Idempotency store write-amplification

**Problem.** `put` does a full-file `atomic_write` of the whole map per mutating
request (`crates/apex-server/src/hardening.rs:230-266`). (PRD-004 R-G.8; audit Med.)

**Change.** Use an append/segmented store or debounce persistence.

**Acceptance criteria.** A test asserts a mutating request doesn't rewrite the entire
cache file each time.

**Files.** `crates/apex-server/src/hardening.rs`. **Size.** M. **Depends on:** none.

## SRV-306 `[P3]` — Request-path unwrap audit

**Problem.** ~317 `.unwrap()`/`unreachable!()`/`.expect()` across the crate; some on
live paths (e.g. `agents.rs:218,220`; `webhooks.rs:56`). (PRD-004 R-G.8; audit Low.)

**Change.** Audit request-adjacent unwraps; return `ApiError` instead of panicking.

**Acceptance criteria.** Live-path unwraps are eliminated or justified; a clippy
lint/CI check guards new ones on handler paths.

**Files.** `crates/apex-server/src/{agents.rs,webhooks.rs,...}`. **Size.** M.
**Depends on:** none.

## SRV-307 `[P3]` — Shared concurrency slots

**Problem.** `QuotaTracker.concurrent` is in-process/per-node
(`crates/apex-server/src/state.rs`; `tenancy.rs:432,516-527`); N nodes multiply the
effective limit. (PRD-004 R-G.6; audit Low.)

**Change.** Track concurrency in a shared store for multi-node correctness.

**Acceptance criteria.** A gated test asserts two nodes share one concurrency budget.

**Files.** `crates/apex-server/src/tenancy.rs`. **Size.** M. **Depends on:** SRV-201.

---

# WS-I / WS-C — Audit, Secret Channel, Re-embedding

## SEC-301 `[P2]` — Audit query: time-range + pagination + indexed sink

**Problem.** `query()` loads the entire log via `sink.all()` then filters in memory;
`AuditFilter` has only tenant/principal/action/limit — no from/to, no cursor; the JSONL
sink re-reads the whole file per op (`crates/apex-audit/src/log.rs:116-140,220-238`).
(PRD-004 R-I.4; audit Med.)

**Change.** Add time-range + cursor pagination and an indexed/DB-backed sink option.

**Acceptance criteria.** A test asserts a time-ranged, paged query doesn't scan the
whole log.

**Files.** `crates/apex-audit/src/log.rs`. **Size.** M. **Depends on:** none.

## SEC-302 `[P3]` — Request-scoped secret channel

**Problem.** Secrets are injected into sandboxes as `APEX_SECRET_*` env vars
(`crates/apex-plugin/src/runtime.rs:55-59,102-110`); a verbose/compromised plugin can
echo its environment. (`SecretValue` is correctly masked in Debug/Display, so tracing
leakage is guarded — this is the child-process surface.) (PRD-004 R-I.5; audit Low.)

**Change.** Prefer a request-scoped secret channel (stdin/vsock) over env vars for
higher-isolation backends.

**Acceptance criteria.** A test asserts secrets reach the guest without appearing in
its environment for the vsock/stdin path.

**Files.** `crates/apex-plugin/src/runtime.rs`. **Size.** M. **Depends on:** none.

## RAG-301 `[P3]` — Incremental re-embedding / model migration

**Problem.** Changing embedding models doesn't re-embed stored records; the store can
silently mix vector dimensionalities. (PRD-004 R-C.6; audit Low.)

**Change.** A re-embedding job that migrates a namespace to a new model, with the
embedding-model id (from RAG-203) driving detection.

**Acceptance criteria.** A test migrates a namespace's embeddings to a new model and
verifies uniform dimensionality after.

**Files.** `crates/apex-memory/src/engine.rs`. **Size.** M. **Depends on:** RAG-203.

---

# WS-K — Dashboard Maturity

## UI-301 `[P2]` — Shared component library

**Problem.** Primitives are CSS classes only; every feature re-implements tabs, modals,
tables, and status-pill mapping (`statusClass()` duplicated verbatim in
`monitoring.ts:63-79`, `execution-detail.ts:46-62`, `workflow-builder.ts:516-524`);
destructive uninstall uses native `confirm()` (`marketplace.ts:184`). (PRD-004 R-K.3;
audit Med/Low.)

**Change.** Extract shared Angular components — StatusPill, Tabs, Modal, Table, and
empty/loading/error primitives — and an in-app confirm dialog.

**Acceptance criteria.** The duplicated `statusClass`/`errText`/status-string patterns
are replaced by shared components; no native `confirm()`.

**Files.** `dashboard/src/app/shared/*`, feature components. **Size.** M.
**Depends on:** UI-102.

## UI-302 `[P2]` — Share SDK types, real YAML, central error handling

**Problem.** `api.types.ts` hand-duplicates server shapes (unused `sdks/typescript`
exists); manifest/workflow (de)serialize YAML via regex + `join`
(`agent.service.ts:67-150`, `workflow.service.ts:136-177`); identical `errText()`/`fail()`
copied in ≥4 components; many `error: () => {}` no-ops swallow failures. (PRD-004 R-K.4;
audit Med.)

**Change.** Consume/generate types from `sdks/typescript` or OpenAPI; use a real YAML
lib; add a central HTTP error interceptor + helper; route swallowed errors through the
toast/logging service.

**Acceptance criteria.** Types come from one source; YAML round-trips via a library; a
failed poll surfaces (not silently swallowed).

**Files.** `dashboard/src/app/core/*`, feature services. **Size.** M.
**Depends on:** none.

## UI-303 `[P2]` — Audit-log viewer

**Problem.** No surface consumes an audit/events endpoint despite the RBAC/tenancy
backend (no `audit` route in `dashboard/src/app/app.routes.ts`). (PRD-004 R-K.5; audit
High.)

**Change.** A read-only, RBAC-gated audit-log viewer (backed by SEC-301's paged query).

**Acceptance criteria.** The viewer lists/pages audit entries for the current tenant.

**Files.** `dashboard/src/app/features/audit/*`, routes. **Size.** M.
**Depends on:** SEC-301.

## UI-304 `[P2]` — Responsive / mobile layout

**Problem.** The only media queries repo-wide are `prefers-reduced-motion`
(`dashboard/src/styles.scss:125`); the nav rail + multi-column canvases assume desktop.
(PRD-004 R-K.6; audit Med.)

**Change.** Add breakpoints + a collapsible nav rail; make canvases scroll/stack on
narrow viewports.

**Acceptance criteria.** The app is usable at mobile/tablet widths (no horizontal
body scroll; nav collapses).

**Files.** `dashboard/src/styles.scss`, layout components. **Size.** M.
**Depends on:** none.

## UI-305 `[P2]` — Accessibility pass

**Problem.** Only ~8 `aria/role/for` occurrences across feature templates; `.field
label`s aren't associated with inputs; nav/icon buttons are SVG-only
(`dashboard/src/styles.scss:99-102`). (PRD-004 R-K.7; audit Med.)

**Change.** Associate labels (`for`/`id`), add `aria-label`s to icon buttons, and add
modal focus management + keyboard nav.

**Acceptance criteria.** An axe/lighthouse a11y check passes the core flows; keyboard
navigation works for nav + modals.

**Files.** `dashboard/src/app/**`. **Size.** M. **Depends on:** UI-301.

## UI-306 `[P3]` — Playground, live nav badges, i18n decision, icon sprite

**Problem.** No dedicated prompt playground (bolted onto Agent Studio only,
`agent-studio.ts:130`); hardcoded fake nav badges (`app.ts:52,79`); `extract-i18n`
target configured but unused (`angular.json:101-103`); inline SVG icon strings nudge
the per-component style budget. (PRD-004 R-K.8; audit Med/Low.)

**Change.** Add a lightweight prompt playground; bind nav badges to real counts (or
remove); decide i18n (adopt or drop the target); move icons to a sprite.

**Acceptance criteria.** Badges reflect real data or are gone; i18n decision recorded;
icons served from a sprite.

**Files.** `dashboard/src/app/**`, `angular.json`. **Size.** M. **Depends on:** UI-301.

---

# WS-J — SDK & Docs Parity

## DX-301 `[P2]` — SDK parity: async Python, mutation retry, poll helper, TS paginateAll

**Problem.** Python is sync-only urllib (`sdks/python/apex_sdk/http.py`); TS has no
retry at all and Python retries GET-only; neither has a `wait_for_completion` poll
helper (`client.ts:174-181`, `client.py:181-184`); TS lacks `paginateAll`. (PRD-004
R-J.4; audit Med/Low.)

**Change.** Add an asyncio Python client; port GET retry to TS and add opt-in mutation
retry keyed by `Idempotency-Key`; add `wait_for_completion(execution_id)` to both; add
TS `paginateAll`.

**Acceptance criteria.** Both SDKs' integration suites cover a poll-to-completion and
(TS) retry; async Python client passes its own suite.

**Files.** `sdks/python/*`, `sdks/typescript/*`. **Size.** L. **Depends on:** none.

## DX-302 `[P2]` — Coverage + benchmark tracking in CI

**Problem.** CI runs `cargo test` but no coverage upload and no benchmark tracking
(`.github/workflows/ci.yml:68`). (PRD-004 R-J.4; audit Med.)

**Change.** Add `cargo-llvm-cov` coverage upload and a criterion benchmark-tracking job.

**Acceptance criteria.** CI publishes a coverage report and flags a benchmark regression.

**Files.** `.github/workflows/ci.yml`. **Size.** S. **Depends on:** none.

## DX-303 `[P2]` — SDK versioning + server-skew warning

**Problem.** Both SDKs pinned `0.1.0`, no CHANGELOG, no server-version compatibility
check; the Python README's PyPI-publish claim isn't corroborated by packaging
(`sdks/python/pyproject.toml`). (PRD-004 R-J.5; audit Med/Low.)

**Change.** Semver tied to API version + per-SDK CHANGELOG; warn on server/SDK skew via
the `health()` version; reconcile the PyPI claim with an actual publish (DX-102) or
soften the wording.

**Acceptance criteria.** SDK versions track the API; a skew emits a warning; the README
claim matches reality.

**Files.** `sdks/*/`. **Size.** S. **Depends on:** DX-102.

## DX-304 `[P2]` — Regenerate `docs/11-cli/commands.md` from the clap tree

**Problem.** `docs/11-cli/commands.md` documents many non-existent commands
(`init/context/config/doctor/...`, a `tools` group, `projects/users/apikeys`) and omits
real ones (`admin backup s3://`, `kms rotate`, `auth`, `dev`, `schedule`). (PRD-004
R-J.6; audit Med.)

**Change.** Regenerate the reference from the actual clap command tree; document the
real top-level groups.

**Acceptance criteria.** Every documented command exists; every real command is
documented; ideally a CI check diffs the doc against `--help` output.

**Files.** `docs/11-cli/commands.md`. **Size.** S. **Depends on:** none.

## DX-305 `[P3]` — Docs status front-matter + quickstart

**Problem.** The `docs/` tree mixes shipped and aspirational content (README flags it
at lines 148-150); README front-loads vision, not getting-started. (PRD-004 R-J.6;
audit Low.)

**Change.** Add per-doc `Status: shipped|aspirational` front-matter; add a top-of-README
5-minute quickstart (`apex dev` → `/healthz` → first agent run).

**Acceptance criteria.** Each spec doc declares its status; the README opens with a
runnable quickstart.

**Files.** `docs/**`, `README.md`. **Size.** M. **Depends on:** DX-304.

## DX-306 `[P3]` — Go/Java client decision

**Problem.** Only TS + Python clients exist; no decision recorded on further languages.
(PRD-004 R-J.7; audit Low.)

**Change.** Record a decision (roadmap or non-goal); if roadmap, scaffold one against
`openapi.yaml`.

**Acceptance criteria.** A decision doc/ADR exists; if built, the client passes the
contract gate.

**Files.** `docs/`, optionally `sdks/`. **Size.** S. **Depends on:** none.

---

# WS-L — Operability

## DEP-301 `[P1]` — systemd unit + install script for the appliance

**Problem.** The README markets a single-binary appliance but there is no `*.service`,
`install.sh`, or distro package under `deployment/`; only container/K8s paths exist.
(PRD-004 R-L.4; audit High.)

**Change.** Add a systemd unit + an install script (user, dirs, `~/.apex` perms, env
file) for the bare-metal appliance install.

**Acceptance criteria.** The unit starts/stops `apex dev`/`serve`; the script produces a
working install on a clean host (documented, ideally smoke-tested in CI on Linux).

**Files.** `deployment/systemd/*`, `deployment/install.sh`. **Size.** M.
**Depends on:** none.

## DEP-302 `[P2]` — Operator upgrade/migration runbook + Helm/Terraform

**Problem.** No upgrade-path/migration runbook tying version bumps to `apex admin
migrate`; Helm is single-replica with no in-chart TLS; no Terraform. (PRD-004 R-L.4/R-L.5;
audit Med/Low.)

**Change.** Write an operators' upgrade + backup/restore + schema-migration runbook;
template optional TLS in Helm; add a minimal Terraform module (or explicitly scope it
out with a note).

**Acceptance criteria.** The runbook covers an end-to-end upgrade; Helm can template
TLS; the Terraform decision is recorded.

**Files.** `docs/12-deployment/*`, `deployment/helm/*`, optionally `deployment/terraform/`.
**Size.** M. **Depends on:** none.

## OBS-301 `[P2]` — Queue-depth / in-flight / DLQ gauges

**Problem.** No gauges for workflow queue depth, pending timers, async-run backlog, or
webhook DLQ — only counters/histograms exist. (PRD-004 R-L.2; audit Med.)

**Change.** Expose gauges for queue depth, in-flight runs, pending timers, and DLQ size.

**Acceptance criteria.** `/metrics` exposes the gauges; a test asserts they move with
load.

**Files.** `crates/apex-server/src/*`, `crates/apex-telemetry/*`. **Size.** M.
**Depends on:** SRV-103.

## OBS-302 `[P3]` — Traces on store/queue/dispatch + SLO burn signals

**Problem.** Store/queue/dispatcher ops are un-instrumented (`postgres.rs`/`queue.rs`/
`timer.rs` have no spans); no SLO burn-rate metric or alert rules. (PRD-004 R-L.3/R-L.5;
audit Low.)

**Change.** Add spans around DB/queue/dispatch operations; ship a starter Prometheus
alert-rule file + Grafana dashboard JSON + multi-window burn-rate metrics under
`deployment/`.

**Acceptance criteria.** An end-to-end trace spans handler→store→queue; alert rules
lint and load.

**Files.** `crates/apex-workflow/src/*`, `deployment/observability/*`. **Size.** M.
**Depends on:** none.

---

# Exit criteria (Phase 3)

1. External tool servers (MCP) and container-backed plugins work; a plugin author
   scaffolds → builds → installs with no hand-edited digests (ECO-301..304).
2. The tool surface includes confined write + sandboxed code-exec, with ergonomic
   authoring and a documented egress matrix (SBX-301..304).
3. Workflows express loops + data-driven fan-out; the Postgres path is bounded and
   indexed at scale (WFL-301..308).
4. Server health items land (OpenAPI served, tests extracted, unwraps audited);
   audit is paginated; secrets have a non-env channel (SRV-302..307, SEC-301/302).
5. The UI has a shared component system, an audit viewer, responsive layout, a11y, and
   shares types with the SDK (UI-301..306).
6. SDK parity + versioning + accurate CLI/docs + a published-image release story
   (DX-301..306).
7. The appliance has a systemd/install path, an upgrade runbook, and operability
   gauges/traces (DEP-301/302, OBS-301/302).

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-09 | Initial Phase-3 tickets from PRD-004 / the 2026-07-09 engineering audit (ecosystem, scale, DX, UI, operability) |
| 1.1.0 | 2026-07-14 | ECO-301 (MCP client tool-source: stdio + streamable-HTTP transports, handshake/paginated discovery/`tools/call` proxying into `ToolRegistry` as permissioned `Tool` impls, fail-closed error mapping + timeouts) implemented and marked DONE with implementation notes — Phase 3 started |
| 1.2.0 | 2026-07-14 | ECO-302 (plugin authoring SDK: new `apex-plugin-sdk` crate with typed `run_tool` stdin/stdout entry point + secret helpers; `apex plugin new` scaffold + `apex plugin build` digest-computing wasm32-wasip1 build step; real scaffold→build→sign→install acceptance round trip, wasm target added to CI) implemented and marked DONE with implementation notes |
