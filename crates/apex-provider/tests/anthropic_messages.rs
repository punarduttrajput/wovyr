//! End-to-end tests for the Anthropic provider, driving the real HTTP +
//! Messages-API translation path against a canned local server (no network) —
//! the recorded-fixture acceptance test for RM-AIM-P2 PRV-201.
//!
//! Covers the full tool round-trip (model requests a tool → the caller feeds
//! the result back → the model answers), asserting both directions of the wire
//! translation: what the provider *sends* (system hoisting, `tool_result`
//! blocks, cache breakpoints) and what it *parses* (tool_use blocks, usage,
//! PRV-101 cost). Streaming is covered against a canned SSE body. The forced
//! tool-choice and JSON-schema-constrained tests are the recorded-fixture
//! acceptance tests for RM-AIM-P2 PRV-202.

use apex_provider::{
    AIProvider, AnthropicProvider, ChatRequest, ChatStreamEvent, Message, PriceBook,
    ResponseFormat, ToolChoice, ToolSpec,
};
use futures::StreamExt;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

/// Spawn a one-shot-per-response HTTP/1.1 server that answers successive
/// connections with `responses` (in order), sending each captured request body
/// through the returned channel. Returns the base URL.
fn serve(responses: Vec<(&'static str, String)>) -> (String, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (content_type, body) in responses {
            let (mut sock, _) = listener.accept().unwrap();
            // Read the request until the Content-Length body is complete.
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let request_body = loop {
                let n = sock.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let content_length = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    let body_so_far = raw.len() - (header_end + 4);
                    if body_so_far >= content_length {
                        break String::from_utf8_lossy(&raw[header_end + 4..]).to_string();
                    }
                }
            };
            tx.send(serde_json::from_str::<Value>(&request_body).unwrap())
                .unwrap();

            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                content_type,
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).unwrap();
            sock.flush().unwrap();
        }
    });
    (format!("http://{addr}"), rx)
}

fn calc_tool() -> ToolSpec {
    ToolSpec {
        name: "calc".to_string(),
        description: "Evaluate an arithmetic expression".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "expr": { "type": "string" } },
            "required": ["expr"]
        }),
        strict: false,
    }
}

/// The acceptance-criteria test: a full model → tool → model round-trip
/// through the Anthropic provider against recorded fixtures.
#[tokio::test]
async fn tool_round_trip_via_recorded_fixtures() {
    // Fixture 1: the model asks for the `calc` tool.
    let turn1 = json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [
            { "type": "text", "text": "Let me compute that." },
            { "type": "tool_use", "id": "toolu_abc", "name": "calc",
              "input": { "expr": "2+2" } }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 120, "output_tokens": 30 }
    });
    // Fixture 2: with the tool result fed back, the model answers.
    let turn2 = json!({
        "id": "msg_2", "type": "message", "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [ { "type": "text", "text": "2+2 = 4." } ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 160, "output_tokens": 8,
                   "cache_read_input_tokens": 100 }
    });
    let (base_url, requests) = serve(vec![
        ("application/json", turn1.to_string()),
        ("application/json", turn2.to_string()),
    ]);
    let provider =
        AnthropicProvider::new(base_url, "test-key").with_price_book(PriceBook::with_defaults());

    // Turn 1: system + user + tool.
    let mut req = ChatRequest::new(
        "claude-opus-4-8",
        vec![
            Message::system("You are a calculator."),
            Message::user("What is 2+2?"),
        ],
    );
    req.tools = vec![calc_tool()];

    let first = provider.chat(req.clone()).await.unwrap();
    assert_eq!(first.finish_reason, "tool_calls");
    assert_eq!(first.message.tool_calls.len(), 1);
    let call = &first.message.tool_calls[0];
    assert_eq!(call.name, "calc");
    let args: Value = serde_json::from_str(&call.arguments).unwrap();
    assert_eq!(args["expr"], "2+2");

    // The wire request hoisted the system message, declared the tool with an
    // input_schema, and placed cache breakpoints on the stable prefix.
    let sent1 = requests.recv().unwrap();
    assert_eq!(sent1["system"][0]["text"], "You are a calculator.");
    assert_eq!(sent1["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(sent1["tools"][0]["name"], "calc");
    assert!(sent1["tools"][0]["input_schema"]["properties"]["expr"].is_object());
    assert_eq!(sent1["tools"][0]["cache_control"]["type"], "ephemeral");
    assert!(
        sent1["max_tokens"].as_u64().unwrap() > 0,
        "max_tokens is required"
    );
    assert_eq!(sent1["messages"][0]["role"], "user");

    // Turn 2: append the assistant turn + the executed tool's result — the
    // exact shape apex_agent::run_agent feeds back.
    req.messages.push(first.message.clone());
    req.messages
        .push(Message::tool_result(&call.id, &call.name, "4"));

    let second = provider.chat(req).await.unwrap();
    assert_eq!(second.finish_reason, "stop");
    assert_eq!(second.message.content.as_deref(), Some("2+2 = 4."));
    // PRV-101: cost comes from the price table, with the cache-read tokens in
    // the prompt count and billed at 0.1x the input rate.
    assert_eq!(second.usage.prompt_tokens, 260);
    let expected = ((160.0 + 0.1 * 100.0) * 5.0 + 8.0 * 25.0) / 1_000_000.0;
    assert!(
        (second.usage.cost_usd - expected).abs() < 1e-12,
        "got {}",
        second.usage.cost_usd
    );

    // The wire request carried the assistant tool_use turn and the tool_result
    // in a user turn, correlated by tool_use_id.
    let sent2 = requests.recv().unwrap();
    let messages = sent2["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "user, assistant, tool-result user turn");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert_eq!(messages[1]["content"][1]["id"], "toolu_abc");
    assert_eq!(messages[1]["content"][1]["input"]["expr"], "2+2");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_abc");
    assert_eq!(messages[2]["content"][0]["content"], "4");
}

#[tokio::test]
async fn streams_text_deltas_then_done() {
    let body = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":2}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

    let (base_url, requests) = serve(vec![("text/event-stream", body.to_string())]);
    let provider =
        AnthropicProvider::new(base_url, "test-key").with_price_book(PriceBook::with_defaults());
    let req = ChatRequest::new("claude-opus-4-8", vec![Message::user("hi")]);
    let mut stream = provider.chat_stream(req).await.unwrap();

    let mut deltas = Vec::new();
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ChatStreamEvent::Delta(d) => deltas.push(d),
            ChatStreamEvent::Done(r) => done = Some(r),
        }
    }

    assert!(requests.recv().unwrap()["stream"].as_bool().unwrap());
    assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
    let done = done.expect("stream must end with Done");
    assert_eq!(done.message.content.as_deref(), Some("Hello"));
    assert_eq!(done.model, "claude-opus-4-8");
    assert_eq!(done.finish_reason, "stop");
    assert_eq!(done.usage.prompt_tokens, 7); // 5 uncached + 2 cache-read
    assert_eq!(done.usage.completion_tokens, 2);
    assert!(done.usage.cost_usd > 0.0);
}

#[tokio::test]
async fn streams_tool_use_assembled_from_partial_json() {
    let body = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":9}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"calc\",\"input\":{}}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"expr\\\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":\\\"2+2\\\"}\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":11}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

    let (base_url, _requests) = serve(vec![("text/event-stream", body.to_string())]);
    let provider =
        AnthropicProvider::new(base_url, "test-key").with_price_book(PriceBook::with_defaults());
    let mut req = ChatRequest::new("claude-opus-4-8", vec![Message::user("2+2?")]);
    req.tools = vec![calc_tool()];
    let mut stream = provider.chat_stream(req).await.unwrap();

    let mut deltas = Vec::new();
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ChatStreamEvent::Delta(d) => deltas.push(d),
            ChatStreamEvent::Done(r) => done = Some(r),
        }
    }

    assert!(
        deltas.is_empty(),
        "tool-use-only stream yields no text deltas"
    );
    let done = done.expect("stream must end with Done");
    assert_eq!(done.finish_reason, "tool_calls");
    assert_eq!(done.message.tool_calls.len(), 1);
    assert_eq!(done.message.tool_calls[0].id, "toolu_1");
    assert_eq!(done.message.tool_calls[0].name, "calc");
    assert_eq!(done.message.tool_calls[0].arguments, "{\"expr\":\"2+2\"}");
    assert!(done.message.content.is_none());
    assert_eq!(done.usage.completion_tokens, 11);
}

/// PRV-202 acceptance: a forced tool choice reaches the wire as
/// `{"type":"tool","name":...}` and the model's answer selects that tool.
#[tokio::test]
async fn forced_tool_choice_selects_the_named_tool() {
    let fixture = json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [
            { "type": "tool_use", "id": "toolu_f1", "name": "calc",
              "input": { "expr": "17*3" } }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 40, "output_tokens": 12 }
    });
    let (base_url, requests) = serve(vec![("application/json", fixture.to_string())]);
    let provider =
        AnthropicProvider::new(base_url, "test-key").with_price_book(PriceBook::with_defaults());

    let mut req = ChatRequest::new("claude-opus-4-8", vec![Message::user("What is 17*3?")])
        .with_tool_choice(ToolChoice::Tool("calc".to_string()));
    req.tools = vec![calc_tool()];

    let resp = provider.chat(req).await.unwrap();

    // The wire carried the forced-tool constraint.
    let sent = requests.recv().unwrap();
    assert_eq!(
        sent["tool_choice"],
        json!({ "type": "tool", "name": "calc" })
    );

    // And the (recorded) model selected exactly the named tool.
    assert_eq!(resp.finish_reason, "tool_calls");
    assert_eq!(resp.message.tool_calls.len(), 1);
    assert_eq!(resp.message.tool_calls[0].name, "calc");
}

/// PRV-202 acceptance: a JSON-schema-constrained request carries the schema on
/// the wire (`output_config.format`) and the answer validates against it.
#[tokio::test]
async fn json_schema_constrained_request_returns_schema_valid_output() {
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "integer" },
            "confident": { "type": "boolean" }
        },
        "required": ["answer", "confident"],
        "additionalProperties": false
    });
    let fixture = json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [ { "type": "text", "text": "{\"answer\":51,\"confident\":true}" } ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 40, "output_tokens": 12 }
    });
    let (base_url, requests) = serve(vec![("application/json", fixture.to_string())]);
    let provider =
        AnthropicProvider::new(base_url, "test-key").with_price_book(PriceBook::with_defaults());

    let req = ChatRequest::new("claude-opus-4-8", vec![Message::user("What is 17*3?")])
        .with_response_format(ResponseFormat::JsonSchema {
            name: "arith_answer".to_string(),
            schema: schema.clone(),
        });
    let resp = provider.chat(req).await.unwrap();

    // The wire carried the schema constraint in Anthropic's shape.
    let sent = requests.recv().unwrap();
    assert_eq!(sent["output_config"]["format"]["type"], "json_schema");
    assert_eq!(sent["output_config"]["format"]["schema"], schema);

    // The answer is valid JSON satisfying the schema: both required fields
    // present, correctly typed, and nothing else.
    let parsed: Value = serde_json::from_str(resp.message.content.as_deref().unwrap()).unwrap();
    let obj = parsed.as_object().unwrap();
    assert!(obj["answer"].is_i64());
    assert!(obj["confident"].is_boolean());
    assert_eq!(obj.len(), 2, "additionalProperties: false");
}

/// PRV-204 acceptance: an image content part round-trips through the
/// multimodal-capable Messages API path — the wire carries the typed image
/// block and the model's (recorded) answer about it comes back.
#[tokio::test]
async fn image_content_part_round_trips_through_the_messages_api() {
    let fixture = json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [ { "type": "text", "text": "A one-pixel red square." } ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 90, "output_tokens": 9 }
    });
    let (base_url, requests) = serve(vec![("application/json", fixture.to_string())]);
    let provider =
        AnthropicProvider::new(base_url, "test-key").with_price_book(PriceBook::with_defaults());

    // A 1x1 PNG, base64-encoded — real image bytes, not a placeholder string.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let req = ChatRequest::new(
        "claude-opus-4-8",
        vec![
            Message::user("Describe this image.")
                .with_part(apex_provider::ContentPart::image_base64("image/png", png)),
        ],
    );
    let resp = provider.chat(req).await.unwrap();

    // Outbound: the typed part became Anthropic's image block, after the text.
    let sent = requests.recv().unwrap();
    let blocks = sent["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "Describe this image.");
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[1]["source"]["type"], "base64");
    assert_eq!(blocks[1]["source"]["media_type"], "image/png");
    assert_eq!(blocks[1]["source"]["data"], png);

    // Inbound: the answer parsed back through the normal response path.
    assert_eq!(
        resp.message.content.as_deref(),
        Some("A one-pixel red square.")
    );
    assert_eq!(resp.finish_reason, "stop");
    assert!(resp.usage.cost_usd > 0.0);
}
