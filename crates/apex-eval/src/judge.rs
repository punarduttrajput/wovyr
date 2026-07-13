//! Model-backed scoring (RM-AIM-P2 EVL-201): an LLM-as-judge grader and a
//! semantic-similarity scorer, dispatched alongside the pure exact matchers
//! by [`Scorer`].
//!
//! The pure [`score`](crate::score::score) function can only string-match; a
//! semantically-correct answer phrased differently ("one month" vs "30 days")
//! fails `contains` even though it is right. A [`JudgeSpec`] grades the
//! answer against a plain-language rubric through an LLM; a [`SimilarSpec`]
//! compares embeddings. Both are **fail-closed**: a grading failure (judge
//! unreachable, unparseable verdict, missing configuration) fails the case
//! with a clear detail — it never passes by default, and unlike the memory
//! engine's reranker there is no "degrade" order to fall back to, because the
//! score *is* the product here.
//!
//! Determinism caveat, stated plainly: a real LLM judge is only as
//! reproducible as its model. The harness's byte-identical-report guarantee
//! holds against a deterministic (scripted/mock) judge — which is exactly how
//! the tests here run — and the eventual FUT-006 ADR owns the open question
//! of judging with a live model.

use crate::fixture::Expectation;
use crate::score::{CaseOutcome, score};
use apex_common::{Error, Result};
use apex_provider::{
    ChatRequest, EmbeddingRequest, Gateway, Message, ModelSelector, ResponseFormat,
    cosine_similarity,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// A judge's graded verdict on one answer.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    /// How well the answer satisfies the rubric, in `[0,1]`.
    pub score: f32,
    /// The judge's stated reasoning (may be empty).
    pub reasoning: String,
}

/// Grades an answer against a plain-language rubric.
///
/// [`LlmJudge`] is the model-backed implementation; tests script this trait
/// (or the provider beneath it) for determinism.
#[async_trait]
pub trait Judge: Send + Sync {
    /// Grade `actual` (the answer produced for `input`) against `rubric`.
    async fn grade(&self, input: &str, rubric: &str, actual: &str) -> Result<JudgeVerdict>;
}

/// An LLM-backed [`Judge`]: one gateway chat call grades the answer, with
/// PRV-202's structured output constraining the reply to
/// `{"score": <0..1>, "reasoning": "..."}` (leniently parsed for providers
/// that ignore the constraint, but a missing/non-numeric score is a clear
/// error — never a silent pass or fail).
pub struct LlmJudge {
    gateway: Arc<Gateway>,
}

impl LlmJudge {
    /// A judge grading through `gateway`'s default chat model. Judging with
    /// the same model that produced the answers is a known bias — prefer a
    /// separate judge gateway where it matters.
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }
}

#[async_trait]
impl Judge for LlmJudge {
    async fn grade(&self, input: &str, rubric: &str, actual: &str) -> Result<JudgeVerdict> {
        let model = self.gateway.resolve_model(None, &ModelSelector::default());
        let request = ChatRequest::new(
            model,
            vec![
                Message::system(
                    "You are an evaluation judge. Grade how well an answer satisfies a \
                     rubric: 1.0 fully satisfies it, 0.0 not at all. Judge substance, not \
                     wording — an answer phrased differently from the rubric still scores \
                     high if it is correct. Respond with JSON \
                     {\"score\": <number>, \"reasoning\": \"<one sentence>\"}.",
                ),
                Message::user(format!(
                    "Rubric: {rubric}\n\nTask input: {input}\n\nAnswer to grade:\n{actual}"
                )),
            ],
        )
        .with_response_format(ResponseFormat::JsonSchema {
            name: "verdict".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "score": { "type": "number" },
                    "reasoning": { "type": "string" }
                },
                "required": ["score", "reasoning"],
                "additionalProperties": false
            }),
        });

        let resp = self.gateway.chat(request).await?;
        parse_verdict(&resp.message.content.unwrap_or_default())
    }
}

/// Extract a [`JudgeVerdict`] from a model reply: a `{"score": ..}` object,
/// possibly wrapped in code fences or prose.
fn parse_verdict(content: &str) -> Result<JudgeVerdict> {
    fn from_value(v: &Value) -> Option<JudgeVerdict> {
        let obj = v.as_object()?;
        let score = obj.get("score")?.as_f64()? as f32;
        Some(JudgeVerdict {
            score: score.clamp(0.0, 1.0),
            reasoning: obj
                .get("reasoning")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    if let Ok(v) = serde_json::from_str::<Value>(content.trim())
        && let Some(verdict) = from_value(&v)
    {
        return Ok(verdict);
    }
    // Lenient fallback: the outermost {...} span inside prose/fences.
    if let (Some(start), Some(end)) = (content.find('{'), content.rfind('}'))
        && start < end
        && let Ok(v) = serde_json::from_str::<Value>(&content[start..=end])
        && let Some(verdict) = from_value(&v)
    {
        return Ok(verdict);
    }
    Err(Error::provider(format!(
        "judge reply is not a {{score, reasoning}} object: {content:.120}"
    )))
}

/// Embedding-cosine similarity scorer for `similar_to` expectations.
pub struct SemanticScorer {
    gateway: Arc<Gateway>,
}

impl SemanticScorer {
    /// A scorer embedding through `gateway`'s default embedding model.
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }

    /// Cosine similarity of the two texts' embeddings (one batched call).
    pub async fn similarity(&self, a: &str, b: &str) -> Result<f32> {
        let model = self.gateway.resolve_embedding_model(None);
        let resp = self
            .gateway
            .embed(EmbeddingRequest::new(
                model,
                vec![a.to_string(), b.to_string()],
            ))
            .await?;
        let [va, vb] = resp.vectors.as_slice() else {
            return Err(Error::provider(format!(
                "embedding response returned {} vectors for 2 inputs",
                resp.vectors.len()
            )));
        };
        Ok(cosine_similarity(va, vb))
    }
}

/// Dispatches a case's expectation to the right scoring mechanism: exact
/// matchers to the pure [`score`] function (unchanged, still deterministic),
/// `judge` to a configured [`Judge`], `similar_to` to a [`SemanticScorer`].
///
/// [`Scorer::exact_only`] (what plain [`run_suite`](crate::run_suite) uses)
/// deliberately configures neither model-backed check: a judge call costs
/// real tokens and must be an explicit choice, so a `judge`/`similar_to` case
/// scored without the matching configuration **fails with a clear detail**
/// rather than silently passing or silently invoking a model.
#[derive(Default)]
pub struct Scorer {
    judge: Option<Arc<dyn Judge>>,
    semantic: Option<SemanticScorer>,
}

impl Scorer {
    /// Exact string matchers only — `judge`/`similar_to` cases fail closed.
    pub fn exact_only() -> Self {
        Self::default()
    }

    /// Grade `judge:` expectations through `judge`.
    pub fn with_judge(mut self, judge: Arc<dyn Judge>) -> Self {
        self.judge = Some(judge);
        self
    }

    /// Grade `similar_to:` expectations by embedding through `gateway`.
    pub fn with_embeddings(mut self, gateway: Arc<Gateway>) -> Self {
        self.semantic = Some(SemanticScorer::new(gateway));
        self
    }

    /// Score one case. Exact matchers are pure; model-backed checks call out
    /// and fail closed on any grading failure.
    pub async fn score(&self, input: &str, actual: &str, expect: &Expectation) -> CaseOutcome {
        if let Some(spec) = &expect.judge {
            let Some(judge) = &self.judge else {
                return CaseOutcome {
                    passed: false,
                    detail: "case requires an LLM judge but the scorer has none configured \
                             (use Scorer::with_judge)"
                        .to_string(),
                };
            };
            return match judge.grade(input, &spec.rubric, actual).await {
                Ok(v) => CaseOutcome {
                    passed: v.score >= spec.min_score,
                    detail: format!(
                        "judge scored {:.2} (min {:.2}): {}",
                        v.score, spec.min_score, v.reasoning
                    ),
                },
                Err(e) => CaseOutcome {
                    passed: false,
                    detail: format!("judge failed (fail-closed): {e}"),
                },
            };
        }
        if let Some(spec) = &expect.similar_to {
            let Some(semantic) = &self.semantic else {
                return CaseOutcome {
                    passed: false,
                    detail: "case requires embedding similarity but the scorer has no \
                             embeddings configured (use Scorer::with_embeddings)"
                        .to_string(),
                };
            };
            return match semantic.similarity(actual, &spec.text).await {
                Ok(sim) => CaseOutcome {
                    passed: sim >= spec.threshold,
                    detail: format!(
                        "cosine similarity {:.3} (threshold {:.3}) to `{}`",
                        sim, spec.threshold, spec.text
                    ),
                },
                Err(e) => CaseOutcome {
                    passed: false,
                    detail: format!("similarity scoring failed (fail-closed): {e}"),
                },
            };
        }
        score(actual, expect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_common::Usage;
    use apex_provider::{AIProvider, ChatResponse, EmbeddingResponse, Role};
    use std::sync::Mutex;

    /// A provider returning a canned judge verdict and recording requests.
    struct CannedJudgeProvider {
        reply: String,
        seen: Arc<Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl AIProvider for CannedJudgeProvider {
        fn name(&self) -> &str {
            "canned-judge"
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            self.seen.lock().unwrap().push(request.clone());
            Ok(ChatResponse {
                message: Message::assistant(self.reply.clone()),
                model: request.model,
                usage: Usage::new(5, 3, 0.0),
                finish_reason: "stop".to_string(),
            })
        }
    }

    fn judge_with(reply: &str) -> (LlmJudge, Arc<Mutex<Vec<ChatRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let gw = Arc::new(Gateway::new(Box::new(CannedJudgeProvider {
            reply: reply.to_string(),
            seen: seen.clone(),
        })));
        (LlmJudge::new(gw), seen)
    }

    /// EVL-201 acceptance (scorer level): a semantically-correct answer that
    /// shares no substring with the expectation passes the judge where
    /// `contains` fails.
    #[tokio::test]
    async fn judge_passes_a_semantically_correct_non_substring_answer() {
        let input = "How long is the refund window?";
        let actual = "You can return items for a full month after purchase.";

        // The exact matcher fails: the answer never says "30 days".
        let exact = Expectation::contains("30 days");
        assert!(!score(actual, &exact).passed, "contains must fail");

        // The scripted judge recognizes the same meaning and passes it.
        let (judge, _) =
            judge_with(r#"{"score": 0.92, "reasoning": "one month equals the 30-day window"}"#);
        let scorer = Scorer::exact_only().with_judge(Arc::new(judge));
        let judged = Expectation::judged("States the refund window is 30 days (one month).");
        let outcome = scorer.score(input, actual, &judged).await;
        assert!(outcome.passed, "judge must pass it: {}", outcome.detail);
        assert!(outcome.detail.contains("0.92"));
        assert!(outcome.detail.contains("one month equals"));
    }

    #[tokio::test]
    async fn judge_min_score_gates_the_pass() {
        let (judge, _) = judge_with(r#"{"score": 0.55, "reasoning": "partially correct"}"#);
        let scorer = Scorer::exact_only().with_judge(Arc::new(judge));
        let outcome = scorer.score("q", "a", &Expectation::judged("rubric")).await;
        assert!(!outcome.passed, "0.55 < default 0.7 must fail");
        assert!(outcome.detail.contains("0.55"));
    }

    #[tokio::test]
    async fn judge_request_carries_rubric_input_answer_and_schema() {
        let (judge, seen) = judge_with(r#"{"score": 1.0, "reasoning": "ok"}"#);
        judge
            .grade("the input", "the rubric", "the answer")
            .await
            .unwrap();
        let requests = seen.lock().unwrap();
        let req = &requests[0];
        assert!(matches!(
            req.response_format,
            Some(ResponseFormat::JsonSchema { .. })
        ));
        let user = req
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap()
            .content
            .as_deref()
            .unwrap();
        assert!(user.contains("the rubric"));
        assert!(user.contains("the input"));
        assert!(user.contains("the answer"));
    }

    #[tokio::test]
    async fn an_unparseable_or_missing_judge_fails_closed() {
        // Garbage reply → fail with the error surfaced.
        let (judge, _) = judge_with("looks good to me!");
        let scorer = Scorer::exact_only().with_judge(Arc::new(judge));
        let outcome = scorer.score("q", "a", &Expectation::judged("rubric")).await;
        assert!(!outcome.passed);
        assert!(outcome.detail.contains("fail-closed"));

        // No judge configured at all → fail with a clear detail.
        let bare = Scorer::exact_only();
        let outcome = bare.score("q", "a", &Expectation::judged("rubric")).await;
        assert!(!outcome.passed);
        assert!(outcome.detail.contains("none configured"));
    }

    #[test]
    fn verdicts_parse_leniently_but_never_silently() {
        let fenced = "```json\n{\"score\": 0.8, \"reasoning\": \"fine\"}\n```";
        assert_eq!(parse_verdict(fenced).unwrap().score, 0.8);
        let clamped = parse_verdict(r#"{"score": 3.0, "reasoning": ""}"#).unwrap();
        assert_eq!(clamped.score, 1.0, "out-of-range scores clamp");
        assert!(parse_verdict("no json here").is_err());
        assert!(parse_verdict(r#"{"reasoning": "no score"}"#).is_err());
    }

    /// A provider with scripted per-text embeddings.
    struct ScriptedEmbedder;

    #[async_trait]
    impl AIProvider for ScriptedEmbedder {
        fn name(&self) -> &str {
            "scripted-embedder"
        }
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            Err(Error::invalid("chat is not scripted"))
        }
        async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            let vectors = request
                .input
                .iter()
                .map(|text| {
                    if text.contains("greeting") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect();
            Ok(EmbeddingResponse {
                vectors,
                model: request.model,
                usage: Usage::new(2, 0, 0.0),
            })
        }
    }

    #[tokio::test]
    async fn similarity_scorer_passes_close_and_fails_orthogonal_texts() {
        let gw = Arc::new(Gateway::new(Box::new(ScriptedEmbedder)));
        let scorer = Scorer::exact_only().with_embeddings(gw);

        // Both texts embed to the same vector → cosine 1.0 ≥ 0.8.
        let close = Expectation::similar_to("a friendly greeting");
        let outcome = scorer.score("q", "warm greeting text", &close).await;
        assert!(outcome.passed, "{}", outcome.detail);

        // Orthogonal vectors → cosine 0.0 < 0.8.
        let far = scorer.score("q", "quarterly revenue table", &close).await;
        assert!(!far.passed);
        assert!(far.detail.contains("0.000"));
    }

    #[tokio::test]
    async fn similarity_without_embeddings_fails_closed() {
        let scorer = Scorer::exact_only();
        let outcome = scorer
            .score("q", "a", &Expectation::similar_to("ref"))
            .await;
        assert!(!outcome.passed);
        assert!(outcome.detail.contains("no embeddings configured"));
    }

    #[tokio::test]
    async fn exact_matchers_still_route_through_the_pure_score() {
        let scorer = Scorer::exact_only();
        assert!(
            scorer
                .score("q", "hello Ada", &Expectation::contains("Ada"))
                .await
                .passed
        );
        assert!(
            !scorer
                .score("q", "hello Bob", &Expectation::contains("Ada"))
                .await
                .passed
        );
    }
}
