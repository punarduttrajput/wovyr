<!--
File: docs/18-roadmap/future.md
Document ID: RM-005
-->

# Roadmap: Future — Beyond 1.0

**Document ID:** RM-005  
**File Path:** `docs/18-roadmap/future.md`  
**Version:** 1.2.0  
**Status:** Exploratory  
**Owner:** Product Team  
**Last Updated:** 2026-07-05

---

# 1. Theme

**Research bets.** Directions beyond GA that could meaningfully expand what the
platform can do. These are exploratory and not committed.

---

# 2. Candidate Directions

Each direction below is elaborated — problem, design sketch, invariants, risks,
and a graduation gate — in its own exploration doc under
[`future/`](future/index.md) (register: [`future/index.md`](future/index.md)).

## 2.1 Autonomous Multi-Agent Systems
Self-organizing agent swarms and richer
[coordination](../04-agent-framework/multi-agent-coordination.md) — negotiation,
delegation, and emergent task decomposition.
→ **Detail:** [FUT-001](future/B1-multi-agent-systems.md)

## 2.2 Self-Optimizing Platform
- Cost/quality-aware [routing](../05-llm-gateway/routing.md) driven by live model
  scoring.
- Self-tuning [ranking](../06-memory-engine/ranking.md) and warm-pool sizing.
- AI-assisted plugin scaffolding and review.

→ **Detail:** [FUT-002](future/B2-self-optimizing-platform.md)

## 2.3 Advanced Memory
- [Knowledge-graph](../06-memory-engine/knowledge-graph.md) reasoning at scale.
- Multi-modal memory (image/audio), time-travel queries, cross-agent memory fusion,
  confidence scoring ([memory futures](../06-memory-engine/overview.md#16-future-enhancements)).

→ **Detail:** [FUT-003](future/B3-advanced-memory.md)

## 2.4 Execution Frontiers
- Snapshot/restore sandboxes for near-instant cold starts
  ([tool runtime futures](../07-tool-runtime/overview.md#15-future-enhancements)).
- GPU-aware scheduling; edge/regional inference pools.
- WASM component model for portable, polyglot plugins.

→ **Detail:** [FUT-004](future/B4-execution-frontiers.md)

## 2.5 Ecosystem & Interop
- MCP gateway and broader protocol interop.
- Prompt/model registries; marketplace monetization and revenue share.
- Federated, cross-organization plugin and memory sharing.

→ **Detail:** [FUT-005](future/B5-ecosystem-interop.md) (interop + federation;
marketplace monetization is tracked as GA-completion work in
[PRD-002 §5.4](../01-product/prd-future.md#54-ecosystem-marketplace-economics--safety))

## 2.6 Trust & Evaluation
- Built-in AI evaluation service and continuous quality regression gates.
- Stronger provenance, attestation, and policy-as-code maturity.

→ **Detail:** [FUT-006](future/B6-trust-evaluation.md) (upstream prerequisite for
2.1, 2.2, and 2.3)

---

# 3. How Ideas Graduate

```text
future (idea) → PRD requirements → ADR (decision) → release roadmap (v1.x/v2) → docs + build
```

An exploratory item becomes real only via an [ADR](../17-adr/index.md) and a slot in
a concrete release. The **requirements** step — turning each §2 direction into a
problem statement, requirements, success criteria, and a graduation gate — lives
in [`01-product/prd-future.md`](../01-product/prd-future.md) (PRD-002), which
also scopes the remaining work to *complete* v1.0.

---

# 4. Related

- [`01-product/prd-future.md`](../01-product/prd-future.md) — requirements PRD for these directions
- [`18-roadmap/v1.0.md`](v1.0.md) · [`18-roadmap/index.md`](index.md)
- [`00-executive/vision.md`](../00-executive/vision.md)

---

# 5. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-05 | Added per-direction exploration docs under [`future/`](future/index.md) (FUT-001…FUT-006 + index): each §2 direction now links to a doc with problem/design-sketch/invariants/risks/graduation-gate detail |
| 1.1.0 | 2026-07-05 | Linked the new requirements PRD ([`01-product/prd-future.md`](../01-product/prd-future.md), PRD-002) that scopes both these research bets and the remaining v1.0-completion work; added the "PRD requirements" step to the graduation flow |
| 1.0.0 | 2026-06-27 | Initial future roadmap |
