<!--
File: docs/17-adr/ADR-0012-mcp-connection-trust-boundary.md
Document ID: ADR-0012
-->

# ADR-0012: The trust boundary for user-managed MCP connections

**Status:** Accepted
**Date:** 2026-07-15
**Owner:** Founder / Architecture
**Executes into:** [PRD-006](../01-product/prd-mcp-connections.md), [v1.3 roadmap](../18-roadmap/v1.3-mcp-connections.md)

---

# 1. Context

`wovyr-tools::mcp` (RM-AIM-P3 ECO-301, shipped 2026-07-14) lets Wovyr connect to an
external MCP server and proxy its tools into a `ToolRegistry`. It is
**programmatic only** — no persisted connection, no API, no UI. [PRD-006](../01-product/prd-mcp-connections.md)
scopes making it a first-class, dashboard-managed capability. Doing that safely
requires deciding, up front, how much trust an MCP connection carries — because
the two transports `McpClient` already supports are not equally dangerous:

- **`StdioTransport`** spawns an arbitrary local process (`program`, `args`) and
  speaks JSON-RPC over its stdin/stdout. Registering a `Stdio` connection *is*
  choosing to execute an arbitrary local command with the platform process's own
  privileges — verified directly during PRD-006's scoping work (`npx -y
  @modelcontextprotocol/server-filesystem <dir>`, a real child process, no
  sandboxing at the transport layer at all).
- **`HttpTransport`** POSTs JSON-RPC to a URL. No local execution, but a URL an
  operator or tenant supplies is exactly the shape of every SSRF vector this
  codebase has already had to close once, for `http_get` (RM-GA-P1 SEC-304):
  loopback/link-local/private/cloud-metadata addresses, DNS rebinding between
  the resolve and the connect.

This codebase already has a precedent for exactly the `Stdio` risk: the `shell`
tool is not in `ToolRegistry::with_builtins()` — it requires an explicit,
operator-only opt-in (`with_shell()`/`WOVYR_ENABLE_SHELL_TOOL`, SEC-301) precisely
because handing every agent arbitrary command execution by default is not a
decision a hosted deployment should make silently. A dashboard "Add MCP Server"
button that lets any authenticated user type a shell command and have it run
server-side would quietly reintroduce the exact risk SEC-301 already closed —
just through a different door.

# 2. Decision

1. **A `Stdio`-transport connection is exactly as privileged as the `shell`
   tool, and is gated identically.** It requires:
   - An explicit operator opt-in at the deployment level
     (`WOVYR_ENABLE_MCP_STDIO=1`), refused otherwise regardless of caller — the
     same shape as `WOVYR_ENABLE_SHELL_TOOL`. A tenant cannot reach this on
     their own no matter what role they hold; only the operator's own process
     environment can.
   - A distinct RBAC tier above ordinary write access: `mcp:admin`, not
     `mcp:write` — mirroring the `kms:write`/`kms:admin` split already in this
     codebase for "routine write" vs. "materially higher blast radius."
     Creating, editing, or refreshing a `Stdio` connection requires `mcp:admin`;
     an `Http` connection only requires `mcp:write`.
2. **An `Http`-transport connection reuses `http_get`'s SEC-304 SSRF guard
   verbatim** — the same `resolve_and_guard`/DNS-pinned-client mechanism,
   extracted to a shared helper rather than re-derived. Refused at
   registration time *and* at every subsequent call, identically to `http_get`
   today. No second, possibly-divergent SSRF implementation.
3. **A connection's credential is a secret reference, never a value.** If an
   `Http` connection needs an auth header, the connection record stores a
   `SecretRef` into `wovyr-secrets`'s `Vault`, tenant-scoped exactly like every
   other secret in this codebase. The connection store itself is never an
   acceptable place for a raw credential to live, encrypted or not — it
   already has a durable, tenant-scoped, audited place to live.
4. **No sandboxing of a `Stdio` connection's spawned process in v1 — stated as
   a residual risk, not silently assumed away.** `Stdio` connections run with
   the platform process's own OS privileges, unconfined by the sandbox
   spectrum (`NativeSandbox`/`ContainerSandbox`/`WasiSandbox`/etc.) that other
   dynamic-code paths in this codebase already have. Gate 1 above (operator
   opt-in + `mcp:admin`) is v1's *entire* mitigation for this — it controls
   *who* can create such a connection, not *how contained* it runs once
   created. A sandboxed variant (spawn inside `ContainerSandbox` the way
   `code_execute` already can) is a named, explicit follow-on, not a
   commitment made by this ADR.
5. **The wire protocol and registry-proxy mechanics are unmodified.**
   `McpClient`, `StdioTransport`, `HttpTransport`, and the `Tool` proxy that
   `register_into` produces do not change. Everything above is enforced in the
   new management layer PRD-006 scopes (the connection store, the API routes,
   the agent-manifest resolution step) — never inside `wovyr-tools::mcp` itself.

# 3. Consequences

**Positive**
- Every genuinely new risk this feature introduces (arbitrary local execution,
  SSRF, credential storage) is closed by reusing a mechanism this codebase has
  already built, tested, and shipped for a structurally identical problem —
  no novel security design, no new class of bug to discover the hard way.
- The `Stdio`/`Http` distinction is legible to an operator at decision time:
  the dashboard can (and per PRD-006 MCX-302, must) show *why* the `Stdio`
  option is unavailable rather than silently hiding or, worse, silently
  allowing it.
- A future sandboxed-`Stdio` slice has a clear, already-anticipated landing
  spot (the existing `SandboxBackend`/`TrustClass` spectrum) rather than
  needing its own design from scratch.

**Negative / accepted costs**
- `Stdio` connections stay a genuinely unsandboxed capability in v1 — a
  deployment that enables `WOVYR_ENABLE_MCP_STDIO` is accepting that any
  `mcp:admin`-scoped principal can execute arbitrary local commands, exactly
  as they already accept for the `shell` tool. This is a real, standing risk,
  not a solved one — it is scoped down to "who," not eliminated.
- Two RBAC tiers for one feature (`mcp:write` vs `mcp:admin`, split by
  transport) is more nuanced than a single `mcp:write` gate would be — the
  dashboard and docs must explain this distinction clearly or it will
  surprise an operator expecting one uniform "MCP" permission.
- Extracting SEC-304's guard into a shared helper touches `wovyr-tools`'
  existing, already-tested `http_get` code path — a real, if small,
  refactor risk against working code, to be done under the existing
  `http_get` SSRF test suite, not a rewrite.

# 4. Alternatives Considered

1. **One uniform `mcp:write` scope for both transports** — rejected: it would
   either under-gate `Stdio` (treating remote-code-execution risk the same as
   "point at a URL") or over-gate `Http` (making a harmless remote-lookup
   connection require admin rights for no reason). The split costs a small
   amount of documentation clarity for a real reduction in blast radius.
2. **Sandbox `Stdio` connections from day one (e.g., always spawn inside
   `ContainerSandbox`)** — rejected for v1: real added complexity (container
   runtime dependency, egress-lockdown wiring, cross-platform story) for a
   feature whose primary near-term use case (PRD-006 UC1: a docs-directory
   filesystem server) doesn't need it to be useful, and the existing
   `shell`-tool precedent already accepts unsandboxed-but-gated as the interim
   stance. Revisit if a design partner's real workload needs it.
3. **Ban `Stdio` transport entirely for v1, ship `Http`-only** — rejected: a
   large share of real-world MCP servers (filesystem, git, local dev tools)
   are stdio-only; shipping without it would make the feature far less useful
   for the exact "connect a local tool" use case that motivates PRD-006, and
   the `shell`-tool precedent shows this codebase already has a workable
   gating pattern rather than needing to avoid the risk outright.
4. **Store connection credentials inline, encrypted with a connection-specific
   key** — rejected: `wovyr-secrets`'s `Vault` already solves tenant-scoped
   credential storage, rotation, and at-rest encryption; a second, bespoke
   encryption scheme for one feature's credentials would be duplicated,
   untested machinery solving an already-solved problem.

# 5. Current Status (2026-07-15)

Accepted; no code exists yet. PRD-006 defines the requirements this decision
gates (MCX-102/103/104/105); the v1.3 roadmap phases them. Nothing in this ADR
changes any shipped contract — `wovyr-tools::mcp` (ECO-301) is unmodified by
this decision, only wrapped by a new management layer that has not been built.
