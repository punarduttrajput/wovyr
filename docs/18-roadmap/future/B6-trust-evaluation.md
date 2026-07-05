<!--
File: docs/18-roadmap/future/B6-trust-evaluation.md
Document ID: FUT-006
-->

# Future Exploration: Trust & Evaluation

**Document ID:** FUT-006
**File Path:** `docs/18-roadmap/future/B6-trust-evaluation.md`
**Version:** 1.2.0
**Status:** Exploratory — research bet, not committed. A prototype spike now
exists in code (`crates/apex-eval`, §8) — it gathers evidence for the
graduation gate below, but does not itself satisfy it (no CI gate, no
real/non-deterministic-provider variance study). A real non-deterministic
provider now exists to eventually run it against (`apex-provider`'s
`mistralrs` feature), but the two are not yet wired together. Still pre-ADR.
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
- **Deterministic offline runs** — `MockProvider` gives reproducible, offline
  agent runs, but (learned while building the §8 spike) it always echoes a
  fixed template regardless of the fixture, so it cannot itself produce
  per-fixture "correct" vs. "wrong" answers for scoring — a purpose-built
  deterministic provider (mirroring `apex-agent/tests/tool_loop.rs`'s
  `ScriptedProvider`) is still needed per suite.
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

# 8. Prototype Spike (2026-07-05)

Per the user's explicit choice, this bet's implementation started with a
**code spike before the ADR** — the ADR should be informed by what the spike
teaches, not speculate ahead of it. `crates/apex-eval` (new crate, ~350 lines +
tests) is the result.

**What it is:** a small, deterministic, fixture-based evaluation harness that
drives the real `apex_agent::run_agent` loop (no new execution path) and scores
the final answer against a YAML-defined [`EvalSuite`]:
- `EvalSuite::from_yaml` — validate-on-load, mirroring
  `AgentDefinition::from_yaml`'s shape (fails closed on an empty suite, an
  empty case id/input, or an `expect` with zero or more than one check set).
- `Expectation` — a validated **one-of struct** (`contains` /
  `contains_all` / `equals`), not a Rust enum. `serde_yaml` 0.9 (this
  workspace's pinned version, itself `+deprecated` upstream) cannot
  deserialize an externally-tagged enum from a YAML map — it demands a `!Tag`
  syntax. This is exactly why no other YAML-DSL struct in the codebase
  (`AgentDefinition`, the workflow `Definition`) uses an enum in its wire
  schema either; the spike followed the same idiom rather than fighting a
  known limitation in a deprecated dependency.
- `score` — a pure function (no clock/rng), the determinism discipline
  [§5.2](#52-invariants-to-preserve) requires.
- `run_suite` — runs every case, scores it, aggregates an `EvalReport`
  (pass rate + accumulated `Usage`).

**What it proves** (`crates/apex-eval/tests/regression_detection.rs`, run
against the real `run_agent` loop, not mocked):
1. **Reproducibility** — the identical suite run twice against the identical
   deterministic provider produces a **byte-identical** `EvalReport`
   (`assert_eq!` on the whole struct).
2. **Regression detection** — a suite passes fully (`pass_rate == 1.0`)
   against a provider that answers every fixture correctly, and fails on
   *exactly* the one case a deliberately-regressed provider gets wrong
   (`failing_case_ids() == ["japan"]`), while the unaffected case still
   passes — the harness localizes a regression, it doesn't just fail the
   whole suite.

**What it explicitly does not prove** (open problems for the ADR):
- **Variance is trivially zero here** only because every provider in these
  tests is deterministic. Evaluating a real, non-deterministic model — where
  "stable variance" in [§7](#7-graduation-gate)'s graduation gate actually
  means something — is untouched. **A real, local, non-deterministic provider
  now exists in the platform** (`apex-provider`'s optional `mistralrs` feature
  — a small real model via [mistral.rs](https://github.com/EricLBuehler/mistral.rs),
  verified end to end running the real `run_agent` tool-calling loop against a
  real HTTP fetch), but `apex-eval` has not been pointed at it — this narrows
  the gap (a real target now exists to run against) without closing it.
- No LLM-as-judge, no CI wiring, no telemetry (`apex-eval` emits no metrics
  yet), and no CLI surface. `MockProvider` cannot drive this harness at all
  (§3, corrected).
- The one-of-struct `Expectation` design is a direct, load-bearing consequence
  of `serde_yaml`'s limitation — a real (non-deprecated) YAML library might
  remove that constraint; the ADR should decide whether to keep the struct
  shape regardless (it's arguably more idiomatic YAML anyway) or revisit it.

---

# 9. Enables

Unlike the other bets, this one is mostly upstream: it is a **prerequisite** for

- [FUT-001 Multi-Agent Systems](B1-multi-agent-systems.md) — to measure
  "outperforms a single agent."
- [FUT-002 Self-Optimizing Platform](B2-self-optimizing-platform.md) — to prove
  "measured improvement, no regression" in shadow mode.
- [FUT-003 Advanced Memory](B3-advanced-memory.md) — to measure retrieval-quality
  changes.

For that reason it is the natural **first** bet to graduate.

---

# 10. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.6 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.6
- [`04-agent-framework/policy-engine.md`](../../04-agent-framework/policy-engine.md)
- [`15-testing/index.md`](../../15-testing/index.md)
- `crates/apex-eval` — the prototype spike (§8)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-05 | Noted a real, local, non-deterministic provider now exists (`apex-provider`'s `mistralrs` feature, verified end to end against a real HTTP fetch) — a real target `apex-eval` could eventually run against, closing part of §8's "not proven" gap without wiring the two together yet |
| 1.1.0 | 2026-07-05 | Added §8 Prototype Spike: `crates/apex-eval` built and tested, proving reproducible fixture-based scoring and deterministic regression detection against the real `run_agent` loop. Corrected §3's claim about `MockProvider` (it cannot drive per-fixture scoring). Still pre-ADR — the spike gathers evidence for §7's graduation gate, it doesn't satisfy it |
| 1.0.0 | 2026-07-05 | Initial exploration doc for the trust-&-evaluation research bet |
