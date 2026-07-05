<!--
File: docs/18-roadmap/future/B3-advanced-memory.md
Document ID: FUT-003
-->

# Future Exploration: Advanced Memory

**Document ID:** FUT-003
**File Path:** `docs/18-roadmap/future/B3-advanced-memory.md`
**Version:** 1.0.0
**Status:** Exploratory — research bet, not committed
**Owner:** Memory Engine Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Flesh out the "Advanced Memory" research bet
([future.md §2.3](../future.md#23-advanced-memory),
[PRD-002 §6.3](../../01-product/prd-future.md#63-advanced-memory)): knowledge-graph
reasoning, multi-modal memory, time-travel queries, cross-agent fusion, and
confidence scoring — extending the memory engine without weakening its isolation
and encryption guarantees.

Exploratory — graduates only via an [ADR](../../17-adr/index.md).

---

# 2. Problem & Opportunity

The memory engine today is strong but flat: hybrid retrieval (vector + keyword
fused with RRF), a weighted ranker, MMR diversification, ABAC filtering, and
compression, over text content ([`apex-memory`](../../../crates/apex-memory/src/lib.rs)).
It cannot reason over *relationships* between memories, store non-text modalities,
answer "what did we know at time T," fuse memory across agents, or express
confidence.

The opportunity ([memory futures](../../06-memory-engine/overview.md#16-future-enhancements),
[knowledge-graph](../../06-memory-engine/knowledge-graph.md) — tagged v1-deferred):

- **Knowledge-graph reasoning at scale** — entities/edges over the memory corpus.
- **Multi-modal memory** — image/audio embeddings alongside text.
- **Time-travel queries** — as-of-T retrieval over an append-only history.
- **Cross-agent memory fusion** — shared/merged memory within a tenant.
- **Confidence scoring** — a per-memory trust signal feeding the ranker.

---

# 3. Current Baseline (what this would build on)

- **Hybrid retrieval + explainable scoring** — the `score_breakdown` contract
  means any new signal (confidence, graph proximity) can be surfaced, not hidden.
- **Pluggable stores** — `InMemoryStore` / `FileStore` / the capability-gated
  `TieredStore` (Postgres system-of-record + Qdrant ANN); a graph/multi-modal
  store would be another backend behind the same engine.
- **Isolation + encryption already enforced** — ABAC `required_scopes`,
  `tenant:<t>` scoping, and the `EncryptingMemoryStore` wrapper for
  `sensitive` records. These are the guarantees new modalities must not break.

---

# 4. Direction (design sketch, non-committal)

- **Knowledge graph:** a graph layer *derived from* memories (entity/edge
  extraction) rather than a replacement store — retrieval fuses graph proximity
  into the existing RRF, keeping one ranking pipeline.
- **Multi-modal:** modality-tagged embeddings; the gateway's embedding path
  generalizes to non-text encoders. Retrieval stays hybrid.
- **Time-travel:** lean on append-only history + a validity timestamp read only
  at the query boundary (mirroring how the workflow engine keeps time out of the
  deterministic core).
- **Confidence:** a ranker input, surfaced in `score_breakdown`.

Each is a separate slice; an ADR should sequence them (knowledge graph is the
largest and is explicitly the deferred v1 item).

---

# 5. Requirements

## 5.1 Functional
- Graph/multi-modal/time-travel retrieval returns results through the existing
  `MemoryQuery` + `score_breakdown` surface (no parallel, unexplainable path).
- Confidence is a first-class, inspectable ranking factor.

## 5.2 Invariants to preserve
- **Tenant isolation & ABAC hold across every new modality/store** — a graph edge
  or an image embedding is scoped and filtered exactly like a text memory.
- **Encryption holds** — `sensitive` content stays sealed at rest in any new
  backend, as it already is across the tiered Postgres backend.
- **Retrieval stays explainable** — no black-box relevance.

---

# 6. Key Risks & Open Questions

- **Isolation fragmentation** — the biggest risk: a purpose-built graph or
  multi-modal store re-implementing (and subtly weakening) the isolation model.
- **Scale** — graph reasoning "at scale" is unproven against the same NFR targets
  that Tier A ([PRD-002 §5.1](../../01-product/prd-future.md#51-scale--performance-validation))
  hasn't yet validated for flat memory.
- **Encryption vs. indexing** — a purpose-built index can't score ciphertext;
  the existing engine already documents this trade for `sensitive` records, and a
  graph/multi-modal store inherits the same tension.

---

# 7. Graduation Gate

Per-slice; each becomes an ADR + roadmap slot only when it can show:

> The new store/modality **upholds tenant isolation and encryption end to end**
> (proven by test), returns results through the existing explainable retrieval
> surface, and has a credible scale story against the NFR targets.

---

# 8. Dependencies

- [PRD-002 §5.1 Scale Validation](../../01-product/prd-future.md#51-scale--performance-validation)
  — flat-memory scale should be proven before graph-at-scale is attempted.
- [FUT-006 Trust & Evaluation](B6-trust-evaluation.md) — to measure retrieval
  quality changes.

---

# 9. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.3 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.3
- [`06-memory-engine/overview.md`](../../06-memory-engine/overview.md#16-future-enhancements)
- [`06-memory-engine/knowledge-graph.md`](../../06-memory-engine/knowledge-graph.md)
- [`06-memory-engine/ranking.md`](../../06-memory-engine/ranking.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial exploration doc for the advanced-memory research bet |
