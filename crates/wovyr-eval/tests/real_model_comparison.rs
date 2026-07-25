#![cfg(feature = "mistralrs")]
//! Points [FUT-001](../../../docs/18-roadmap/future/B1-multi-agent-systems.md)'s
//! comparison harness (`crates/wovyr-eval/src/compare.rs`) at a **real** model —
//! `MistralRsProvider` (Qwen2.5-0.5B-Instruct via mistral.rs) — instead of the
//! scripted deterministic `BalancedViewProvider` `multi_agent_vs_single_agent.rs`
//! uses. Off by default (`--features mistralrs`), downloads a real ~400MB GGUF
//! file on first run (needs network) and runs 4 real CPU inferences — run
//! `--release` or it will be slow.
//!
//! **Deliberately does not assert `workflow_wins()`.** `MistralRsProvider` sets
//! no sampling parameters, so this was assumed to be non-deterministic going
//! in — hard-asserting a specific outcome against real, assumed-uncontrollable
//! model output would be exactly the "flaky eval eroding trust in the gate"
//! risk [FUT-006](../../../docs/18-roadmap/future/B6-trust-evaluation.md)'s
//! own §6 calls out as the central risk to avoid. This test asserts only
//! structural/plumbing properties (both paths complete, produce non-empty
//! answers, consume real token usage) and prints the full `ComparisonReport` so
//! the actual result is observable — the real observed outcome is recorded by
//! hand in `B6-trust-evaluation.md` §8.2, not gated on here.
//!
//! **That assumption turned out to be wrong for this model/config**:
//! `real_model_comparison_variance_over_n_runs` below repeated the identical
//! comparison 4 times total and got byte-identical results every time (see
//! `mistralrs_provider.rs`'s doc comment and `B6-trust-evaluation.md` §8.2).
//! The "don't hard-assert" design is kept anyway — it's still the right
//! default for output from a real model, and nothing here guarantees this
//! determinism holds for a different prompt, model, or sampler config.

use std::collections::BTreeMap;
use std::sync::Arc;
use wovyr_agent::AgentDefinition;
use wovyr_eval::{ComparisonSuite, run_comparison};
use wovyr_provider::{Gateway, MistralRsProvider};
use wovyr_tools::ToolRegistry;
use wovyr_workflow::Definition;

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

/// Runs the same real-model comparison `RUNS` times (loading the model once,
/// reusing it across iterations — the load itself is a small fixed cost next
/// to the per-run inference) to gather more than the single data point
/// `real_model_comparison_runs_end_to_end` gives. Still not the "quantified,
/// stable variance" [FUT-006 §7](../../../docs/18-roadmap/future/B6-trust-evaluation.md#7-graduation-gate)'s
/// gate needs — `RUNS` is small (real CPU inference is slow: ~80s/call, 4
/// calls/run) — but enough to see whether the single tie from the first run
/// was representative or noise. No outcome is asserted here either, for the
/// same reason as the test above; the tally is printed, not gated on.
const RUNS: usize = 3;

#[tokio::test]
async fn real_model_comparison_variance_over_n_runs() {
    println!("Loading Qwen2.5-0.5B-Instruct via mistral.rs for {RUNS} repeated comparison runs...");
    let provider = MistralRsProvider::from_env()
        .await
        .expect("failed to load the real mistralrs model — check network access");
    let gateway = Arc::new(Gateway::new(Box::new(provider)));
    let registry = ToolRegistry::with_builtins();

    let mut workflow_wins = 0;
    let mut ties = 0;
    let mut single_agent_wins = 0;

    for run in 1..=RUNS {
        let report = run_comparison(
            &suite(),
            &single_agent(),
            &research_team_workflow(),
            &workflow_agents(),
            gateway.clone(),
            &registry,
        )
        .await
        .expect("comparison against the real model should complete without error");

        println!("\n=== run {run}/{RUNS} ===\n{report:#?}\n");

        match report
            .workflow
            .pass_rate
            .partial_cmp(&report.single_agent.pass_rate)
            .expect("pass rates are always finite")
        {
            std::cmp::Ordering::Greater => workflow_wins += 1,
            std::cmp::Ordering::Equal => ties += 1,
            std::cmp::Ordering::Less => single_agent_wins += 1,
        }
    }

    println!(
        "\n=== tally over {RUNS} runs: workflow_wins={workflow_wins} ties={ties} single_agent_wins={single_agent_wins} ==="
    );
}
