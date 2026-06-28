//! A deterministic, offline provider for local development and tests.
//!
//! The [v0.1 roadmap](../../docs/18-roadmap/v0.1.md) requires the hello agent to
//! run end-to-end locally. Real providers need API keys and network; the
//! `MockProvider` lets `apex agents run --local` work with zero configuration and
//! gives tests a deterministic model. Determinism is a coding-standard
//! requirement ([§7](../../docs/19-implementation-guide/coding-standards.md)).

use crate::embeddings::{EmbeddingRequest, EmbeddingResponse};
use crate::provider::AIProvider;
use crate::types::{ChatRequest, ChatResponse, Message, Role};
use apex_common::{Result, Usage};
use async_trait::async_trait;

/// Rough $/token figure used only to populate the cost field for mock runs.
const MOCK_USD_PER_TOKEN: f64 = 0.000_000_5;

/// Dimensionality of mock embedding vectors.
const MOCK_EMBED_DIM: usize = 16;

/// A provider that synthesizes a plausible reply without any network call.
#[derive(Debug, Default, Clone)]
pub struct MockProvider;

impl MockProvider {
    /// Create a new mock provider.
    pub fn new() -> Self {
        Self
    }
}

/// Estimate token count from text (~4 chars/token), with a small floor.
fn estimate_tokens(text: &str) -> u32 {
    ((text.len() as f32 / 4.0).ceil() as u32).max(1)
}

#[async_trait]
impl AIProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let system = request
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("");

        let user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("");

        // Deterministic, instruction-aware canned reply.
        let reply = if user.is_empty() {
            "Hello! I'm an Apex agent running locally with the mock provider. \
             Set OPENAI_API_KEY to use a real model."
                .to_string()
        } else {
            format!(
                "Hello! I'm an Apex agent (mock provider). You said: \"{user}\". \
                 I'd normally reason about this using my instructions{} and any \
                 tools available, then reply. Set OPENAI_API_KEY for a real model.",
                if system.is_empty() {
                    String::new()
                } else {
                    format!(" ({} chars of guidance)", system.len())
                }
            )
        };

        let prompt_tokens = request
            .messages
            .iter()
            .map(|m| estimate_tokens(m.content.as_deref().unwrap_or("")))
            .sum::<u32>();
        let completion_tokens = estimate_tokens(&reply);
        let cost = (prompt_tokens + completion_tokens) as f64 * MOCK_USD_PER_TOKEN;

        Ok(ChatResponse {
            message: Message::assistant(reply),
            model: request.model,
            usage: Usage::new(prompt_tokens, completion_tokens, cost),
            finish_reason: "stop".to_string(),
        })
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let vectors: Vec<Vec<f32>> = request.input.iter().map(|t| mock_embedding(t)).collect();
        let prompt_tokens = request
            .input
            .iter()
            .map(|t| estimate_tokens(t))
            .sum::<u32>();
        let cost = prompt_tokens as f64 * MOCK_USD_PER_TOKEN;
        Ok(EmbeddingResponse {
            model: request.model,
            vectors,
            usage: Usage::new(prompt_tokens, 0, cost),
        })
    }
}

/// Produce a deterministic, unit-length pseudo-embedding for `text`.
///
/// Not semantically meaningful — it only needs to be stable (same text → same
/// vector) and distinct enough for tests. Real semantics come from a model
/// provider; this keeps embedding-dependent code testable offline.
fn mock_embedding(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; MOCK_EMBED_DIM];
    for (j, byte) in text.bytes().enumerate() {
        let idx = j % MOCK_EMBED_DIM;
        v[idx] += ((byte as f32) * (idx as f32 + 1.0)).sin();
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echoes_user_message_deterministically() {
        let p = MockProvider::new();
        let req = ChatRequest::new(
            "apex-mock-fast",
            vec![Message::system("be nice"), Message::user("hi there")],
        );
        let a = p.chat(req.clone()).await.unwrap();
        let b = p.chat(req).await.unwrap();

        assert_eq!(
            a.message.content, b.message.content,
            "mock must be deterministic"
        );
        assert!(a.message.content.unwrap().contains("hi there"));
        assert!(a.usage.total_tokens > 0);
        assert_eq!(a.finish_reason, "stop");
    }

    #[tokio::test]
    async fn embeds_deterministically() {
        use crate::embeddings::cosine_similarity;

        let p = MockProvider::new();
        let req = EmbeddingRequest::new("mock-embed", vec!["alpha".into(), "beta".into()]);
        let a = p.embed(req.clone()).await.unwrap();
        let b = p.embed(req).await.unwrap();

        assert_eq!(a.vectors.len(), 2);
        assert_eq!(a.vectors[0].len(), MOCK_EMBED_DIM);
        // Deterministic: same input → identical vectors.
        assert_eq!(a.vectors, b.vectors);
        // Each vector is unit length, so self-similarity is ~1.
        assert!((cosine_similarity(&a.vectors[0], &a.vectors[0]) - 1.0).abs() < 1e-5);
        // Distinct inputs generally produce distinct vectors.
        assert_ne!(a.vectors[0], a.vectors[1]);
    }
}
