<!--
File: docs/18-roadmap/index.md
Document ID: RM-INDEX-001
-->

# Roadmap Index

**Document ID:** RM-INDEX-001  
**File Path:** `docs/18-roadmap/index.md`  
**Version:** 1.3.0  
**Status:** Active  
**Owner:** Product Team  
**Last Updated:** 2026-07-15

---

# 1. Purpose

This section describes the planned evolution of the Apex AI Platform — milestone releases and their themes. It complements the high-level phases in the [README roadmap](../../README.md) with per-release detail.

Current position (2026-07-15): **v0.3.0 is tagged**; the v1.0 GA-hardening
engineering scope ([PRD-003](../01-product/prd-ga-hardening.md), all four phases)
is complete, with the Tier-A GA-completion validation workstreams still open —
see [v1.0.md](v1.0.md); **v1.1 is in progress** (Phases 1–2 done, Phase 3
partial); **v1.2 (Generative UI Trust Runtime) and v1.3 (MCP Connection
Management) are shipped**.

---

# 2. Release Themes

| Release | Theme | Doc |
|---------|-------|-----|
| v0.1 | Foundations: core engines runnable | [v0.1.md](v0.1.md) |
| v0.2 | Memory, tools, and the gateway hardened | [v0.2.md](v0.2.md) |
| v0.3 | Plugins, dashboard, and multi-tenancy | [v0.3.md](v0.3.md) |
| v1.0 | Production-ready, GA, enterprise | [v1.0.md](v1.0.md) |
| v1.1 | AI Platform Maturity: capable, operable, extensible (post-GA) | [v1.1/index.md](v1.1/index.md) |
| v1.2 | **Generative UI Trust Runtime** — the product milestone: frame protocol, trust/policy engine, durable interaction loop, renderer SDK, internal-tools beachhead | [v1.2-generative-ui.md](v1.2-generative-ui.md) |
| v1.3 | **MCP Connection Management** — a persisted, UI-managed layer over the shipped MCP client: connection store, agent-manifest wiring, dashboard panel | [v1.3-mcp-connections.md](v1.3-mcp-connections.md) |
| Future | Beyond 1.0 — research bets | [future.md](future.md) |

**v1.1 (AI Platform Maturity)** is scoped by [PRD-004](../01-product/prd-ai-platform-maturity.md)
from a 2026-07-09 five-front engineering audit — real cost accounting, context/token
management, activated sandboxing, native Anthropic, a real RAG middle, an evaluation
gate, MCP/plugin-SDK ecosystem work, and UI/DX/operability maturity. Three phased
ticket docs: [P1](v1.1/phase1-production-truth-tickets.md) ·
[P2](v1.1/phase2-credible-ai-product-tickets.md) ·
[P3](v1.1/phase3-ecosystem-scale-tickets.md).

**v1.2 (Generative UI Trust Runtime)** executes the strategic repositioning of
[ADR-0011](../17-adr/ADR-0011-generative-ui-repositioning.md), scoped by
[PRD-005](../01-product/prd-generative-ui-runtime.md): the platform becomes the
engine; the product is the runtime that lets AI agents render interactive
interfaces to humans safely, auditable, and with durable human-in-the-loop
decisions. v1.1 Phase 3 is re-scoped through PRD-005 — ecosystem items that serve
the trust runtime fold into v1.2 P3; purely horizontal items defer to
[future.md](future.md).

**v1.3 (MCP Connection Management)** executes
[ADR-0012](../17-adr/ADR-0012-mcp-connection-trust-boundary.md), scoped by
[PRD-006](../01-product/prd-mcp-connections.md): a persisted, dashboard-managed
layer over the already-shipped, programmatic-only MCP client (v1.1 P3's
ECO-301) — a connection store, agent-manifest wiring, and a dashboard panel, with
`Stdio`-transport connections gated exactly like the `shell` tool and
`Http`-transport connections reusing the existing SSRF guard. Deliberately
narrower than [future.md](future.md)'s exploratory outbound MCP-gateway/
federation bet (FUT-005) — this milestone is inbound-only.

---

# 3. Principles

1. **Vertical slices** — each release runs end to end, not just lower layers.
2. **Docs-then-build** — specs (this repo) precede implementation.
3. **Security & multi-tenancy early** — not bolted on later.
4. **Dogfood** — the platform builds/operates itself where possible.

---

# 4. How This Maps to the Docs

Each release advances the subsystems documented in sections 03–16. The
[SUMMARY](../SUMMARY.md) tracks which docs exist; the roadmap tracks which
*implementations* land when.

---

# 5. Status & Disclaimer

The roadmap is **directional**, not a commitment; dates and scope adjust with
learning. ADRs ([section 17](../17-adr/index.md)) record decisions that reshape it.

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.3.0 | 2026-07-15 | Status-truth pass: replaced the stale "Planning / Documentation phase (v0.1.0)" claim with the real current position — v0.3.0 tagged, PRD-003 engineering scope complete, v1.1 P3 partial, v1.2/v1.3 shipped |
| 1.2.0 | 2026-07-15 | Added v1.3 (MCP Connection Management, PRD-006/ADR-0012) — a scoped, committed milestone narrower than FUT-005's exploratory outbound MCP-gateway bet |
| 1.1.0 | 2026-07-14 | Added v1.2 (Generative UI Trust Runtime, PRD-005/ADR-0011); noted the v1.1-P3 re-scope |
| 1.0.0 | 2026-06-27 | Initial Roadmap index |
