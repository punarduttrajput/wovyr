//! Stage 2: the *full* Apex agent loop calling a tool proxied from a real,
//! externally-spawned MCP server (`@modelcontextprotocol/server-filesystem`
//! via `npx`) — not a mock, not a scripted tool. Proves the whole chain:
//!
//!   npx-spawned real MCP server
//!     -> McpClient (real JSON-RPC handshake + tools/list)
//!     -> ToolRegistry::register_into (permission-checked proxy Tool impls)
//!     -> run_agent's real model -> tool -> model loop
//!     -> final answer containing the real file's real content
//!
//! The *model* side is a scripted `AIProvider` (deterministic, no API key
//! needed) so the run is reproducible — everything on the *tool* side is real.
//!
//! Run: `cargo run -p apex-agent --example mcp_filesystem_agent_demo -- <allowed-dir> <file-name>`

use apex_agent::{AgentDefinition, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_common::{Result, Usage};
use apex_provider::{AIProvider, ChatRequest, ChatResponse, Gateway, Message, Role, ToolCall};
use apex_tools::{McpClient, StdioTransport, ToolRegistry};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Requests the given MCP-proxied tool (with fixed arguments) on the first
/// turn, then folds the tool's real result into a final answer.
struct ReadFileProvider {
    tool_id: String,
    tool_args: Value,
}

#[async_trait]
impl AIProvider for ReadFileProvider {
    fn name(&self) -> &str {
        "scripted-mcp-demo"
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        if let Some(tool_msg) = request.messages.iter().rev().find(|m| m.role == Role::Tool) {
            let observed = tool_msg.content.clone().unwrap_or_default();
            return Ok(ChatResponse {
                message: Message::assistant(format!(
                    "The file's real content, read through a live MCP server, is: {observed}"
                )),
                model: request.model,
                usage: Usage::new(5, 5, 0.0),
                finish_reason: "stop".to_string(),
            });
        }
        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                parts: Vec::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: self.tool_id.clone(),
                    arguments: self.tool_args.to_string(),
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

/// Prints the interesting parts of the run as they happen.
struct PrintSink;
impl RunEventSink for PrintSink {
    fn emit(&mut self, event: RunEvent<'_>) {
        match event {
            RunEvent::ToolCall {
                name, arguments, ..
            } => {
                println!("  -> agent calls tool `{name}` with {arguments}")
            }
            RunEvent::ToolResult { name, ok } => println!("  <- tool `{name}` returned ok={ok}"),
            RunEvent::Done { .. } => println!("  (run complete)"),
            _ => {}
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: mcp_filesystem_agent_demo <allowed-dir> <file-name>");
    let file_name = args.next().unwrap_or_else(|| "greeting.txt".to_string());

    println!("1. Spawning a real @modelcontextprotocol/server-filesystem for: {dir}");
    #[cfg(windows)]
    let transport = StdioTransport::spawn(
        "cmd",
        [
            "/C",
            "npx",
            "-y",
            "@modelcontextprotocol/server-filesystem",
            &dir,
        ],
    )
    .expect("failed to spawn npx via cmd /C");
    #[cfg(not(windows))]
    let transport = StdioTransport::spawn(
        "npx",
        ["-y", "@modelcontextprotocol/server-filesystem", &dir],
    )
    .expect("failed to spawn npx");

    let client = Arc::new(
        McpClient::connect("fs", transport)
            .await
            .expect("MCP initialize handshake failed"),
    );
    println!("2. MCP handshake OK.");

    let mut registry = ToolRegistry::with_builtins();
    let registered_ids = client
        .register_into(&mut registry)
        .await
        .expect("register_into failed");
    println!(
        "3. Registered {} real MCP tools into the ToolRegistry.",
        registered_ids.len()
    );

    let tool_id = registered_ids
        .iter()
        .find(|id| id.ends_with("__read_text_file"))
        .unwrap_or_else(|| {
            panic!("no `read_text_file` tool among registered ids: {registered_ids:?}")
        })
        .clone();
    println!("4. Agent will call: {tool_id}");

    let file_path = format!("{dir}/{file_name}");
    let provider = ReadFileProvider {
        tool_id: tool_id.clone(),
        tool_args: json!({ "path": file_path }),
    };
    let gateway = Gateway::new(Box::new(provider));

    let manifest = format!(
        "metadata:\n  name: mcp-fs-demo\nspec:\n  instructions: Use tools to answer.\n  tools: [{tool_id}]\n"
    );
    let def = AgentDefinition::from_yaml(&manifest).expect("agent manifest failed to parse");

    println!("5. Running the real agent loop (model -> tool -> model)...");
    let mut sink = PrintSink;
    let out = run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({ "message": format!("What's in {file_name}?") })),
        &mut sink,
    )
    .await
    .expect("agent run failed");

    println!("\nFinal agent answer:\n  {}", out.text);
    println!(
        "\nSteps: {}, total tokens: {}",
        out.steps, out.usage.total_tokens
    );

    assert!(
        out.text.contains("hello from apex mcp test"),
        "expected the real file content (read via the real MCP server) to reach the final answer"
    );
    println!(
        "\n\u{2705} End-to-end verified: a real npx-spawned MCP server's tool was discovered, \
         registered, permission-checked, and called by the real Apex agent loop, and its real \
         file content reached the model's final answer."
    );
}
