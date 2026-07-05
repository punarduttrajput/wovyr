//! # apex-eval — deterministic, fixture-based AI evaluation harness
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
//! It deliberately does **not** attempt: LLM-as-judge scoring, a CI regression
//! gate, measuring variance against a *non-deterministic* real provider (every
//! provider in this crate's own tests is deterministic, so variance is
//! trivially zero — evaluating a real model is the open problem the eventual
//! ADR needs to address), telemetry, or a CLI surface. [`score::score`] is a
//! pure function with no ambient clock/randomness
//! ([coding-standards §7](../../docs/19-implementation-guide/coding-standards.md)),
//! matching the same determinism discipline as the rest of the platform.
//!
//! [`fixture::EvalSuite::from_yaml`] mirrors
//! [`apex_agent::AgentDefinition::from_yaml`]'s validate-on-load shape.
//! [`runner::run_suite`] drives the real [`apex_agent::run_agent`] loop — this
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
mod report;
mod runner;
mod score;

pub use compare::{ComparisonCase, ComparisonReport, ComparisonSuite, run_comparison};
pub use fixture::{EvalSuite, Expectation, Fixture};
pub use report::{CaseResult, EvalReport};
pub use runner::run_suite;
pub use score::{CaseOutcome, score};
