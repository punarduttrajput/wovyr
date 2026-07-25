//! Vendor-neutral embedding types and helpers.
//!
//! Mirrors the embedding interface in the
//! [Provider SDK spec §16](../../docs/04-agent-framework/provider-sdk.md)
//! (`embed`, `embed_batch`, `similarity`). v0.1 ships the gateway-level capability
//! (chat + embeddings) called for in the [roadmap](../../docs/18-roadmap/v0.1.md);
//! the memory engine that consumes embeddings arrives in a later milestone.

use serde::{Deserialize, Serialize};

/// A request to embed one or more texts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    /// Concrete embedding model id (resolved by the [`crate::Gateway`]).
    pub model: String,
    /// Texts to embed. A single call may embed a batch.
    pub input: Vec<String>,
}

impl EmbeddingRequest {
    /// Build a request for `model` over `input`.
    pub fn new(model: impl Into<String>, input: Vec<String>) -> Self {
        Self {
            model: model.into(),
            input,
        }
    }
}

/// The result of an embedding request: one vector per input, in order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    /// Concrete model that produced the vectors.
    pub model: String,
    /// One embedding vector per input text, preserving order.
    pub vectors: Vec<Vec<f32>>,
    /// Token/cost accounting for the request.
    pub usage: wovyr_common::Usage,
}

/// Cosine similarity of two equal-length vectors, in `[-1.0, 1.0]`.
///
/// Returns `0.0` for mismatched lengths or a zero-magnitude vector.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_are_maximally_similar() {
        let v = vec![0.1, 0.2, 0.3];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_are_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn mismatched_lengths_are_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
    }
}
