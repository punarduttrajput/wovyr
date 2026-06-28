//! Workflow execution events.
//!
//! Workflow progress is event-sourced
//! ([execution model §7](../../docs/03-workflow-engine/execution-model.md)): every
//! significant transition appends an immutable event to the log. Events are the
//! durable audit trail; checkpoints (see [`crate::store`]) are an optimization on
//! top of them, never a replacement
//! ([checkpointing §3](../../docs/03-workflow-engine/checkpointing-specification.md)).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An immutable workflow execution event.
///
/// Serialized with a `type` tag so the log is human-inspectable (one JSON object
/// per line in the file store).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkflowEvent {
    /// Instance created from a definition.
    WorkflowCreated { workflow: String, version: String },
    /// Definition validated.
    WorkflowValidated,
    /// Scheduled for execution.
    WorkflowScheduled,
    /// Execution started.
    WorkflowStarted,
    /// An activity's dependencies are satisfied.
    ActivityReady { id: String },
    /// An activity attempt started.
    ActivityStarted { id: String, attempt: u32 },
    /// An activity completed with `output`.
    ActivityCompleted { id: String, output: Value },
    /// An activity was skipped: every inbound branch was disabled by a guard.
    ActivitySkipped { id: String },
    /// An activity attempt failed and will be retried after `delay_ms`.
    ActivityRetried {
        id: String,
        attempt: u32,
        delay_ms: u64,
        reason: String,
    },
    /// An activity failed terminally.
    ActivityFailed { id: String, error: String },
    /// Execution completed successfully.
    WorkflowCompleted,
    /// Execution failed.
    WorkflowFailed { error: String },
    /// A worker yielded mid-execution; the run is resumable.
    WorkflowInterrupted { activity: String },
    /// Rollback (saga compensation) began after a failure.
    CompensationStarted,
    /// A single activity's compensation handler completed.
    CompensationStepCompleted {
        activity: String,
        compensation: String,
    },
    /// A compensation handler failed (rollback could not complete).
    CompensationStepFailed {
        activity: String,
        compensation: String,
        error: String,
    },
    /// Rollback completed; the workflow is consistently rolled back.
    CompensationCompleted,
}
