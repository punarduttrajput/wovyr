//! [`run_suite`] drives the real [`apex_agent::run_agent`] loop for every case
//! in a suite and scores the answer. This crate adds no new agent-execution
//! path — it only adds a scoring layer on top of the one that already exists.

use crate::fixture::EvalSuite;
use crate::report::{CaseResult, EvalReport};
use crate::score::score;
use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_common::Result;
use apex_provider::Gateway;
use apex_tools::ToolRegistry;
use serde_json::json;

/// Run every case in `suite` against `def` over `gateway`/`registry`, scoring
/// each case's final answer. Cases run sequentially and each uses a fresh
/// [`RunOptions`] — no state carries between cases, and nothing here reads a
/// clock or randomness, so a re-run against the same (deterministic) provider
/// always produces a byte-identical [`EvalReport`].
pub async fn run_suite(
    suite: &EvalSuite,
    def: &AgentDefinition,
    gateway: &Gateway,
    registry: &ToolRegistry,
) -> Result<EvalReport> {
    let mut cases = Vec::with_capacity(suite.cases.len());
    for fixture in &suite.cases {
        let opts = RunOptions::new(json!({ "message": fixture.input }));
        let mut sink = NullSink;
        let output = run_agent(def, gateway, registry, opts, &mut sink).await?;
        let outcome = score(&output.text, &fixture.expect);
        cases.push(CaseResult {
            id: fixture.id.clone(),
            passed: outcome.passed,
            detail: outcome.detail,
            usage: output.usage,
        });
    }
    Ok(EvalReport::from_cases(suite.name.clone(), cases))
}
