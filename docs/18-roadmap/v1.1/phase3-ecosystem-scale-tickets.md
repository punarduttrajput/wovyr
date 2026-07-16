<!--
File: docs/18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md
Document ID: RM-AIM-P3
-->

# Phase 3 — Ecosystem & Scale: Implementation Tickets

**Document ID:** RM-AIM-P3
**File Path:** `docs/18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md`
**Version:** 1.9.0
**Status:** In progress — ECO-301..304 (all but ECO-305 of WS-F's ECO row), WFL-301..308 (all of WS-H), SBX-301..304 (all of WS-E), SRV-302..307 (all of WS-G), SEC-301, DEP-301 done; everything else planned
**Owner:** Engineering (Ecosystem / Platform / DX / Frontend)
**Last Updated:** 2026-07-16

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

## ECO-303 `[P2]` — Container capability loader — **DONE (2026-07-16)**

**Problem.** Only `WasiCapabilityRuntime` exists; without the `wasi` feature the
default `NotLoadedRuntime` errors on call (`crates/apex-plugin/src/{lib.rs:31-34,
runtime.rs:20-33}`). (PRD-004 R-F.3; audit Med.)

**Change.** Add a container capability loader reusing `ContainerSandbox` (SBX-101).

**Acceptance criteria.** A gated test runs a container-backed capability end to end.

**Files.** `crates/apex-plugin/src/runtime.rs`. **Size.** M. **Depends on:** SBX-101.

**Resolution.** `ContainerCapabilityRuntime` (`runtime.rs`, now compiled
unconditionally — the WASM loader stays behind `wasi`): Docker/Podman/gVisor
constructors over `ContainerSandbox`, speaking the exact WASM-loader ABI (request
JSON → stdin, response JSON ← stdout, `with_secrets` → `APEX_SECRET_*` env,
`with_limits`/`with_network` pass-through; shared `staged_entry` +
`capability_response` helpers so the two loaders can't drift). The staged artifact
dir is bind-mounted at `/workspace` and the entry runs inside the image (exec bit
stamped at invoke — staging preserves no mode bits). Routing is fail-closed both
ways: the container loader only accepts manifest `sandbox: container|gvisor`, the
WASM loader only `wasm|wasi|` — and a `gvisor` capability on a plain-Docker runtime
is refused, never silently demoted. Enabler in apex-tools:
`ContainerSandbox::execute_with_stdin` (a `-i` interactive run feeding piped stdin,
unified with `execute` incl. the egress-lockdown `docker exec` path), which also
makes the backend honor `SandboxCommand.env` — variable *names* on the argv
(`-e NAME`), values via the CLI's process environment, so secrets never appear in a
host `ps`. Proven by docker-gated end-to-end tests (install → enable → registry
execute of a `/bin/sh` capability round-tripping stdin + injected secret; a gVisor
variant runs the same flow under `runsc`) plus ungated fail-closed unit tests
(wrong-sandbox / gvisor-demotion / missing staging/entry) and an argv unit test
that asserts the secret value is absent from the argv. Not done here (later
slices): a microVM loader, and wiring a container runtime choice into the CLI's
`plugin run` (WASI-only today via `--features plugin-wasi`).

## ECO-304 `[P2]` — One-shot `apex plugin publish` — **DONE (2026-07-14)**

**Problem.** Publishing is multi-step and manual: `keygen` → `sign` → operator
`trust`, with manual digest embedding (`apps/apex-cli/src/plugin.rs:163-208`). (PRD-004
R-F.4; audit Med.)

**Change.** A one-shot `apex plugin publish` that signs, fills digests, and emits the
trust snippet.

**Acceptance criteria.** One command produces a signed, digest-complete package + the
trust line an operator pastes.

**Files.** `apps/apex-cli/src/plugin.rs`. **Size.** M. **Depends on:** ECO-302.

**Implementation notes (2026-07-14).** Extended the existing `apex plugin
publish <source>` (registry upload) with an optional `--key <pkcs8-file>`
flag rather than adding a new verb — non-breaking, since without `--key`
publish behaves exactly as before (source must already be signed). With
`--key`, a new `prepare_and_sign` helper runs first: `source` must be a
package **directory** (a clear error otherwise, since there's nowhere to
rewrite); every artifact `path` the manifest declares is read from disk and
its real sha256 recomputed — whatever digest was hand-authored in
`plugin.yaml` is discarded, not validated, which is the actual fix for
"manual digest embedding" (a placeholder like `sha256:000…0` is fine as
input). The digest-complete manifest is rewritten to `plugin.yaml`, signed
with the given key (ed25519 over the rewritten bytes, so the signature
always matches what's on disk), `plugin.sig` written, and — the part that
makes the printed trust line self-contained — the public key is derived from
the private key and written to `<dir>/<publisher>.pub` *inside the package
directory*, so it travels with the package to whoever receives it rather
than depending on `keygen`'s separate `.pub` file still being around.
Prints `apex plugin trust <publisher> --key <path>` referencing that exact
file. Deliberately fails closed before writing anything if any declared
artifact is missing (loop reads all artifacts before either file is
touched), so a partial run never leaves a `plugin.sig` that doesn't match
`plugin.yaml`. Proven by 4 unit tests in `apps/apex-cli/src/plugin.rs`
(`prepare_and_sign_*`): the happy path — a hand-authored placeholder digest
is discarded, the real one is computed, and the produced package/`.pub` file
install cleanly through the real `PluginEngine` (the full acceptance bar,
without needing a marketplace registry); a non-directory source rejected; a
missing artifact file rejected before anything is written; an invalid
signing key rejected. All offline, no wasm toolchain needed (hashes
arbitrary bytes, not specifically a compiled module).

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

## SBX-301 `[P2]` — Confined `fs_write` builtin — **DONE (2026-07-14)**

**Problem.** No write tool; write access explicitly deferred
(`crates/apex-tools/src/builtin.rs:5-8`). (PRD-004 R-E.3; audit Med.)

**Change.** Add an `fs_write` builtin confined via the existing `confine_path`
(the same canonicalize-and-prefix guard `fs_read` uses), opt-in like `shell`.

**Acceptance criteria.** A test writes inside the workspace root and is denied outside
it (symlink-escape included).

**Files.** `crates/apex-tools/src/builtin.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-14).** `FsWriteTool` (`fs_write`) writes (or, with
`append: true`, appends) UTF-8 `content` to `path`, confined to `ctx.workdir` —
opt-in via `ToolRegistry::with_fs_write()`/folded into `with_privileged_builtins()`,
never in `with_builtins()` (SBX-301 extends SEC-301's stance to writes). Couldn't
reuse `confine_path` as-is: it canonicalizes the *whole* candidate path, which fails
outright for a brand-new file that doesn't exist yet. New `confine_path_for_write`
instead canonicalizes just the *parent directory* (which must already exist) and
checks that against the root — plus, since a write specifically can be tricked into
following a symlink to overwrite something outside the root (a read merely leaking
already-readable data is a materially smaller risk), an extra check: if the target
path already exists as a symlink, its *resolved* destination is verified to stay
under the root too. Proven by 8 tests mirroring `fs_read`'s own confinement suite
(create, overwrite, append, `../` traversal, an absolute path outside the root, a
missing-file-name path, a symlinked *file* escaping the root, and — Unix-only — a
symlinked *parent directory* escaping the root, with an assertion that the external
target is provably untouched in both symlink cases).

## SBX-302 `[P2]` — Sandboxed code-execution tool — **DONE (2026-07-14)**

**Problem.** No code-exec/python tool. (PRD-004 R-E.4; audit Med.)

**Change.** A code-execution tool routed through the sandbox (SBX-101), language
runtime configurable, resource-limited and egress-controlled.

**Acceptance criteria.** A gated test runs a snippet in the sandbox and captures
stdout/exit; resource limits apply.

**Files.** `crates/apex-tools/src/builtin.rs` + sandbox wiring. **Size.** L.
**Depends on:** SBX-101.

**Implementation notes (2026-07-14).** `CodeExecuteTool` (`code_execute`) runs a
`code` snippet in a declared `language` (`python` or `node`) rather than a raw shell
command line — staged to a process-uniquely-named file directly under `ctx.workdir`
(the tool picks the name itself, so there's no caller-supplied-path confinement
concern the way `fs_write` has) and executed via the *identical* sandbox backend
selection `ShellTool` uses (SBX-101/SEC-305): a first-party run executes natively; a
verified/untrusted run is floored to a network-isolated container when one is
available, else fails closed, never a silent native fallback. `ResourceLimits`
(timeout, memory, CPU, output cap) apply on every backend; an optional `network`
allow-list (container/gVisor path only — a native run always has full host network
access, unchanged from `ShellTool`'s own native path) maps to a `NetworkPolicy`.
Opt-in like `shell`/`fs_write` (`with_code_execute`/`with_code_execute_using`,
folded into `with_privileged_builtins()`), since it's arbitrary code execution just
in a language runtime rather than a shell. The interpreter must actually exist in
the execution environment — the default sandbox image (`alpine:3.20`, same default
as `ShellTool`) has neither Python nor Node installed; override via `with_image`/
`APEX_SANDBOX_IMAGE` for a container/gVisor run. Proven by 9 tests, each gated on
the real interpreter actually being present (skip cleanly otherwise, the same
"skip, don't fail" pattern this workspace uses for Postgres/Docker/wasm-toolchain
tests) — including a real trap found live on this dev box: Windows' `python`/
`python3` "app execution alias" stub spawns successfully but exits non-zero with a
"install from the Microsoft Store" message when no real interpreter is installed
behind it, so the availability probe checks exit *status*, not just spawn success,
or it silently believes a fake interpreter is real. Covers: a Python snippet's
stdout/exit code; a Node snippet's stdout/exit code; a non-zero exit reported as
unsuccessful; **the resource-limit acceptance bar** — a snippet that sleeps past
`timeout_secs` is killed and reported `timed_out`; the staged snippet file is
cleaned up afterward (present or absent, success or failure); and the SEC-305
fail-closed selection guard applies here too (an untrusted run on a native-only
manager is denied, never silently run natively). A Docker-backed container-path
test was deliberately not added (the default alpine image has no Python/Node, and
pulling a language-specific image would make the test network-dependent/flaky) —
the routing itself already rides the same `SandboxManager`/`ContainerSandbox`
primitives `ShellTool`'s own container tests already prove correct.

## SBX-303 `[P2]` — `#[derive(Tool)]` / schemars ergonomics — **DONE (2026-07-14)**

**Problem.** Authors hand-write JSON Schema as `json!` literals and parse params with
`.get().and_then()` (`crates/apex-tools/src/tool.rs:177-190`; e.g.
`builtin.rs:120-124`). (PRD-004 R-E.5; audit Med.)

**Change.** A proc-macro / `schemars`-based derive generating schema + typed param
deserialization from a struct.

**Acceptance criteria.** A tool defined via the derive round-trips schema + typed args
with no hand-written JSON.

**Files.** new derive crate + `crates/apex-tools/src/tool.rs`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-14).** New proc-macro crate `apex-tool-macros`
(`#[derive(Tool)]`, `#[tool(id, version, category, description, params, permissions)]`)
generates the *declarative* boilerplate a `Tool` impl needs — `ToolMetadata`
construction and a JSON Schema via `schemars::schema_for!` over a separately-declared
params struct that derives `schemars::JsonSchema` (+ `serde::Deserialize` for the
actual typed parsing) — as three inherent associated functions
(`__tool_metadata`/`__tool_input_schema`/`__tool_parse_params`) the author's own
`impl Tool for X` delegates to. Deliberately does **not** attempt to generate
`execute()` itself — that's the tool's real logic, and there's nothing to derive it
from; the value is eliminating the `json!({...})` schema literal (kept in sync with
the params type by the compiler now, not by hand) and the `.get().and_then()`
parameter-extraction chain, replaced by one `Self::__tool_parse_params(&request)?`
call yielding a typed struct. `schemars` (workspace dep, `1.x`) was already
resolvable from the offline cargo cache — no network needed. Proven end to end by
`crates/apex-tools/tests/derive_tool.rs`: a `GreetTool`/`GreetParams` pair defined
purely via the derive, asserting the generated metadata matches the `#[tool(...)]`
attributes, the generated schema is a real object schema with correct
`properties`/`required` (derived from `#[serde(default = ...)]` presence, not
hand-listed), a full round trip through a real `ToolRegistry::execute` call
(including a serde default applying when a field is omitted), and that malformed
parameters (missing/wrong-typed) come back as `ToolError::Validation`, never a panic.

## SBX-304 `[P2]` — Egress platform matrix + fail-closed — **DONE (2026-07-14)**

**Problem.** `egress_lockdown` is Linux/Docker-only; on Windows/macOS the L3 egress
protection silently doesn't exist (`crates/apex-tools/src/lib.rs:14-18`,
`sandbox/types.rs:243-259`). (PRD-004 R-E.6; audit Med.)

**Change.** Document the platform matrix; **fail closed** (refuse a non-empty
`NetworkPolicy`) when lockdown is unavailable, rather than silently allowing egress.

**Acceptance criteria.** A test asserts a network-policy run on a non-lockdown platform
is refused, not silently unrestricted.

**Files.** `crates/apex-tools/src/{lib.rs,sandbox/*}`. **Size.** S. **Depends on:** none.

**Implementation notes (2026-07-14).** Investigation first: the lockdown path
(`nsenter`+`iptables`, spawned from the host) was *already* fail-closed in effect on
a non-Linux host — those binaries simply don't exist there, so the spawn errors and
propagates before the untrusted command's `docker exec` ever runs. But that was an
**accidental** side effect of a missing binary, not a deliberate check: it wasted a
full container start on a doomed attempt, gave a generic/unhelpful error, and (per
the module's own pre-existing caveat) never accounted for a **Podman** runtime,
whose `network inspect` output shape this module doesn't parse — a gap with no
error-path guarantee at all before this ticket. New `egress_lockdown::
lockdown_supported()` (`cfg!(target_os = "linux")`) plus a runtime-name check are now
called explicitly, and *first*, in `ContainerSandbox::execute()` whenever a
non-empty egress allow-list is requested — refusing (`SandboxError::Internal`, a
specific message naming the reason) **before** any container ever starts, for both
the platform case and the Podman case. The module's doc comment now carries an
explicit platform-matrix table. Proven by 4 tests in `sandbox/container.rs`
(deliberately requiring no real Docker daemon, since the refusal happens before any
docker command runs — meaning the test is directly meaningful on whichever platform
the suite actually runs on, this Windows dev box included): the refusal fires for
both the plain Docker and gVisor constructors on an unsupported platform; a Podman
runtime is refused regardless of platform; and — the regression guard — a deny-all
or fully-open policy (neither needs the lockdown at all) is provably unaffected by
the new gate.

---

# WS-H — Workflow Expressiveness & Scale

## WFL-301 `[P1]` — Loop / for-each activity — **DONE (2026-07-14)**

**Problem.** The DAG is strictly acyclic with a static activity list
(`crates/apex-workflow/src/definition.rs:62,236`); no map-over-collection. (PRD-004
R-H.5; audit High.)

**Change.** A `map`/`for_each` activity that expands over a runtime collection into a
bounded sub-DAG (concurrency-capped), results collected in order.

**Acceptance criteria.** A workflow maps an activity over an N-element input and joins
N results deterministically.

**Files.** `crates/apex-workflow/src/{definition.rs,engine.rs}`. **Size.** L.
**Depends on:** none.

## WFL-302 `[P1]` — Dynamic (data-driven) fan-out — **DONE (2026-07-14)**

**Problem.** The concurrent `ready_batch` is only over statically-declared activities
(`engine.rs:686,1099-1121`); K can't be derived from data. (PRD-004 R-H.6; audit High.)

**Change.** Support spawning K instances of one activity keyed by an input array
(the runtime companion to WFL-301).

**Acceptance criteria.** A workflow fans out to a data-determined K and joins.

**Files.** `crates/apex-workflow/src/engine.rs`. **Size.** L. **Depends on:** WFL-301.

**Implementation notes (2026-07-14).** `is_for_each`/`ForEachSpec` in
`definition.rs` recognize `for_each` (with `map` as an alias) as a third
engine-native activity type alongside `wait`/`workflow`. Wire shape (under
`inputs`): `items` (a `${...}` reference or a literal array), `activity` (the
per-item body — any non-engine-native type; `wait`/`workflow`/`for_each`/`map`
are rejected as a nested body at *load* time, fail-closed), and optional
`max_concurrent` (default 8) / `max_items` (default 1000, a hard fail-closed
bound against unbounded fan-out). Declared activity ids may not contain
`[`/`]` — reserved for the per-item instance id `<parent_id>[<index>]` this
introduces. `Engine::run_for_each` resolves `items` **once**, on first
encounter, and durably pins the resolved array into the checkpoint (a
`__for_each.<id>` variable) exactly like a durable timer pins its `fire_at` —
it is never recomputed on resume, even if the source variable a `${...}`
reference pointed at has since changed. Each element becomes its own durable
`ActivityRecord` under `<id>[<index>]`, run to a terminal outcome
concurrency-capped at `max_concurrent` (a sliding window over `JoinSet`, the
same isolate-then-commit shape `run_ready_batch` already used for static
parallel branches), then committed to the event log/checkpoint in **item
order** regardless of completion order — so a resume re-drives only the
instances that never reached `Completed`, and the joined output is
reproducible. An item's permanent failure fails the `for_each` (and thus the
workflow, subject to saga compensation like any other activity) but still
durably commits the *other* items launched in the same phase rather than
discarding their completed work; an item that interrupts resets just that
instance to `Ready` and keeps the parent `for_each` itself `Ready` too, so a
resume re-enters and only relaunches the pending instance(s). Proven by 9
integration tests in `crates/apex-workflow/tests/engine.rs` (referenced vs.
literal-array `items`; empty-collection short-circuit with zero instances
spawned; `max_items` fail-closed before any instance is created; a
non-array-resolving `items` fail-closed; the `max_concurrent` cap actually
reached — not just never exceeded — under a timeout; partial-failure commits
completed siblings; durable resume re-runs only the incomplete instance,
proven by registering no handler for the already-completed ones so a re-run
would panic the test; and the pinned-collection guarantee, proven by
mutating the source variable directly in the checkpoint between runs and
asserting the resumed instance still sees the originally-resolved item) plus
9 definition-load-time unit tests in `definition.rs` (nested engine-native
body types, zero `max_concurrent`/`max_items`, non-array/non-reference
`items`, the reserved `[`/`]` id characters, and the documented defaults).

## WFL-303 `[P2]` — Checkpoint size cap + out-of-line large outputs — **DONE (2026-07-14)**

**Problem.** Every step re-serializes and upserts the entire `ExecutionState`; activity
outputs merge into `variables` unbounded with no cap (`engine.rs:843,1387`;
`postgres.rs:186`). (PRD-004 R-H.7; audit Med.)

**Change.** Cap serialized snapshot size (fail-closed with a clear error) and/or store
large activity outputs out-of-line (blob ref).

**Acceptance criteria.** A test asserts an over-cap output is rejected or externalized,
not silently bloating every checkpoint.

**Files.** `crates/apex-workflow/src/{engine.rs,postgres.rs,store.rs}`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-14).** Took the fail-closed-cap half of the "and/or":
`Engine` gained `max_activity_output_bytes` (`DEFAULT_MAX_ACTIVITY_OUTPUT_BYTES` = 1
MiB, overridable via `with_max_activity_output_bytes`) and a `check_output_size(id,
&output)` helper (serialized-size check, no I/O) called **before** an output ever
merges into `state.variables` or reaches the event log/checkpoint — an over-cap output
is a permanent activity failure via the existing `terminal_activity_failure` path
(saga compensation applies exactly like any other permanent error), not a hard abort.
Wired at the two commit sites most likely to actually produce something large: the
sequential path (`run_activity`) and `for_each`'s **joined** output
(`complete_for_each`) — many individually-small item outputs can still aggregate into
an oversized whole, so the join itself needs its own check, not just each item.
Deliberately not wired into the static-parallel-batch commit or a subworkflow's child
result in this slice — a documented follow-on, not a silent gap. Proven by 3 tests in
`tests/engine.rs`: an oversized single output fails closed and never reaches
`variables`; an ordinary-sized output is unaffected (no false positive); a `for_each`
whose individually-fine items join into an oversized array fails closed at the join.

## WFL-304 `[P2]` — Event-log compaction + paged load — **DONE (2026-07-14)**

**Problem.** Append-only with no retention; `load`/`history` deserialize every event
(`store.rs:225`; `postgres.rs:169`). (PRD-004 R-H.7; audit Med.)

**Change.** Add event compaction/retention and a bounded/paged `load`.

**Acceptance criteria.** A test asserts `history` pages and that recovery doesn't read
the full log for a long execution.

**Files.** `crates/apex-workflow/src/{store.rs,postgres.rs}`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-14).** `EventLog` gained two default-implemented
methods: `load_page(execution_id, offset, limit)` (default: load-then-slice; overridden
per backend for a real bound) and `compact(execution_id, keep_after_seq)` (default:
no-op — **never** called automatically by the engine, since the log is the append-only
source of truth; an explicit, caller-invoked prune only). `Engine::history_page`/
`compact_history` are the new public entry points, additive alongside the unchanged
`history()`. **`FileStore::load_page`** streams the JSONL file line-by-line
(`AsyncBufReadExt::lines()`), skipping raw (undeserialized) lines up to `offset` and
stopping as soon as `limit` is satisfied — the file's tail past the requested page is
never read at all. **`FileStore::compact`** rewrites the file dropping the first
`keep_after_seq` lines (line position *is* the seq, per `append`'s existing
seq-from-line-count convention), atomically. **`PostgresStore`** pushes both
operations into SQL (`OFFSET`/`LIMIT`, `DELETE … WHERE seq <= $2`) — genuinely bounded
at the database, not just in application code. The "recovery doesn't read the full
log" half of the acceptance criterion turned out to already be true by construction —
`resume` only ever reads the **checkpoint**, never `EventLog::load`/`load_page` —
proven directly by wrapping the log in a decorator that panics if `load`/`load_page`
is ever called and driving a crash-and-resume through it
(`resume_never_reads_the_full_event_log`, `tests/temporal_gaps.rs`). Proven overall by
9 tests: `history_page` reconstructing the exact full timeline by paging through it in
small chunks; `compact_history` dropping exactly the oldest events and leaving paging
correct over the shorter remainder; the resume-never-reads-the-log proof; plus 3
`FileStore`-specific unit tests exercising the real streaming/rewrite code paths (not
just the trait defaults) and a live-Postgres integration assertion folded into
WFL-305's own test (see below).

## WFL-305 `[P2]` — Indexed `list()` columns + SQL-side filtering — **DONE (2026-07-14)**

**Problem.** `list()` scans and decodes every checkpoint, filtering in Rust
(`postgres.rs:216-238`; `store.rs:259-278`). (PRD-004 R-H.7; audit Med.)

**Change.** Promote `workflow_name`/`status` to indexed columns; push filtering +
pagination into SQL.

**Acceptance criteria.** A test asserts filtered `list()` doesn't load non-matching
rows; a migration adds the columns/indexes.

**Files.** `crates/apex-workflow/src/postgres.rs` + migration. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-14).** New migration `V3__checkpoint_index_columns.sql`
adds `workflow_name`/`status` `TEXT` columns to `workflow_checkpoints` (backfilled from
the existing JSON `snapshot` for any pre-existing rows via `snapshot::jsonb ->> '…'`,
then set `NOT NULL`) plus one index per column. `PostgresStore::save` keeps both
columns in lockstep with the JSON snapshot on every upsert — `status`'s exact
`snake_case` wire string is derived from `WorkflowState`'s real `Serialize` impl
(`status_str`, via `serde_json::to_value`) rather than hand-duplicated, so it can never
drift from what the JSON itself would encode. `list()` now builds its `WHERE` clause
dynamically against these indexed columns (plus, as a bonus matching "push filtering
**+ pagination** into SQL" literally, the `LIMIT` — previously applied via
`Vec::truncate` *after* decoding every filtered row in Rust) instead of loading
everything and filtering in application code. Proven by
`filtered_list_never_decodes_a_non_matching_rows_corrupt_snapshot`
(`tests/postgres_store.rs`, capability-gated like the rest of that file): a row is
inserted directly (bypassing `save`) with a **deliberately corrupt, unparseable**
`snapshot` but indexed columns that don't match the filter — if `list()` ever fell back
to decoding every row, this test would fail with a JSON parse error instead of
succeeding; a companion assertion in the same test confirms selecting that row (by
matching its indexed columns) really does fail to decode, proving the row is genuinely
corrupt and not just absent for an unrelated reason.

## WFL-306 `[P2]` — `fire_at`-indexed timers + adaptive dispatch — **DONE (2026-07-14)**

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

**Implementation notes (2026-07-14).** `InMemoryTimerStore` now maintains a secondary
index — a `BTreeSet<(fire_at_ms, execution_id, timer_id)>` kept in lockstep with the
primary `(execution_id, timer_id)` map — so `due`/the new `next_deadline` query it
directly via `BTreeSet::range`/`.first()` (bounded by the number of due entries, or
O(1) for the next deadline, not a scan over every pending timer regardless of how far
out its deadline is). `TimerStore`/`ScheduleStore` both gained a `next_deadline()`
trait method (default: derived from a full scan, for backends that haven't indexed;
`InMemoryTimerStore` overrides it with the real bound). `TimerDispatcher`/
`ScheduleDispatcher` each gained `run_adaptive(max_interval, should_stop)`: poll, then
sleep exactly until the next known deadline — **capped** at `max_interval` so the loop
still wakes up periodically to notice a timer/schedule registered by another process in
the meantime (the same guarantee a fixed interval gave), while a near-deadline timer
fires promptly instead of waiting out a stale interval. The file scope's `lib.rs`
reference turned out stale (no dispatch loop lives in `apex-workflow/src/lib.rs`, a
64-line module-wiring file; the actual background poll loop is in `apex-server`) —
`run_adaptive` is the engine-crate-side capability; wiring the server's own background
loop to use it instead of its fixed `APEX_DISPATCH_INTERVAL_SECS` is a documented
follow-on, not in this ticket's stated file scope. Proven by: `next_deadline` tracking
the true minimum across schedule/cancel/replace (incl. a paused-schedule exclusion
test); a 20,000-far-future-timer test asserting `due`/`next_deadline` stay fast (a
generous timing backstop on top of the `BTreeSet`-range structural bound — the same
"large headroom" philosophy `tests/perf.rs` uses elsewhere in this crate); and a real
end-to-end **wall-clock** test (`run_adaptive_fires_a_near_deadline_timer_promptly_
not_after_the_full_interval`, `tests/temporal_gaps.rs`) — the one deliberate exception
to that file's otherwise fully-deterministic `ManualClock` tests, since "fires
promptly" can only be demonstrated in real elapsed time — asserting a 60ms-out timer
completes well under a 5s `max_interval` cap.

## WFL-307 `[P3]` — Activity progress events — **DONE (2026-07-14)**

**Problem.** `ActivityExecutor::execute` returns a single `Value`; no progress channel
(`crates/apex-workflow/src/executor.rs:69-72`; `event.rs:27-83`). (PRD-004 R-H.8; audit
Low.)

**Change.** Add an optional progress sink to `ActivityContext` + an `ActivityProgress`
event.

**Acceptance criteria.** A test asserts a long activity emits progress events.

**Files.** `crates/apex-workflow/src/{executor.rs,event.rs}`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-14).** `ActivityContext` gained `progress: Option<
tokio::sync::mpsc::UnboundedSender<String>>`; `WorkflowEvent` gained `ActivityProgress
{ id, attempt, message }` (display-only — never consulted by scheduling/resume). Wired
live on the **sequential** path (`run_activity`, the common single-activity case): a
fresh channel is created per attempt, and `tokio::select!` drives the executor future
concurrently with draining the channel, durably `emit`-ing an `ActivityProgress` event
for each report **as it arrives** rather than only after the activity settles; any
message sent right before the executor returns (and thus not yet observed by the
select loop) is caught by a post-loop drain so nothing sent is ever silently lost. This
needed a new `tokio` `macros` feature on the crate's real (non-dev) dependency, since
`tokio::select!` requires it — the now-redundant `[dev-dependencies]` override for the
same feature was removed (Cargo unions feature requests for one crate across every
dependency table, so listing it once in `[dependencies]` is sufficient). Concurrent
batch paths (`run_ready_batch`/`for_each` item instances, and compensation-handler
runs) pass `progress: None` — they isolate an attempt off the shared `ExecutionState`
until it settles (the two-phase isolate-then-commit shape), so there's nowhere to
durably emit an event to mid-flight; live progress there is a documented follow-on,
not a silent gap, and sending on a `None` context is simply a no-op for an executor
that checks first. Proven by 2 tests in `tests/engine.rs`: a long activity that yields
between each of 3 progress reports (`tokio::task::yield_now().await`, forcing the
select loop to genuinely interleave rather than the whole closure running to
completion in one poll) gets each one recorded in order, all preceding the terminal
`ActivityCompleted` event; an activity that never reports progress emits zero
`ActivityProgress` events (no false positives / behavior change for the common case).

## WFL-308 `[P3]` — Event-enum schema versioning — **DONE (2026-07-14)**

**Problem.** The event enum wire format has no version tag; a rename breaks the on-disk
log (`crates/apex-workflow/src/event.rs:18-24`). (PRD-004 R-H.8; audit Low.)

**Change.** Add a schema-version tag + a migration path before any future rename.

**Acceptance criteria.** A test round-trips a versioned event and rejects an unknown
future version cleanly.

**Files.** `crates/apex-workflow/src/event.rs`. **Size.** S. **Depends on:** none.

**Implementation notes (2026-07-14).** `EVENT_SCHEMA_VERSION` (currently `1`) plus a
`VersionedEvent { v: u32, #[serde(flatten)] event: WorkflowEvent }` wrapper — `flatten`
keeps a logged line reading as one flat JSON object (`{"v":1,"type":"workflow_
completed",…}`) rather than nesting the event under its own key. `encode_event`/
`decode_event` are the one place a `WorkflowEvent` is serialized/deserialized for
durable storage; every store (`FileStore`, `PostgresStore`) now goes through them
instead of calling `serde_json::to_string`/`from_str` on the bare enum directly, so the
version tag can never be forgotten at a new call site (`InMemoryStore` is unaffected —
it holds `WorkflowEvent`s in-process and never serializes them, so there's no wire
boundary to version). `decode_event` fails closed on a `v` newer than this binary
understands (`Error::Config`, a clear upgrade-the-binary message) — and, since this is
the format's first version with no prior unversioned data to preserve (same "breaking,
acceptable pre-GA, no real deployment to migrate" stance the earlier `snake_case` tag
rename already established for this exact format), a line with **no** `v` field at all
is rejected too, not silently accepted as "version 0". No translation path exists yet
since there's only one version to translate from; the doc comment on
`EVENT_SCHEMA_VERSION` is explicit that one must be added to `decode_event` before any
future variant rename ships. Proven by 3 unit tests in `event.rs`: a round trip through
`encode_event`/`decode_event` preserves the event and confirms the flat wire shape; a
`v: 99` line is rejected with a message naming both the found and understood versions;
a line missing `v` entirely is rejected. The full `apex-workflow` suite (incl.
`FileStore`/`PostgresStore`-backed durable-resume tests) stayed green through this
change, confirming the swap didn't alter any other store's on-disk behavior.

---

# WS-G — Server Health (R-G.8 cluster)

## SRV-302 `[P2]` — Cache `FileApiKeyStore` in memory — **DONE (2026-07-14)**

**Problem.** `principal_for` calls `self.load()` per authenticated request — disk I/O +
full deserialize on the hot auth path (`crates/apex-server/src/auth.rs:315-318,291-296`).
(PRD-004 R-G.8; audit Med.)

**Change.** Cache the key map in memory with file-watch/invalidation; O(1) lookup.

**Acceptance criteria.** A test asserts no per-request file read after warm-up and that
an external key change is picked up.

**Files.** `crates/apex-server/src/auth.rs`. **Size.** S. **Depends on:** SRV-104.

**Resolution.** `FileApiKeyStore` gained an mtime-stamped in-memory cache
(`CachedKeys`): `principal_for` now calls `load_cached()`, which `stat()`s the file
(one cheap syscall) and only re-reads + reparses when the mtime has moved since the
last load — a single `stat()` replaces a full read+JSON-parse on every request in the
common case (nothing changed). `save()` (called by `create_key`/`revoke`/`rotate`)
refreshes the cache in the same process immediately after writing, so a mutation this
process makes is visible without an extra disk round-trip. A `load_count` atomic
(test-observability only, mirrors nothing constructed at runtime) proves the cache
actually avoids repeat reads. Mutating operations (`create_key`/`list_keys`/`revoke`/
`rotate`) intentionally keep calling the raw uncached `load()` — a read-modify-write
must see the true current disk state, never a possibly-stale cache. Proven by
`file_api_key_store_avoids_repeat_reads_after_warm_up` (five repeated lookups after
warm-up trigger zero additional real reads) and `file_api_key_store_picks_up_an_
external_change` (a second `FileApiKeyStore` handle on the same directory — standing
in for a separate process — writes a new key, and the first handle picks it up on its
next lookup without being reconstructed).

## SRV-303 `[P2]` — Serve a generated OpenAPI spec — **DONE (2026-07-14)**

**Problem.** No `utoipa`/OpenAPI generation; `openapi.yaml` is hand-synced
(audit grep). (PRD-004 R-G.8; audit Med.)

**Change.** Generate the OpenAPI doc from route/handler types and serve it at
`/openapi.json`; keep the CI contract gate (redocly) against the generated doc.

**Acceptance criteria.** The served spec matches the handlers; the contract gate runs
against it.

**Files.** `crates/apex-server/src/*` + `docs/09-api/openapi.yaml` pipeline.
**Size.** L. **Depends on:** none.

**Resolution.** New `crates/apex-server/src/openapi.rs`: every one of the ~65 routes
`router()` mounts (agents, workflows, tenancy, webhooks, memory, plugins,
marketplace, audit, tools, secrets, kms, health/metrics/this-doc) carries a real
`#[utoipa::path(...)]` attribute on its handler function, and every request-body/
error type used on the wire derives `utoipa::ToSchema` (foreign types from other
crates — `apex_tenancy::QuotaLimits`/`Role`/`ProjectStatus` — are documented via
`#[schema(value_type = ...)]` overrides or left untyped with a prose note, rather
than pulling `utoipa` into crates that shouldn't need to know about it). A
`SecurityAddon` (`utoipa::Modify`) registers the two real auth schemes
(`tenantHeader`/`bearerAuth`, matching `crates/apex-server/src/auth.rs`'s actual
`APEX_AUTH_MODE` contract) since utoipa's macro syntax only expresses security
*requirements*, not the schemes themselves; `/healthz`/`/metrics`/`/openapi.json`
carry an explicit `security(())` override matching their real unauthenticated
status. `ApiDoc` (a `#[derive(OpenApi)]` struct) aggregates it all and is served as
JSON at `GET /openapi.json`, unauthenticated (describes the API's shape, not any
tenant's data) alongside health/metrics. A same-crate test,
`served_spec_covers_every_mounted_route`, asserts every mounted path+method has a
generated entry — a handler added to `router()` but never annotated (or vice versa)
is a compile/test failure, not just a convention. Verified live end to end: started
a real `apex dev` server, fetched the actual `/openapi.json` it serves, and ran
`redocly lint` against it — 0 errors (29 stylistic warnings, comparable to the old
hand-written file's 112). `docs/09-api/openapi.yaml` remains as a browsable,
checked-in snapshot, but `docs/09-api/overview.md` now states plainly that
`/openapi.json` is the generated, drift-proof ground truth and the CI contract gate
(`sdks/typescript`'s `npm run lint:openapi`) now lints the **live served document**
(`http://127.0.0.1:8080/openapi.json`, the address the contract-gate job's `apex dev`
already binds) instead of the static file.

## SRV-304 `[P2]` — Extract the inline `lib.rs` test suite — **DONE (2026-07-14)**

**Problem.** `lib.rs` is ~86% inline test code (~2,260 of 2,618 lines,
`crates/apex-server/src/lib.rs:356`→EOF). (PRD-004 R-G.8; audit Med.)

**Change.** Move the suite to `tests/` or a `tests.rs` submodule (widening the few
`pub(crate)` fields the tests reach only as needed).

**Acceptance criteria.** `lib.rs` production module is navigable; the suite still runs.

**Files.** `crates/apex-server/src/lib.rs` (+ new test module). **Size.** M.
**Depends on:** none.

**Resolution.** Took the `tests.rs` **submodule** branch of the "or", not the external
`tests/` directory branch: `lib.rs`'s `#[cfg(test)] mod tests { ... }` (2,410 lines)
moved verbatim into a new file-backed `crates/apex-server/src/tests.rs`
(`#[cfg(test)] mod tests;` in `lib.rs`), keeping the exact same crate-internal
visibility the tests already relied on — several reach into `AppState`'s `pub(crate)`
fields directly (seeding a workflow engine/tenancy store before driving a request
through `router()`), so a true external `tests/` integration-test crate would have
forced those fields to `pub`, a real API-surface change out of scope for a pure
code-motion refactor (exactly the tradeoff CLAUDE.md's HLTH-904 note already
flagged as deliberately deferred). `lib.rs` is now 444 lines — genuinely navigable
production code (`router()`/`serve()`/TLS bootstrap), no scrolling past a test module
to find it. All 137 (now 139, +2 from SRV-302's own tests) tests still pass
unchanged; `cargo fmt` normalized the moved code's indentation. One real hazard hit
mid-move and fixed: Windows PowerShell's `Get-Content` (no explicit `-Encoding`)
silently misreads UTF-8-without-BOM as the system codepage, mangling every em-dash/
§/smart-quote in the file into mojibake — caught immediately via a post-move `Read`,
recovered via `git checkout` + reapplying the (small, already-correct) `Edit`-tool
changes, then redone with `[System.IO.File]::ReadAllLines(path,
[System.Text.Encoding]::UTF8)` / `WriteAllLines(..., new UTF8Encoding(false))`
(explicit UTF-8, no BOM) instead.

## SRV-305 `[P2]` — Idempotency store write-amplification — **DONE (2026-07-14)**

**Problem.** `put` does a full-file `atomic_write` of the whole map per mutating
request (`crates/apex-server/src/hardening.rs:230-266`). (PRD-004 R-G.8; audit Med.)

**Change.** Use an append/segmented store or debounce persistence.

**Acceptance criteria.** A test asserts a mutating request doesn't rewrite the entire
cache file each time.

**Files.** `crates/apex-server/src/hardening.rs`. **Size.** M. **Depends on:** none.

**Resolution.** Took the append-only-log branch, not debounce: debouncing would
reintroduce the exact "a crash-loop loses a recently-cached idempotent response" bug
RM-GA-P2 DUR-404 exists to prevent, since a response would sit unpersisted in memory
for some window before the next flush. `IdempotencyStore::put` now appends exactly
one JSON-encoded line per call (`fsync` the file, then `fsync` the parent directory
via `apex_common::fs::sync_parent_dir` — the same durability an `atomic_write`
whole-file rewrite gives, without paying for one; the identical primitive
`apex-workflow`'s event log and `apex-audit`'s hash chain already use for this exact
reason). The on-disk log can accumulate more lines than live entries (an expired/
evicted key's old line is never retroactively deleted), so `put` compacts — one full
`atomic_write` rewrite collapsing back to exactly the current live entries — once the
log has grown to `max_entries * 2`, keeping the amortized cost per `put` O(1) instead
of the old O(entries)-per-call. The file extension changed `idempotency.json` →
`idempotency.jsonl` (a breaking on-disk format change for a pre-existing file, made
deliberately visible via the new name rather than silently misread under the old one
— low-stakes to lose across an upgrade, since entries are a short-TTL dedup cache,
not a source of truth, and per this workspace's established pre-GA stance no real
deployment exists yet to migrate). Proven by two new tests:
`put_appends_instead_of_rewriting_the_whole_log_each_time` (30 puts under a large
`max_entries` produce exactly 30 appends and 0 compactions, verified via
test-observability counters mirroring `FileApiKeyStore::load_count`'s SRV-302
pattern; a fresh store reopened from the same path recovers every entry) and
`put_compacts_the_log_once_it_grows_past_the_threshold` (a small `max_entries`
crosses the `*2` threshold on schedule, and the post-compaction file holds exactly
the live entry count, not every line ever appended).

## SRV-306 `[P3]` — Request-path unwrap audit — **DONE (2026-07-14)**

**Problem.** ~317 `.unwrap()`/`unreachable!()`/`.expect()` across the crate; some on
live paths (e.g. `agents.rs:218,220`; `webhooks.rs:56`). (PRD-004 R-G.8; audit Low.)

**Change.** Audit request-adjacent unwraps; return `ApiError` instead of panicking.

**Acceptance criteria.** Live-path unwraps are eliminated or justified; a clippy
lint/CI check guards new ones on handler paths.

**Files.** `crates/apex-server/src/{agents.rs,webhooks.rs,...}`. **Size.** M.
**Depends on:** none.

**Resolution.** The audit (all 30 production-code — i.e. outside `#[cfg(test)]` —
`.unwrap()`/`.expect()`/`unreachable!()` call sites across the 11 route-handler
modules, read in their containing-function context) found **zero** that are
attacker/client-triggerable: every one is a `Mutex`/`RwLock` poison guard (panics
only if an earlier panic already happened while holding the same lock, never from
request content) or, in `agents.rs`'s `get_run_handler`, an `.unwrap()`/
`unreachable!()` pair operating on a `json!({"run_id": run_id})` literal (always a
`Value::Object` regardless of what `run_id` contains) and a fixed-shape internal
enum's `Serialize` output — neither shape depends on request data. The two file:line
examples the ticket cited (`agents.rs:218,220`/`webhooks.rs:56`) no longer point at
unwraps at all — `webhooks.rs` in particular has **zero** production-code unwraps;
every match is inside its own `#[cfg(test)] mod tests`. So "eliminate" had nothing
left to do; the ticket's other half — "a clippy lint/CI check guards new ones on
handler paths" — is what actually shipped: all 11 route-handler modules (`agents.rs`,
`audit.rs`, `kms.rs`, `marketplace.rs`, `memory.rs`, `plugins.rs`, `secrets.rs`,
`tenancy.rs`, `tools.rs`, `webhooks.rs`, `workflow_runner.rs`) now carry
`#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used,
clippy::unreachable))]`. The `cfg_attr(not(test), ...)` gate is deliberate, not
decorative: `cargo clippy --all-targets` compiles this crate twice — once as the
plain lib (`cfg(test)` false, where the lint is active and every handler file's
`#[cfg(test)] mod tests` is stripped out entirely, so there's nothing inside it to
false-positive on) and once as the test binary (`cfg(test)` true crate-wide, where
`not(test)` is false and the lint is inert) — so the guard fires exactly on
production code and never on test code, with no need to separately annotate every
test module. The 4 pre-existing legitimate call sites this actually affected
(`agents.rs`'s json-literal/enum-shape pair, `tenancy.rs`'s two `quota.usage.lock()`
mutex-poison `.expect()`s) each carry a scoped `#[allow(...)]` with a one-line
justification. Verified the guard is real, not a no-op, by temporarily deleting one
`#[allow]` and confirming `cargo clippy -p apex-server -- -D warnings` fails with
exactly the expected `unwrap_used`/`unreachable` diagnostics, then restoring it.

## SRV-307 `[P3]` — Shared concurrency slots — **DONE (2026-07-14)**

**Problem.** `QuotaTracker.concurrent` is in-process/per-node
(`crates/apex-server/src/state.rs`; `tenancy.rs:432,516-527`); N nodes multiply the
effective limit. (PRD-004 R-G.6; audit Low.)

**Change.** Track concurrency in a shared store for multi-node correctness.

**Acceptance criteria.** A gated test asserts two nodes share one concurrency budget.

**Files.** `crates/apex-server/src/tenancy.rs`. **Size.** M. **Depends on:** SRV-201.

**Resolution.** Mirrors SRV-201's `RateLimiter`/`redis_shared` design closely (same
`with_redis`/`from_env` shape, same lazily-dialed-and-redialed connection, same 1s
`REDIS_BUDGET` timeout, same degrade-to-local-never-to-unlimited posture) rather than
inventing a new pattern: a new `redis_concurrency` module (behind the existing
`redis` cargo feature) provides `SharedConcurrency`, whose atomic "increment only if
under the limit" Lua script lets a fleet of nodes serialize on one Redis counter per
`(prefix, project)` instead of each independently reading-then-incrementing (which
could race two nodes both past the limit). `QuotaTracker::from_env` (wired into
`state.rs` in place of the old bare `QuotaTracker::new`) enables it when
`APEX_QUOTA_REDIS_URL` is set on a `redis`-feature build (a dedicated var, so CI's
`APEX_REDIS_URL` for the live capability-gated tests doesn't silently flip
production config). `admit_run` became `async fn` (the shared path is a network
call) — every call site (`agents.rs` ×3, `workflow_runner.rs`'s `StoredAgentResolver
::admit`, plus the test suite) now `.await`s it. The harder half: `RunPermit`
releases a slot in `Drop::drop`, which is unavoidably sync, but a shared release is
an async Redis call — solved by spawning a detached `tokio::spawn` from `drop`
(fire-and-forget, safe because a `RunPermit` only ever drops from within
request-handling code, always inside an active Tokio runtime — the same pattern this
codebase already uses for best-effort webhook delivery). **A known, documented,
bounded gap, not silently assumed away:** `Drop` never runs on a hard crash
(`kill -9`, power loss), which would otherwise permanently strand a shared slot for
the whole fleet — closed by a generous `PEXPIRE` safety-net TTL (24h) refreshed on
every successful admit, self-healing a stranded slot rather than requiring operator
intervention, at the honestly-stated cost that a run genuinely still in flight past
24h could have its slot expire and race a fresh admission past the limit. Only the
concurrency dimension is fleet-shared; cost/token budgets stay per-node
(disk-persisted, RM-GA-P2 DUR-404) exactly as before — widening those the same way is
a documented follow-on, not silently assumed to already work. Proven by three new
capability-gated tests in `tenancy::redis_tests` (skip cleanly, logging a `skipping:`
line, when `APEX_REDIS_URL` is unset/unreachable — identical convention to
`rate_limit::redis_tests`): `two_nodes_share_one_concurrency_budget` is the ticket's
literal acceptance criterion (two independently-constructed `QuotaTracker`s over one
Redis prefix admit exactly `concurrent_agent_runs` permits combined, not 2×, and a
release on one node is visible to the other); `shared_concurrency_is_still_per_
project` (two projects' counters don't cross-contaminate); and
`unreachable_redis_degrades_to_local_concurrency_limiting_not_unlimited` — this one
needs no live Redis and was run for real: it deliberately avoids the fixed
"port 9 = connection refused" assumption `rate_limit`'s equivalent test uses (which
was found, while validating this ticket, to behave inconsistently on this Windows
dev machine — flagged separately, out of scope here) in favor of binding an
ephemeral TCP listener and immediately dropping it, which passed reliably.
**Not independently verified against a live Redis** in this environment (no
Docker/`redis-server` available here) — the two live-Redis tests are exercised for
real by CI's service-container job, the same honest limitation SRV-201's own
integration tests already carry.

---

# WS-I / WS-C — Audit, Secret Channel, Re-embedding

## SEC-301 `[P2]` — Audit query: time-range + pagination + indexed sink — **DONE (2026-07-14)**

**Problem.** `query()` loads the entire log via `sink.all()` then filters in memory;
`AuditFilter` has only tenant/principal/action/limit — no from/to, no cursor; the JSONL
sink re-reads the whole file per op (`crates/apex-audit/src/log.rs:116-140,220-238`).
(PRD-004 R-I.4; audit Med.)

**Change.** Add time-range + cursor pagination and an indexed/DB-backed sink option.

**Acceptance criteria.** A test asserts a time-ranged, paged query doesn't scan the
whole log.

**Files.** `crates/apex-audit/src/log.rs`. **Size.** M. **Depends on:** none.

**Done.** `AuditFilter` gained inclusive `after_ms`/`before_ms` bounds (shared by
`query()` and the new paged path via `AuditFilter::matches`, so the two can't drift);
`AuditSink` gained `query_page(filter, before_seq, limit) -> AuditPage` — a
most-recent-first page plus a seq-based cursor — with a default read-everything
implementation any sink inherits (`InMemoryAuditSink` keeps it; an in-memory scan
isn't the I/O cost this ticket targets) and a real bounded override on
`FileAuditSink`: `scan_reverse` reads `audit.jsonl` **backward in 64 KiB chunks**,
stopping as soon as the page is filled — no separate index needed, because the
append-only file's line order *is* the seq order, read tail-first. A dedicated
DB-backed sink was deliberately not built: the reverse scan meets the acceptance
criterion without new infrastructure, and the trait's `query_page` is the port a
future Postgres sink would implement. `GET /api/v1/audit` gained
`after_ms`/`before_ms` and now reads via `query_page` (cursor wraps a seq, same
opaque wire encoding as every other list route); its `total_estimate` is now always
`null` — the one documented envelope exception, since an exact count would require
the full scan the route exists to avoid. Both SDKs + `openapi.yaml` updated in
lockstep (`AuditPage` type). **Fixed in passing:** the first-draft `scan_reverse`
parsed the bytes before a chunk's first `\n` (the tail of a line starting in an
earlier chunk) as a complete line and carried the wrong end of the buffer — masked
whenever a page early-returned inside the first chunk, but any multi-chunk scan
failed with a JSON parse error; proven by a written-first failing test forcing
7-byte chunks. Acceptance proven by
`time_ranged_paged_query_reads_only_the_tail_of_the_log` (400-entry log, 1 KiB
chunks: a time-ranged page of 5 reads < ¼ of the file, asserted via the scan's
actual bytes-read count), plus `reverse_scan_reassembles_lines_split_across_chunks`,
`query_page_cursor_walks_the_log_without_gaps_or_overlap`,
`default_query_page_and_file_override_agree` (page-by-page parity between the
trait default and the file override, filtered), `time_range_bounds_are_inclusive`,
and the route-level
`audit_route_time_range_and_cursor_page_through_the_window` in `apex-server`.

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

## DEP-301 `[P1]` — systemd unit + install script for the appliance — **DONE (2026-07-14)**

**Problem.** The README markets a single-binary appliance but there is no `*.service`,
`install.sh`, or distro package under `deployment/`; only container/K8s paths exist.
(PRD-004 R-L.4; audit High.)

**Change.** Add a systemd unit + an install script (user, dirs, `~/.apex` perms, env
file) for the bare-metal appliance install.

**Acceptance criteria.** The unit starts/stops `apex dev`/`serve`; the script produces a
working install on a clean host (documented, ideally smoke-tested in CI on Linux).

**Files.** `deployment/systemd/*`, `deployment/install.sh`. **Size.** M.
**Depends on:** none.

**Resolution.** `deployment/systemd/apex.service` runs the one real production
entrypoint (`apex dev --addr $APEX_BIND_ADDR` — there is no separate `apex
serve` command; `serve()` in `crates/apex-server/src/lib.rs` already handles
graceful SIGTERM shutdown and refuses a non-loopback bind without TLS per
SEC-202, so the unit doesn't duplicate either) as a dedicated `apex` system
user, with a real (not decorative) systemd sandbox —
`ProtectSystem=strict`/`ReadWritePaths=/var/lib/apex`/`NoNewPrivileges`/
`PrivateTmp`/`ProtectHome`/etc. — documented as a moderate default an
operator enabling the (off-by-default) `shell`/`code_execute` tool builtins
without a container/gVisor backend may need to relax.
`deployment/install.sh` is idempotent (safe to re-run after building a new
binary): creates the system user/group (home `/var/lib/apex`, no login
shell), `/var/lib/apex/.apex` (`0700`), installs the binary + unit +
`deployment/systemd/apex.env.example` → `/etc/apex/apex.env` (**only** if
that file doesn't already exist, so operator edits survive a re-run/upgrade),
and runs `systemctl daemon-reload` — deliberately without enabling/starting
the service, so `/etc/apex/apex.env` (shipped default: loopback-only,
`disabled-loopback` auth) gets a review first. New doc
`docs/12-deployment/systemd.md` covers install/config/sandboxing/backup/
upgrade/uninstall, linked from `docs/12-deployment/index.md`'s topology table
and doc map (→1.1.0). A `.gitattributes` was added alongside this (`*.sh`/
`*.service` forced to LF) after this session's own Windows dev box warned it
would rewrite the just-authored script's line endings to CRLF on a future
checkout — which would have broken the shebang on a real Linux host
(`bad interpreter`) — the exact class of bug this ticket's own artifacts are
otherwise most exposed to. **The "ideally smoke-tested in CI on Linux" half
of the acceptance criterion is real, not aspirational**: a new
`systemd-install` CI job (`.github/workflows/ci.yml`) runs on a genuine
`ubuntu-latest` VM (systemd actually works there, unlike inside a Docker
container) — builds the release binary, runs `install.sh`, `systemctl enable
--now apex`, polls `/healthz` until healthy (or dumps `journalctl`/`systemctl
status` and fails), **then re-runs `install.sh` and asserts
`/etc/apex/apex.env`'s checksum is unchanged** — the idempotency/never-clobber
claim, checked mechanically rather than left as an unverified comment.

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
| 1.3.0 | 2026-07-14 | WFL-301/302 (engine-native `for_each`/`map` fan-out: runtime-collection expansion into concurrency-capped, durably-resumable per-item instances joined in item order; `max_items` fail-closed bound; collection pinned into the checkpoint on first encounter, never recomputed on resume) implemented and marked DONE with implementation notes — 18 new tests (9 engine integration + 9 definition-load unit) |
| 1.4.0 | 2026-07-14 | ECO-304 (one-shot `apex plugin publish --key`: recomputes real artifact digests from disk, rewrites `plugin.yaml`, signs it, and writes the publisher's `.pub` alongside the package so the printed trust line is directly actionable — collapsing `keygen`→hand-edit-digests→`sign`→operator-`trust` into one command) implemented and marked DONE with implementation notes — 4 new unit tests, no marketplace or wasm toolchain needed to run them |
| 1.5.0 | 2026-07-14 | WFL-303..308 (all remaining WS-H tickets) implemented and marked DONE with implementation notes, closing out WS-H entirely: WFL-303 fail-closed activity-output size cap (permanent failure via the saga path, not a hard abort); WFL-304 event-log paging (`history_page`, real bounded `FileStore`/`PostgresStore` implementations) + explicit opt-in retention (`compact_history`), plus a proof that `resume` already never reads the log at all; WFL-305 indexed `workflow_name`/`status` Postgres columns + SQL-side filtering/pagination (migration V3), proven via a deliberately-corrupt non-matching row that would fail to decode if `list()` ever fell back to scanning; WFL-306 a `fire_at`-sorted `BTreeSet` index for `InMemoryTimerStore` + `TimerDispatcher`/`ScheduleDispatcher::run_adaptive` (sleep until the next deadline, capped at a max interval), proven with a real wall-clock near-deadline-timer test; WFL-307 an `ActivityContext.progress` channel + `ActivityProgress` event, live on the sequential activity path via `tokio::select!`; WFL-308 a versioned event wire envelope (`encode_event`/`decode_event`, fail-closed on an unknown future version) now used by every store. 30 new tests total; full `apex-workflow` suite + whole-workspace `cargo build`/`clippy -D warnings`/`fmt`/`test` clean throughout |
| 1.6.0 | 2026-07-14 | SBX-301..304 (all of WS-E) implemented and marked DONE with implementation notes, closing out WS-E entirely: SBX-301 a confined `fs_write` builtin (opt-in like `shell`) with a write-specific symlink-escape guard beyond `fs_read`'s existing confinement; SBX-302 a sandboxed `code_execute` tool (Python/Node) routed through the identical SBX-101/SEC-305 backend selection `ShellTool` uses, resource-limited and egress-controlled, including a real Windows "app execution alias" false-positive found and fixed in the test gating itself; SBX-303 a new `apex-tool-macros` proc-macro crate (`#[derive(Tool)]`) generating `ToolMetadata`/JSON-Schema (via `schemars`)/typed-parse boilerplate so a tool author never hand-writes a schema literal or a `.get().and_then()` chain; SBX-304 an explicit, documented platform-matrix fail-closed check (Linux+Docker only) in `ContainerSandbox::execute`, replacing what used to be only an *accidental* fail-closed side effect of a missing `nsenter` binary, and additionally closing a real, previously-unguarded Podman gap. 27 new tests total (8 fs_write + 9 code_execute + 4 derive(Tool) + 2 registry opt-in + 4 egress-lockdown-gate); full workspace `cargo build`/`clippy -D warnings`/`fmt`/`test` clean throughout |
| 1.7.0 | 2026-07-14 | SRV-302..307 (all of WS-G) implemented and marked DONE with implementation notes, closing out WS-G entirely: SRV-302 an mtime-stamped in-memory cache for `FileApiKeyStore` (one `stat()` replaces a full read+parse per request in the common case); SRV-303 a real generated OpenAPI spec (`#[utoipa::path]` on all ~65 routes + `ToSchema` request/error types, served at `GET /openapi.json`, verified live end-to-end via a real `apex dev` server + `redocly lint` at 0 errors, with the CI contract gate repointed at the live document instead of the hand-written `openapi.yaml`); SRV-304 `lib.rs`'s inline test suite moved to a file-backed `tests.rs` submodule (2,842 → 444 lines, same crate-internal visibility, no `pub` widening); SRV-305 the idempotency store rewritten from a per-`put` full-file rewrite to an append-only JSON-lines log with periodic compaction (O(1) amortized instead of O(entries) per call); SRV-306 an 11-file audit finding zero attacker-triggerable unwraps in production handler code, plus a real `cfg_attr(not(test), warn(...))`-gated clippy lint (verified to actually fire) guarding regressions; SRV-307 Redis-shared concurrency slots mirroring SRV-201's `RateLimiter` design (atomic Lua increment-if-under-limit, `Drop`-triggered fire-and-forget async release, a documented 24h crash-recovery safety-net TTL), `admit_run` converted to `async fn` across all call sites. 8 new tests total (2 SRV-302 + 2 SRV-305 + 3 SRV-307 capability-gated); a pre-existing, unrelated flaky test in `rate_limit.rs` was found (and, in a same-day follow-up, fixed) on this Windows dev machine while validating SRV-307; full workspace `cargo build`/`clippy -D warnings`/`fmt`/`test` clean throughout, incl. the `redis` feature build |
| 1.9.0 | 2026-07-16 | ECO-303 (container capability loader) implemented and marked DONE with implementation notes: `ContainerCapabilityRuntime` in `apex-plugin` (compiled unconditionally — plugins are finally executable without the heavy `wasi` feature), Docker/Podman/gVisor over `ContainerSandbox`, same stdin/stdout JSON ABI + `APEX_SECRET_*` injection as the WASM loader via shared helpers, fail-closed loader routing (a `gvisor`-declared capability is refused by a plain-Docker runtime, never demoted); enabler in `apex-tools`: `ContainerSandbox::execute_with_stdin` + `SandboxCommand.env` support with env *names* on the argv and values via the CLI process environment (secrets never visible in host `ps`). 8 new tests (2 docker/runsc-gated e2e — both actually executed against the real runtimes on this dev box — + 4 fail-closed unit + 1 argv-shape + 1 WASI-parity via the shared-helper refactor); the pre-existing status line was also corrected to count SEC-301 (done since 2026-07-14 but never listed) |
| 1.8.0 | 2026-07-14 | DEP-301 (systemd unit + install script for the appliance) implemented and marked DONE with implementation notes: `deployment/systemd/apex.service` (real sandboxing — `ProtectSystem=strict` etc. — not decorative) + `apex.env.example`, idempotent `deployment/install.sh` (dedicated system user, `/var/lib/apex/.apex` at `0700`, never overwrites an existing env file), new `docs/12-deployment/systemd.md`, a `.gitattributes` forcing LF on `*.sh`/`*.service` (a real corruption risk caught before it shipped, not hypothetical), and a `systemd-install` CI job that runs the actual install on a genuine systemd VM, polls `/healthz`, and re-runs `install.sh` to mechanically check the idempotency claim rather than trust a comment |
