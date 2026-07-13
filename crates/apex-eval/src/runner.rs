//! [`run_suite`]/[`run_suite_scored`] drive the real [`apex_agent::run_agent`]
//! loop for every case in a suite and score the answer. This crate adds no new
//! agent-execution path — it only adds a scoring layer on top of the one that
//! already exists.

use crate::fixture::EvalSuite;
use crate::judge::Scorer;
use crate::report::{CaseResult, EvalReport};
use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_common::Result;
use apex_provider::Gateway;
use apex_tools::ToolRegistry;
use serde_json::json;

/// Run every case in `suite` against `def` over `gateway`/`registry` with
/// **exact-match scoring only** — the original, fully deterministic path.
/// A `judge:`/`similar_to:` case fails with a clear detail here (a judge
/// call costs real tokens and must be an explicit choice); score those suites
/// via [`run_suite_scored`] with a configured [`Scorer`].
pub async fn run_suite(
    suite: &EvalSuite,
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
) -> Result<EvalReport> {
    run_suite_scored(suite, def, gateway, registry, &Scorer::exact_only()).await
}

/// Like [`run_suite`], but scoring through `scorer`, which may grade
/// `judge:`/`similar_to:` expectations with a model (RM-AIM-P2 EVL-201).
///
/// Cases run sequentially and each uses a fresh [`RunOptions`] — no state
/// carries between cases, and nothing *here* reads a clock or randomness, so
/// a re-run against the same deterministic provider **and** deterministic
/// scorer produces a byte-identical [`EvalReport`]; a live LLM judge is only
/// as reproducible as its model (see [`crate::judge`]).
pub async fn run_suite_scored(
    suite: &EvalSuite,
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
    scorer: &Scorer,
) -> Result<EvalReport> {
    let mut cases = Vec::with_capacity(suite.cases.len());
    for fixture in &suite.cases {
        let opts = RunOptions::new(json!({ "message": fixture.input }));
        let mut sink = NullSink;
        let output = run_agent(def, gateway, registry, opts, &mut sink).await?;
        let outcome = scorer
            .score(&fixture.input, &output.text, &fixture.expect)
            .await;
        cases.push(CaseResult {
            id: fixture.id.clone(),
            passed: outcome.passed,
            detail: outcome.detail,
            usage: output.usage,
        });
    }
    Ok(EvalReport::from_cases(suite.name.clone(), cases))
}
