//! RM-AIM-P2 EVL-201 acceptance, end to end through the real
//! [`apex_agent::run_agent`] loop: an LLM judge (scripted for determinism)
//! grades a semantically-correct-but-non-substring answer as **passing**
//! where the exact `contains` matcher fails — the whole point of
//! rubric-graded scoring. The judge runs on its **own** gateway, separate
//! from the gateway the agent under evaluation answers through (judging with
//! the model that produced the answers is a known bias).

use apex_agent::AgentDefinition;
use apex_common::{Result, Usage};
use apex_eval::{EvalSuite, LlmJudge, Scorer, run_suite, run_suite_scored};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role};
use apex_tools::ToolRegistry;
use async_trait::async_trait;
use std::sync::Arc;

/// The agent-side provider: answers the refund question correctly but with
/// wording that shares no substring with the expectation ("one month", never
/// "30 days"). Deterministic, no clock/rng.
struct ParaphrasingProvider;

#[async_trait]
impl AIProvider for ParaphrasingProvider {
    fn name(&self) -> &str {
        "paraphrasing"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let reply = if user.contains("refund") {
            "Customers may return purchases for a full month after buying."
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

/// The judge-side provider: recognizes the paraphrase as correct. Scripted so
/// the whole test is deterministic — the FUT-006 ADR owns live-judge variance.
struct ScriptedJudgeProvider;

#[async_trait]
impl AIProvider for ScriptedJudgeProvider {
    fn name(&self) -> &str {
        "scripted-judge"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        // Grade high only when the answer under review conveys the one-month
        // meaning; anything else scores low. Keyed on the *answer* text the
        // judge prompt embeds, so a wrong answer would genuinely fail.
        let reply = if user.contains("full month") {
            r#"{"score": 0.95, "reasoning": "a full month states the 30-day window correctly"}"#
        } else {
            r#"{"score": 0.1, "reasoning": "the answer does not state the refund window"}"#
        };
        Ok(ChatResponse {
            message: Message::assistant(reply.to_string()),
            model: request.model,
            usage: Usage::new(8, 4, 0.0),
            finish_reason: "stop".to_string(),
        })
    }
}

fn agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: support-bot\nspec:\n  instructions: Answer support questions.\n",
    )
    .unwrap()
}

fn contains_suite() -> EvalSuite {
    EvalSuite::from_yaml(
        "
name: refund-exact
cases:
  - id: refund-window
    input: How long is the refund window?
    expect:
      contains: 30 days
",
    )
    .unwrap()
}

fn judged_suite() -> EvalSuite {
    EvalSuite::from_yaml(
        "
name: refund-judged
cases:
  - id: refund-window
    input: How long is the refund window?
    expect:
      judge:
        rubric: States that the refund window is 30 days (one month).
",
    )
    .unwrap()
}

fn judge_scorer() -> Scorer {
    let judge_gateway = Arc::new(Gateway::new(Box::new(ScriptedJudgeProvider)));
    Scorer::exact_only().with_judge(Arc::new(LlmJudge::new(judge_gateway)))
}

/// EVL-201 acceptance: the identical agent answer fails `contains` but passes
/// the rubric-graded judge.
#[tokio::test]
async fn judge_passes_a_paraphrased_answer_that_contains_would_fail() {
    let def = agent();
    let gateway = Gateway::new(Box::new(ParaphrasingProvider));
    let registry = ToolRegistry::with_builtins();

    // Exact matching: the correct-but-paraphrased answer fails.
    let exact = run_suite(&contains_suite(), &def, &gateway, &registry)
        .await
        .unwrap();
    println!("{exact:#?}");
    assert_eq!(
        exact.pass_rate, 0.0,
        "contains must fail the paraphrase: {exact:#?}"
    );

    // Judge scoring: the same answer passes on substance.
    let judged = run_suite_scored(&judged_suite(), &def, &gateway, &registry, &judge_scorer())
        .await
        .unwrap();
    println!("{judged:#?}");
    assert_eq!(
        judged.pass_rate, 1.0,
        "the judge must pass the paraphrase: {judged:#?}"
    );
    assert!(judged.cases[0].detail.contains("judge scored 0.95"));
}

/// A judged case run through plain `run_suite` (no scorer) fails with a clear
/// detail instead of silently spending judge tokens or silently passing.
#[tokio::test]
async fn plain_run_suite_fails_judged_cases_closed() {
    let def = agent();
    let gateway = Gateway::new(Box::new(ParaphrasingProvider));
    let registry = ToolRegistry::with_builtins();

    let report = run_suite(&judged_suite(), &def, &gateway, &registry)
        .await
        .unwrap();
    assert_eq!(report.pass_rate, 0.0);
    assert!(
        report.cases[0].detail.contains("none configured"),
        "the failure must say why: {}",
        report.cases[0].detail
    );
}

/// The scripted judge + deterministic agent make judged runs byte-for-byte
/// reproducible — the harness's core claim extends to model-backed scoring
/// when the judge itself is deterministic.
#[tokio::test]
async fn judged_scoring_is_reproducible_against_a_deterministic_judge() {
    let def = agent();
    let registry = ToolRegistry::with_builtins();

    let gateway_a = Gateway::new(Box::new(ParaphrasingProvider));
    let report_a = run_suite_scored(
        &judged_suite(),
        &def,
        &gateway_a,
        &registry,
        &judge_scorer(),
    )
    .await
    .unwrap();
    let gateway_b = Gateway::new(Box::new(ParaphrasingProvider));
    let report_b = run_suite_scored(
        &judged_suite(),
        &def,
        &gateway_b,
        &registry,
        &judge_scorer(),
    )
    .await
    .unwrap();
    assert_eq!(report_a, report_b);
}
