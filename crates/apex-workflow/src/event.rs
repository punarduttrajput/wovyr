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
    /// An activity reported incremental progress on a still-running attempt
    /// (WFL-307) — display-only, informational: never consulted by scheduling
    /// or resume, so it carries no state an activity's own completion doesn't
    /// already capture. Emitted via [`ActivityContext::progress`](crate::
    /// ActivityContext::progress).
    ActivityProgress {
        id: String,
        attempt: u32,
        message: String,
    },
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

/// The current on-wire schema version for a logged [`WorkflowEvent`] (WFL-308).
///
/// The event enum's wire format had no version tag at all before this — a
/// future variant rename would silently break every already-written
/// `*.events.jsonl`/`workflow_events` row with no way to detect it, let alone
/// migrate it. Bump this constant, and extend [`decode_event`] with a
/// translation path from the old shape to the new one, **before** any future
/// rename ships — never rename a tag string that may already exist in a
/// durable log without one.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// The durable wire envelope: a schema version alongside the event itself.
/// `#[serde(flatten)]` keeps the event's own internally-tagged `type` field
/// (and its variant fields) at the same JSON object level as `v`, so a logged
/// line still reads as one flat object (e.g. `{"v":1,"type":"workflow_completed"}`)
/// rather than nesting the event under its own key.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionedEvent {
    v: u32,
    #[serde(flatten)]
    event: WorkflowEvent,
}

/// Encode `event` for the durable log, stamped with [`EVENT_SCHEMA_VERSION`].
/// The one place a `WorkflowEvent` is serialized for storage — every store
/// (`FileStore`, `PostgresStore`) goes through this rather than serializing
/// the bare enum directly, so the version tag can never be forgotten at a new
/// call site.
pub fn encode_event(event: &WorkflowEvent) -> serde_json::Result<String> {
    serde_json::to_string(&VersionedEvent {
        v: EVENT_SCHEMA_VERSION,
        event: event.clone(),
    })
}

/// Decode one logged event, fail-closed on a schema version newer than this
/// binary understands (`v` missing entirely — a pre-versioning log line — is
/// also rejected: this is the same "acceptable pre-GA, no real deployment to
/// migrate" breaking change the `snake_case` tag rename above already made,
/// not a new one). An older *known* version (`v < EVENT_SCHEMA_VERSION`)
/// would take a translation path here once one exists; none does yet, since
/// this is the format's first version.
pub fn decode_event(line: &str) -> apex_common::Result<WorkflowEvent> {
    let versioned: VersionedEvent = serde_json::from_str(line)?;
    if versioned.v > EVENT_SCHEMA_VERSION {
        return Err(apex_common::Error::config(format!(
            "workflow event log entry has schema version {}, newer than this binary's \
             version {EVENT_SCHEMA_VERSION} — upgrade the apex binary before reading this log",
            versioned.v
        )));
    }
    Ok(versioned.event)
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
                WorkflowEvent::ActivityProgress {
                    id: "a".into(),
                    attempt: 1,
                    message: "50% done".into(),
                },
                "activity_progress",
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

    /// WFL-308: `encode_event`/`decode_event` stamp and round-trip the current
    /// schema version, and the encoded line still reads as one flat JSON object
    /// (`v` alongside the event's own `type` tag and fields), not a nested
    /// wrapper.
    #[test]
    fn encode_decode_round_trips_a_versioned_event() {
        let event = WorkflowEvent::ActivityCompleted {
            id: "a".into(),
            output: serde_json::json!({"ok": true}),
        };

        let encoded = encode_event(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(parsed["v"], EVENT_SCHEMA_VERSION);
        assert_eq!(parsed["type"], "activity_completed");
        assert_eq!(parsed["id"], "a");

        let decoded = decode_event(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::to_value(&event).unwrap(),
        );
    }

    /// A schema version newer than this binary understands is rejected
    /// cleanly — not silently misread, not a panic.
    #[test]
    fn decode_event_rejects_an_unknown_future_schema_version() {
        let line = r#"{"v":99,"type":"workflow_completed"}"#;
        let err = decode_event(line).unwrap_err();
        assert!(
            err.to_string().contains("99") && err.to_string().contains("newer"),
            "{err}"
        );
    }

    /// A line with no `v` field at all (the pre-versioning wire shape) is also
    /// rejected — the same "breaking pre-GA, no real deployment to migrate"
    /// stance the `snake_case` tag rename already established for this format.
    #[test]
    fn decode_event_rejects_a_line_with_no_version_field() {
        let line = r#"{"type":"workflow_completed"}"#;
        assert!(decode_event(line).is_err());
    }
}
