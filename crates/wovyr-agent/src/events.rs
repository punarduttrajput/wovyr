//! Run events emitted by the agent loop.
//!
//! The loop reports progress through a [`RunEventSink`] so different front-ends
//! (the CLI `--stream` renderer, tests, a future API streamer) can observe a run
//! without the loop knowing about any of them. The event set matches the
//! `start · … / delta · … / done · …` shape shown in the
//! [hello agent](../../docs/16-examples/hello-agent.md) example, plus tool events.

use wovyr_common::Usage;

/// A single observable moment in an agent run.
#[derive(Debug, Clone)]
pub enum RunEvent<'a> {
    /// Run started; resolved model and provider are known.
    Start { model: &'a str, provider: &'a str },
    /// A memory was retrieved and injected as grounding context (one per result),
    /// exposing the source and score for the trace.
    MemoryRetrieved { source: &'a str, score: f32 },
    /// A chunk of streamed assistant text (multiple per answer as tokens arrive).
    Delta { text: &'a str },
    /// An incremental fragment of a tool call's streamed JSON arguments (AIC-202),
    /// emitted as the model composes the call — before [`ToolCall`](Self::ToolCall)
    /// announces the complete one. `name` is the name known so far (may be empty
    /// early in the stream); `arguments` is this event's fragment only, empty on
    /// the announcement that opens a call.
    ToolCallDelta {
        index: usize,
        name: &'a str,
        arguments: &'a str,
    },
    /// An incremental piece of the model's reasoning/thinking channel, where the
    /// provider exposes one (AIC-202). Display-only — never part of the answer.
    ReasoningDelta { text: &'a str },
    /// The model requested a tool call.
    ToolCall { name: &'a str, arguments: &'a str },
    /// A tool finished.
    ToolResult { name: &'a str, ok: bool },
    /// A **validated** generative-UI frame presented to the human (PRD-005
    /// UIP-104): the JSON serialization of an `wovyr_ui::UiFrame` that has
    /// already passed the trust layer — raw/unchecked frames must never reach
    /// a sink (the GRD-202 buffering stance). `frame_id` is the handle a
    /// decision is posted against. Carried as JSON (not the typed frame) so
    /// this crate stays protocol-agnostic; emission from the agent loop
    /// itself arrives with HIL-304 (P2) — today the server's workflow path
    /// emits it.
    UiFrame {
        frame_id: &'a str,
        frame: &'a serde_json::Value,
    },
    /// Run finished; carries cumulative usage.
    Done { usage: Usage },
}

/// Receiver of [`RunEvent`]s.
///
/// `Send` is required because the run loop holds a sink across `.await` points;
/// keeping it `Send` lets `run_agent` be driven from `Send` futures (e.g. an
/// Axum request handler in [`wovyr-server`]).
pub trait RunEventSink: Send {
    /// Handle one event. Implementations should be cheap and non-blocking.
    fn emit(&mut self, event: RunEvent<'_>);
}

/// A sink that discards all events (used when streaming is disabled or in tests).
pub struct NullSink;

impl RunEventSink for NullSink {
    fn emit(&mut self, _event: RunEvent<'_>) {}
}
