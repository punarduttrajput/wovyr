<!--
File: docs/17-adr/ADR-0011-generative-ui-repositioning.md
Document ID: ADR-0011
-->

# ADR-0011: Reposition the product as the Generative UI Trust Runtime

**Status:** Accepted
**Date:** 2026-07-14
**Owner:** Founder / Architecture
**Executes into:** [PRD-005](../01-product/prd-generative-ui-runtime.md), [v1.2 roadmap](../18-roadmap/v1.2-generative-ui.md)

---

# 1. Context

Wovyr was conceived and documented as a horizontal "Enterprise AI Agent Operating
System" ("the Linux of AI Agents"). Through v1.1 Phase 2 that produced a deep,
hardened platform: durable event-sourced workflows with human-in-the-loop
suspension, a sandbox spectrum with real egress lockdown, fail-closed tool
permissions, a tamper-evident audit chain, plugin signing + reviewed marketplace,
multi-tenancy/RBAC/KMS, an LLM gateway, and TS/Python SDKs.

Three market facts (July 2026) force a positioning decision:

1. **The horizontal category is a capital war.** Agent platforms (LangGraph,
   Temporal + AI stacks, Vercel AI SDK, every cloud vendor) and AI browsers
   (OpenAI Atlas, Perplexity Comet — free on all platforms, ~18M MAU) are funded
   at levels a small team cannot out-spend. Differentiation-by-breadth is not
   available to us.
2. **Generative UI is inflecting.** Interfaces generated at runtime around user
   intent are shipping in mainstream products (Google Gemini dynamic view, AI Mode
   in Search); open shapes are emerging (A2UI, MCP Apps, AG-UI). 2026 is widely
   called the year this shift starts.
3. **Nobody owns the trust layer.** Generated UI breaks the web's security
   assumptions (unreviewed interfaces, prompt injection manifesting *as UI*, no
   provenance of what was shown, no durable human-decision loop). Feature vendors
   are shipping capability, not governance — and governance is what enterprise
   adoption is blocked on.

An earlier direction — building a consumer **browser** with generative-UI support —
was evaluated and rejected: distribution economics (free, better-funded
incumbents; enormous switching costs) make it unwinnable for a small team, and a
browser is not required to capture the trust-layer value.

# 2. Decision

1. **The product is the Generative UI Trust Runtime** — the infrastructure that
   lets AI agents render interactive interfaces to humans safely, auditable, and
   with durable human-in-the-loop decisions. Three combined plays, per PRD-005:
   the trust/policy layer, generative internal tools as the beachhead vertical,
   and the UI runtime for the broader agent economy (embeddable, MCP-addressable).
2. **The platform becomes the engine, not the pitch.** Wovyr's crates remain the
   foundation and keep their names and contracts; horizontal platform breadth
   (v1.1 P3 "ecosystem & scale" and beyond) is re-prioritized strictly by what
   the trust runtime needs.
3. **Adopt open UI shapes; do not invent a proprietary standard.** The frame
   protocol (`wovyr-ui`) is designed for versioned mapping to/from the emerging
   open component-JSON shapes (A2UI-style, MCP Apps conventions). We compete on
   the runtime and enforcement point, not on schema ownership.
4. **The renderable surface is a constrained, declarative component vocabulary.**
   No raw model-authored HTML/JS is ever rendered. This is the load-bearing
   security decision: most deception/injection classes become *structurally
   impossible* rather than detected.
5. **New code lands as two crates on the existing spine**: `wovyr-ui` (frame
   protocol, transport events, interop mapping) and `wovyr-ui-guard` (policy
   engine, guardrail-stage enforcement, audit integration) — plus a TypeScript
   renderer SDK (`@wovyr/ui-react` + web-component build) outside the Cargo
   workspace. Existing primitives are reused, not duplicated: frames ride
   `RunEventSink`/SSE, decisions ride the `human`-approval signal path, verdicts
   ride `wovyr-audit`, templates ride plugin signing, custom validators ride
   `WasiSandbox`.
6. **No browser.** Explicit non-goal (PRD-005 §4.2).

# 3. Consequences

**Positive**
- Differentiated, largely uncontested positioning; the existing security/durability
  depth converts into the moat instead of overhead.
- Nearly all shipped work is load-bearing for the new product (see PRD-005 §3
  baseline table); estimated 12–18 months of backend build avoided.
- A concrete enterprise buyer (security-gated agent teams; internal-tools/platform
  teams) instead of a diffuse "AI developers" audience.

**Negative / accepted costs**
- The user-facing half (renderer SDK, DX, docs-as-product) is net-new muscle for
  this codebase; P2 of the roadmap carries that risk explicitly.
- Some v1.1-P3-planned horizontal work is deferred indefinitely; contributors
  drawn by the "agent OS" story may perceive a narrowing.
- Interop mapping (UIP-105) takes a dependency on standards still in motion;
  the mapping layer is versioned to contain churn.
- README/vision/marketing surfaces must be rewritten; until then, docs disagree —
  mitigated by updating vision + README in the same change as this ADR.

# 4. Alternatives Considered

1. **Stay horizontal ("Linux of AI Agents")** — rejected: capital-war category,
   no distribution advantage, differentiation-by-breadth unavailable at team size.
2. **Build the generative-UI browser** — rejected: consumer browser distribution
   economics (free incumbents, switching costs) are unwinnable; trust-layer value
   is capturable without owning the chrome.
3. **Pure security scanner for generated UI (no runtime/renderer)** — rejected:
   detection without an enforcement point and decision loop is a feature, not a
   product; the durable interaction loop is the defensible half.
4. **Invent a proprietary UI schema** — rejected: a standards war is already
   underway among better-distributed players; the runtime/enforcement point is
   the durable position (decision §2.3).
5. **Renderer-first (ship a pretty agent-UI kit, add trust later)** — rejected:
   crowded space (assistant-ui, CopilotKit et al.), abandons the differentiator,
   and "add security later" contradicts the codebase's fail-closed engineering
   culture.

# 5. Current Status (2026-07-14)

Accepted; executing. PRD-005 defines requirements; the v1.2 roadmap phases them
(P1 protocol & trust core → P2 renderer & interaction loop → P3 beachhead &
embeddability). No code exists yet for `wovyr-ui`/`wovyr-ui-guard`; nothing in this
ADR changes shipped contracts.
