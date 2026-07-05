//! The two claims this crate's [FUT-006 prototype
//! spike](../../../docs/18-roadmap/future/B6-trust-evaluation.md) exists to
//! prove, exercised against the real [`run_agent`] loop (not mocked):
//!
//! 1. **Reproducibility** — the same suite run twice against the same
//!    deterministic provider produces a byte-identical [`EvalReport`].
//! 2. **Regression detection** — a suite that passes fully against a correct
//!    provider fails on exactly the case a regressed provider gets wrong.
//!
//! Mirrors the `ScriptedProvider` pattern from
//! `apex-agent/tests/tool_loop.rs`: `MockProvider` alone can't be used here
//! since it always echoes a fixed template regardless of the fixture, so each
//! provider below is purpose-built to answer deterministically per fixture.

use apex_agent::AgentDefinition;
use apex_common::{Result, Usage};
use apex_eval::{EvalSuite, run_suite};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role};
use apex_tools::ToolRegistry;
use async_trait::async_trait;

fn suite() -> EvalSuite {
    EvalSuite::from_yaml(
        "
name: capital-facts
cases:
  - id: france
    input: What is the capital of France?
    expect:
      contains: Paris
  - id: japan
    input: What is the capital of Japan?
    expect:
      contains: Tokyo
",
    )
    .unwrap()
}

fn agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: geography-bot\nspec:\n  instructions: Answer geography questions.\n",
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

/// Answers every fixture correctly — deterministic, no clock/rng.
struct CorrectProvider;

#[async_trait]
impl AIProvider for CorrectProvider {
    fn name(&self) -> &str {
        "correct"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = user_text(&request);
        let reply = if user.contains("France") {
            "The capital of France is Paris."
        } else if user.contains("Japan") {
            "The capital of Japan is Tokyo."
        } else {
            "I don't know."
        };
        Ok(ChatResponse {
            message: Message::assistant(reply.to_string()),
            model: request.model,
            usage: Usage::new(5, 5, 0.0),
            finish_reason: "stop".to_string(),
        })
    }
}

/// Gets the Japan case wrong (answers "Kyoto" instead of "Tokyo") — a
/// deliberate, deterministic quality regression on exactly one fixture.
struct RegressedProvider;

#[async_trait]
impl AIProvider for RegressedProvider {
    fn name(&self) -> &str {
        "regressed"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = user_text(&request);
        let reply = if user.contains("France") {
            "The capital of France is Paris."
        } else if user.contains("Japan") {
            "The capital of Japan is Kyoto." // wrong on purpose
        } else {
            "I don't know."
        };
        Ok(ChatResponse {
            message: Message::assistant(reply.to_string()),
            model: request.model,
            usage: Usage::new(5, 5, 0.0),
            finish_reason: "stop".to_string(),
        })
    }
}

#[tokio::test]
async fn a_correct_provider_passes_every_case() {
    let def = agent();
    let gateway = Gateway::new(Box::new(CorrectProvider));
    let registry = ToolRegistry::with_builtins();

    let report = run_suite(&suite(), &def, &gateway, &registry)
        .await
        .unwrap();

    assert_eq!(
        report.pass_rate, 1.0,
        "expected all cases to pass: {report:#?}"
    );
    assert!(report.failing_case_ids().is_empty());
}

#[tokio::test]
async fn a_regressed_provider_fails_exactly_the_regressed_case() {
    let def = agent();
    let gateway = Gateway::new(Box::new(RegressedProvider));
    let registry = ToolRegistry::with_builtins();

    let report = run_suite(&suite(), &def, &gateway, &registry)
        .await
        .unwrap();

    assert!(
        report.pass_rate < 1.0,
        "expected a regression to be caught: {report:#?}"
    );
    assert_eq!(report.failing_case_ids(), vec!["japan"]);
    // The unaffected case still passes — the harness localizes the regression,
    // it doesn't just fail the whole suite.
    assert!(
        report
            .cases
            .iter()
            .find(|c| c.id == "france")
            .unwrap()
            .passed
    );
}

#[tokio::test]
async fn the_same_suite_against_the_same_provider_is_byte_for_byte_reproducible() {
    let def = agent();
    let registry = ToolRegistry::with_builtins();
    let suite = suite();

    let gateway_a = Gateway::new(Box::new(CorrectProvider));
    let report_a = run_suite(&suite, &def, &gateway_a, &registry)
        .await
        .unwrap();

    let gateway_b = Gateway::new(Box::new(CorrectProvider));
    let report_b = run_suite(&suite, &def, &gateway_b, &registry)
        .await
        .unwrap();

    assert_eq!(
        report_a, report_b,
        "identical suite + deterministic provider must produce an identical report"
    );
}
