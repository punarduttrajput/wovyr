//! Integration test for the agent tool-calling loop.
//!
//! Drives the real [`run_agent`] loop with the real [`ToolRegistry`] and built-in
//! `echo` tool, using a *scripted* provider that requests a tool on its first turn
//! and answers on its second. This exercises the full
//! model → tool → model → respond cycle end to end
//! ([Agent Runtime spec §14](../../../docs/03-workflow-engine/agent-runtime.md)) —
//! the core path the offline mock provider deliberately does not trigger.

use apex_agent::{AgentDefinition, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_common::{Result, Usage};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role, ToolCall};
use apex_tools::ToolRegistry;
use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A provider whose behavior is fixed for the test: ask for the `echo` tool once,
/// then summarize the tool result. Deterministic — no clocks, no randomness.
struct ScriptedProvider {
    calls: AtomicUsize,
}

impl ScriptedProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AIProvider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        // Once a tool result is in the history, produce a final answer.
        if let Some(tool_msg) = request.messages.iter().rev().find(|m| m.role == Role::Tool) {
            let observed = tool_msg.content.clone().unwrap_or_default();
            return Ok(ChatResponse {
                message: Message::assistant(format!("done: {observed}")),
                model: request.model,
                usage: Usage::new(5, 5, 0.0),
                finish_reason: "stop".to_string(),
            });
        }

        // First turn: request the echo tool.
        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "echo".to_string(),
                    arguments: json!({ "ping": "pong" }).to_string(),
                }],
                tool_call_id: None,
                name: None,
            },
            model: request.model,
            usage: Usage::new(5, 0, 0.0),
            finish_reason: "tool_calls".to_string(),
        })
    }
}

/// A sink that records events as simple strings so we can assert on the stream.
#[derive(Default)]
struct Capture {
    events: Vec<String>,
}

impl RunEventSink for Capture {
    fn emit(&mut self, event: RunEvent<'_>) {
        let line = match event {
            RunEvent::Start { provider, .. } => format!("start:{provider}"),
            RunEvent::MemoryRetrieved { source, .. } => format!("memory:{source}"),
            RunEvent::Delta { .. } => "delta".to_string(),
            RunEvent::ToolCall { name, .. } => format!("toolcall:{name}"),
            RunEvent::ToolResult { name, ok } => format!("toolresult:{name}:{ok}"),
            RunEvent::Done { .. } => "done".to_string(),
        };
        self.events.push(line);
    }
}

fn tool_agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: tooler\nspec:\n  instructions: Use tools to answer.\n  tools: [echo]\n",
    )
    .unwrap()
}

#[tokio::test]
async fn agent_completes_a_tool_round_trip() {
    let def = tool_agent();
    let gateway = Gateway::new(Box::new(ScriptedProvider::new()));
    let registry = ToolRegistry::with_builtins();

    let mut capture = Capture::default();
    let out = run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({ "message": "go" })),
        &mut capture,
    )
    .await
    .unwrap();

    // Two model calls: one to request the tool, one to answer.
    assert_eq!(out.steps, 2, "expected a model → tool → model round trip");
    // The final answer incorporates the echo tool's returned payload.
    assert!(out.text.starts_with("done:"), "got: {}", out.text);
    assert!(
        out.text.contains("ping"),
        "tool payload should reach the model: {}",
        out.text
    );
    // Usage accumulated across both calls (5+0 then 5+5).
    assert_eq!(out.usage.total_tokens, 15);

    // The event stream reflects the tool call and its successful result.
    assert!(capture.events.contains(&"start:scripted".to_string()));
    assert!(capture.events.contains(&"toolcall:echo".to_string()));
    assert!(capture.events.contains(&"toolresult:echo:true".to_string()));
    assert!(capture.events.contains(&"delta".to_string()));
    assert!(capture.events.contains(&"done".to_string()));
}

#[tokio::test]
async fn run_loop_terminates_on_step_budget() {
    // A provider that always asks for a tool would loop forever without the budget.
    struct AlwaysToolProvider;
    #[async_trait]
    impl AIProvider for AlwaysToolProvider {
        fn name(&self) -> &str {
            "always-tool"
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message {
                    role: Role::Assistant,
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "c".to_string(),
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    }],
                    tool_call_id: None,
                    name: None,
                },
                model: request.model,
                usage: Usage::new(1, 0, 0.0),
                finish_reason: "tool_calls".to_string(),
            })
        }
    }

    let def = tool_agent();
    let gateway = Gateway::new(Box::new(AlwaysToolProvider));
    let registry = ToolRegistry::with_builtins();

    let mut opts = RunOptions::new(json!("loop"));
    opts.max_steps = 3;

    let err = run_agent(&def, &gateway, &registry, opts, &mut Capture::default())
        .await
        .unwrap_err();
    assert!(matches!(err, apex_common::Error::Runtime(_)));
}
