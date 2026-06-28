//! OpenAI-compatible chat provider.
//!
//! Targets the `/chat/completions` API shape used by OpenAI and many compatible
//! servers (Azure OpenAI, OpenRouter, Ollama, llama.cpp, vLLM, …), so a single
//! adapter covers several entries in the
//! [supported-providers table](../../docs/04-agent-framework/provider-sdk.md#5-supported-providers).
//! The base URL and key come from the environment, keeping credentials out of
//! code ([coding standards §9](../../docs/19-implementation-guide/coding-standards.md)).

use crate::embeddings::{EmbeddingRequest, EmbeddingResponse};
use crate::provider::AIProvider;
use crate::types::{ChatRequest, ChatResponse, Message, Role, ToolCall};
use apex_common::{Error, Result, Usage};
use async_trait::async_trait;
use serde_json::{Value, json};

/// Default OpenAI API base URL.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// An adapter for OpenAI-compatible chat completion endpoints.
pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAiProvider {
    /// Build a provider from explicit configuration.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    /// Build from the environment.
    ///
    /// Reads `OPENAI_API_KEY` (required) and `APEX_OPENAI_BASE_URL` (optional,
    /// defaults to the OpenAI public endpoint). Returns [`Error::Config`] when no
    /// key is set so the caller can fall back to the mock provider.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| Error::config("OPENAI_API_KEY is not set"))?;
        let base_url =
            std::env::var("APEX_OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self::new(base_url, api_key))
    }

    /// Translate a normalized [`Message`] into the OpenAI wire shape.
    fn encode_message(msg: &Message) -> Value {
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut obj = json!({ "role": role });

        // `content` may be null on an assistant turn that is purely tool calls.
        obj["content"] = match &msg.content {
            Some(c) => json!(c),
            None => Value::Null,
        };

        if !msg.tool_calls.is_empty() {
            obj["tool_calls"] = Value::Array(
                msg.tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": { "name": tc.name, "arguments": tc.arguments },
                        })
                    })
                    .collect(),
            );
        }
        if let Some(id) = &msg.tool_call_id {
            obj["tool_call_id"] = json!(id);
        }
        if let Some(name) = &msg.name {
            obj["name"] = json!(name);
        }
        obj
    }
}

#[async_trait]
impl AIProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let messages: Vec<Value> = request.messages.iter().map(Self::encode_message).collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
        });
        if let Some(t) = request.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = request.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            },
                        })
                    })
                    .collect(),
            );
        }

        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::provider(format!("request to {url} failed: {e}")))?;

        let status = resp.status();
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| Error::provider(format!("decoding response failed: {e}")))?;

        if !status.is_success() {
            let msg = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(classify_http_error(status.as_u16(), msg));
        }

        parse_response(&payload, &request.model)
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let body = json!({ "model": request.model, "input": request.input });
        let url = format!("{}/embeddings", self.base_url);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::provider(format!("request to {url} failed: {e}")))?;

        let status = resp.status();
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| Error::provider(format!("decoding response failed: {e}")))?;

        if !status.is_success() {
            let msg = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(classify_http_error(status.as_u16(), msg));
        }

        parse_embeddings(&payload, &request.model)
    }
}

/// Map an HTTP error status to the right error kind for the resilience layer:
/// 429 and 5xx are transient ([`Error::Provider`], retry/failover); other 4xx are
/// permanent client errors ([`Error::Invalid`])
/// ([resilience §8](../../docs/05-llm-gateway/resilience.md)).
fn classify_http_error(status: u16, msg: &str) -> Error {
    if status == 429 || status >= 500 {
        Error::provider(format!("provider returned {status}: {msg}"))
    } else {
        Error::invalid(format!("provider returned {status}: {msg}"))
    }
}

/// Parse an OpenAI-shaped embeddings payload into an [`EmbeddingResponse`].
fn parse_embeddings(payload: &Value, requested_model: &str) -> Result<EmbeddingResponse> {
    let data = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::provider("embeddings response had no data array"))?;

    let vectors = data
        .iter()
        .map(|item| {
            item.get("embedding")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|n| n.as_f64().map(|f| f as f32))
                        .collect()
                })
                .ok_or_else(|| Error::provider("embeddings item had no embedding array"))
        })
        .collect::<Result<Vec<Vec<f32>>>>()?;

    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(requested_model)
        .to_string();

    let prompt_tokens = payload
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    Ok(EmbeddingResponse {
        model,
        vectors,
        usage: Usage::new(prompt_tokens, 0, 0.0),
    })
}

/// Parse an OpenAI-shaped completion payload into a [`ChatResponse`].
fn parse_response(payload: &Value, requested_model: &str) -> Result<ChatResponse> {
    let choice = payload
        .pointer("/choices/0")
        .ok_or_else(|| Error::provider("response had no choices"))?;
    let msg = choice
        .get("message")
        .ok_or_else(|| Error::provider("choice had no message"))?;

    let content = msg
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string);

    let tool_calls = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| {
                    Some(ToolCall {
                        id: c.get("id")?.as_str()?.to_string(),
                        name: c.pointer("/function/name")?.as_str()?.to_string(),
                        arguments: c
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop")
        .to_string();

    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(requested_model)
        .to_string();

    let prompt_tokens = payload
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion_tokens = payload
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    Ok(ChatResponse {
        message: Message {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
        },
        model,
        // Cost is left at 0.0 in v0.1; per-model pricing arrives with the
        // token-management work in a later milestone.
        usage: Usage::new(prompt_tokens, completion_tokens, 0.0),
        finish_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_completion() {
        let payload = json!({
            "model": "gpt-4o-mini",
            "choices": [{
                "message": { "role": "assistant", "content": "hi!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 3 }
        });
        let r = parse_response(&payload, "requested").unwrap();
        assert_eq!(r.message.content.as_deref(), Some("hi!"));
        assert_eq!(r.model, "gpt-4o-mini");
        assert_eq!(r.usage.total_tokens, 15);
        assert!(r.message.tool_calls.is_empty());
    }

    #[test]
    fn parses_tool_calls() {
        let payload = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "echo", "arguments": "{\"x\":1}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let r = parse_response(&payload, "m").unwrap();
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!(r.message.tool_calls.len(), 1);
        assert_eq!(r.message.tool_calls[0].name, "echo");
    }

    #[test]
    fn encodes_assistant_tool_call_with_null_content() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "echo".to_string(),
                arguments: "{\"x\":1}".to_string(),
            }],
            tool_call_id: None,
            name: None,
        };
        let v = OpenAiProvider::encode_message(&msg);
        assert_eq!(v["role"], "assistant");
        assert!(v["content"].is_null());
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "echo");
    }

    #[test]
    fn encodes_tool_result_message() {
        let msg = Message::tool_result("call_1", "echo", "{\"ok\":true}");
        let v = OpenAiProvider::encode_message(&msg);
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "call_1");
        assert_eq!(v["name"], "echo");
        assert_eq!(v["content"], "{\"ok\":true}");
    }

    #[test]
    fn parses_embeddings() {
        let payload = json!({
            "model": "text-embedding-3-small",
            "data": [
                { "embedding": [0.1, 0.2, 0.3] },
                { "embedding": [0.4, 0.5, 0.6] }
            ],
            "usage": { "prompt_tokens": 7 }
        });
        let r = parse_embeddings(&payload, "requested").unwrap();
        assert_eq!(r.model, "text-embedding-3-small");
        assert_eq!(r.vectors.len(), 2);
        assert_eq!(r.vectors[0], vec![0.1f32, 0.2, 0.3]);
        assert_eq!(r.usage.prompt_tokens, 7);
    }
}
