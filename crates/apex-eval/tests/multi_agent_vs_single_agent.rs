//! Evidence for [FUT-001](../../../docs/18-roadmap/future/B1-multi-agent-systems.md)'s
//! graduation gate: run the same task as a single agent and as the real
//! `examples/workflows/research-team.yaml` workflow, score both the same way,
//! and compare.
//!
//! **Honesty note** (see `src/compare.rs`'s doc comment): both paths run
//! against a purpose-built deterministic `BalancedViewProvider` below, not a
//! real model — `MockProvider` can't drive per-fixture scoring (it always
//! echoes a fixed template), and no real, non-deterministic provider is wired
//! into this harness yet. So this test is an **illustrative, reproducible
//! demonstration that the comparison mechanism works**, not the "real
//! benchmark" evidence FUT-001's graduation gate ultimately needs.
//!
//! The scripted provider models one concrete, explainable reason decomposition
//! can help: a single generalist agent given a plain topic has nothing telling
//! it to consider both sides, so it gives a one-sided answer; the workflow's
//! `proResearch`/`conResearch` activities are explicitly framed ("the case
//! FOR"/"the case AGAINST"), so both sides are gathered before `synthesize`
//! folds them together — which it does here by construction, since its prompt
//! embeds both prior findings verbatim (the same "read what's actually in the
//! message" trick `MockProvider` and `regression_detection.rs`'s scripted
//! providers already use).

use apex_agent::AgentDefinition;
use apex_common::{Result, Usage};
use apex_eval::{ComparisonSuite, run_comparison};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role};
use apex_tools::ToolRegistry;
use apex_workflow::Definition;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;

fn suite() -> ComparisonSuite {
    ComparisonSuite::from_yaml(
        "
name: balanced-view
final_activity: synthesize
cases:
  - id: remote-work
    input: remote work
    expect:
      contains_all: [support, risk]
",
    )
    .unwrap()
}

fn research_team_workflow() -> Definition {
    Definition::from_file(&format!(
        "{}/../../examples/workflows/research-team.yaml",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("examples/workflows/research-team.yaml should parse")
}

fn workflow_agents() -> BTreeMap<String, AgentDefinition> {
    let mut agents = BTreeMap::new();
    for name in ["pro-researcher", "con-researcher", "synthesizer"] {
        agents.insert(
            name.to_string(),
            AgentDefinition::from_yaml(&format!(
                "metadata:\n  name: {name}\nspec:\n  instructions: Be terse.\n"
            ))
            .unwrap(),
        );
    }
    agents
}

fn single_agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: generalist\nspec:\n  instructions: Answer questions about the given topic.\n",
    )
    .unwrap()
}

fn user_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .and_then(|m| m.content.clone())
        .unwrap_or_default()
}

/// Deterministic, framing-aware provider: gives a one-sided answer to a plain
/// question, and a balanced one only when explicitly asked to combine two
/// prior (already-gathered) findings — the shape a real model's behavior would
/// plausibly take, encoded here without any real inference.
struct BalancedViewProvider;

#[async_trait]
impl AIProvider for BalancedViewProvider {
    fn name(&self) -> &str {
        "balanced-view"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = user_text(&request);
        let reply = if user.contains("the case FOR") {
            "There is real support for this.".to_string()
        } else if user.contains("the case AGAINST") {
            "There is real risk in this.".to_string()
        } else if user.contains("Combine") {
            // The synthesize activity's prompt already embeds both prior
            // findings verbatim; folding them into one summary is enough to
            // carry both required keywords through.
            format!("Balanced summary — {user}")
        } else {
            // No framing telling it to consider the other side: a single pass
            // defaults to one perspective, the same one a plain "FOR" framing
            // would give.
            "There is real support for this.".to_string()
        };
        Ok(ChatResponse {
            message: Message::assistant(reply),
            model: request.model,
            usage: Usage::new(5, 5, 0.0),
            finish_reason: "stop".to_string(),
        })
    }
}

#[tokio::test]
async fn workflow_covers_both_perspectives_the_single_agent_misses() {
    let gateway = Arc::new(Gateway::new(Box::new(BalancedViewProvider)));
    let registry = ToolRegistry::with_builtins();

    let report = run_comparison(
        &suite(),
        &single_agent(),
        &research_team_workflow(),
        &workflow_agents(),
        gateway,
        &registry,
    )
    .await
    .unwrap();

    assert_eq!(
        report.single_agent.pass_rate, 0.0,
        "a single pass with no pro/con framing should miss one required perspective: {:#?}",
        report.single_agent
    );
    assert_eq!(
        report.workflow.pass_rate, 1.0,
        "the fan-out/join workflow should cover both perspectives: {:#?}",
        report.workflow
    );
    assert!(
        report.workflow_wins(),
        "expected the workflow to outperform the single agent: {report:#?}"
    );
}

#[tokio::test]
async fn comparison_is_reproducible() {
    let registry = ToolRegistry::with_builtins();
    let suite = suite();
    let workflow = research_team_workflow();
    let agents = workflow_agents();

    let gateway_a = Arc::new(Gateway::new(Box::new(BalancedViewProvider)));
    let report_a = run_comparison(
        &suite,
        &single_agent(),
        &workflow,
        &agents,
        gateway_a,
        &registry,
    )
    .await
    .unwrap();

    let gateway_b = Arc::new(Gateway::new(Box::new(BalancedViewProvider)));
    let report_b = run_comparison(
        &suite,
        &single_agent(),
        &workflow,
        &agents,
        gateway_b,
        &registry,
    )
    .await
    .unwrap();

    assert_eq!(
        report_a, report_b,
        "identical suite + deterministic provider must produce an identical comparison report"
    );
}
