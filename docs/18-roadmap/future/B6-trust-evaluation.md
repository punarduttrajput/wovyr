<!--
File: docs/18-roadmap/future/B6-trust-evaluation.md
Document ID: FUT-006
-->

# Future Exploration: Trust & Evaluation

**Document ID:** FUT-006
**File Path:** `docs/18-roadmap/future/B6-trust-evaluation.md`
**Version:** 1.0.0
**Status:** Exploratory — research bet, not committed
**Owner:** Quality / Security Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Flesh out the "Trust & Evaluation" research bet
([future.md §2.6](../future.md#26-trust--evaluation),
[PRD-002 §6.6](../../01-product/prd-future.md#66-trust--evaluation)): a built-in
AI evaluation service, continuous quality-regression gates, and maturing
provenance/attestation/policy-as-code.

Exploratory — graduates only via an [ADR](../../17-adr/index.md). It is called out
first among the bets because **two other bets depend on it**
([FUT-001](B1-multi-agent-systems.md), [FUT-002](B2-self-optimizing-platform.md))
cannot substantiate their graduation gates without an evaluation harness.

---

# 2. Problem & Opportunity

The platform can test *deterministic* behavior thoroughly (unit, chaos, perf,
security), but it has **no way to measure AI output quality** — so claims like
"this policy improved results" or "this agent group beat a single agent" cannot
be substantiated. There is no continuous quality-regression gate for
model/prompt/policy changes.

The opportunity:

- **AI evaluation service** — score outputs against fixtures/rubrics reproducibly.
- **Continuous quality-regression gates** — block a merge that regresses quality,
  the way the clippy gate blocks warnings today.
- **Provenance/attestation/policy-as-code maturity** — extend the existing
  supply-chain and Policy Engine surfaces.

---

# 3. Current Baseline (what this would build on)

- **Deterministic test culture** — assertion-style perf tests (p95 gates), chaos
  tests, and the security battery already gate CI; an eval gate is the same shape
  for a new signal.
- **Deterministic mock provider** — `MockProvider` gives reproducible, offline
  runs — the substrate for fixture-based evaluation without live model calls.
- **Telemetry** — the `apex-telemetry` `Metrics` registry can carry quality
  metrics alongside RED/cost.
- **Policy Engine + supply chain** — the governance surface
  ([policy-engine](../../04-agent-framework/policy-engine.md)) and
  provenance/SBOM (`apex-plugin`) are what policy-as-code and attestation mature.

---

# 4. Direction (design sketch, non-committal)

- **Eval service:** a fixture/rubric-driven scorer (deterministic seeds, recorded
  inputs/expected signals) runnable offline against the mock provider and online
  against real ones. LLM-as-judge is an option, but the *harness* stays
  deterministic and reproducible.
- **Regression gate:** wire eval into CI as a quantified, stable-variance check
  that can block a merge — mirroring the existing perf p95 gates.
- **Policy-as-code:** grow the Policy Engine toward declarative, versioned,
  testable governance rules.

---

# 5. Requirements

## 5.1 Functional
- Evaluations are **reproducible**: fixed seeds, recorded fixtures, versioned
  rubrics.
- The regression gate produces a quantified score with a known, stable variance
  band and a clear pass/fail threshold.
- Quality metrics are observable through the existing telemetry surface.

## 5.2 Invariants to preserve
- **Determinism of the harness** — the evaluator itself must be reproducible even
  when scoring non-deterministic model output (fix seeds, record fixtures).
- **No flaky gate** — a gate that blocks merges must have quantified, stable
  variance, or it erodes trust in itself.

---

# 6. Key Risks & Open Questions

- **Flaky evals eroding trust in the gate** — the central risk; an unstable gate
  is worse than none.
- **Rubric validity** — does the score actually track user-perceived quality?
- **Judge bias/cost** — LLM-as-judge introduces its own non-determinism and cost.
- **Coverage** — which tasks/dimensions are evaluated, and who curates fixtures?

---

# 7. Graduation Gate

Becomes an ADR + roadmap slot only when it can show:

> A **regression suite with quantified, stable variance** on a real task set —
> stable enough to block a merge without false positives — plus reproducible,
> fixture-based scoring.

---

# 8. Enables

Unlike the other bets, this one is mostly upstream: it is a **prerequisite** for

- [FUT-001 Multi-Agent Systems](B1-multi-agent-systems.md) — to measure
  "outperforms a single agent."
- [FUT-002 Self-Optimizing Platform](B2-self-optimizing-platform.md) — to prove
  "measured improvement, no regression" in shadow mode.
- [FUT-003 Advanced Memory](B3-advanced-memory.md) — to measure retrieval-quality
  changes.

For that reason it is the natural **first** bet to graduate.

---

# 9. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.6 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.6
- [`04-agent-framework/policy-engine.md`](../../04-agent-framework/policy-engine.md)
- [`15-testing/index.md`](../../15-testing/index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial exploration doc for the trust-&-evaluation research bet |
