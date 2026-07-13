//! First-class Anthropic (Claude) chat provider.
//!
//! Targets the native [Messages API](https://docs.claude.com/en/api/messages)
//! (`POST /v1/messages`) rather than an OpenAI-compatible shim, so tool use,
//! system-prompt handling, and prompt caching all use Anthropic's own wire
//! shapes (RM-AIM-P2 PRV-201). The base URL and key come from the environment,
//! keeping credentials out of code
//! ([coding standards §9](../../docs/19-implementation-guide/coding-standards.md)).
//!
//! Translation notes (normalized types ↔ Anthropic shapes):
//! - `Role::System` messages become the top-level `system` block list — the
//!   Messages API has no `system` chat role.
//! - Assistant `tool_calls` become `tool_use` content blocks (`arguments` is a
//!   JSON *string* on our side, a JSON *object* — `input` — on the wire).
//! - `Role::Tool` results become `tool_result` blocks in a `user` turn;
//!   consecutive tool results are merged into one turn, since Anthropic
//!   requires all results for a parallel tool call in a single user message.
//! - `max_tokens` is **required** by the API, so an unset request gets
//!   [`DEFAULT_MAX_TOKENS`].
//! - `stop_reason` is normalized toward the vendor-neutral vocabulary the rest
//!   of the platform already uses: `end_turn` → `stop`, `tool_use` →
//!   `tool_calls`; anything else (`max_tokens`, `refusal`, …) passes through.
//!
//! **Prompt caching** is on by default: a `cache_control: {type: "ephemeral"}`
//! breakpoint is placed on the last tool and the last system block — the
//! stable prefix of an agent loop — so step 2..n of a tool loop reads the
//! cached prefix instead of re-paying for it. Cache reads/writes are billed at
//! 0.1×/1.25× the input rate, which the cost computation accounts for.

use crate::pricing::PriceBook;
use crate::provider::{AIProvider, ChatStream, ChatStreamEvent};
use crate::types::{ChatRequest, ChatResponse, Message, Role, ToolCall};
use apex_common::{Error, Result, Usage};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

/// Default Anthropic API base URL.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Messages API version header value (the stable version identifier).
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// `max_tokens` is a required field on the Messages API; used when the
/// normalized request doesn't specify one.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// An adapter for Anthropic's native Messages API.
pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Per-model price table used to compute `cost_usd` from returned token usage.
    prices: PriceBook,
    /// Place `cache_control` breakpoints on the stable prefix (tools + system).
    prompt_caching: bool,
}

impl AnthropicProvider {
    /// Build a provider from explicit configuration.
    ///
    /// Uses the operator-overridable price table ([`PriceBook::from_env`]) so a run
    /// through this provider records real cost regardless of which constructor the
    /// caller used; swap it with [`AnthropicProvider::with_price_book`].
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            prices: PriceBook::from_env(),
            prompt_caching: true,
        }
    }

    /// Build from the environment.
    ///
    /// Reads `ANTHROPIC_API_KEY` (required) and `APEX_ANTHROPIC_BASE_URL`
    /// (optional, defaults to the public endpoint). Returns [`Error::Config`]
    /// when no key is set so the caller can fall back to another provider.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| Error::config("ANTHROPIC_API_KEY is not set"))?;
        let base_url = std::env::var("APEX_ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self::new(base_url, api_key))
    }

    /// Override the price table (builder-style).
    pub fn with_price_book(mut self, prices: PriceBook) -> Self {
        self.prices = prices;
        self
    }

    /// Enable/disable prompt-caching breakpoints (builder-style; default on).
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.prompt_caching = enabled;
        self
    }

    /// Build the `/v1/messages` request body from a normalized request.
    fn request_body(&self, request: &ChatRequest) -> Value {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": encode_messages(&request.messages),
        });
        if let Some(t) = request.temperature {
            body["temperature"] = json!(t);
        }

        let system = system_blocks(&request.messages, self.prompt_caching);
        if !system.is_empty() {
            body["system"] = Value::Array(system);
        }

        if !request.tools.is_empty() {
            let mut tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect();
            if self.prompt_caching
                && let Some(last) = tools.last_mut()
            {
                last["cache_control"] = json!({ "type": "ephemeral" });
            }
            body["tools"] = Value::Array(tools);
        }
        body
    }

    /// POST `body` to `/v1/messages`, mapping HTTP errors to the platform's
    /// transient/permanent error kinds.
    async fn post_messages(&self, body: &Value) -> Result<reqwest::Response> {
        let url = format!("{}/v1/messages", self.base_url);
        self.client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::provider(format!("request to {url} failed: {e}")))
    }
}

#[async_trait]
impl AIProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.request_body(&request);
        let resp = self.post_messages(&body).await?;

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

        parse_response(&payload, &request.model, &self.prices)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let mut body = self.request_body(&request);
        body["stream"] = json!(true);

        let requested_model = request.model.clone();
        let resp = self.post_messages(&body).await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(status.as_u16(), &text));
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
                        let Ok(json) = serde_json::from_str::<Value>(data.trim()) else { continue };
                        match acc.ingest(&json) {
                            Ingested::Delta(text) => yield Ok(ChatStreamEvent::Delta(text)),
                            Ingested::Done => {
                                yield Ok(ChatStreamEvent::Done(acc.finish()));
                                return;
                            }
                            Ingested::Error(msg) => {
                                yield Err(Error::provider(format!("stream error event: {msg}")));
                                return;
                            }
                            Ingested::Continue => {}
                        }
                    }
                }
            }
            // Stream ended without an explicit message_stop.
            yield Ok(ChatStreamEvent::Done(acc.finish()));
        };
        Ok(Box::pin(stream))
    }
}

/// Collect `Role::System` messages into the top-level `system` block list,
/// with a cache breakpoint on the last block when prompt caching is on.
fn system_blocks(messages: &[Message], prompt_caching: bool) -> Vec<Value> {
    let mut blocks: Vec<Value> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(|m| m.content.as_deref())
        .filter(|c| !c.is_empty())
        .map(|c| json!({ "type": "text", "text": c }))
        .collect();
    if prompt_caching && let Some(last) = blocks.last_mut() {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }
    blocks
}

/// Translate the non-system conversation into Anthropic `messages`, merging
/// consecutive tool results into a single `user` turn.
fn encode_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {} // hoisted into the top-level `system` field
            Role::User => {
                out.push(json!({
                    "role": "user",
                    "content": msg.content.clone().unwrap_or_default(),
                }));
            }
            Role::Assistant => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = msg.content.as_deref().filter(|c| !c.is_empty()) {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for tc in &msg.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": parse_arguments(&tc.arguments),
                    }));
                }
                out.push(json!({ "role": "assistant", "content": blocks }));
            }
            Role::Tool => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": msg.content.clone().unwrap_or_default(),
                });
                // All results for one parallel tool call must share one user turn.
                match out.last_mut() {
                    Some(last)
                        if last["role"] == "user"
                            && last["content"].as_array().is_some_and(|blocks| {
                                blocks.iter().all(|b| b["type"] == "tool_result")
                            }) =>
                    {
                        last["content"]
                            .as_array_mut()
                            .expect("checked above")
                            .push(block);
                    }
                    _ => out.push(json!({ "role": "user", "content": [block] })),
                }
            }
        }
    }
    out
}

/// Parse a tool call's JSON-string arguments into the object the wire wants;
/// malformed/empty arguments degrade to `{}` rather than failing the request.
fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

/// Map an HTTP error status to the right error kind for the resilience layer:
/// 429 and 5xx (incl. Anthropic's 529 `overloaded_error`) are transient
/// ([`Error::Provider`], retry/failover); other 4xx are permanent client errors
/// ([`Error::Invalid`]) ([resilience §8](../../docs/05-llm-gateway/resilience.md)).
fn classify_http_error(status: u16, msg: &str) -> Error {
    if status == 429 || status >= 500 {
        Error::provider(format!("provider returned {status}: {msg}"))
    } else {
        Error::invalid(format!("provider returned {status}: {msg}"))
    }
}

/// Normalize Anthropic's `stop_reason` toward the vocabulary the rest of the
/// platform uses; unmapped reasons (`max_tokens`, `refusal`, …) pass through.
fn normalize_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" => "stop".to_string(),
        "tool_use" => "tool_calls".to_string(),
        other => other.to_string(),
    }
}

/// Compute `cost_usd` from the price table, weighting cached prompt tokens at
/// their real billing rates (cache writes 1.25×, cache reads 0.1× the input
/// price). Falls back to [`PriceBook::cost`]'s unweighted path (and its
/// one-time unknown-model warn) when the model has no price entry.
fn compute_cost(
    prices: &PriceBook,
    model: &str,
    input: u32,
    output: u32,
    cache_creation: u32,
    cache_read: u32,
) -> f64 {
    match prices.price(model) {
        Some(p) => {
            let weighted_input =
                input as f64 + 1.25 * cache_creation as f64 + 0.1 * cache_read as f64;
            (weighted_input * p.input_per_1m + output as f64 * p.output_per_1m) / 1_000_000.0
        }
        None => prices.cost(
            model,
            &Usage::new(input + cache_creation + cache_read, output, 0.0),
        ),
    }
}

/// Parse a Messages API response payload into a [`ChatResponse`].
fn parse_response(
    payload: &Value,
    requested_model: &str,
    prices: &PriceBook,
) -> Result<ChatResponse> {
    let content = payload
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::provider("response had no content array"))?;

    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arguments: block.get("input").cloned().unwrap_or(json!({})).to_string(),
            }),
            // thinking / server-side blocks carry nothing the loop consumes.
            _ => {}
        }
    }

    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(requested_model)
        .to_string();

    let finish_reason = payload
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(normalize_stop_reason)
        .unwrap_or_else(|| "stop".to_string());

    let token = |ptr: &str| payload.pointer(ptr).and_then(Value::as_u64).unwrap_or(0) as u32;
    let input = token("/usage/input_tokens");
    let output = token("/usage/output_tokens");
    let cache_creation = token("/usage/cache_creation_input_tokens");
    let cache_read = token("/usage/cache_read_input_tokens");

    // `input_tokens` is the *uncached remainder* — the real prompt size is the
    // sum of all three input categories (PRV-101: report true counts, price
    // each category at its own rate).
    let cost_usd = compute_cost(prices, &model, input, output, cache_creation, cache_read);
    tracing::debug!(target: "apex.pricing", model = %model, input, output,
        cache_creation, cache_read, cost_usd, "computed llm call cost");
    let usage = Usage::new(input + cache_creation + cache_read, output, cost_usd);

    let content = if text.is_empty() && !tool_calls.is_empty() {
        None
    } else {
        Some(text)
    };
    Ok(ChatResponse {
        message: Message {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
        },
        model,
        usage,
        finish_reason,
    })
}

/// What one ingested SSE event asks the stream driver to do.
enum Ingested {
    /// Surface a text delta to the caller.
    Delta(String),
    /// `message_stop` arrived — finish and end the stream.
    Done,
    /// The server sent an `error` event.
    Error(String),
    /// Bookkeeping only; keep reading.
    Continue,
}

/// Accumulates Messages API stream events into a final [`ChatResponse`].
/// Text arrives as `text_delta`s; tool calls arrive as a `tool_use`
/// `content_block_start` (id + name) followed by `input_json_delta` fragments,
/// keyed by block index.
struct StreamAccumulator {
    model: String,
    content: String,
    /// Block index → in-progress tool call (id, name, partial input JSON).
    tool_blocks: Vec<(u64, String, String, String)>,
    stop_reason: String,
    input_tokens: u32,
    output_tokens: u32,
    cache_creation: u32,
    cache_read: u32,
    prices: PriceBook,
}

impl StreamAccumulator {
    fn new(model: String, prices: PriceBook) -> Self {
        Self {
            model,
            content: String::new(),
            tool_blocks: Vec::new(),
            stop_reason: "stop".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation: 0,
            cache_read: 0,
            prices,
        }
    }

    /// Fold one event in, telling the driver what (if anything) to surface.
    fn ingest(&mut self, json: &Value) -> Ingested {
        match json.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(m) = json.pointer("/message/model").and_then(Value::as_str) {
                    self.model = m.to_string();
                }
                let token =
                    |ptr: &str| json.pointer(ptr).and_then(Value::as_u64).unwrap_or(0) as u32;
                self.input_tokens = token("/message/usage/input_tokens");
                self.cache_creation = token("/message/usage/cache_creation_input_tokens");
                self.cache_read = token("/message/usage/cache_read_input_tokens");
                Ingested::Continue
            }
            Some("content_block_start") => {
                if json.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                    let index = json.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let id = json
                        .pointer("/content_block/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = json
                        .pointer("/content_block/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.tool_blocks.push((index, id, name, String::new()));
                }
                Ingested::Continue
            }
            Some("content_block_delta") => {
                match json.pointer("/delta/type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = json
                            .pointer("/delta/text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if text.is_empty() {
                            return Ingested::Continue;
                        }
                        self.content.push_str(text);
                        Ingested::Delta(text.to_string())
                    }
                    Some("input_json_delta") => {
                        let index = json.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(partial) =
                            json.pointer("/delta/partial_json").and_then(Value::as_str)
                            && let Some(block) = self.tool_blocks.iter_mut().find(|b| b.0 == index)
                        {
                            block.3.push_str(partial);
                        }
                        Ingested::Continue
                    }
                    _ => Ingested::Continue, // thinking_delta etc.
                }
            }
            Some("message_delta") => {
                if let Some(sr) = json.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = normalize_stop_reason(sr);
                }
                if let Some(out) = json.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.output_tokens = out as u32;
                }
                Ingested::Continue
            }
            Some("message_stop") => Ingested::Done,
            Some("error") => Ingested::Error(
                json.pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            ),
            _ => Ingested::Continue, // ping etc.
        }
    }

    /// Assemble the completed response from the accumulated state.
    fn finish(&self) -> ChatResponse {
        let tool_calls: Vec<ToolCall> = self
            .tool_blocks
            .iter()
            .map(|(_, id, name, partial)| ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: if partial.is_empty() {
                    "{}".to_string()
                } else {
                    partial.clone()
                },
            })
            .collect();
        let cost_usd = compute_cost(
            &self.prices,
            &self.model,
            self.input_tokens,
            self.output_tokens,
            self.cache_creation,
            self.cache_read,
        );
        let usage = Usage::new(
            self.input_tokens + self.cache_creation + self.cache_read,
            self.output_tokens,
            cost_usd,
        );
        let content = if self.content.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(self.content.clone())
        };
        ChatResponse {
            message: Message {
                role: Role::Assistant,
                content,
                tool_calls,
                tool_call_id: None,
                name: None,
            },
            model: self.model.clone(),
            usage,
            finish_reason: self.stop_reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSpec;

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new(DEFAULT_BASE_URL, "test-key")
            .with_price_book(PriceBook::with_defaults())
    }

    fn request_with_tools() -> ChatRequest {
        let mut req = ChatRequest::new(
            "claude-opus-4-8",
            vec![Message::system("be terse"), Message::user("what's 2+2?")],
        );
        req.tools = vec![ToolSpec {
            name: "calc".to_string(),
            description: "arithmetic".to_string(),
            parameters: json!({ "type": "object", "properties": { "expr": { "type": "string" } } }),
        }];
        req
    }

    #[test]
    fn system_messages_hoist_to_top_level_with_cache_breakpoint() {
        let body = provider().request_body(&request_with_tools());
        assert_eq!(body["system"][0]["type"], "text");
        assert_eq!(body["system"][0]["text"], "be terse");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        // No system-role message leaks into `messages`.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn tools_use_input_schema_and_last_gets_cache_breakpoint() {
        let body = provider().request_body(&request_with_tools());
        assert_eq!(body["tools"][0]["name"], "calc");
        assert!(body["tools"][0]["input_schema"]["properties"]["expr"].is_object());
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn prompt_caching_off_omits_cache_control() {
        let body = provider()
            .with_prompt_caching(false)
            .request_body(&request_with_tools());
        assert!(body["system"][0].get("cache_control").is_none());
        assert!(body["tools"][0].get("cache_control").is_none());
    }

    #[test]
    fn max_tokens_is_always_present() {
        // Required by the Messages API — default when the request leaves it unset.
        let body = provider().request_body(&ChatRequest::new("m", vec![Message::user("hi")]));
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);

        let mut req = ChatRequest::new("m", vec![Message::user("hi")]);
        req.max_tokens = Some(99);
        assert_eq!(provider().request_body(&req)["max_tokens"], 99);
    }

    #[test]
    fn assistant_tool_calls_encode_as_tool_use_blocks_with_parsed_input() {
        let assistant = Message {
            role: Role::Assistant,
            content: Some("checking".to_string()),
            tool_calls: vec![ToolCall {
                id: "toolu_1".to_string(),
                name: "calc".to_string(),
                arguments: "{\"expr\":\"2+2\"}".to_string(),
            }],
            tool_call_id: None,
            name: None,
        };
        let msgs = encode_messages(&[Message::user("q"), assistant]);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "text");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][1]["id"], "toolu_1");
        // `arguments` (JSON string) became `input` (JSON object).
        assert_eq!(msgs[1]["content"][1]["input"]["expr"], "2+2");
    }

    #[test]
    fn consecutive_tool_results_merge_into_one_user_turn() {
        let assistant = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![
                ToolCall {
                    id: "a".into(),
                    name: "t1".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "b".into(),
                    name: "t2".into(),
                    arguments: "{}".into(),
                },
            ],
            tool_call_id: None,
            name: None,
        };
        let msgs = encode_messages(&[
            Message::user("q"),
            assistant,
            Message::tool_result("a", "t1", "r1"),
            Message::tool_result("b", "t2", "r2"),
        ]);
        // user, assistant, then ONE user turn holding both tool_result blocks.
        assert_eq!(msgs.len(), 3);
        let results = msgs[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "a");
        assert_eq!(results[1]["tool_use_id"], "b");
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_empty_object() {
        assert_eq!(parse_arguments("not json"), json!({}));
        assert_eq!(parse_arguments(""), json!({}));
        assert_eq!(parse_arguments("{\"x\":1}"), json!({"x":1}));
    }

    #[test]
    fn parses_text_completion_with_cost_from_price_table() {
        let payload = json!({
            "model": "claude-opus-4-8",
            "content": [ { "type": "text", "text": "4" } ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 100, "output_tokens": 10 }
        });
        let r = parse_response(&payload, "requested", &PriceBook::with_defaults()).unwrap();
        assert_eq!(r.message.content.as_deref(), Some("4"));
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.finish_reason, "stop");
        assert_eq!(r.usage.total_tokens, 110);
        // claude-opus-4-8: $5/1M in, $25/1M out.
        let expected = (100.0 * 5.0 + 10.0 * 25.0) / 1_000_000.0;
        assert!(
            (r.usage.cost_usd - expected).abs() < 1e-12,
            "got {}",
            r.usage.cost_usd
        );
    }

    #[test]
    fn parses_tool_use_blocks_and_normalizes_stop_reason() {
        let payload = json!({
            "model": "claude-opus-4-8",
            "content": [
                { "type": "text", "text": "Let me check." },
                { "type": "tool_use", "id": "toolu_1", "name": "calc", "input": { "expr": "2+2" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 5, "output_tokens": 3 }
        });
        let r = parse_response(&payload, "m", &PriceBook::with_defaults()).unwrap();
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!(r.message.tool_calls.len(), 1);
        assert_eq!(r.message.tool_calls[0].id, "toolu_1");
        assert_eq!(r.message.tool_calls[0].name, "calc");
        // `input` object round-trips back into a JSON-string `arguments`.
        let args: Value = serde_json::from_str(&r.message.tool_calls[0].arguments).unwrap();
        assert_eq!(args["expr"], "2+2");
    }

    #[test]
    fn cache_tokens_are_counted_and_priced_at_their_real_rates() {
        let payload = json!({
            "model": "claude-opus-4-8",
            "content": [ { "type": "text", "text": "hi" } ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 10,
                "cache_creation_input_tokens": 1000,
                "cache_read_input_tokens": 2000
            }
        });
        let r = parse_response(&payload, "m", &PriceBook::with_defaults()).unwrap();
        // The real prompt size is the sum of all three input categories.
        assert_eq!(r.usage.prompt_tokens, 3100);
        // Writes bill at 1.25x input, reads at 0.1x input.
        let expected = ((100.0 + 1.25 * 1000.0 + 0.1 * 2000.0) * 5.0 + 10.0 * 25.0) / 1_000_000.0;
        assert!(
            (r.usage.cost_usd - expected).abs() < 1e-12,
            "got {}",
            r.usage.cost_usd
        );
    }

    #[test]
    fn unmapped_stop_reasons_pass_through() {
        assert_eq!(normalize_stop_reason("max_tokens"), "max_tokens");
        assert_eq!(normalize_stop_reason("refusal"), "refusal");
    }

    #[test]
    fn accumulates_streamed_text_and_usage() {
        let mut acc = StreamAccumulator::new("requested".into(), PriceBook::with_defaults());
        acc.ingest(&json!({
            "type": "message_start",
            "message": { "model": "claude-opus-4-8",
                "usage": { "input_tokens": 7, "cache_read_input_tokens": 3 } }
        }));
        acc.ingest(&json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" } }));
        assert!(matches!(
            acc.ingest(&json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "Hel" } })),
            Ingested::Delta(t) if t == "Hel"
        ));
        assert!(matches!(
            acc.ingest(&json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "lo" } })),
            Ingested::Delta(t) if t == "lo"
        ));
        acc.ingest(&json!({ "type": "message_delta",
            "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 2 } }));
        assert!(matches!(
            acc.ingest(&json!({ "type": "message_stop" })),
            Ingested::Done
        ));

        let r = acc.finish();
        assert_eq!(r.message.content.as_deref(), Some("Hello"));
        assert_eq!(r.model, "claude-opus-4-8");
        assert_eq!(r.finish_reason, "stop");
        assert_eq!(r.usage.prompt_tokens, 10); // 7 uncached + 3 cache-read
        assert_eq!(r.usage.completion_tokens, 2);
        assert!(r.usage.cost_usd > 0.0);
    }

    #[test]
    fn accumulates_streamed_tool_use_across_json_deltas() {
        let mut acc = StreamAccumulator::new("m".into(), PriceBook::with_defaults());
        acc.ingest(&json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "calc", "input": {} } }));
        acc.ingest(&json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"expr\"" } }));
        acc.ingest(&json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": ":\"2+2\"}" } }));
        acc.ingest(&json!({ "type": "message_delta",
            "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 9 } }));

        let r = acc.finish();
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!(r.message.tool_calls.len(), 1);
        assert_eq!(r.message.tool_calls[0].id, "toolu_1");
        assert_eq!(r.message.tool_calls[0].name, "calc");
        assert_eq!(r.message.tool_calls[0].arguments, "{\"expr\":\"2+2\"}");
        assert!(
            r.message.content.is_none(),
            "tool-only response carries no text"
        );
    }

    #[test]
    fn empty_tool_use_input_defaults_to_object() {
        let mut acc = StreamAccumulator::new("m".into(), PriceBook::with_defaults());
        acc.ingest(&json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "noop", "input": {} } }));
        assert_eq!(acc.finish().message.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn error_event_surfaces_as_stream_error() {
        let mut acc = StreamAccumulator::new("m".into(), PriceBook::with_defaults());
        assert!(matches!(
            acc.ingest(&json!({ "type": "error",
                "error": { "type": "overloaded_error", "message": "busy" } })),
            Ingested::Error(m) if m == "busy"
        ));
    }
}
