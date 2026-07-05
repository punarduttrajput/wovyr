<!--
File: docs/18-roadmap/future/index.md
Document ID: FUT-INDEX-001
-->

# Future Research Bets (Tier B) — Index

**Document ID:** FUT-INDEX-001
**File Path:** `docs/18-roadmap/future/index.md`
**Version:** 1.3.0
**Status:** Exploratory
**Owner:** Product Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

This folder holds one exploration doc per **Tier B research bet** — the
"beyond-1.0" directions from [`future.md §2`](../future.md), turned into
requirements-level detail by [PRD-002 §6](../../01-product/prd-future.md#6-tier-b--research-bets).

Each doc states the problem, what real code it would build on, a non-committal
design sketch, the invariants it must preserve, the risks, and — most
importantly — the **graduation gate**: the evidence required before the bet
becomes an [ADR](../../17-adr/index.md) and takes a slot in a concrete release.

These are **exploratory and not committed.** They exist to make the bets
plannable and prunable, not to promise them.

---

# 2. Register

| Doc | Direction | Origin | Status |
|-----|-----------|--------|--------|
| [FUT-001](B1-multi-agent-systems.md) | Autonomous Multi-Agent Systems | [future §2.1](../future.md#21-autonomous-multi-agent-systems) | Exploratory — prototype slice for direction (b) in `examples/workflows/research-team.yaml` + `apex-server`, pre-ADR |
| [FUT-002](B2-self-optimizing-platform.md) | Self-Optimizing Platform | [future §2.2](../future.md#22-self-optimizing-platform) | Exploratory |
| [FUT-003](B3-advanced-memory.md) | Advanced Memory | [future §2.3](../future.md#23-advanced-memory) | Exploratory |
| [FUT-004](B4-execution-frontiers.md) | Execution Frontiers | [future §2.4](../future.md#24-execution-frontiers) | Exploratory |
| [FUT-005](B5-ecosystem-interop.md) | Ecosystem & Interoperability | [future §2.5](../future.md#25-ecosystem--interop) | Exploratory |
| [FUT-006](B6-trust-evaluation.md) | Trust & Evaluation | [future §2.6](../future.md#26-trust--evaluation) | Exploratory — prototype spike in `crates/apex-eval`, now pointed at FUT-001's workflow via a `compare` module, pre-ADR |

---

# 3. Dependency Ordering

The bets are not independent. **[FUT-006 Trust & Evaluation](B6-trust-evaluation.md)
is upstream of most of the others** — FUT-001, FUT-002, and FUT-003 all state
graduation gates that require a working evaluation harness to substantiate
("outperforms a single agent," "measured improvement / no regression,"
"retrieval-quality change"). So the natural first bet to graduate is FUT-006.

```text
FUT-006 (evaluation harness)
   ├─ enables → FUT-001 (multi-agent: "outperforms?")
   ├─ enables → FUT-002 (self-optimizing: "improved, no regression?")
   └─ enables → FUT-003 (advanced memory: "better retrieval?")
FUT-004 (execution frontiers)  — gated by the sandbox-escape battery, independent
FUT-005 (ecosystem/interop)    — federation gated by a cross-org threat model, independent
```

---

# 4. Graduation Process

Shared with [future.md §3](../future.md#3-how-ideas-graduate):

```text
future (idea) → PRD requirements → ADR (decision) → release roadmap (v1.x/v2) → docs + build
```

These docs are the **requirements** elaboration. A bet becomes committed only via
an ADR and a concrete release slot; bets that never graduate are pruned, not
carried indefinitely.

---

# 5. Related Documents

- [`18-roadmap/future.md`](../future.md) — the research-bet catalogue (RM-005)
- [`01-product/prd-future.md`](../../01-product/prd-future.md) — the requirements PRD (PRD-002)
- [`17-adr/index.md`](../../17-adr/index.md) — where graduation decisions land

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.3.0 | 2026-07-05 | FUT-006's `apex-eval` gained a `compare` module pointed at FUT-001's `research-team.yaml` — updated FUT-006's register row; see [B6-trust-evaluation.md §8.1](B6-trust-evaluation.md#81-pointed-at-fut-001-2026-07-05) for what it proves and doesn't |
| 1.2.0 | 2026-07-05 | FUT-001 gained a code prototype for direction (b) (`examples/workflows/research-team.yaml` + `apex-server`) — updated its register row; see [B1-multi-agent-systems.md §8](B1-multi-agent-systems.md#8-prototype-slice-2026-07-05) for what it proves and doesn't |
| 1.1.0 | 2026-07-05 | FUT-006 gained a code prototype (`crates/apex-eval`) — updated its register row; see [B6-trust-evaluation.md §8](B6-trust-evaluation.md#8-prototype-spike-2026-07-05) for what it proves and doesn't |
| 1.0.0 | 2026-07-05 | Initial Tier B research-bet index (FUT-001…FUT-006) with dependency ordering |
