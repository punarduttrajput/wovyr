<!--
File: docs/18-roadmap/v1.0/A1-scale-performance.md
Document ID: GA-001
-->

# GA Completion: Scale & Performance Validation

**Document ID:** GA-001
**File Path:** `docs/18-roadmap/v1.0/A1-scale-performance.md`
**Version:** 1.0.0
**Status:** Planned — GA-completion work, not started
**Owner:** Platform Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Turn the "Scale & Performance Validation" GA gap
([PRD-002 §5.1](../../01-product/prd-future.md#51-scale--performance-validation),
[v1.0 §3 Scale row](../v1.0.md#3-in-scope)) into a concrete delivery plan:
current state, work breakdown, exit criteria, and the environment dependencies
that block validation today.

Unlike the Tier B [research bets](../future/index.md), this is **committed
intent** — scoped, near-term, GA-blocking work, not an exploratory bet.

---

# 2. Current State

- **Perf tests exist but are deliberately toy-scale.** Assertion-style p95 gates
  run under ordinary `cargo test`:
  [`crates/wovyr-provider/tests/perf.rs`](../../../crates/wovyr-provider/tests/perf.rs)
  (gateway overhead) and
  [`crates/wovyr-memory/tests/perf.rs`](../../../crates/wovyr-memory/tests/perf.rs)
  (warm retrieval), on the order of hundreds of records against the in-process
  mock provider. Large headroom keeps them stable in CI — they prove *no
  regression*, not *scale*.
- **The horizontal-scaling mechanics are built.** Workflow queue partitioning
  (G6) shards executions across disjoint worker pools, and a scaling envelope
  with measured single-node baselines is published in
  [distributed-execution §3.3](../../03-workflow-engine/distributed-execution.md#33-scaling-envelope-g6).
- **The tiered memory backend exists** (Postgres system-of-record + Qdrant ANN,
  behind the `tiered` feature) with capability-gated integration tests.

---

# 3. Gap

The NFR targets — **billions of memories, thousands of concurrent runs**
([performance-tests](../../15-testing/performance-tests.md)) — are **unvalidated
against real capacity**. The mechanics are in place; the proof at scale is not.

---

# 4. Scope & Requirements

## 4.1 Functional / deliverables
- A reproducible **load-generation harness** (memory ingest/query, workflow
  submit/drive) parameterized by cardinality and concurrency.
- **Memory scale validation** at target cardinality against a live `TieredStore`,
  publishing p50/p95/p99 retrieval latency and ingest throughput.
- **Workflow throughput validation** across a real multi-worker pool using the
  existing partitioning (G6), under sustained load.
- An updated, honest **scaling envelope** extending
  [distributed-execution §3.3](../../03-workflow-engine/distributed-execution.md#33-scaling-envelope-g6).

## 4.2 Non-functional
- Measurements are reproducible (fixed dataset generators, recorded topology).
- Results are published with methodology, not just headline numbers.

---

# 5. Exit Criteria

> Documented, **reproduced** NFR numbers against live Postgres/Qdrant — **or** an
> honest, published statement of where the current architecture tops out and why
> (a real ceiling is an acceptable, GA-worthy outcome; an unmeasured claim is
> not).

This feeds the v1.0 exit criterion "meets published SLOs in production"
([v1.0 §5](../v1.0.md#5-exit-criteria)).

---

# 6. Dependencies & Environment Caveats

- **Requires real cloud capacity** (managed Postgres + Qdrant at scale) — absent
  in the current dev environment, so this cannot be validated in-house today. This
  is the primary blocker, and the reason the work is *planned* not *in progress*.
- Should follow, or run alongside, an ADR recording the target topology and
  measurement method (per the graduation flow).

---

# 7. Risks

| Risk | Mitigation |
|------|-----------|
| Numbers unverifiable in-house | Flag the environment dependency explicitly; never claim what wasn't run |
| A real ceiling below target | Treat an honest, published ceiling as a valid outcome, not a failure to hide |
| Benchmark drift | Reproducible generators + recorded topology |

---

# 8. Related Documents

- [`01-product/prd-future.md`](../../01-product/prd-future.md) §5.1 — requirements
- [`18-roadmap/v1.0.md`](../v1.0.md) — GA milestone (Scale row, §5 exit criteria)
- [`15-testing/performance-tests.md`](../../15-testing/performance-tests.md)
- [`03-workflow-engine/distributed-execution.md`](../../03-workflow-engine/distributed-execution.md#33-scaling-envelope-g6)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for scale & performance validation |
