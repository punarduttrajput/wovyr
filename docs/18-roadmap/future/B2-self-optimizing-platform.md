<!--
File: docs/18-roadmap/future/B2-self-optimizing-platform.md
Document ID: FUT-002
-->

# Future Exploration: Self-Optimizing Platform

**Document ID:** FUT-002
**File Path:** `docs/18-roadmap/future/B2-self-optimizing-platform.md`
**Version:** 1.0.0
**Status:** Exploratory — research bet, not committed
**Owner:** Platform / Gateway Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Flesh out the "Self-Optimizing Platform" research bet
([future.md §2.2](../future.md#22-self-optimizing-platform),
[PRD-002 §6.2](../../01-product/prd-future.md#62-self-optimizing-platform)):
replacing hand-tuned constants with decisions driven by the platform's own
live telemetry, without ever compromising the deterministic core.

Exploratory — graduates only via an [ADR](../../17-adr/index.md).

---

# 2. Problem & Opportunity

Several load-bearing decisions are currently fixed constants set by a human:

- **Gateway routing** — the ordered provider/model candidate list is static; it
  does not react to live latency, error rate, or cost
  ([routing](../../05-llm-gateway/routing.md)).
- **Memory ranking** — the weighted ranker (relevance + recency + importance)
  uses fixed weights ([ranking](../../06-memory-engine/ranking.md)); they are not
  tuned to what actually gets used.
- **Warm-pool sizing** — the `SandboxPool` `AutoscalePolicy` (`min_warm` /
  `max_warm`) is operator-set, not demand-learned.

The platform already *emits* the signals that could tune these (cost events,
metrics, retrieval outcomes). The opportunity is to close the loop: observe,
score, and adjust — cost/quality-aware routing, self-tuning ranking, adaptive
warm pools, and AI-assisted plugin scaffolding/review.

---

# 3. Current Baseline (what this would build on)

- **Cost/quality signals** — the `Gateway`'s `CostObserver` and the
  `apex-telemetry` `Metrics` registry (RED metrics, `apex_llm_*`) already produce
  the raw telemetry.
- **Resilience knobs** — the `Gateway` already has failover, a circuit breaker,
  caching, and hedging; live scoring would feed the *candidate ordering* those
  mechanisms consume.
- **Ranker + MMR** — `apex-memory` already exposes a `score_breakdown`, so the
  effect of any weight change is observable and explainable.
- **Autoscaler** — `SandboxPool::autoscale()` is already caller-driven and
  deterministic; a demand signal would drive it instead of a static policy.

---

# 4. Direction (design sketch, non-committal)

The unifying principle: **learning happens at the boundary; the deterministic
core is untouched.** A scoring/decision layer sits *outside* the schedulable
paths and hands them a resolved decision (a candidate ordering, a weight vector,
a target pool size). The core still runs deterministically given that input.

- **Routing:** a live model-scorer that reorders candidates by a rolling
  cost/latency/quality score, consumed by the existing gateway candidate list.
- **Ranking:** offline/shadow weight tuning against observed retrieval usefulness,
  promoted only after evaluation.
- **Warm pools:** an autoscaler driven by observed acquire pressure.
- **Plugin scaffolding/review:** AI-assisted generation checked by the existing
  static scanner (`apex-marketplace` `scan.rs`) — assistance, not autonomy.

---

# 5. Requirements

## 5.1 Functional
- Each optimized decision is produced by a named, versioned policy.
- Every automated decision is **observable** (logged/metered) and
  **overridable** (an operator can pin a static value).
- A policy can run in **shadow mode** (compute-but-don't-apply) for evaluation
  before it goes live.

## 5.2 Invariants to preserve
- **Determinism in core logic** ([coding-standards §7](../../19-implementation-guide/coding-standards.md)):
  no ambient clock/randomness enters a schedulable path. The scorer reads
  telemetry at the boundary and passes a fixed decision in.
- **No silent regression:** a policy may not go live without passing an
  evaluation gate.

---

# 6. Key Risks & Open Questions

- **Proxy-metric gaming** — optimizing a measurable proxy (e.g. latency) into a
  quality regression is the central risk.
- **Feedback instability** — a policy reacting to its own effects can oscillate.
- **Attribution** — did the policy help, or did traffic change? Requires
  shadow/A-B rigor.
- **Explainability** — an operator must be able to see *why* a decision was made.

---

# 7. Graduation Gate

Becomes an ADR + roadmap slot only when a policy can show:

> A **shadow-mode deployment** demonstrating measured improvement on its target
> metric with **zero quality regression**, plus a working operator override.

---

# 8. Dependencies

- [FUT-006 Trust & Evaluation](B6-trust-evaluation.md) — the evaluation harness
  is a hard prerequisite; without it, "measured improvement / no regression"
  cannot be substantiated.

---

# 9. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.2 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.2
- [`05-llm-gateway/routing.md`](../../05-llm-gateway/routing.md)
- [`06-memory-engine/ranking.md`](../../06-memory-engine/ranking.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial exploration doc for the self-optimizing-platform research bet |
