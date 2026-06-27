//! The `--stream` event renderer.
//!
//! Prints the live run in the format shown in the
//! [hello agent](../../docs/16-examples/hello-agent.md) example:
//!
//! ```text
//! start  · model: gpt-4o-mini (provider: openai)
//! delta  · "Hi! I'm a friendly assistant ..."
//! done   · tokens: 48, cost_usd: 0.000100
//! ```

use apex_agent::{RunEvent, RunEventSink};

/// Renders [`RunEvent`]s to stdout as human-readable stream lines.
pub struct StreamSink;

impl StreamSink {
    /// Create a new stream renderer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for StreamSink {
    fn default() -> Self {
        Self::new()
    }
}

impl RunEventSink for StreamSink {
    fn emit(&mut self, event: RunEvent<'_>) {
        match event {
            RunEvent::Start { model, provider } => {
                println!("start  · model: {model} (provider: {provider})");
            }
            RunEvent::Delta { text } => {
                println!("delta  · {text:?}");
            }
            RunEvent::ToolCall { name, arguments } => {
                println!("tool   · {name}({arguments})");
            }
            RunEvent::ToolResult { name, ok } => {
                println!("result · {name} {}", if ok { "ok" } else { "err" });
            }
            RunEvent::Done { usage } => {
                println!(
                    "done   · tokens: {}, cost_usd: {:.6}",
                    usage.total_tokens, usage.cost_usd
                );
            }
        }
    }
}
