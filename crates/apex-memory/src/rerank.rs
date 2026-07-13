//! Optional re-ranking stage after retrieval fusion (RM-AIM-P2 RAG-202).
//!
//! RRF fusion + the weighted ranker order candidates by *lexical/embedding*
//! signals; a [`Reranker`] re-scores the fused top-N against the query with a
//! stronger (and slower) relevance model — the classic two-stage
//! retrieve-then-rerank pipeline. Opt-in via
//! [`MemoryEngine::with_reranker`](crate::MemoryEngine::with_reranker): the
//! default engine behavior is byte-identical to before this stage existed.
//!
//! The trait returns **scores, not a permutation**, so reranked relevance
//! flows through the existing weighted ranker (recency/importance still
//! apply) and stays visible in each result's `ScoreBreakdown`.

use apex_common::{Error, Result};
use apex_provider::{ChatRequest, Gateway, Message, ModelSelector, ResponseFormat};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Re-scores retrieval candidates against a query.
///
/// Implementations must return one score in `[0,1]` per candidate, in input
/// order (the engine treats any other shape as a failure and degrades to the
/// fused order). A cross-encoder-backed implementation drops in behind this
/// trait the same way [`LlmReranker`] does.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Relevance of each candidate text to `query`, in `[0,1]`, input order.
    async fn rerank(&self, query: &str, candidates: &[&str]) -> Result<Vec<f32>>;
}

/// An LLM-backed [`Reranker`]: one gateway chat call scores every candidate.
///
/// Uses PRV-202's structured output (`ResponseFormat::JsonSchema`) so a
/// schema-capable provider returns machine-parseable scores; the parser is
/// additionally lenient about a bare array or surrounding prose, since not
/// every provider honors the constraint.
pub struct LlmReranker {
    gateway: Arc<Gateway>,
}

impl LlmReranker {
    /// A reranker scoring through `gateway`'s default chat model.
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }
}

#[async_trait]
impl Reranker for LlmReranker {
    async fn rerank(&self, query: &str, candidates: &[&str]) -> Result<Vec<f32>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let listing = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {c}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let model = self.gateway.resolve_model(None, &ModelSelector::default());
        let request = ChatRequest::new(
            model,
            vec![
                Message::system(
                    "You score document relevance. Given a query and a numbered list of \
                     documents, respond with JSON {\"scores\": [...]}: one relevance score \
                     between 0.0 and 1.0 per document, in the listed order.",
                ),
                Message::user(format!("Query: {query}\n\nDocuments:\n{listing}")),
            ],
        )
        .with_response_format(ResponseFormat::JsonSchema {
            name: "scores".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "scores": { "type": "array", "items": { "type": "number" } }
                },
                "required": ["scores"],
                "additionalProperties": false
            }),
        });

        let resp = self.gateway.chat(request).await?;
        let content = resp.message.content.unwrap_or_default();
        let scores = parse_scores(&content)?;
        if scores.len() != candidates.len() {
            return Err(Error::provider(format!(
                "reranker returned {} scores for {} candidates",
                scores.len(),
                candidates.len()
            )));
        }
        Ok(scores.into_iter().map(|s| s.clamp(0.0, 1.0)).collect())
    }
}

/// Extract the score array from a model response: `{"scores": [...]}`, a bare
/// array, or either embedded in surrounding prose/code fences.
fn parse_scores(content: &str) -> Result<Vec<f32>> {
    fn from_value(v: &Value) -> Option<Vec<f32>> {
        let arr = match v {
            Value::Object(o) => o.get("scores")?.as_array()?,
            Value::Array(a) => a,
            _ => return None,
        };
        arr.iter()
            .map(|n| n.as_f64().map(|f| f as f32))
            .collect::<Option<Vec<f32>>>()
    }

    if let Ok(v) = serde_json::from_str::<Value>(content.trim())
        && let Some(scores) = from_value(&v)
    {
        return Ok(scores);
    }
    // Lenient fallback: the outermost {...} or [...] span inside prose.
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(start), Some(end)) = (content.find(open), content.rfind(close))
            && start < end
            && let Ok(v) = serde_json::from_str::<Value>(&content[start..=end])
            && let Some(scores) = from_value(&v)
        {
            return Ok(scores);
        }
    }
    Err(Error::provider(format!(
        "reranker response is not a score list: {content:.120}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_common::Usage;
    use apex_provider::{AIProvider, ChatResponse, Role};
    use std::sync::Mutex;

    /// A provider that returns a canned reply and records the request.
    struct CannedProvider {
        reply: String,
        seen: Mutex<Vec<ChatRequest>>,
    }

    #[async_trait]
    impl AIProvider for CannedProvider {
        fn name(&self) -> &str {
            "canned"
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            self.seen.lock().unwrap().push(request.clone());
            Ok(ChatResponse {
                message: Message::assistant(self.reply.clone()),
                model: request.model,
                usage: Usage::new(3, 2, 0.0),
                finish_reason: "stop".to_string(),
            })
        }
    }

    fn reranker(reply: &str) -> (LlmReranker, Arc<Gateway>) {
        let gw = Arc::new(Gateway::new(Box::new(CannedProvider {
            reply: reply.to_string(),
            seen: Mutex::new(Vec::new()),
        })));
        (LlmReranker::new(gw.clone()), gw)
    }

    #[tokio::test]
    async fn scores_parse_and_clamp_from_a_schema_shaped_reply() {
        let (rr, _) = reranker(r#"{"scores": [0.9, 1.7, -0.2]}"#);
        let scores = rr.rerank("q", &["a", "b", "c"]).await.unwrap();
        assert_eq!(scores, vec![0.9, 1.0, 0.0], "out-of-range scores clamp");
    }

    #[tokio::test]
    async fn a_bare_array_or_fenced_reply_still_parses() {
        let (rr, _) = reranker("[0.1, 0.2]");
        assert_eq!(rr.rerank("q", &["a", "b"]).await.unwrap(), vec![0.1, 0.2]);

        let (rr, _) = reranker("```json\n{\"scores\": [0.5, 0.6]}\n```");
        assert_eq!(rr.rerank("q", &["a", "b"]).await.unwrap(), vec![0.5, 0.6]);
    }

    #[tokio::test]
    async fn a_length_mismatch_is_an_error_not_silent_misalignment() {
        let (rr, _) = reranker(r#"{"scores": [0.9]}"#);
        let err = rr.rerank("q", &["a", "b"]).await.unwrap_err();
        assert!(err.to_string().contains("1 scores for 2 candidates"));
    }

    #[tokio::test]
    async fn non_numeric_garbage_is_a_clear_error() {
        let (rr, _) = reranker("the documents look fine to me");
        assert!(rr.rerank("q", &["a"]).await.is_err());
    }

    #[tokio::test]
    async fn empty_candidates_short_circuit_without_a_model_call() {
        let gw = Arc::new(Gateway::new(Box::new(CannedProvider {
            reply: "unused".into(),
            seen: Mutex::new(Vec::new()),
        })));
        let rr = LlmReranker::new(gw);
        assert!(rr.rerank("q", &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_request_carries_a_json_schema_constraint_and_the_candidates() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        struct Recording {
            seen: Arc<Mutex<Vec<ChatRequest>>>,
        }
        #[async_trait]
        impl AIProvider for Recording {
            fn name(&self) -> &str {
                "rec"
            }
            async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
                self.seen.lock().unwrap().push(request.clone());
                Ok(ChatResponse {
                    message: Message::assistant(r#"{"scores": [0.4, 0.8]}"#),
                    model: request.model,
                    usage: Usage::new(1, 1, 0.0),
                    finish_reason: "stop".to_string(),
                })
            }
        }
        let gw = Arc::new(Gateway::new(Box::new(Recording { seen: seen.clone() })));
        let rr = LlmReranker::new(gw);
        let scores = rr
            .rerank("refund window", &["doc a", "doc b"])
            .await
            .unwrap();
        assert_eq!(scores, vec![0.4, 0.8]);

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
        assert!(user.contains("refund window"));
        assert!(user.contains("1. doc a") && user.contains("2. doc b"));
    }
}
