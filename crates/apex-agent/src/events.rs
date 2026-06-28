//! Run events emitted by the agent loop.
//!
//! The loop reports progress through a [`RunEventSink`] so different front-ends
//! (the CLI `--stream` renderer, tests, a future API streamer) can observe a run
//! without the loop knowing about any of them. The event set matches the
//! `start · … / delta · … / done · …` shape shown in the
//! [hello agent](../../docs/16-examples/hello-agent.md) example, plus tool events.

use apex_common::Usage;

/// A single observable moment in an agent run.
#[derive(Debug, Clone)]
pub enum RunEvent<'a> {
    /// Run started; resolved model and provider are known.
    Start { model: &'a str, provider: &'a str },
    /// A memory was retrieved and injected as grounding context (one per result),
    /// exposing the source and score for the trace.
    MemoryRetrieved { source: &'a str, score: f32 },
    /// A chunk of assistant text. v0.1 emits the full message as one delta.
    Delta { text: &'a str },
    /// The model requested a tool call.
    ToolCall { name: &'a str, arguments: &'a str },
    /// A tool finished.
    ToolResult { name: &'a str, ok: bool },
    /// Run finished; carries cumulative usage.
    Done { usage: Usage },
}

/// Receiver of [`RunEvent`]s.
///
/// `Send` is required because the run loop holds a sink across `.await` points;
/// keeping it `Send` lets `run_agent` be driven from `Send` futures (e.g. an
/// Axum request handler in [`apex-server`]).
pub trait RunEventSink: Send {
    /// Handle one event. Implementations should be cheap and non-blocking.
    fn emit(&mut self, event: RunEvent<'_>);
}

/// A sink that discards all events (used when streaming is disabled or in tests).
pub struct NullSink;

impl RunEventSink for NullSink {
    fn emit(&mut self, _event: RunEvent<'_>) {}
}
