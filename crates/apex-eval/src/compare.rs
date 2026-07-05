//! Compare a single agent against a workflow on the same fixtures — the
//! evidence [FUT-001](../../../docs/18-roadmap/future/B1-multi-agent-systems.md)'s
//! graduation gate asks for ("measurably outperforms a single agent on a real
//! benchmark").
//!
//! **Honesty note, carried over from this crate's own limitations
//! ([lib.rs](crate) / [FUT-006 §8](../../../docs/18-roadmap/future/B6-trust-evaluation.md#8-prototype-spike-2026-07-05)):**
//! `MockProvider` cannot drive this (it always echoes a fixed template, not a
//! per-fixture answer), and there is no real, non-deterministic provider run
//! wired up yet. So [`run_comparison`] necessarily runs both paths against a
//! purpose-built deterministic "scripted" provider (mirroring
//! `tests/regression_detection.rs`'s `CorrectProvider`/`RegressedProvider`).
//! That makes this module's own tests an **illustrative, reproducible
//! demonstration that the comparison mechanism works correctly — not yet the
//! "real benchmark" evidence the graduation gate needs.** A real benchmark
//! needs a real, non-deterministic model in the loop, which remains FUT-006's
//! open gap (`apex-provider`'s `mistralrs` feature exists but isn't pointed at
//! this harness).
//!
//! The workflow side is driven by a minimal, eval-local [`ActivityExecutor`]
//! (a third instance of the "resolve `${...}` via
//! [`apex_workflow::resolve_template`], dispatch `agent` activities through
//! [`run_agent`]" pattern already used by the CLI's `PlatformExecutor` and the
//! server's `ServerExecutor`) that resolves an activity's `name` against an
//! in-memory map of [`AgentDefinition`]s, since this harness has neither a
//! local file convention nor a server-side agent store.

use crate::fixture::{EvalSuite, Expectation, Fixture};
use crate::report::{CaseResult, EvalReport};
use crate::runner::run_suite;
use crate::score::{CaseOutcome, score};
use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_common::{Error, Result, Usage};
use apex_provider::Gateway;
use apex_tools::ToolRegistry;
use apex_workflow::{
    ActivityContext, ActivityError, ActivityExecutor, Definition, Engine, InMemoryStore, RunOutcome,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One comparison case: an input run both ways, scored by the same [`Expectation`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComparisonCase {
    /// Stable case id, unique within its suite.
    pub id: String,
    /// The input text — becomes the single agent's user turn and the
    /// workflow's `input.topic` (matching `research-team.yaml`'s convention).
    pub input: String,
    /// The check both paths' final answers are scored against.
    pub expect: Expectation,
}

/// A named set of [`ComparisonCase`]s, loadable from YAML. Mirrors
/// [`EvalSuite::from_yaml`]'s validate-on-load shape exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComparisonSuite {
    pub name: String,
    /// The workflow activity id whose output is the "final answer" to score
    /// (e.g. `synthesize` for `research-team.yaml`) — explicit rather than
    /// inferred from the DAG, since this harness only needs to support
    /// whatever shape the caller points it at.
    pub final_activity: String,
    pub cases: Vec<ComparisonCase>,
}

impl ComparisonSuite {
    /// Parse a suite from YAML, failing closed on anything malformed — same
    /// checks as [`EvalSuite::from_yaml`], plus a non-empty `final_activity`.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let suite: ComparisonSuite = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid comparison suite: {e}")))?;
        suite.validate()?;
        Ok(suite)
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::invalid("suite name must not be empty"));
        }
        if self.final_activity.trim().is_empty() {
            return Err(Error::invalid("suite must set a non-empty final_activity"));
        }
        if self.cases.is_empty() {
            return Err(Error::invalid("suite must have at least one case"));
        }
        for case in &self.cases {
            if case.id.trim().is_empty() {
                return Err(Error::invalid("every case must have a non-empty id"));
            }
            if case.input.trim().is_empty() {
                return Err(Error::invalid(format!(
                    "case `{}` must have a non-empty input",
                    case.id
                )));
            }
        }
        Ok(())
    }
}

/// The result of running a [`ComparisonSuite`] both ways: one [`EvalReport`]
/// per path, scored identically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonReport {
    pub suite: String,
    pub single_agent: EvalReport,
    pub workflow: EvalReport,
}

impl ComparisonReport {
    /// Whether the workflow path passed strictly more cases than the single
    /// agent — the "measurably outperforms" claim, reduced to a pass-rate
    /// comparison over the same fixtures.
    pub fn workflow_wins(&self) -> bool {
        self.workflow.pass_rate > self.single_agent.pass_rate
    }
}

/// A minimal [`ActivityExecutor`] for driving a workflow inside this crate's
/// comparison harness: only `agent` activities are supported (this harness has
/// no need for `tool`/`ai`/`human`), resolved against an in-memory map instead
/// of a file path (the CLI) or a stored-agent id (the server) — eval has
/// neither.
struct EvalWorkflowExecutor {
    agents: BTreeMap<String, AgentDefinition>,
    gateway: Arc<Gateway>,
    registry: ToolRegistry,
}

#[async_trait]
impl ActivityExecutor for EvalWorkflowExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> std::result::Result<Value, ActivityError> {
        let inputs = apex_workflow::resolve_template(&ctx.inputs, ctx);
        match ctx.activity_type.as_str() {
            "agent" => {
                let agent_id = ctx.name.as_deref().ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: `name` required for agent type",
                        ctx.id
                    ))
                })?;
                let def = self.agents.get(agent_id).ok_or_else(|| {
                    ActivityError::Permanent(format!(
                        "activity `{}`: no agent `{agent_id}` in the comparison's agent map",
                        ctx.id
                    ))
                })?;
                let input = if inputs.is_null() { json!({}) } else { inputs };
                let opts = RunOptions::new(input);
                let mut sink = NullSink;
                let output = run_agent(def, &self.gateway, &self.registry, opts, &mut sink)
                    .await
                    .map_err(|e| ActivityError::Retryable(e.to_string()))?;
                Ok(json!({ "message": output.text, "steps": output.steps }))
            }
            other => Err(ActivityError::Permanent(format!(
                "the comparison harness only supports `agent` activities, got `{other}`"
            ))),
        }
    }
}

/// Run every case in `suite` both as `single_agent_def` (via [`run_suite`], no
/// duplicated logic) and as `workflow_def` (a fresh in-memory
/// [`Engine`]/[`EvalWorkflowExecutor`] per case, so cases can't leak state into
/// each other), scoring both paths' final answers with the same
/// [`Expectation`]. Known gap: the workflow side's [`EvalReport::usage`] is
/// always zero — a workflow activity's output is a bare `{message, steps}`
/// JSON value, not a [`Usage`]-carrying struct, so per-case cost isn't
/// surfaced through [`apex_workflow::ExecutionState`] today.
pub async fn run_comparison(
    suite: &ComparisonSuite,
    single_agent_def: &AgentDefinition,
    workflow_def: &Definition,
    workflow_agents: &BTreeMap<String, AgentDefinition>,
    gateway: Arc<Gateway>,
    registry: &ToolRegistry,
) -> Result<ComparisonReport> {
    let single_suite = EvalSuite {
        name: suite.name.clone(),
        cases: suite
            .cases
            .iter()
            .map(|c| Fixture {
                id: c.id.clone(),
                input: c.input.clone(),
                expect: c.expect.clone(),
            })
            .collect(),
    };
    let single_agent = run_suite(&single_suite, single_agent_def, &gateway, registry).await?;

    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        let executor = Arc::new(EvalWorkflowExecutor {
            agents: workflow_agents.clone(),
            gateway: gateway.clone(),
            registry: registry.clone(),
        });
        let engine = Engine::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(InMemoryStore::new()),
            executor,
        );
        let exec_id = format!("cmp-{}", case.id);
        let (outcome, state) = engine
            .run(workflow_def, &exec_id, json!({ "topic": case.input }))
            .await?;

        let outcome_detail = match outcome {
            RunOutcome::Completed => {
                let answer = state
                    .activities
                    .get(&suite.final_activity)
                    .and_then(|record| record.output.as_ref())
                    .and_then(|output| output.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                score(answer, &case.expect)
            }
            other => CaseOutcome {
                passed: false,
                detail: format!("workflow did not complete: {other:?}"),
            },
        };
        cases.push(CaseResult {
            id: case.id.clone(),
            passed: outcome_detail.passed,
            detail: outcome_detail.detail,
            usage: Usage::default(),
        });
    }
    let workflow = EvalReport::from_cases(suite.name.clone(), cases);

    Ok(ComparisonReport {
        suite: suite.name.clone(),
        single_agent,
        workflow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_yaml() -> &'static str {
        "
name: comparison-suite
final_activity: synthesize
cases:
  - id: case-1
    input: remote work
    expect:
      contains_all: [support, risk]
"
    }

    #[test]
    fn parses_a_valid_suite() {
        let suite = ComparisonSuite::from_yaml(valid_yaml()).unwrap();
        assert_eq!(suite.name, "comparison-suite");
        assert_eq!(suite.final_activity, "synthesize");
        assert_eq!(suite.cases.len(), 1);
    }

    #[test]
    fn rejects_empty_final_activity() {
        let yaml = "name: s\nfinal_activity: \"\"\ncases:\n  - id: c1\n    input: hi\n    expect:\n      equals: hi\n";
        assert!(matches!(
            ComparisonSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }

    #[test]
    fn rejects_no_cases() {
        let yaml = "name: s\nfinal_activity: a\ncases: []\n";
        assert!(matches!(
            ComparisonSuite::from_yaml(yaml).unwrap_err(),
            Error::Invalid(_)
        ));
    }
}
