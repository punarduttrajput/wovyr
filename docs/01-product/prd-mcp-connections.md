<!--
File: docs/01-product/prd-mcp-connections.md
Document ID: PRD-006
-->

# PRD: MCP Connection Management

**Document ID:** PRD-006
**File Path:** `docs/01-product/prd-mcp-connections.md`
**Version:** 1.0.0
**Status:** Draft — planning input, not a commitment
**Owner:** Product / Founder
**Last Updated:** 2026-07-15

---

# 1. Purpose

`apex-tools::mcp` (RM-AIM-P3 ECO-301, shipped 2026-07-14) already lets Apex connect
to an external MCP (Model Context Protocol) server, discover its tools, and proxy
them into a `ToolRegistry` as permission-checked `Tool` impls — a real agent can
already call a real external MCP server's tool through the normal tool-calling
loop, proven end to end against a live `@modelcontextprotocol/server-filesystem`
instance.

**The gap:** that capability is programmatic only. Using it today means writing
Rust code that constructs a `StdioTransport`/`HttpTransport`, calls
`McpClient::connect`, and registers the result into a `ToolRegistry` by hand before
`run_agent` ever runs — confirmed by direct search of this codebase: zero
occurrences of "mcp" anywhere in `dashboard/src` or `crates/apex-server/src`.
There is no persisted connection, no API, no agent-manifest field, and no
dashboard surface at all.

This PRD scopes closing that gap: **a persisted, UI-managed layer over the
already-shipped MCP client** — connect a server once from the dashboard (or the
API), grant it to specific agents declaratively, and see its tools show up in the
tool picker next to built-ins. No new wire protocol, no change to `McpClient`'s
mechanics — this is a management and safety layer on top of what already works.

**This is explicitly narrower than two adjacent, easily-confused ideas — read
this before scoping any ticket against it:**

- [FUT-005](../18-roadmap/future/B5-ecosystem-interop.md) ("Ecosystem &
  Interop") is an **exploratory, non-committed** research bet covering an
  *outbound* MCP gateway (Apex's own tools/agents reachable **over** MCP by
  external clients), cross-org federation, and prompt/model registries. This
  PRD does none of that.
- [PRD-005](prd-generative-ui-runtime.md)'s **EMB-702** (cut, RM-GUI-P3) was
  narrower still: exposing the generative-UI runtime specifically
  (`ui_present`/`ui_await_decision`) as MCP tools. Also an outbound direction,
  also not this PRD.

This PRD is **inbound only**: Apex *consuming* external MCP servers' tools,
made usable without writing code. That is the entire scope.

---

# 2. Problem Statement

1. **A real, shipped capability nobody can actually use.** ECO-301's own ticket
   doc says it plainly: "Programmatic only... no agent-manifest/server/CLI
   configuration surface for MCP connections yet." A user who wants to connect
   Apex to a real MCP server today needs to write and compile Rust code — most
   of this platform's actual audience (an agent author using the dashboard,
   or an operator running the CLI) cannot do that at all.
2. **No persistence.** A connection built by hand in a Rust binary lives only
   as long as that process. Every other external-integration surface in this
   codebase — plugins, secrets, webhooks — has a durable, tenant-scoped store;
   MCP connections have none.
3. **No declarative agent wiring.** An agent's YAML manifest can already
   allow-list built-in tools (`spec.tools`); there is no equivalent for "this
   agent may use tools from MCP connection X." The registry an MCP tool lives
   in has to be hand-assembled in code before the agent ever runs.
4. **A real, not-yet-addressed security surface.** The MCP client supports a
   `Stdio` transport that spawns an arbitrary local command — verified during
   this PRD's own scoping exercise (`npx -y
   @modelcontextprotocol/server-filesystem <dir>`, a real child process). That
   is the same blast radius as the `shell` tool, which this codebase already
   gates behind an explicit operator opt-in (`APEX_ENABLE_SHELL_TOOL`, SEC-301)
   precisely because it should never be a tenant-self-service feature in a
   hosted deployment. Building a UI on top of the MCP client without carrying
   that same gate forward would quietly reintroduce the exact risk SEC-301
   already closed for the `shell` tool.

---

# 3. Baseline: what already exists to build on

| Existing asset | Role in this PRD |
|---|---|
| `apex-tools::mcp` (ECO-301) — `StdioTransport`/`HttpTransport`/`McpClient`/`register_into` | The wire protocol and registry-proxy mechanics this PRD adds a management layer on top of, **unmodified** |
| `ToolRegistry::execute`'s fail-closed permission check (`mcp:<server>` scope) | The authorization mechanism this PRD's UI only needs to *grant*, never reinvent |
| `http_get`'s SEC-304 SSRF guard (`resolve_and_guard` + DNS-pinned client) | The exact pattern an HTTP-transport MCP connection's egress must reuse verbatim |
| The `shell` tool's opt-in gating (`with_shell()`, `APEX_ENABLE_SHELL_TOOL`, SEC-301) | The direct precedent for gating `Stdio`-transport (arbitrary local command) connections the same way |
| `apex-secrets`'s `Vault` (reference-addressed, tenant-scoped secrets) | Where a connection's auth header/API key must live — a `SecretRef`, never an inline value |
| The plugin/secrets/webhook file-backed stores (`atomic_write` + `FileLock`, DUR-401/403) | The exact persistence shape a new connection store should follow — nothing new to invent |
| `apex-tenancy`'s generic `<domain>:read`/`<domain>:write` scope pattern, plus the `kms:write`/`kms:admin` split | The scope-naming convention (`mcp:read`/`mcp:write`) and the precedent for a materially higher-risk action needing its own `:admin` tier |
| The agent manifest's `spec.tools` allow-list | The exact shape a new `spec.mcp_servers` allow-list mirrors |
| The dashboard's Surfaces panel (`dashboard/src/app/features/surfaces/`) | The concrete UI pattern (compose/configure → call an API → render/manage the result) this PRD's panel should mirror |

---

# 4. Goals & Non-Goals

## 4.1 Goals

- **G1 — Persist connections.** A tenant-scoped store that survives a restart,
  meeting the same durability bar (DUR-401/403/404) every other store in this
  codebase meets.
- **G2 — Manage without writing code.** Create, list, refresh, and delete a
  connection via API and dashboard — zero Rust required for the common path.
- **G3 — Safe by construction, not by operator diligence.** `Stdio`-transport
  connections are gated exactly like the `shell` tool; `Http`-transport
  connections get `http_get`'s SSRF protection for free; a connection's
  credential is a vault reference, never a plaintext field.
- **G4 — Declarative, fail-closed agent wiring.** An agent's manifest names
  which connections it may use; an agent that doesn't name one can't reach its
  tools even if the tenant has it configured.
- **G5 — Discoverable where an author already looks.** MCP-sourced tools
  appear in the existing Agent Studio tool picker, not a separate, easy-to-miss
  surface.
- **G6 — Leave the wire protocol alone.** `McpClient`/the `Tool` proxy
  mechanics are unmodified; every requirement here is persistence, API,
  security-gating, or UI.

## 4.2 Non-Goals

- **Apex as an MCP server (outbound).** FUT-005/EMB-702 territory. Not this PRD.
- **Sandboxing a `Stdio` connection's spawned process.** A real residual risk,
  explicitly flagged (see [ADR-0012](../17-adr/ADR-0012-mcp-connection-trust-boundary.md)),
  not solved here — v1 gates *who* may create such a connection, not *how
  isolated* it runs once created.
- **Federation / cross-org connection sharing.** FUT-005.
- **A marketplace of pre-vetted MCP servers.** A plausible future extension of
  the plugin marketplace's signed-artifact model; out of scope here.
- **Per-tool permission grants within one server.** v1 keeps the existing
  blanket `mcp:<server>` scope (one grant covers a server's whole tool set,
  same as ECO-301 today); finer-grained per-tool grants are a later slice if
  a real need shows up.

---

# 5. Personas & Use Cases

## 5.1 Personas

- **P1 — Platform/tenant admin.** The only role trusted to register a
  `Stdio`-transport connection (arbitrary local command execution) in a hosted
  deployment.
- **P2 — Agent author (Editor role).** Wants to pick an already-registered MCP
  tool for their agent the same way they pick a built-in today — no admin
  rights, no Rust.
- **P3 — Security-conscious operator.** Needs to see, at a glance, which agents
  can reach which external command or URL, and revoke a connection instantly
  with immediate effect.

## 5.2 Canonical use cases

- **UC1 — Local stdio server, admin-gated.** An admin registers a real
  `@modelcontextprotocol/server-filesystem` connection scoped to a docs
  directory; it's refused until the operator opt-in is set; once set, it
  succeeds and the dashboard shows its real discovered tools. The admin grants
  it to one agent by name.
- **UC2 — Remote HTTP server, self-service.** An Editor-role author connects a
  public HTTP-transport MCP server (no local execution risk) and wires it into
  their own agent, entirely from the dashboard, no admin involvement.
- **UC3 — Immediate revocation.** An admin deletes a connection; every agent
  that referenced it fails closed with a clear "unknown tool" error on its very
  next run — never a stale cached success.
- **UC4 — SSRF containment.** A connection's HTTP URL resolves to a private or
  loopback address; registration is refused, and so is every subsequent call —
  the identical guarantee `http_get`'s own SEC-304 tests already prove, applied
  here instead of reimplemented.

---

# 6. Workstreams & Requirements

Requirement IDs are stable and referenced by roadmap tickets
([v1.3 roadmap](../18-roadmap/v1.3-mcp-connections.md)). "Fail-closed" carries
the same meaning as every other PRD in this codebase: an error or unvalidated
state must never degrade into a silently-broader grant.

## WS1 — Connection Core (server) — MCX-1xx

- **MCX-101** `McpConnection` model + a tenant-scoped, file-backed
  `McpConnectionStore` (`atomic_write` + `FileLock`, the DUR-401/403 shape):
  name, transport (`Stdio { command, args }` | `Http { url }`), an optional
  `secret_ref` (a vault `SecretRef`, never an inline value), a
  `tool_permissions` override, created/updated metadata.
- **MCX-102** `POST/GET/DELETE /api/v1/mcp/connections[/{name}]` +
  `POST /api/v1/mcp/connections/{name}/refresh` (re-run `tools/list` and
  return the live discovered tool set — a light JSON-RPC round trip, not a
  restart). RBAC: `mcp:read` for list/get; `mcp:write` for an `Http`-transport
  create/refresh/delete; **`mcp:admin` required specifically for any
  `Stdio`-transport connection** — mirroring the `kms:write`/`kms:admin` split
  for a materially higher-risk action.
- **MCX-103** Hosted-safety gate: a `Stdio`-transport connection is refused
  server-side unless the operator has set `APEX_ENABLE_MCP_STDIO=1` — the
  exact `APEX_ENABLE_SHELL_TOOL` precedent, so the escape hatch is reachable
  only by the deployment operator's own config, never by a tenant alone.
- **MCX-104** An `Http`-transport connection's egress reuses `http_get`'s
  existing SEC-304 `resolve_and_guard`/DNS-pinned-client logic verbatim (shared
  helper, not a second implementation) — refused at registration *and* at
  every subsequent call if the resolved address is loopback/link-local/private/
  metadata.
- **MCX-105** A connection's optional credential (bearer token/header for an
  HTTP server) is a `SecretRef` into `apex-secrets`'s `Vault`; the connection
  store itself never holds a raw value — the tool-call path resolves it
  tenant-scoped, same as a plugin capability's secret injection today.
- **MCX-106** A per-connection client cache (keyed by tenant + connection
  name), not a spawn-per-call: an `Http` connection reuses one client; a
  `Stdio` connection's spawned process is kept warm across calls within a
  bounded idle timeout and torn down on edit/delete/idle-expiry — bounded by a
  per-tenant connection-count quota dimension (`apex-tenancy::QuotaLimits`,
  the same pattern token/cost budgets already use).

## WS2 — Agent & Tool Integration — MCX-2xx

- **MCX-201** `AgentDefinition`'s manifest gains an optional
  `spec.mcp_servers: [<connection-name>, ...]` allow-list, parallel to the
  existing `spec.tools`. A run resolves each named connection (scoped to the
  run's tenant) and registers *only* its currently-discovered tools into that
  run's `ToolRegistry` — an agent that doesn't name a connection cannot reach
  its tools even if the tenant has it configured (fail-closed, the platform's
  standing default-deny stance).
- **MCX-202** `GET /api/v1/tools` (already the Agent Studio tool picker's data
  source) includes currently-registered `mcp__<server>__<tool>` ids for
  connections the caller's tenant has configured, so an author picks an MCP
  tool exactly like a built-in.
- **MCX-203** A connection's discovered-tool list is resolved per run, bounded
  by MCX-106's cache — never silently stale past the cache's TTL; MCX-102's
  explicit `/refresh` forces immediate re-discovery for the dashboard's
  "see what's new" action.
- **MCX-204** Workflow `agent`/`tool` activities inherit MCX-201's wiring for
  free — they already run through the shared `run_agent`/`ToolRegistry` path
  (`apex-runtime`'s `PlatformActivityExecutor`), so no separate
  workflow-specific implementation is needed.

## WS3 — SDK & Dashboard — MCX-3xx

- **MCX-301** TypeScript SDK gains an `mcp` resource
  (`list`/`create`/`delete`/`refresh`), mirroring `UiResource`'s shape.
- **MCX-302** A dashboard "MCP Servers" panel mirroring the Surfaces panel's
  compose → call → render pattern: a form to add a connection (name,
  transport choice, command+args or URL, optional secret-reference picker), a
  list of configured connections with their live discovered tool counts, and
  per-connection refresh/delete actions. The `Stdio` transport option is only
  offered when the connected server reports `APEX_ENABLE_MCP_STDIO` is on
  (MCX-103) — never silently shown but rejected on submit.
- **MCX-303** The Agent Studio's existing tool picker surfaces MCP-sourced
  tools (fed by MCX-202) alongside built-ins, so wiring
  `spec.mcp_servers` into a manifest is something an author discovers by
  browsing, not something they have to already know exists.

---

# 7. Phasing

Detailed tickets live in the [v1.3 roadmap](../18-roadmap/v1.3-mcp-connections.md).

| Phase | Theme | Workstreams | Exit criterion |
|---|---|---|---|
| **P1 — Connection Core** | Persist, secure, gate | MCX-1xx | A connection created via API survives a server restart; a `Stdio` connection is refused without the operator opt-in; an `Http` connection pointed at a private IP is refused the same way `http_get` already proves; a credential never appears in the connection store's on-disk file |
| **P2 — Agent Wiring** | Declarative, fail-closed | MCX-2xx | An agent manifest naming a connection picks up its tools with zero Rust code; an agent that doesn't name it can't reach it even with the tenant's connection configured; a workflow `agent` activity gets the same wiring for free |
| **P3 — Dashboard & DX** | No-code, discoverable | MCX-3xx | An admin connects a real external MCP server, an Editor-role author picks its tool in Agent Studio, and runs the agent — entirely from the dashboard, no terminal |

---

# 8. Success Metrics

- **Zero-code time-to-first-external-tool-call:** an author with no Rust
  experience wires an agent to a real MCP server's tool from the dashboard in
  under 10 minutes (this PRD's version of PRD-005's 30-minute quickstart bar —
  tighter, because there's no new protocol or policy concept to learn, just
  point-and-click).
- **Security, verified not asserted:** every `Stdio`-transport connection
  attempt without the operator opt-in is refused, 100% of the time
  (CI-gated); every `Http`-transport connection pointed at a
  private/loopback/link-local/metadata address is refused, 100% — reusing
  SEC-304's own proven test vectors rather than writing new ones from scratch.
- **No plaintext credential ever appears** in the connection store's on-disk
  file — a direct grep-the-file test, the same shape `apex-secrets`'s own
  encrypting-store tests use.

---

# 9. Acceptance Narrative

> A platform admin registers a real `@modelcontextprotocol/server-filesystem`
> connection (stdio, scoped to a docs directory) from the dashboard — refused
> until they set the operator opt-in, then succeeds and shows its 14 real
> tools. They grant it to a "docs-qa" agent. A teammate with only Editor
> rights opens Agent Studio, sees `mcp__docs__read_text_file` in the tool
> picker next to the built-ins, adds it to a new agent's manifest, and runs
> it — the agent reads a real file through a real external MCP server and
> answers correctly, with no one having written a line of Rust or touched a
> terminal. The admin then deletes the connection; the agent's next run fails
> closed with a clear "unknown tool" error, never a stale success.

Every clause maps to a requirement above; this is the acceptance test first,
the demo second.

---

# 10. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| `Stdio` connections become a de facto unsandboxed remote-code-execution feature for hosted tenants | MCX-103's operator-only opt-in (mirrors SEC-301's `shell`-tool stance) + `mcp:admin` scope tier; no sandboxing in v1 is an explicit, documented residual risk ([ADR-0012](../17-adr/ADR-0012-mcp-connection-trust-boundary.md)), not a silent gap |
| An `Http` connection becomes a new SSRF vector distinct from `http_get`'s already-solved one | MCX-104 reuses the *exact* SEC-304 mechanism rather than a parallel, possibly-weaker reimplementation |
| A connection's credential leaks via the connection store file | MCX-105: only a `SecretRef` is ever persisted; the value lives only in the vault, sealed at rest like every other secret |
| Warm-process caching (MCX-106) becomes a resource-exhaustion vector (many idle spawned processes) | Bounded idle timeout + a per-tenant connection-count quota dimension, the same pattern token/cost budgets already use |
| Scope creep into "Apex as MCP server" / federation | Explicit non-goals (§4.2); every ticket reviewed against the FUT-005/EMB-702 boundary before it starts |

---

# 11. Relationship to Other Docs

- [ADR-0012](../17-adr/ADR-0012-mcp-connection-trust-boundary.md) — the
  trust-boundary decision this PRD depends on.
- [v1.1 Phase 3 tickets](../18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md)'
  **ECO-301** — the shipped MCP-client foundation this PRD manages, unmodified.
- [FUT-005](../18-roadmap/future/B5-ecosystem-interop.md) — the larger,
  non-committed *outbound* MCP-gateway/federation ambition this PRD is
  explicitly not.
- [PRD-005](prd-generative-ui-runtime.md) §6 **EMB-702** — the cut "expose
  `ui_present` as MCP tools" slice; a different, outbound direction, not
  reactivated by this PRD.
- [v1.3 roadmap](../18-roadmap/v1.3-mcp-connections.md) — phased tickets.

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-15 | Initial PRD: a persisted, UI-managed connection layer over the already-shipped MCP client (ECO-301) — connection store, agent-manifest wiring, dashboard panel. Scoped inbound-only, explicitly distinct from FUT-005/EMB-702's outbound ambitions |
