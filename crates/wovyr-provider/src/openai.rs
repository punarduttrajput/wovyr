//! OpenAI-compatible chat provider.
//!
//! Targets the `/chat/completions` API shape used by OpenAI and many compatible
//! servers (Azure OpenAI, OpenRouter, Ollama, llama.cpp, vLLM, …), so a single
//! adapter covers several entries in the
//! [supported-providers table](../../docs/04-agent-framework/provider-sdk.md#5-supported-providers).
//! The base URL and key come from the environment, keeping credentials out of
//! code ([coding standards §9](../../docs/19-implementation-guide/coding-standards.md)).

use crate::embeddings::{EmbeddingRequest, EmbeddingResponse};
use crate::image::{ImageGenRequest, ImageGenResponse};
use crate::pricing::PriceBook;
use crate::provider::{AIProvider, ChatStream, ChatStreamEvent};
use crate::types::{
    ChatRequest, ChatResponse, ContentPart, Message, ResponseFormat, Role, ToolCall, ToolChoice,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use wovyr_common::{Error, Result, Usage};

/// Default OpenAI API base URL.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// An adapter for OpenAI-compatible chat completion endpoints.
pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Per-model price table used to compute `cost_usd` from returned token usage.
    prices: PriceBook,
}

impl OpenAiProvider {
    /// Build a provider from explicit configuration.
    ///
    /// Uses the operator-overridable price table ([`PriceBook::from_env`]) so a run
    /// through this provider records real cost regardless of which constructor the
    /// caller used; swap it with [`OpenAiProvider::with_price_book`].
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            prices: PriceBook::from_env(),
        }
    }

    /// Build from the environment.
    ///
    /// Reads `OPENAI_API_KEY` (required) and `WOVYR_OPENAI_BASE_URL` (optional,
    /// defaults to the OpenAI public endpoint). Returns [`Error::Config`] when no
    /// key is set so the caller can fall back to the mock provider.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| Error::config("OPENAI_API_KEY is not set"))?;
        let base_url =
            std::env::var("WOVYR_OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self::new(base_url, api_key))
    }

    /// Override the price table (builder-style).
    pub fn with_price_book(mut self, prices: PriceBook) -> Self {
        self.prices = prices;
        self
    }

    /// Build the `/chat/completions` request body from a normalized request.
    ///
    /// Fallible since PRV-204: multimodal parts are only valid on user turns —
    /// anywhere else is a permanent [`Error::Invalid`] (no failover), same
    /// fail-closed contract as the PRV-202 constraints.
    fn request_body(request: &ChatRequest) -> Result<Value> {
        if let Some(bad) = request
            .messages
            .iter()
            .find(|m| !m.parts.is_empty() && m.role != Role::User)
        {
            return Err(Error::invalid(format!(
                "multimodal content parts are only supported on user messages (found on a {:?} turn)",
                bad.role
            )));
        }
        let messages: Vec<Value> = request.messages.iter().map(Self::encode_message).collect();
        let mut body = json!({ "model": request.model, "messages": messages });
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
                        // A strict tool gets the normalized schema subset +
                        // the vendor strict flag (PRV-203); a non-strict one
                        // is forwarded verbatim so real constraints
                        // (`minimum`, `format`, …) aren't discarded.
                        let mut function = json!({
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        });
                        if t.strict {
                            function["parameters"] = crate::schema::normalize_strict(&t.parameters);
                            function["strict"] = json!(true);
                        }
                        json!({ "type": "function", "function": function })
                    })
                    .collect(),
            );
        }
        // Tool-selection / output-shape constraints (RM-AIM-P2 PRV-202).
        if let Some(tc) = &request.tool_choice {
            body["tool_choice"] = match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Tool(name) => {
                    json!({ "type": "function", "function": { "name": name } })
                }
            };
        }
        if let Some(rf) = &request.response_format {
            body["response_format"] = match rf {
                ResponseFormat::JsonObject => json!({ "type": "json_object" }),
                ResponseFormat::JsonSchema { name, schema } => json!({
                    "type": "json_schema",
                    "json_schema": { "name": name, "schema": schema, "strict": true },
                }),
            };
        }
        Ok(body)
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
        // A message with multimodal parts (PRV-204) renders as a content-block
        // array instead: the `content` text (if any) first, then each part.
        obj["content"] = if msg.parts.is_empty() {
            match &msg.content {
                Some(c) => json!(c),
                None => Value::Null,
            }
        } else {
            let mut blocks: Vec<Value> = Vec::new();
            if let Some(text) = msg.content.as_deref().filter(|c| !c.is_empty()) {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            for part in &msg.parts {
                blocks.push(match part {
                    ContentPart::Text { text } => json!({ "type": "text", "text": text }),
                    ContentPart::ImageUrl { url } => {
                        json!({ "type": "image_url", "image_url": { "url": url } })
                    }
                    // Inline images ride as a data URI inside image_url.
                    ContentPart::Image { media_type, data } => json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{media_type};base64,{data}") },
                    }),
                    // OpenAI wants the bare format name (e.g. `wav`), not a MIME type.
                    ContentPart::Audio { media_type, data } => json!({
                        "type": "input_audio",
                        "input_audio": {
                            "data": data,
                            "format": media_type.strip_prefix("audio/").unwrap_or(media_type),
                        },
                    }),
                });
            }
            Value::Array(blocks)
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
        let body = Self::request_body(&request)?;
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
        let retry_after_ms = crate::resilience::parse_retry_after_ms(resp.headers());
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| Error::provider(format!("decoding response failed: {e}")))?;

        if !status.is_success() {
            let msg = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(classify_http_error(status.as_u16(), msg, retry_after_ms));
        }

        parse_response(&payload, &request.model, &self.prices)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        // Ask for an SSE stream and (where supported) usage in the final chunk.
        let mut body = Self::request_body(&request)?;
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });

        let url = format!("{}/chat/completions", self.base_url);
        let requested_model = request.model.clone();
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::provider(format!("request to {url} failed: {e}")))?;

        let status = resp.status();
        let retry_after_ms = crate::resilience::parse_retry_after_ms(resp.headers());
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(status.as_u16(), &text, retry_after_ms));
        }

        let prices = self.prices.clone();
        let mut bytes = resp.bytes_stream();
        let stream = async_stream::stream! {
            let mut buf = String::new();
            let mut acc = StreamAccumulator::new(requested_model, prices);

            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield Err(Error::provider(format!("stream read failed: {e}"))); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE events (delimited by a blank line).
                while let Some(pos) = buf.find("\n\n") {
                    let event: String = buf.drain(..pos + 2).collect();
                    for line in event.lines() {
                        let Some(data) = line.trim().strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data == "[DONE]" {
                            yield Ok(ChatStreamEvent::Done(acc.finish()));
                            return;
                        }
                        let Ok(json) = serde_json::from_str::<Value>(data) else { continue };
                        for event in acc.ingest(&json) {
                            yield Ok(event);
                        }
                    }
                }
            }
            // Stream ended without an explicit [DONE].
            yield Ok(ChatStreamEvent::Done(acc.finish()));
        };
        Ok(Box::pin(stream))
    }

    fn supports_embeddings(&self) -> bool {
        true
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
            // Not part of the retry loop (embed has no retry/failover pipeline),
            // so there's no Retry-After hint to thread through.
            return Err(classify_http_error(status.as_u16(), msg, None));
        }

        parse_embeddings(&payload, &request.model)
    }

    async fn generate_image(&self, request: ImageGenRequest) -> Result<ImageGenResponse> {
        let url = format!("{}/images/generations", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&json!({ "prompt": request.prompt, "size": request.size, "n": request.n }))
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
            // Not part of the retry loop (generate_image has no retry/failover
            // pipeline), so there's no Retry-After hint to thread through.
            return Err(classify_http_error(status.as_u16(), msg, None));
        }

        let images = payload
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(ImageGenResponse { images })
    }
}

/// Map an HTTP error status to the right error kind for the resilience layer:
/// 429 and 5xx are transient ([`Error::Provider`], retry/failover); other 4xx are
/// permanent client errors ([`Error::Invalid`])
/// ([resilience §8](../../docs/05-llm-gateway/resilience.md)). A parsed
/// `Retry-After` (RM-AIM-P2 PRV-205) rides along on the transient case so the
/// gateway's retry loop can honor it in place of its own backoff.
fn classify_http_error(status: u16, msg: &str, retry_after_ms: Option<u64>) -> Error {
    if status == 429 || status >= 500 {
        match retry_after_ms {
            Some(ms) => {
                Error::provider_with_retry_after(format!("provider returned {status}: {msg}"), ms)
            }
            None => Error::provider(format!("provider returned {status}: {msg}")),
        }
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

/// Accumulates streamed `/chat/completions` chunks into a final [`ChatResponse`].
/// Content arrives as `delta.content`; tool calls arrive as `delta.tool_calls` whose
/// `id`/`function.name`/`function.arguments` are filled incrementally, keyed by index.
struct StreamAccumulator {
    model: String,
    content: String,
    tool_calls: Vec<(String, String, String)>,
    finish_reason: String,
    prompt_tokens: u32,
    completion_tokens: u32,
    prices: PriceBook,
}

impl StreamAccumulator {
    fn new(model: String, prices: PriceBook) -> Self {
        Self {
            model,
            content: String::new(),
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            prices,
        }
    }

    /// Fold one chunk in; return the incremental events to surface to the caller
    /// (tool-call-argument fragments, reasoning, text — AIC-202), in wire order.
    fn ingest(&mut self, json: &Value) -> Vec<ChatStreamEvent> {
        if let Some(m) = json.get("model").and_then(Value::as_str) {
            self.model = m.to_string();
        }
        if let Some(pt) = json.pointer("/usage/prompt_tokens").and_then(Value::as_u64) {
            self.prompt_tokens = pt as u32;
        }
        if let Some(ct) = json
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
        {
            self.completion_tokens = ct as u32;
        }

        let Some(choice) = json.pointer("/choices/0") else {
            return Vec::new();
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = fr.to_string();
        }
        let delta = choice.get("delta");
        let mut events = Vec::new();

        if let Some(tcs) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for tc in tcs {
                let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                while self.tool_calls.len() <= idx {
                    self.tool_calls
                        .push((String::new(), String::new(), String::new()));
                }
                if let Some(id) = tc
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.tool_calls[idx].0 = id.to_string();
                }
                if let Some(n) = tc.pointer("/function/name").and_then(Value::as_str) {
                    self.tool_calls[idx].1.push_str(n);
                }
                let fragment = tc
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.tool_calls[idx].2.push_str(fragment);
                // Surface every chunk (incl. the fragment-less opener carrying
                // id/name) with the id/name accumulated so far, so a consumer can
                // announce the call before its arguments finish arriving.
                events.push(ChatStreamEvent::ToolCallDelta {
                    index: idx,
                    id: self.tool_calls[idx].0.clone(),
                    name: self.tool_calls[idx].1.clone(),
                    arguments: fragment.to_string(),
                });
            }
        }

        // A reasoning channel, where the (OpenAI-compatible) server exposes one —
        // e.g. DeepSeek-style `delta.reasoning_content`. Display-only: not
        // accumulated into the final message.
        if let Some(reasoning) = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        {
            events.push(ChatStreamEvent::ReasoningDelta(reasoning.to_string()));
        }

        if let Some(content) = delta
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        {
            self.content.push_str(content);
            events.push(ChatStreamEvent::Delta(content.to_string()));
        }
        events
    }

    /// Assemble the completed response from the accumulated state.
    fn finish(self) -> ChatResponse {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_iter()
            .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments: if arguments.is_empty() {
                    "{}".to_string()
                } else {
                    arguments
                },
            })
            .collect();
        let content = if self.content.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(self.content)
        };
        let usage = Usage::new(self.prompt_tokens, self.completion_tokens, 0.0);
        let cost_usd = self.prices.cost(&self.model, &usage);
        // Same "observe then enforce" debug line `parse_response` emits. PRV-101's
        // notes claimed both paths logged it, but only the non-streaming one did —
        // and streaming is the default for the CLI and the server's SSE route, so
        // the path an operator most needs to watch cost on was the silent one.
        tracing::debug!(target: "wovyr.pricing", model = %self.model,
            prompt_tokens = self.prompt_tokens, completion_tokens = self.completion_tokens,
            cost_usd, "computed llm call cost (streamed)");
        let usage = Usage::new(self.prompt_tokens, self.completion_tokens, cost_usd);
        ChatResponse {
            message: Message {
                role: Role::Assistant,
                content,
                parts: Vec::new(),
                tool_calls,
                tool_call_id: None,
                name: None,
            },
            model: self.model,
            usage,
            finish_reason: self.finish_reason,
        }
    }
}

/// Parse an OpenAI-shaped completion payload into a [`ChatResponse`].
fn parse_response(
    payload: &Value,
    requested_model: &str,
    prices: &PriceBook,
) -> Result<ChatResponse> {
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

    // Compute cost from the returned token usage against the price table, keyed by
    // the model the server actually billed (`model`), not the requested selector
    // (RM-AIM-P1 PRV-101). "Observe then enforce": log it so an operator can watch
    // real cost accrue for a release before quota enforcement bites.
    let usage = Usage::new(prompt_tokens, completion_tokens, 0.0);
    let cost_usd = prices.cost(&model, &usage);
    tracing::debug!(target: "wovyr.pricing", model = %model, prompt_tokens,
        completion_tokens, cost_usd, "computed llm call cost");
    let usage = Usage::new(prompt_tokens, completion_tokens, cost_usd);

    Ok(ChatResponse {
        message: Message {
            role: Role::Assistant,
            content,
            parts: Vec::new(),
            tool_calls,
            tool_call_id: None,
            name: None,
        },
        model,
        usage,
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
        let r = parse_response(&payload, "requested", &PriceBook::with_defaults()).unwrap();
        assert_eq!(r.message.content.as_deref(), Some("hi!"));
        assert_eq!(r.model, "gpt-4o-mini");
        assert_eq!(r.usage.total_tokens, 15);
        assert!(r.message.tool_calls.is_empty());
        // Cost is computed from the billed model's price, not left at 0.
        // gpt-4o-mini: 12*0.15/1e6 + 3*0.60/1e6.
        let expected = (12.0 * 0.15 + 3.0 * 0.60) / 1_000_000.0;
        assert!(
            (r.usage.cost_usd - expected).abs() < 1e-12,
            "got {}",
            r.usage.cost_usd
        );
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
        let r = parse_response(&payload, "m", &PriceBook::with_defaults()).unwrap();
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!(r.message.tool_calls.len(), 1);
        assert_eq!(r.message.tool_calls[0].name, "echo");
    }

    #[test]
    fn encodes_assistant_tool_call_with_null_content() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            parts: Vec::new(),
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
    fn multimodal_user_message_encodes_as_content_blocks() {
        let msg = Message::user("what is this?")
            .with_part(ContentPart::image_url("https://x/img.png"))
            .with_part(ContentPart::image_base64("image/png", "AAAA"))
            .with_part(ContentPart::audio_base64("audio/wav", "BBBB"));
        let v = OpenAiProvider::encode_message(&msg);
        let blocks = v["content"].as_array().unwrap();
        // `content` text leads, then the parts in order.
        assert_eq!(
            blocks[0],
            json!({ "type": "text", "text": "what is this?" })
        );
        assert_eq!(
            blocks[1],
            json!({ "type": "image_url", "image_url": { "url": "https://x/img.png" } })
        );
        // Inline image rides as a data URI.
        assert_eq!(
            blocks[2]["image_url"]["url"],
            json!("data:image/png;base64,AAAA")
        );
        // Audio format is the bare name, not the MIME type.
        assert_eq!(
            blocks[3],
            json!({ "type": "input_audio", "input_audio": { "data": "BBBB", "format": "wav" } })
        );
    }

    #[test]
    fn parts_on_a_non_user_turn_fail_closed() {
        let mut assistant = Message::assistant("look");
        assistant.parts = vec![ContentPart::image_url("https://x/img.png")];
        let req = ChatRequest::new("m", vec![Message::user("hi"), assistant]);
        let err = OpenAiProvider::request_body(&req).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "got {err:?}");
    }

    #[test]
    fn strict_tool_emits_strict_flag_and_normalized_schema() {
        use crate::types::ToolSpec;
        let mut req = ChatRequest::new("m", vec![Message::user("hi")]);
        req.tools = vec![ToolSpec {
            name: "calc".to_string(),
            description: "arithmetic".to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "n": { "type": "integer", "minimum": 0 } }
            }),
            strict: true,
        }];
        let body = OpenAiProvider::request_body(&req).unwrap();
        let function = &body["tools"][0]["function"];
        assert_eq!(function["strict"], json!(true));
        assert_eq!(
            function["parameters"]["properties"]["n"],
            json!({ "type": "integer" })
        );
        assert_eq!(function["parameters"]["additionalProperties"], json!(false));
        assert_eq!(function["parameters"]["required"], json!(["n"]));

        // Non-strict: schema forwarded verbatim, no strict flag.
        req.tools[0].strict = false;
        let body = OpenAiProvider::request_body(&req).unwrap();
        let function = &body["tools"][0]["function"];
        assert!(function.get("strict").is_none());
        assert_eq!(
            function["parameters"]["properties"]["n"]["minimum"],
            json!(0)
        );
    }

    #[test]
    fn encodes_tool_choice_variants() {
        let mut req = ChatRequest::new("m", vec![Message::user("hi")]);
        req.tool_choice = Some(ToolChoice::Auto);
        assert_eq!(
            OpenAiProvider::request_body(&req).unwrap()["tool_choice"],
            json!("auto")
        );
        req.tool_choice = Some(ToolChoice::None);
        assert_eq!(
            OpenAiProvider::request_body(&req).unwrap()["tool_choice"],
            json!("none")
        );
        req.tool_choice = Some(ToolChoice::Required);
        assert_eq!(
            OpenAiProvider::request_body(&req).unwrap()["tool_choice"],
            json!("required")
        );
        req.tool_choice = Some(ToolChoice::Tool("echo".to_string()));
        assert_eq!(
            OpenAiProvider::request_body(&req).unwrap()["tool_choice"],
            json!({ "type": "function", "function": { "name": "echo" } })
        );
    }

    #[test]
    fn encodes_response_format_variants() {
        let mut req = ChatRequest::new("m", vec![Message::user("hi")]);
        req.response_format = Some(ResponseFormat::JsonObject);
        assert_eq!(
            OpenAiProvider::request_body(&req).unwrap()["response_format"],
            json!({ "type": "json_object" })
        );
        req.response_format = Some(ResponseFormat::JsonSchema {
            name: "answer".to_string(),
            schema: json!({ "type": "object", "properties": { "n": { "type": "integer" } } }),
        });
        let body = OpenAiProvider::request_body(&req).unwrap();
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "answer");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert!(body["response_format"]["json_schema"]["schema"]["properties"]["n"].is_object());
    }

    #[test]
    fn unconstrained_request_omits_choice_and_format() {
        let body = OpenAiProvider::request_body(&ChatRequest::new("m", vec![Message::user("hi")]))
            .unwrap();
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("response_format").is_none());
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

    /// Extract the text of a lone `Delta` event (test helper).
    fn text_delta(events: &[ChatStreamEvent]) -> Option<String> {
        match events {
            [ChatStreamEvent::Delta(t)] => Some(t.clone()),
            [] => None,
            other => panic!(
                "expected at most one text delta, got {} events",
                other.len()
            ),
        }
    }

    #[test]
    fn accumulates_streamed_text_and_usage() {
        let mut acc = StreamAccumulator::new("requested".to_string(), PriceBook::with_defaults());
        // Role-only opening chunk carries no content delta.
        assert_eq!(
            text_delta(&acc.ingest(&json!({
                "model": "gpt-4o-mini",
                "choices": [{ "delta": { "role": "assistant" } }]
            }))),
            None
        );
        assert_eq!(
            text_delta(&acc.ingest(&json!({ "choices": [{ "delta": { "content": "Hel" } }] }))),
            Some("Hel".to_string())
        );
        assert_eq!(
            text_delta(&acc.ingest(&json!({ "choices": [{ "delta": { "content": "lo" } }] }))),
            Some("lo".to_string())
        );
        // Terminal chunk: finish reason + a usage-only frame.
        acc.ingest(&json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }));
        acc.ingest(&json!({
            "choices": [],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
        }));

        let r = acc.finish();
        assert_eq!(r.message.content.as_deref(), Some("Hello"));
        assert_eq!(r.model, "gpt-4o-mini");
        assert_eq!(r.finish_reason, "stop");
        assert_eq!(r.usage.prompt_tokens, 5);
        assert_eq!(r.usage.total_tokens, 7);
        assert!(r.message.tool_calls.is_empty());
    }

    #[test]
    fn accumulates_streamed_tool_call_across_chunks() {
        let mut acc = StreamAccumulator::new("m".to_string(), PriceBook::with_defaults());
        // id + name arrive first, arguments stream in fragments keyed by index.
        let first = acc.ingest(&json!({
            "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "call_1",
                "function": { "name": "echo", "arguments": "{\"x\"" }
            }] } }]
        }));
        let second = acc.ingest(&json!({
            "choices": [{ "delta": { "tool_calls": [{
                "index": 0,
                "function": { "arguments": ":1}" }
            }] }, "finish_reason": "tool_calls" }]
        }));

        // Each chunk surfaces a ToolCallDelta fragment (AIC-202), carrying the
        // id/name accumulated so far even when the wire chunk omitted them.
        match (&first[..], &second[..]) {
            (
                [
                    ChatStreamEvent::ToolCallDelta {
                        index: 0,
                        id: id1,
                        name: name1,
                        arguments: args1,
                    },
                ],
                [
                    ChatStreamEvent::ToolCallDelta {
                        index: 0,
                        id: id2,
                        name: name2,
                        arguments: args2,
                    },
                ],
            ) => {
                assert_eq!(
                    (id1.as_str(), name1.as_str(), args1.as_str()),
                    ("call_1", "echo", "{\"x\"")
                );
                assert_eq!(
                    (id2.as_str(), name2.as_str(), args2.as_str()),
                    ("call_1", "echo", ":1}")
                );
            }
            _ => panic!("expected one ToolCallDelta per chunk"),
        }

        let r = acc.finish();
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!(r.message.tool_calls.len(), 1);
        assert_eq!(r.message.tool_calls[0].id, "call_1");
        assert_eq!(r.message.tool_calls[0].name, "echo");
        assert_eq!(r.message.tool_calls[0].arguments, "{\"x\":1}");
        // A tool-call-only response carries no text content.
        assert!(r.message.content.is_none());
    }

    #[test]
    fn reasoning_content_surfaces_as_a_reasoning_delta() {
        let mut acc = StreamAccumulator::new("m".to_string(), PriceBook::with_defaults());
        let events = acc.ingest(&json!({
            "choices": [{ "delta": { "reasoning_content": "thinking about it" } }]
        }));
        assert!(
            matches!(&events[..], [ChatStreamEvent::ReasoningDelta(t)] if t == "thinking about it"),
            "expected a lone ReasoningDelta"
        );
        // Reasoning is display-only: it never lands in the final message content.
        assert_eq!(acc.finish().message.content.as_deref(), Some(""));
    }

    #[test]
    fn empty_tool_call_arguments_default_to_object() {
        let mut acc = StreamAccumulator::new("m".to_string(), PriceBook::with_defaults());
        acc.ingest(&json!({
            "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "call_1",
                "function": { "name": "noop" }
            }] } }]
        }));
        let r = acc.finish();
        assert_eq!(r.message.tool_calls[0].arguments, "{}");
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
