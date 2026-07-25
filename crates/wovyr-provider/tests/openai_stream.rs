//! End-to-end `chat_stream` test for the OpenAI provider, driving the real
//! HTTP + SSE parsing path against a canned local server (no network).
//!
//! Covers [provider SDK §streaming](../../../docs/04-agent-framework/provider-sdk.md):
//! incremental `Delta`s followed by a terminal `Done` carrying the assembled
//! message, tool calls, and usage.

use futures::StreamExt;
use std::io::{Read, Write};
use std::net::TcpListener;
use wovyr_provider::{AIProvider, ChatRequest, ChatStreamEvent, Message, OpenAiProvider};

/// A short label for an unexpected stream event (test diagnostics).
fn kind_of(event: &ChatStreamEvent) -> &'static str {
    match event {
        ChatStreamEvent::Delta(_) => "Delta",
        ChatStreamEvent::ToolCallDelta { .. } => "ToolCallDelta",
        ChatStreamEvent::ReasoningDelta(_) => "ReasoningDelta",
        ChatStreamEvent::Done(_) => "Done",
    }
}

/// Spawn a one-shot HTTP/1.1 server that replies to the first connection with
/// `body` (served as `text/event-stream`), and return its base URL.
fn serve_sse(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        // Drain the request headers so the client's write completes.
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(resp.as_bytes()).unwrap();
        sock.flush().unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn streams_text_deltas_then_done() {
    // Two content chunks, a finish chunk, a usage-only chunk, then [DONE].
    let body = "\
data: {\"model\":\"gpt-4o-mini\",\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

    let provider = OpenAiProvider::new(serve_sse(body), "test-key");
    let req = ChatRequest::new("gpt-4o-mini", vec![Message::user("hi")]);
    let mut stream = provider.chat_stream(req).await.unwrap();

    let mut deltas = Vec::new();
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ChatStreamEvent::Delta(d) => deltas.push(d),
            ChatStreamEvent::Done(r) => done = Some(r),
            other => panic!("unexpected event kind: {}", kind_of(&other)),
        }
    }

    assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
    let done = done.expect("stream must end with Done");
    assert_eq!(done.message.content.as_deref(), Some("Hello"));
    assert_eq!(done.model, "gpt-4o-mini");
    assert_eq!(done.finish_reason, "stop");
    assert_eq!(done.usage.prompt_tokens, 5);
    assert_eq!(done.usage.total_tokens, 7);
}

#[tokio::test]
async fn streams_tool_call_in_final_done() {
    // Tool call assembled across chunks; no text content surfaces as a delta, but
    // each chunk surfaces a ToolCallDelta argument fragment (AIC-202).
    let body = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"x\\\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";

    let provider = OpenAiProvider::new(serve_sse(body), "test-key");
    let req = ChatRequest::new("m", vec![Message::user("call echo")]);
    let mut stream = provider.chat_stream(req).await.unwrap();

    let mut deltas = Vec::new();
    let mut tool_fragments = Vec::new();
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ChatStreamEvent::Delta(d) => deltas.push(d),
            ChatStreamEvent::ToolCallDelta {
                index,
                name,
                arguments,
                ..
            } => tool_fragments.push((index, name, arguments)),
            ChatStreamEvent::Done(r) => done = Some(r),
            other => panic!("unexpected event kind: {}", kind_of(&other)),
        }
    }

    assert!(
        deltas.is_empty(),
        "tool-call-only stream yields no text deltas"
    );
    // The argument fragments streamed live, in wire order, each with the name.
    assert_eq!(
        tool_fragments,
        vec![
            (0, "echo".to_string(), "{\"x\"".to_string()),
            (0, "echo".to_string(), ":1}".to_string()),
        ]
    );
    let done = done.expect("stream must end with Done");
    assert_eq!(done.finish_reason, "tool_calls");
    assert_eq!(done.message.tool_calls.len(), 1);
    assert_eq!(done.message.tool_calls[0].id, "call_1");
    assert_eq!(done.message.tool_calls[0].name, "echo");
    assert_eq!(done.message.tool_calls[0].arguments, "{\"x\":1}");
    assert!(done.message.content.is_none());
}
