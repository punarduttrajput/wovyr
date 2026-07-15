<!--
File: docs/07-tool-runtime/e2b-gap-analysis.md
Document ID: TRT-GAP-001
-->

# Tool Runtime: E2B Gap Closure (Next Phase)

**Document ID:** TRT-GAP-001
**File Path:** `docs/07-tool-runtime/e2b-gap-analysis.md`
**Version:** 1.0.1
**Status:** Planned — not started as scoped. The v1.1 P3 sandbox tickets
(SBX-301 `fs_write`, SBX-302 `code_execute`) shipped adjacent **tool-surface**
capabilities, not this doc's sandbox-**session** APIs (persistent sessions,
filesystem/process APIs, streaming) — those all remain open
**Owner:** AI Platform Team
**Last Updated:** 2026-07-15

---

# 1. Purpose

This document captures the **gaps that matter** between Apex's `apex-tools`
sandbox layer and E2B (the agent code-execution sandbox), scoped as the next
development phase. It is deliberately narrow: it lists only the gaps a team
evaluating Apex for *agent code execution* would hit, and that are worth closing
given Apex's positioning. Capabilities that conflict with Apex's embedded,
security-first model — or that are pure hosted-product surface — are
**out of scope** (see [§6](#6-explicitly-not-doing)).

For the full competitive comparison and the rationale behind this scope, see the
positioning note in [§7](#7-related).

---

# 2. Framing

Apex and E2B optimize for different jobs:

- **E2B** is a **persistent sandbox-as-a-service**: spawn a long-lived microVM,
  run many commands against it over its lifetime (stateful filesystem, processes,
  a code-interpreter REPL), driven by **Python/JS SDKs**.
- **Apex `apex-tools`** is an **embedded, one-shot isolation primitive**: a Rust
  library compiled into the agent binary that runs *one* `SandboxCommand` and
  returns a `CommandOutcome`, picking the **strongest of** (tool preference,
  tenant floor, trust class) across a backend **spectrum**
  (native → WASI → container → gVisor → Firecracker → k8s), then checking node
  capability.

Apex's differentiators — the isolation spectrum, trust-graded floors,
deny-by-default egress, and fully embedded/air-gapped operation — are **stronger
than E2B** on the dimensions they cover. The gaps below are everything *above*
the isolation primitive: the stateful, interactive, developer-facing surface that
makes E2B the default for coding/data agents. Closing them must **not** abandon
the embedded, security-first model.

---

# 3. Gaps In Scope (priority order)

Effort is rough (S ≤ 1 wk, M ≈ 1–3 wk, L ≈ 1 mo+, per engineer). Impact is the
perceived value to a team evaluating Apex for agent code execution.

| # | Gap | Impact | Effort | Status |
|---|-----|--------|--------|--------|
| G1 | Persistent / stateful sandbox sessions | High | L | Planned |
| G2 | Filesystem API (read/write/list/watch) | High | M | Planned |
| G3 | Process API (start/stream/stdin/kill) | High | M | Planned |
| G4 | Streaming stdout/stderr | Med-High | S–M | Planned |
| G5 | Code-interpreter with rich outputs | Med-High | M | Investigate |
| G6 | Thin Python/JS client SDK | High | M | Planned |
| G7 | Custom environment templates + cache | Med | M | Planned |

The enabling insight for G1–G4: the **Firecracker backend already runs a guest
agent** inside the VM (the one-shot `/init` block-device protocol in
`deployment/firecracker/`). Evolving that agent into a **long-lived in-VM API
server** — the equivalent of E2B's `envd` — is the single foundation that unlocks
sessions, filesystem, processes, and streaming. **Build it once, reuse it four
times.**

---

## G1 — Persistent / stateful sandbox sessions

**Problem.** Every call is one-shot today: a `SandboxCommand` runs and the sandbox
is torn down. Firecracker literally boots, runs, and reboots/exits per call. A
code-interpreter agent needs a *living* sandbox — define a variable, run more
code, it's still there; install a package once, reuse it. The one-shot model
cannot express a stateful REPL, and re-booting a microVM per call is also slow.

**Approach.** Promote the in-VM guest agent to a **long-lived API server**: boot
the sandbox once, keep it warm, and expose a session over a host↔guest channel
(vsock for Firecracker; exec-into for container/gVisor). Add a session lifecycle
to the `Sandbox` trait — `open() -> Session`, `Session::exec(cmd)` (repeatable),
`Session::close()` — layered over the existing `SandboxPool` for warm reuse.
Sessions carry an idle TTL and a hard max lifetime; on expiry the pool reclaims
them. The one-shot path stays as a convenience wrapper (`open` + single `exec` +
`close`).

**Acceptance.**
- A session survives across multiple `exec` calls with filesystem + process state
  preserved between them.
- Idle TTL and max-lifetime enforced; reclaimed deterministically by the pool.
- Works on at least container/gVisor first, then Firecracker via vsock.

**Touches.** `sandbox.rs` (session trait + lifecycle), `pool.rs` (session-aware
pooling), `deployment/firecracker/` (guest agent → server), the Firecracker
host↔guest channel.

---

## G2 — Filesystem API

**Problem.** Only a bind-mounted workspace exists — no programmatic read/write/
list/watch. Coding agents need to write files, read results, and list a tree
inside the sandbox over a session's lifetime.

**Approach.** Extend the G1 guest-agent protocol with filesystem operations:
`read(path)`, `write(path, bytes)`, `list(dir)`, `stat`, `remove`, and `watch`
(change events). Enforce the same path-confinement and read-only-rootfs rules the
container backend already applies; all access stays inside the sandbox boundary.

**Acceptance.**
- Read/write/list/stat/remove round-trip within a session; writes are visible to
  subsequent `exec`s.
- `watch` streams change events; path confinement prevents escape from the
  workspace.

**Touches.** Guest agent protocol, `sandbox.rs` (FS surface on `Session`),
[`filesystem.md`](filesystem.md).

---

## G3 — Process API

**Problem.** One command per exec, no interaction. Real coding/data agents start
long-running processes (a dev server, a notebook kernel), stream their output,
write to stdin, and kill them.

**Approach.** Add process control to the session: `start(cmd) -> ProcessHandle`,
`stdin.write`, `stdout/stderr` streams, `signal/kill`, and optional PTY
allocation for interactive tools. Bound by the existing `ResourceLimits`
(timeout, output cap, cgroup CPU/mem/PID).

**Acceptance.**
- Start a process, stream its output, write stdin, and kill it within a session.
- Multiple concurrent processes respect the sandbox's resource limits.

**Touches.** Guest agent protocol, `sandbox.rs` (`ProcessHandle`),
[`execution-api.md`](execution-api.md).

---

## G4 — Streaming stdout/stderr

**Problem.** Output is captured-then-returned; nothing streams. Interactive UX and
long-running commands need incremental output. (Already flagged deferred in the
crate docs and [roadmap v0.2](../18-roadmap/v0.2.md).)

**Approach.** Surface a streaming variant of `exec`/`start` that yields output
chunks as they arrive (mirroring the agent runtime's `Delta` streaming and the
gateway's `chat_stream`), backed by the guest-agent channel. The non-streaming
`CommandOutcome` becomes a fold over the stream.

**Acceptance.**
- A long-running command's output arrives incrementally, not only at completion.
- Output cap still enforced across the stream.

**Touches.** `sandbox.rs` (streaming exec), guest agent channel,
[`execution-api.md`](execution-api.md).

---

## G5 — Code-interpreter with rich outputs  *(investigate first)*

**Problem.** No first-class "run code, get results + charts/images" surface — the
capability that made E2B's Code Interpreter a default for data-analysis agents.
Needs the G1–G4 foundation and a design decision on scope.

**Approach (spike).** Layer a code-interpreter convenience on top of sessions: a
prebuilt template (G7) with a Jupyter-style kernel, an `run_code(lang, src)` that
returns structured results (stdout/stderr **plus** rich MIME outputs — tables,
images, charts), and result framing the agent runtime can render. Evaluate
reusing an existing kernel protocol vs. a minimal custom one.

**Acceptance (for the spike).** A decision recorded as an ADR
([section 17](../17-adr/index.md)) with a prototype running Python that returns a
chart image and a stdout stream from one session.

**Touches.** New code-interpreter layer over `Session`, a kernel template, an ADR.

---

## G6 — Thin Python/JS client SDK

**Problem.** The execution surface is Rust-only. Agent developers live in
Python/TS; a Rust-only API caps adoption no matter how strong the core is.

**Approach.** Expose the session/filesystem/process API over the existing server
([`execution-api.md`](execution-api.md)) and ship **thin** Python and JS/TS
clients mirroring the E2B SDK shape (`Sandbox.create()`, `.commands.run()`,
`.files.read/write()`, `.process.start()`). The clients are transport shims over
the server contract — the isolation core stays in Rust. Scope to parity with the
session/FS/process/stream surface; nothing bespoke.

**Acceptance.**
- A Python and a JS example create a sandbox, write a file, run code, and stream
  output against a running `apex-server`.
- SDK surface is documented and versioned with the server contract.

**Touches.** `apex-server` (session/FS/process routes), new `sdk/python` +
`sdk/js` clients, [`execution-api.md`](execution-api.md).

---

## G7 — Custom environment templates + cache

**Problem.** Users bring their own container image by hand; no template/prebuilt
story. E2B's Dockerfile templates + prebuilt environments + registry are a major
ergonomics driver (and feed G5's interpreter kernel).

**Approach.** A `Template` (base image / rootfs + setup steps + declared
resources) that builds once and is cached for fast session start; a small set of
prebuilt templates (a code-interpreter kernel, a generic toolbox). Reuse the
existing image/rootfs plumbing; no marketplace — that stays a
[v0.3](../18-roadmap/v0.3.md) concern.

**Acceptance.**
- A user-defined template builds, caches, and starts sessions measurably faster
  than cold image pull.
- At least one prebuilt template (code-interpreter) ships and is documented.

**Touches.** New `template.rs`, `pool.rs` (template-keyed warm sets),
`deployment/firecracker/` (rootfs build), docs.

---

# 4. Suggested Sequencing

1. **G1 (sessions)** first — the guest-agent-as-server foundation that unlocks
   G2/G3/G4. Highest-leverage work in the phase.
2. **G2 (filesystem)** + **G3 (process)** + **G4 (streaming)** — all ride the
   same guest-agent channel; parallelizable once G1 lands.
3. **G6 (SDK)** — as soon as the session/FS/process surface stabilizes on the
   server, so external developers can use it.
4. **G7 (templates)** — needed before G5 (provides the interpreter kernel).
5. **G5 (code interpreter)** — spike + ADR, then build on G1–G4 + G7.

---

# 5. Cross-Cutting Acceptance

- Every gap preserves the security model: path confinement, read-only rootfs,
  cgroup/`setrlimit` resource caps, and **deny-by-default egress** via the
  [`EgressProxy`](../../crates/apex-tools/src/egress.rs) all hold for sessions,
  not just one-shot calls ([security-isolation.md](security-isolation.md)).
- Backend *selection* stays pure and deterministic; only node capability
  detection and the guest-agent channel do ambient I/O.
- Sessions respect `TrustClass` floors — an untrusted tool's session still runs
  in gVisor/Firecracker, never relaxed for convenience.
- The [README](../../README.md) status, this doc's own status table, and the
  roadmap exit criteria are updated as each gap lands.

---

# 6. Explicitly Not Doing

These E2B characteristics are **out of scope** because they conflict with Apex's
positioning or are pure hosted-product surface, not because they were overlooked:

- **Hosted multi-tenant cloud service + billing/metering dashboard** — Apex is an
  embedded, self-hosted library; managed-service surface is a separate product
  bet, not a runtime feature.
- **Internet-on-by-default networking** — Apex is deliberately deny-by-default
  ([`EgressProxy`](../../crates/apex-tools/src/egress.rs)); convenience networking
  would undercut the security differentiator.
- **Single fixed isolation backend** — Apex's spectrum + trust-graded floors are a
  feature, not a gap to "simplify away."
- **Desktop / GUI (computer-use) sandboxes** — revisit only if a concrete agent
  use case appears; not core to code execution.

---

# 7. Related

- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md) ·
  [`security-isolation.md`](security-isolation.md) ·
  [`execution-api.md`](execution-api.md) ·
  [`filesystem.md`](filesystem.md) ·
  [`worker-pool.md`](worker-pool.md)
- [`18-roadmap/v0.2.md`](../18-roadmap/v0.2.md) (streaming, egress proxy) ·
  [`18-roadmap/v0.3.md`](../18-roadmap/v0.3.md) (marketplace / dashboard)
- Implementation: [`crates/apex-tools`](../../crates/apex-tools/src/lib.rs) ·
  [`deployment/firecracker/`](../../deployment/firecracker)
- Sibling analysis: [`03-workflow-engine/temporal-gap-analysis.md`](../03-workflow-engine/temporal-gap-analysis.md)

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.1 | 2026-07-15 | Status clarified: still not started as scoped; noted that SBX-301/302 shipped adjacent tool-surface builtins, distinct from these sandbox-session APIs |
| 1.0.0 | 2026-06-29 | Initial E2B gap-closure scope for the next phase |
