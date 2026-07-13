//! Compare a single agent against a workflow on the same fixtures â€” the
//! evidence [FUT-001](../../../docs/18-roadmap/future/B1-multi-agent-systems.md)'s
//! graduation gate asks for ("measurably outperforms a single agent on a real
//! benchmark").
//!
//! **Honesty note, carried over from this crate's own limitations
//! ([lib.rs](crate) / [FUT-006 Â§8](../../../docs/18-roadmap/future/B6-trust-evaluation.md#8-prototype-spike-2026-07-05)):**
//! `MockProvider` cannot drive this (it always echoes a fixed template, not a
//! per-fixture answer), and there is no real, non-deterministic provider run
//! wired up yet. So [`run_comparison`] necessarily runs both paths against a
//! purpose-built deterministic "scripted" provider (mirroring
//! `tests/regression_detection.rs`'s `CorrectProvider`/`RegressedProvider`).
//! That makes this module's own tests an **illustrative, reproducible
//! demonstration that the comparison mechanism works correctly â€” not yet the
//! "real benchmark" evidence the graduation gate needs.** A real benchmark
//! needs a real, non-deterministic model in the loop, which remains FUT-006's
//! open gap (`apex-provider`'s `mistralrs` feature exists but isn't pointed at
//! this harness).
//!
//! The workflow side is driven by the shared
//! [`apex_runtime::PlatformActivityExecutor`] (RM-GA-P4 HLTH-901 â€” the same
//! dispatch body the CLI's local runner and the server use), parameterized here
//! by [`MapAgentResolver`], which resolves an activity's `name` against an
//! in-memory map of [`AgentDefinition`]s, since this harness has neither a
//! local file convention nor a server-side agent store.

use crate::fixture::{EvalSuite, Expectation, Fixture};
use crate::report::{CaseResult, EvalReport};
use crate::runner::run_suite;
use crate::score::{CaseOutcome, score};
use apex_agent::AgentDefinition;
use apex_common::{Error, Result, Usage};
use apex_provider::Gateway;
use apex_runtime::{AgentResolver, PlatformActivityExecutor};
use apex_tools::ToolRegistry;
use apex_workflow::{ActivityContext, Definition, Engine, InMemoryStore, RunOutcome};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One comparison case: an input run both ways, scored by the same [`Expectation`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonCase {
    /// Stable case id, unique within its suite.
    pub id: String,
    /// The input text â€” becomes the single agent's user turn and the
    /// workflow's `input.topic` (matching `research-team.yaml`'s convention).
    pub input: String,
    /// The check both paths' final answers are scored against.
    pub expect: Expectation,
}

/// A named set of [`ComparisonCase`]s, loadable from YAML. Mirrors
/// [`EvalSuite::from_yaml`]'s validate-on-load shape exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonSuite {
    pub name: String,
    /// The workflow activity id whose output is the "final answer" to score
    /// (e.g. `synthesize` for `research-team.yaml`) â€” explicit rather than
    /// inferred from the DAG, since this harness only needs to support
    /// whatever shape the caller points it at.
    pub final_activity: String,
    pub cases: Vec<ComparisonCase>,
}

impl ComparisonSuite {
    /// Parse a suite from YAML, failing closed on anything malformed â€” same
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
    /// agent â€” the "measurably outperforms" claim, reduced to a pass-rate
    /// comparison over the same fixtures.
    pub fn workflow_wins(&self) -> bool {
        self.workflow.pass_rate > self.single_agent.pass_rate
    }
}

/// Resolves `agent`-typed activities against an in-memory map instead of a file
/// path (the CLI) or a stored-agent id (the server) â€” eval has neither. No
/// tenant, unhosted, no admission gate (the [`AgentResolver`] trait's default
/// methods already model exactly this, so this impl only needs `resolve`).
struct MapAgentResolver {
    agents: BTreeMap<String, AgentDefinition>,
}

#[async_trait]
impl AgentResolver for MapAgentResolver {
    async fn resolve(
        &self,
        ctx: &ActivityContext,
        agent_id: &str,
    ) -> std::result::Result<AgentDefinition, String> {
        self.agents.get(agent_id).cloned().ok_or_else(|| {
            format!(
                "activity `{}`: no agent `{agent_id}` in the comparison's agent map",
                ctx.id
            )
        })
    }
}

/// Run every case in `suite` both as `single_agent_def` (via [`run_suite`], no
/// duplicated logic) and as `workflow_def` (a fresh in-memory
/// [`Engine`]/[`PlatformActivityExecutor`] per case, so cases can't leak state
/// into each other), scoring both paths' final answers with the same
/// [`Expectation`]. Known gap: the workflow side's [`EvalReport::usage`] is
/// always zero â€” a workflow activity's output is a bare `{message, steps}`
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
        let executor = Arc::new(PlatformActivityExecutor::new(
            registry.clone(),
            gateway.clone(),
            Arc::new(MapAgentResolver {
                agents: workflow_agents.clone(),
            }),
        ));
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
