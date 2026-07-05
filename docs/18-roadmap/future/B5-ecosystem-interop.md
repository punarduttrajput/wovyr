<!--
File: docs/18-roadmap/future/B5-ecosystem-interop.md
Document ID: FUT-005
-->

# Future Exploration: Ecosystem & Interoperability

**Document ID:** FUT-005
**File Path:** `docs/18-roadmap/future/B5-ecosystem-interop.md`
**Version:** 1.0.0
**Status:** Exploratory — research bet, not committed
**Owner:** Ecosystem Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Flesh out the "Ecosystem & Interop" research bet
([future.md §2.5](../future.md#25-ecosystem--interop),
[PRD-002 §6.5](../../01-product/prd-future.md#65-ecosystem--interoperability)):
an MCP gateway and broader protocol interop, prompt/model registries, and
federated cross-organization plugin/memory sharing — riding existing
abstractions, with federation fail-closed across trust boundaries.

Exploratory — graduates only via an [ADR](../../17-adr/index.md).

> **Scope note.** Marketplace *monetization* and abuse handling are **Tier A**
> (near-term GA completion, [PRD-002 §5.4](../../01-product/prd-future.md#54-ecosystem-marketplace-economics--safety)),
> not part of this exploratory doc. This doc covers the *interop and federation*
> frontier only.

---

# 2. Problem & Opportunity

The platform is provider- and tool-neutral internally (the `AIProvider` trait,
the `ToolRegistry`) but does not yet **interoperate outward** with the broader
agent/tool ecosystem, nor **federate** across organizations:

- **MCP gateway / protocol interop** — speak the Model Context Protocol (and
  peers) so external tools/agents interoperate with Apex.
- **Prompt/model registries** — versioned, shareable prompt and model catalogues.
- **Federated sharing** — cross-organization plugin and memory sharing.

---

# 3. Current Baseline (what this would build on)

- **Vendor-neutral provider abstraction** — the `AIProvider` trait (chat +
  streaming + embeddings) and the `Gateway` already decouple the platform from
  any one provider; an MCP gateway is another adapter at that boundary.
- **Tool registry + MCP-shaped tools** — `apex-tools`' `ToolRegistry` and the
  plugin tool host already model external capabilities as tools.
- **Marketplace supply chain** — signing, SBOM/provenance, trust store, and the
  human-review workflow (`apex-plugin` / `apex-marketplace`) are the trust
  substrate any federation must extend, not bypass.
- **Hard tenant/org isolation** — `apex-tenancy`'s default-deny RBAC/ABAC and the
  server's cross-tenant isolation are the boundary federation must cross
  *explicitly and revocably*.

---

# 4. Direction (design sketch, non-committal)

- **MCP gateway:** an adapter that exposes Apex tools/agents over MCP and consumes
  external MCP servers as tools — built at the existing provider/tool boundary, so
  the core is unaware of the wire protocol.
- **Registries:** prompt/model registries modeled like the plugin marketplace
  (versioned, signed artifacts) rather than a new bespoke store.
- **Federation:** cross-org sharing as an **explicit, scoped, revocable grant**
  between orgs — never a default, never implicit. Reuses the marketplace trust
  substrate for provenance across the boundary.

---

# 5. Requirements

## 5.1 Functional
- Apex tools/agents are reachable over MCP; external MCP tools are usable inside
  Apex runs.
- Cross-org sharing requires an explicit grant and is fully revocable.
- Shared artifacts carry provenance across the boundary (signed, verifiable).

## 5.2 Invariants to preserve
- **Federation is fail-closed.** Cross-org access defaults to denied; sharing is
  opt-in, scoped, and auditable — the isolation model is not relaxed for
  convenience.
- **Interop rides existing abstractions** — no protocol leaks into the
  deterministic core; adapters live at the edge.
- **Supply-chain trust holds** across federation (no unsigned/unverified
  artifacts crossing an org boundary).

---

# 6. Key Risks & Open Questions

- **Federation as a cross-tenant leak vector** — the central risk; a sharing path
  that under-scopes becomes a data-exfiltration channel.
- **Protocol churn** — MCP and peers are evolving; the adapter must absorb change.
- **Trust transitivity** — does trusting org A's plugin imply trusting its
  dependencies/publishers? Needs an explicit model.
- **Revocation semantics** — what happens to in-flight/derived state when a share
  is revoked?

---

# 7. Graduation Gate

Per-slice; federation specifically becomes an ADR + roadmap slot only with:

> A **threat model + ADR for the cross-org trust boundary** demonstrating
> fail-closed, scoped, revocable sharing with provenance preserved — *before* any
> cross-org path ships. (The MCP-gateway slice can graduate independently on an
> adapter design that keeps the protocol out of the core.)

---

# 8. Dependencies

- Tier A marketplace economics/safety
  ([PRD-002 §5.4](../../01-product/prd-future.md#54-ecosystem-marketplace-economics--safety))
  — a healthy first-party ecosystem should precede federation.
- The existing supply-chain trust model (`apex-plugin` / `apex-marketplace`,
  [ADR-0009](../../17-adr/ADR-0009-keyless-signing.md)).

---

# 9. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.5 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.5
- [`08-plugin-sdk/marketplace.md`](../../08-plugin-sdk/marketplace.md)
- [`17-adr/ADR-0009-keyless-signing.md`](../../17-adr/ADR-0009-keyless-signing.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial exploration doc for the ecosystem/interop research bet |
