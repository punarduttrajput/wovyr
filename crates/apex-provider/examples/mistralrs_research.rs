//! A real research agent: a real local model (Qwen2.5-0.5B-Instruct via
//! mistral.rs) driving the real `apex_agent::run_agent` tool-calling loop,
//! with the real `http_get` builtin tool — no mocks anywhere in this path.
//!
//! Run with (from the workspace root):
//! ```text
//! cargo run -p apex-provider --example mistralrs_research --features mistralrs --release -- "your question"
//! ```
//!
//! First run downloads the ~400MB GGUF file (needs network); the agent
//! manifest is `examples/agents/web-reader.yaml` (already in this repo).
//!
//! Caveat, stated up front rather than papered over: Qwen2.5-0.5B is a tiny
//! model. This proves the plumbing (real model → real tool call → real HTTP
//! fetch → real synthesis) works end to end; it does not guarantee
//! high-quality research synthesis — that's a model-capability limit, not an
//! integration bug.

use apex_agent::{AgentDefinition, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_provider::{Gateway, MistralRsProvider};
use apex_tools::ToolRegistry;
use serde_json::json;

/// Prints each run event live so the loop is observable, not a silent black box.
struct PrintSink;

impl RunEventSink for PrintSink {
    fn emit(&mut self, event: RunEvent<'_>) {
        match event {
            RunEvent::Start { model, provider } => {
                println!("[start] model={model} provider={provider}");
            }
            RunEvent::Delta { text } => print!("{text}"),
            RunEvent::ToolCall { name, arguments } => {
                println!("\n[tool call] {name}({arguments})");
            }
            RunEvent::ToolResult { name, ok } => {
                println!("[tool result] {name} ok={ok}");
            }
            RunEvent::MemoryRetrieved { .. } | RunEvent::Done { .. } => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let question = std::env::args().nth(1).unwrap_or_else(|| {
        "What is the current stable version of Rust, per the Rust blog?".to_string()
    });

    println!("Loading Qwen2.5-0.5B-Instruct via mistral.rs (first run downloads the GGUF file)...");
    let provider = MistralRsProvider::from_env().await?;
    let gateway = Gateway::new(Box::new(provider));

    let def = AgentDefinition::from_file("examples/agents/web-reader.yaml")?;
    let registry = ToolRegistry::with_builtins();

    println!("\nQuestion: {question}\n---");

    let mut sink = PrintSink;
    let output = run_agent(
        &def,
        &gateway,
        &registry,
        RunOptions::new(json!({ "message": question })),
        &mut sink,
    )
    .await?;

    println!("\n---\nFinal answer:\n{}", output.text);
    println!(
        "\n({} step(s), {} total tokens)",
        output.steps, output.usage.total_tokens
    );

    Ok(())
}
