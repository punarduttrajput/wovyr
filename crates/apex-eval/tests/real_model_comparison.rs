#![cfg(feature = "mistralrs")]
//! Points [FUT-001](../../../docs/18-roadmap/future/B1-multi-agent-systems.md)'s
//! comparison harness (`crates/apex-eval/src/compare.rs`) at a **real** model —
//! `MistralRsProvider` (Qwen2.5-0.5B-Instruct via mistral.rs) — instead of the
//! scripted deterministic `BalancedViewProvider` `multi_agent_vs_single_agent.rs`
//! uses. Off by default (`--features mistralrs`), downloads a real ~400MB GGUF
//! file on first run (needs network) and runs 4 real CPU inferences — run
//! `--release` or it will be slow.
//!
//! **Deliberately does not assert `workflow_wins()`.** `MistralRsProvider` sets
//! no sampling parameters — it is genuinely non-deterministic (its own module
//! doc: "the first genuinely non-deterministic provider in this workspace's own
//! tests"), and Qwen2.5-0.5B's synthesis quality is already documented
//! elsewhere as mediocre. Hard-asserting a specific outcome against real,
//! uncontrollable model output would be exactly the "flaky eval eroding trust
//! in the gate" risk [FUT-006](../../../docs/18-roadmap/future/B6-trust-evaluation.md)'s
//! own §6 calls out as the central risk to avoid. This test asserts only
//! structural/plumbing properties (both paths complete, produce non-empty
//! answers, consume real token usage) and prints the full `ComparisonReport` so
//! the actual result is observable — the real observed outcome is recorded by
//! hand in `B6-trust-evaluation.md` §8.2, not gated on here.

use apex_agent::AgentDefinition;
use apex_eval::{ComparisonSuite, run_comparison};
use apex_provider::{Gateway, MistralRsProvider};
use apex_tools::ToolRegistry;
use apex_workflow::Definition;
use std::collections::BTreeMap;
use std::sync::Arc;

fn suite() -> ComparisonSuite {
    ComparisonSuite::from_yaml(
        "
name: balanced-view-real-model
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

#[tokio::test]
async fn real_model_comparison_runs_end_to_end() {
    println!("Loading Qwen2.5-0.5B-Instruct via mistral.rs (first run downloads the GGUF file)...");
    let provider = MistralRsProvider::from_env()
        .await
        .expect("failed to load the real mistralrs model — check network access");
    let gateway = Arc::new(Gateway::new(Box::new(provider)));
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
    .expect("comparison against the real model should complete without error");

    println!("\n=== real-model ComparisonReport ===\n{report:#?}\n");

    // Structural/plumbing checks only — see this file's module doc for why the
    // outcome itself (workflow_wins()) isn't asserted.
    assert_eq!(report.single_agent.total, 1);
    assert_eq!(report.workflow.total, 1);
    assert!(
        report.single_agent.usage.total_tokens > 0,
        "expected real inference to consume real tokens, got: {:#?}",
        report.single_agent
    );
    for case in &report.single_agent.cases {
        assert!(
            !case.detail.is_empty(),
            "expected a scored detail message for case `{}`",
            case.id
        );
    }
}
