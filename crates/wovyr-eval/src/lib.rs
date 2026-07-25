//! # wovyr-eval — deterministic, fixture-based AI evaluation harness
//!
//! **This is a prototype spike for
//! [FUT-006 Trust & Evaluation](../../docs/18-roadmap/future/B6-trust-evaluation.md)
//! — not a committed platform surface.** It exists to gather the evidence
//! FUT-006's own graduation gate asks for before an ADR is written, per the
//! documented graduation flow (idea → PRD → ADR → release → build): a working
//! spike informs the ADR here, rather than the ADR speculating about a harness
//! that doesn't exist yet.
//!
//! It proves exactly two things:
//! 1. **Reproducible, fixture-based scoring** — the same [`EvalSuite`] run
//!    against the same deterministic provider twice produces a byte-identical
//!    [`EvalReport`] ([`report`]).
//! 2. **The harness detects a real quality regression** — a suite that passes
//!    against a correct provider and fails against a regressed one (see
//!    `tests/regression_detection.rs`).
//!
//! **LLM-as-judge + semantic scoring exist now (RM-AIM-P2 EVL-201):** a
//! [`Scorer`] dispatches `judge:` expectations (rubric → graded score, via the
//! [`Judge`] trait / [`LlmJudge`]) and `similar_to:` expectations
//! (embedding-cosine, [`SemanticScorer`]) alongside the exact matchers —
//! opt-in via [`run_suite_scored`]; plain [`run_suite`] stays exact-only and
//! fails model-backed cases with a clear detail rather than silently spending
//! judge tokens.
//!
//! **The regression gate is quantified now (RM-AIM-P2 EVL-202):** a committed
//! golden [`Baseline`] (suite + `min_pass_rate` + per-case expected outcomes)
//! is [`check`]ed against a fresh report — pure, fail-closed: a dropped rate,
//! a regressed case, or a vanished case fails the gate; improvements and new
//! cases are notes prompting a baseline refresh. [`run_suite_repeated`] +
//! [`VarianceReport`] report repeat-N variance (distinct-report count makes
//! any nondeterminism visible instead of a flake). `tests/regression_gate.rs`
//! is the CI-runnable command, gating `suites/capital-facts.yaml` against
//! `baselines/capital-facts.json` and writing report/variance/gate JSON
//! artifacts when `WOVYR_EVAL_ARTIFACT_DIR` is set.
//!
//! **The RAG path and retrievers are evaluable too (RM-AIM-P2 EVL-203):**
//! [`run_suite_with_memory`] drives [`wovyr_agent::run_agent_with_memory`], so
//! a suite grades the retrieval-grounded agent a deployment actually runs
//! (the manifest's `spec.max_steps` is honored on every runner path per
//! AIC-103); the [`retrieval`] module grades the *retriever* itself —
//! recall@k / nDCG@k / MRR over labeled [`RetrievalSuite`] fixtures through
//! the [`RankedRetriever`] trait (driven against the real `wovyr-memory`
//! engine in `tests/rag_eval.rs`, as a dev-dependency so the library spine
//! stays memory-free).
//!
//! It deliberately does **not** attempt: measuring variance against a
//! *non-deterministic* real judge (the tests script the judge — evaluating
//! with a live one is the open problem the eventual ADR needs to address),
//! telemetry, or a CLI surface. [`score::score`] is a pure function with no
//! ambient clock/randomness
//! ([coding-standards §7](../../docs/19-implementation-guide/coding-standards.md)),
//! matching the same determinism discipline as the rest of the platform.
//!
//! [`fixture::EvalSuite::from_yaml`] mirrors
//! [`wovyr_agent::AgentDefinition::from_yaml`]'s validate-on-load shape.
//! [`runner::run_suite`] drives the real [`wovyr_agent::run_agent`] loop — this
//! crate adds no new agent-execution path, only a scoring layer on top of the
//! one that already exists.
//!
//! [`compare::run_comparison`] extends the harness to
//! [FUT-001](../../docs/18-roadmap/future/B1-multi-agent-systems.md)'s
//! evidence need: run the same fixtures as a single agent and as a workflow
//! (e.g. `examples/workflows/research-team.yaml`), score both the same way,
//! and compare. See `compare`'s own doc comment for the honesty caveat: this
//! runs against a scripted deterministic provider, so it proves the
//! *comparison mechanism* works, not yet the "real benchmark" FUT-001's
//! graduation gate needs.

mod compare;
mod fixture;
mod gate;
mod judge;
mod report;
mod retrieval;
mod runner;
mod score;

pub use compare::{ComparisonCase, ComparisonReport, ComparisonSuite, run_comparison};
pub use fixture::{EvalSuite, Expectation, Fixture, JudgeSpec, SimilarSpec};
pub use gate::{Baseline, GateResult, VarianceReport, check, run_suite_repeated};
pub use judge::{Judge, JudgeVerdict, LlmJudge, Scorer, SemanticScorer};
pub use report::{CaseResult, EvalReport};
pub use retrieval::{
    RankedRetriever, RetrievalCase, RetrievalCaseResult, RetrievalReport, RetrievalSuite,
    evaluate_retrieval, ndcg_at_k, recall_at_k, reciprocal_rank,
};
pub use runner::{run_suite, run_suite_scored, run_suite_with_memory};
pub use score::{CaseOutcome, score};
