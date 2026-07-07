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
/// per line in the file store). `snake_case` variant names on the wire (RM-GA-P4
/// API-702), matching [`crate::WorkflowState`]/[`crate::ActivityState`] —
/// previously this was the one wire-facing enum in the workflow engine still
/// using the bare (PascalCase) Rust variant name. **Note:** this changes the
/// on-disk/on-wire encoding of every event, including the persisted
/// `*.events.jsonl` file-store log and the Postgres `workflow_events` table —
/// an event log written before this change will not deserialize after
/// upgrading. Acceptable pre-GA (no real deployment exists to migrate); a real
/// migration path would be needed before this could ship as a later change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    /// A `wait` activity suspended pending a timer or event (`waiting_for`).
    ActivityWaiting { id: String, waiting_for: String },
    /// A durable wall-clock timer was scheduled to fire at `fire_at_ms` (Unix-epoch
    /// milliseconds). Recording the deadline in the log keeps recovery
    /// deterministic — the timer is not recomputed on resume
    /// ([gap closure G1](../../docs/03-workflow-engine/temporal-gap-analysis.md#g1--durable-wall-clock-timers)).
    TimerScheduled { id: String, fire_at_ms: u64 },
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
    /// Execution cancelled by an operator ([`Engine::cancel`](crate::Engine::cancel)).
    WorkflowCancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RM-GA-P4 API-702: every `WorkflowEvent` variant's `type` tag round-trips
    /// through JSON as `snake_case` — this used to be the one wire-facing enum
    /// in the workflow engine still using the bare (PascalCase) Rust variant
    /// name (`"WorkflowCreated"`, `"ActivityCompleted"`, …).
    #[test]
    fn every_variant_round_trips_with_a_snake_case_type_tag() {
        let cases: Vec<(WorkflowEvent, &str)> = vec![
            (
                WorkflowEvent::WorkflowCreated {
                    workflow: "wf".into(),
                    version: "1".into(),
                },
                "workflow_created",
            ),
            (WorkflowEvent::WorkflowValidated, "workflow_validated"),
            (WorkflowEvent::WorkflowScheduled, "workflow_scheduled"),
            (WorkflowEvent::WorkflowStarted, "workflow_started"),
            (
                WorkflowEvent::ActivityReady { id: "a".into() },
                "activity_ready",
            ),
            (
                WorkflowEvent::ActivityStarted {
                    id: "a".into(),
                    attempt: 1,
                },
                "activity_started",
            ),
            (
                WorkflowEvent::ActivityCompleted {
                    id: "a".into(),
                    output: serde_json::json!({}),
                },
                "activity_completed",
            ),
            (
                WorkflowEvent::ActivitySkipped { id: "a".into() },
                "activity_skipped",
            ),
            (
                WorkflowEvent::ActivityWaiting {
                    id: "a".into(),
                    waiting_for: "event.go".into(),
                },
                "activity_waiting",
            ),
            (
                WorkflowEvent::TimerScheduled {
                    id: "t".into(),
                    fire_at_ms: 0,
                },
                "timer_scheduled",
            ),
            (
                WorkflowEvent::ActivityRetried {
                    id: "a".into(),
                    attempt: 1,
                    delay_ms: 0,
                    reason: "r".into(),
                },
                "activity_retried",
            ),
            (
                WorkflowEvent::ActivityFailed {
                    id: "a".into(),
                    error: "e".into(),
                },
                "activity_failed",
            ),
            (WorkflowEvent::WorkflowCompleted, "workflow_completed"),
            (
                WorkflowEvent::WorkflowFailed { error: "e".into() },
                "workflow_failed",
            ),
            (
                WorkflowEvent::WorkflowInterrupted {
                    activity: "a".into(),
                },
                "workflow_interrupted",
            ),
            (WorkflowEvent::CompensationStarted, "compensation_started"),
            (
                WorkflowEvent::CompensationStepCompleted {
                    activity: "a".into(),
                    compensation: "c".into(),
                },
                "compensation_step_completed",
            ),
            (
                WorkflowEvent::CompensationStepFailed {
                    activity: "a".into(),
                    compensation: "c".into(),
                    error: "e".into(),
                },
                "compensation_step_failed",
            ),
            (
                WorkflowEvent::CompensationCompleted,
                "compensation_completed",
            ),
            (WorkflowEvent::WorkflowCancelled, "workflow_cancelled"),
        ];

        for (event, tag) in cases {
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(
                json["type"], tag,
                "expected type tag {tag} for {event:?}, got {json}"
            );
            let back: WorkflowEvent = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(&back).unwrap(),
                json,
                "{tag} must deserialize back to an equivalent event"
            );
        }
    }
}
