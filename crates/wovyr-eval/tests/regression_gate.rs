//! The CI-runnable regression gate (RM-AIM-P2 EVL-202): runs the **committed**
//! suite (`suites/capital-facts.yaml`) against a deterministic scripted
//! provider, gates the report against the **committed** golden baseline
//! (`baselines/capital-facts.json`), and reports repeat-N variance. CI runs
//! this file explicitly (see `.github/workflows/ci.yml`'s eval step) with
//! `WOVYR_EVAL_ARTIFACT_DIR` set, so the report/variance/gate JSON persist as
//! build artifacts.
//!
//! To refresh the baseline after an intentional suite/behavior change:
//! `WOVYR_EVAL_UPDATE_BASELINE=1 cargo test -p wovyr-eval --test regression_gate`
//! then commit the rewritten `baselines/capital-facts.json`.
//!
//! Both gate directions are proven here: the committed baseline passes against
//! the correct provider, and the identical gate **fails** against a regressed
//! provider — so a green run means the gate mechanism itself is alive, not
//! just that nothing changed.

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use wovyr_agent::AgentDefinition;
use wovyr_common::{Result, Usage};
use wovyr_eval::{
    Baseline, EvalSuite, LlmJudge, Scorer, VarianceReport, check, run_suite_repeated,
    run_suite_scored,
};
use wovyr_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role};
use wovyr_tools::ToolRegistry;

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn suite() -> EvalSuite {
    let yaml = std::fs::read_to_string(crate_path("suites/capital-facts.yaml")).unwrap();
    EvalSuite::from_yaml(&yaml).unwrap()
}

fn baseline() -> Baseline {
    Baseline::load(crate_path("baselines/capital-facts.json")).unwrap()
}

fn agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: gate-bot\nspec:\n  instructions: Answer questions.\n",
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

fn reply(text: &str, request: ChatRequest) -> Result<ChatResponse> {
    Ok(ChatResponse {
        message: Message::assistant(text.to_string()),
        model: request.model,
        usage: Usage::new(5, 5, 0.0),
        finish_reason: "stop".to_string(),
    })
}

/// Answers every committed fixture correctly (the refund one deliberately as
/// a paraphrase, so that case exercises the judge path end to end).
struct CorrectProvider;

#[async_trait]
impl AIProvider for CorrectProvider {
    fn name(&self) -> &str {
        "correct"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = user_text(&request);
        let text = if user.contains("France") {
            "The capital of France is Paris."
        } else if user.contains("Japan") {
            "The capital of Japan is Tokyo."
        } else if user.contains("refund") {
            "Purchases can be returned for a full month."
        } else {
            "I don't know."
        };
        reply(text, request)
    }
}

/// Regresses exactly one case (Japan → Kyoto).
struct RegressedProvider;

#[async_trait]
impl AIProvider for RegressedProvider {
    fn name(&self) -> &str {
        "regressed"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = user_text(&request);
        let text = if user.contains("France") {
            "The capital of France is Paris."
        } else if user.contains("Japan") {
            "The capital of Japan is Kyoto." // wrong on purpose
        } else if user.contains("refund") {
            "Purchases can be returned for a full month."
        } else {
            "I don't know."
        };
        reply(text, request)
    }
}

/// The deterministic judge for the paraphrased refund case.
struct ScriptedJudgeProvider;

#[async_trait]
impl AIProvider for ScriptedJudgeProvider {
    fn name(&self) -> &str {
        "scripted-judge"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let user = user_text(&request);
        let text = if user.contains("full month") {
            r#"{"score": 0.9, "reasoning": "a full month matches the 30-day window"}"#
        } else {
            r#"{"score": 0.1, "reasoning": "does not state the refund window"}"#
        };
        reply(text, request)
    }
}

fn scorer() -> Scorer {
    let judge_gateway = Arc::new(Gateway::new(Box::new(ScriptedJudgeProvider)));
    Scorer::exact_only().with_judge(Arc::new(LlmJudge::new(judge_gateway)))
}

/// Write a JSON artifact into `WOVYR_EVAL_ARTIFACT_DIR` when set (CI sets it
/// and uploads the directory); a local run without it skips silently.
fn write_artifact(name: &str, value: &impl serde::Serialize) {
    let Ok(dir) = std::env::var("WOVYR_EVAL_ARTIFACT_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create artifact dir");
    let body = serde_json::to_string_pretty(value).expect("serialize artifact");
    std::fs::write(dir.join(name), body + "\n").expect("write artifact");
}

/// EVL-202 acceptance (pass direction + variance): the committed suite meets
/// the committed baseline, and variance over N=3 identical runs is zero.
#[tokio::test]
async fn committed_suite_meets_the_committed_baseline_with_zero_variance() {
    let def = agent();
    let gateway = Gateway::new(Box::new(CorrectProvider));
    let registry = ToolRegistry::with_builtins();
    let scorer = scorer();

    let reports = run_suite_repeated(3, &suite(), &def, &gateway, &registry, &scorer)
        .await
        .unwrap();
    let variance = VarianceReport::from_reports(&reports);
    println!("{variance:#?}");
    let report = reports.into_iter().next().unwrap();
    println!("{report:#?}");

    // Optional refresh flow: rewrite the committed golden file, then still
    // gate against it (a freshly-refreshed baseline must gate clean).
    let baseline_path = crate_path("baselines/capital-facts.json");
    if std::env::var("WOVYR_EVAL_UPDATE_BASELINE").is_ok() {
        Baseline::from_report(&report, 1.0)
            .save(&baseline_path)
            .unwrap();
        println!("baseline refreshed at {}", baseline_path.display());
    }

    let gate = check(&report, &baseline());
    println!("{gate:#?}");
    write_artifact("report.json", &report);
    write_artifact("variance.json", &variance);
    write_artifact("gate.json", &gate);

    assert!(
        gate.passed,
        "the committed suite must meet the committed baseline: {gate:#?}"
    );
    assert_eq!(
        variance.distinct_reports, 1,
        "a deterministic provider + scripted judge must produce zero variance: {variance:#?}"
    );
    assert_eq!(variance.runs, 3);
    assert_eq!(variance.min_pass_rate, variance.max_pass_rate);
}

/// EVL-202 acceptance (fail direction): the identical gate fails when the
/// pass rate drops below the committed baseline threshold, naming the
/// regressed case.
#[tokio::test]
async fn the_gate_fails_a_regressed_run_against_the_same_baseline() {
    let def = agent();
    let gateway = Gateway::new(Box::new(RegressedProvider));
    let registry = ToolRegistry::with_builtins();

    let report = run_suite_scored(&suite(), &def, &gateway, &registry, &scorer())
        .await
        .unwrap();
    let gate = check(&report, &baseline());
    println!("{gate:#?}");

    assert!(!gate.passed, "a regression must fail the gate: {gate:#?}");
    assert!(
        gate.violations
            .iter()
            .any(|v| v.contains("below the baseline threshold")),
        "the rate violation is named: {gate:#?}"
    );
    assert!(
        gate.violations
            .iter()
            .any(|v| v.contains("`japan` regressed")),
        "the regressed case is named: {gate:#?}"
    );
}

/// The committed golden file itself stays consistent with the committed suite:
/// every baseline case exists in the suite and vice versa — a drift here means
/// someone edited one file without the other (run the refresh flow).
#[test]
fn committed_baseline_and_suite_agree_on_the_case_set() {
    let suite = suite();
    let baseline = baseline();
    assert_eq!(baseline.suite, suite.name);
    let suite_ids: Vec<&str> = suite.cases.iter().map(|c| c.id.as_str()).collect();
    for id in baseline.cases.keys() {
        assert!(
            suite_ids.contains(&id.as_str()),
            "baseline case `{id}` is not in the committed suite"
        );
    }
    assert_eq!(
        baseline.cases.len(),
        suite.cases.len(),
        "the suite has cases the baseline does not gate — refresh the baseline"
    );
}
