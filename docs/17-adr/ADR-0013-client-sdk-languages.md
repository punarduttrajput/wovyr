<!--
File: docs/17-adr/ADR-0013-client-sdk-languages.md
Document ID: ADR-0013
-->

# ADR-0013: First-party client SDK languages — TypeScript + Python only

**Status:** Accepted
**Date:** 2026-07-17
**Owner:** Founder / Architecture
**Closes:** RM-AIM-P3 DX-306 (Go/Java client decision)

---

# 1. Context

Two hand-written first-party clients exist and are maintained in lockstep:
`sdks/typescript` (`@wovyr/sdk`) and `sdks/python` (`wovyr-sdk`, on PyPI).
Both mirror the server's real routes 1:1, both carry integration suites CI
runs against a live `wovyr dev` (the contract gate), and both cost real work
on every wire change — the "update both SDKs + openapi.yaml in lockstep"
convention is the tax the repo already pays for two languages.

DX-306 asks for a recorded decision on further languages (Go and Java being
the obvious candidates for the enterprise beachhead), rather than an
undecided gap.

# 2. Decision

**No first-party Go or Java client for now — a documented non-goal.** The
supported integration path for languages beyond TypeScript/Python is the
**generated OpenAPI contract**: `GET /openapi.json` on any running server is
produced from the route annotations themselves (SRV-303) and is the
drift-proof ground truth; `openapi-generator`/`oapi-codegen`-style tooling
against it yields a serviceable client in any language. The API's
conventions were deliberately kept generator-friendly: natural-key paths,
header auth, one error envelope, cursor pagination.

# 3. Rationale

- **Lockstep cost scales linearly with SDK count.** Each additional
  hand-written client adds a per-wire-change tax and a CI leg, paid forever —
  against zero demonstrated demand today (pre-GA, no external consumers).
- **The two existing SDKs cover the actual consumers.** TypeScript serves the
  dashboard + the renderer ecosystem (the product's beachhead); Python serves
  the agent/ML audience. Go/Java consumers are hypothetical until an
  enterprise integration asks.
- **The OpenAPI escape hatch is real, not aspirational** — the served
  document is generated from code and gated in CI, so a third-party generated
  client tracks reality mechanically.

# 4. Revisit trigger

Reopen this decision when either: (a) a concrete external integration
requests Go/Java and a generated client demonstrably falls short (streaming
SSE ergonomics are the likely gap), or (b) GA brings paying consumers whose
platform standard is JVM/Go. The first artifact should then be a *generated*
client hardened with a hand-written SSE/pagination layer, not a third
hand-written SDK.

# 5. Consequences

- `sdks/` stays two-language; docs and the roadmap stop listing "more
  clients" as an open gap and point here instead.
- The contract gate (redocly lint + both SDK suites) remains the invariant
  that keeps third-party generation viable.
