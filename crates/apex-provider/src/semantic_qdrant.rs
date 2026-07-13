//! Qdrant-backed distributed semantic cache.
//!
//! Implements [`SemanticCacheStore`](crate::resilience::SemanticCacheStore) over
//! Qdrant's REST API so a fleet of gateways shares one semantic cache
//! ([caching §4](../../docs/05-llm-gateway/caching.md)): request embeddings are
//! points (cosine distance), the cached [`ChatResponse`] and its param-compatibility
//! key/created-at live in the payload. Enabled by the `qdrant` cargo feature.
//!
//! Entries are not auto-expired in Qdrant; `lookup` enforces the TTL against the
//! payload `created_ms`, so stale points are ignored (a background sweep is left for
//! a later slice).

use crate::resilience::SemanticCacheStore;
use crate::types::ChatResponse;
use apex_common::{Error, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};

fn qd_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("qdrant semantic cache {context}: {e}"))
}

/// Deterministic point id from the param key + embedding model + embedding, so
/// re-storing the same request overwrites its entry rather than duplicating it.
fn point_id(param_key: &str, embedding_model: &str, embedding: &[f32]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    param_key.hash(&mut hasher);
    embedding_model.hash(&mut hasher);
    for v in embedding {
        v.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// A Qdrant-backed semantic cache addressed over its REST API.
pub struct QdrantSemanticCache {
    base_url: String,
    collection: String,
    client: reqwest::Client,
    ready: tokio::sync::Mutex<bool>,
}

impl QdrantSemanticCache {
    /// Create a cache over `collection` at `base_url` (e.g. `http://localhost:6333`).
    pub fn new(base_url: impl Into<String>, collection: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            collection: collection.into(),
            client: reqwest::Client::new(),
            ready: tokio::sync::Mutex::new(false),
        }
    }

    fn collection_url(&self) -> String {
        format!("{}/collections/{}", self.base_url, self.collection)
    }

    /// Create the collection (sized to `dim`, cosine distance) once, if absent.
    async fn ensure_collection(&self, dim: usize) -> Result<()> {
        let mut ready = self.ready.lock().await;
        if *ready {
            return Ok(());
        }
        let exists = self
            .client
            .get(self.collection_url())
            .send()
            .await
            .map_err(|e| qd_err("collection check", e))?
            .status()
            .is_success();
        if !exists {
            let body = json!({ "vectors": { "size": dim, "distance": "Cosine" } });
            let resp = self
                .client
                .put(self.collection_url())
                .json(&body)
                .send()
                .await
                .map_err(|e| qd_err("create collection", e))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(qd_err("create collection", format!("{status}: {text}")));
            }
        }
        *ready = true;
        Ok(())
    }
}

#[async_trait]
impl SemanticCacheStore for QdrantSemanticCache {
    async fn lookup(
        &self,
        param_key: &str,
        embedding_model: &str,
        embedding: &[f32],
        threshold: f32,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ChatResponse>> {
        let body = json!({
            "vector": embedding,
            "limit": 1,
            "with_payload": true,
            // Entries from a different embedding model are filtered out
            // server-side (RM-AIM-P2 RAG-203): their vectors live in a
            // different space, so a cosine score against them is meaningless.
            "filter": { "must": [
                { "key": "param_key", "match": { "value": param_key } },
                { "key": "embedding_model", "match": { "value": embedding_model } }
            ] }
        });
        let resp = self
            .client
            .post(format!("{}/points/search", self.collection_url()))
            .json(&body)
            .send()
            .await
            .map_err(|e| qd_err("search", e))?;

        // A missing collection (nothing cached yet) is a miss, not an error.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(qd_err("search", format!("{status}: {text}")));
        }

        let parsed: Value = resp.json().await.map_err(|e| qd_err("decode", e))?;
        let Some(hit) = parsed
            .get("result")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
        else {
            return Ok(None);
        };

        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        if score < threshold {
            return Ok(None);
        }
        let created_ms = hit
            .pointer("/payload/created_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if now_ms.saturating_sub(created_ms) > ttl_ms {
            return Ok(None);
        }
        let response = hit
            .pointer("/payload/response")
            .ok_or_else(|| qd_err("decode", "missing response payload"))?;
        let response: ChatResponse =
            serde_json::from_value(response.clone()).map_err(|e| qd_err("decode response", e))?;
        Ok(Some(response))
    }

    async fn store(
        &self,
        param_key: &str,
        embedding_model: &str,
        embedding: &[f32],
        response: &ChatResponse,
        now_ms: u64,
    ) -> Result<()> {
        self.ensure_collection(embedding.len()).await?;
        let response_json =
            serde_json::to_value(response).map_err(|e| qd_err("encode response", e))?;
        let body = json!({
            "points": [{
                "id": point_id(param_key, embedding_model, embedding),
                "vector": embedding,
                "payload": {
                    "param_key": param_key,
                    "embedding_model": embedding_model,
                    "created_ms": now_ms,
                    "response": response_json
                }
            }]
        });
        let resp = self
            .client
            .put(format!("{}/points?wait=true", self.collection_url()))
            .json(&body)
            .send()
            .await
            .map_err(|e| qd_err("upsert", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(qd_err("upsert", format!("{status}: {text}")));
        }
        Ok(())
    }
}
