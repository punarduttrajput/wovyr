//! Workflow and activity finite-state machines.
//!
//! Encodes the states and transition matrices from the
//! [State Machine spec](../../docs/03-workflow-engine/state-machine.md): invalid
//! transitions are rejected and terminal states are immutable.

use serde::{Deserialize, Serialize};

/// Lifecycle state of a workflow execution
/// ([spec §5](../../docs/03-workflow-engine/state-machine.md)).
///
/// `snake_case` on the wire (RM-GA-P4 API-702): the `?status=` query filter
/// (`parse_workflow_status`) already normalized its input to lowercase before
/// this change, so the two now agree by construction instead of by
/// coincidence — a client no longer sees `"Completed"` in a response body
/// while filtering with `?status=completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    /// Instance created.
    Created,
    /// Definition validated.
    Validated,
    /// Ready for execution.
    Scheduled,
    /// Currently executing.
    Running,
    /// Waiting for an event, timer, or human (not yet used in v0.2).
    Waiting,
    /// Resumed after waiting.
    Resumed,
    /// Executing rollback logic (compensation — reserved for a later slice).
    Compensating,
    /// Successfully finished (terminal).
    Completed,
    /// Execution failed (terminal).
    Failed,
    /// Execution cancelled (terminal).
    Cancelled,
}

impl WorkflowState {
    /// Whether this is a terminal (immutable) state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled
        )
    }

    /// Whether `self -> to` is an allowed workflow transition
    /// ([spec §8](../../docs/03-workflow-engine/state-machine.md)).
    pub fn can_transition(self, to: WorkflowState) -> bool {
        use WorkflowState::*;
        matches!(
            (self, to),
            (Created, Validated)
                | (Validated, Scheduled)
                | (Scheduled, Running)
                | (Running, Waiting)
                | (Waiting, Resumed)
                | (Resumed, Running)
                | (Running, Completed)
                | (Running, Failed)
                | (Running, Cancelled)
                | (Failed, Compensating)
                | (Compensating, Completed)
        )
    }
}

/// Lifecycle state of a single activity
/// ([spec §7](../../docs/03-workflow-engine/state-machine.md)).
///
/// `snake_case` on the wire (RM-GA-P4 API-702), matching [`WorkflowState`] —
/// the two appear side by side in the same `GET /api/v1/workflows/{id}`
/// response, so they must share one casing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    /// Instantiated.
    Created,
    /// Dependencies satisfied.
    Ready,
    /// Assigned to the scheduler.
    Scheduled,
    /// Executing.
    Running,
    /// Successfully finished.
    Completed,
    /// Execution failed.
    Failed,
    /// Waiting for a retry.
    Retrying,
    /// Suspended pending a timer or event (a `wait` activity); resumes when the
    /// signal is delivered ([waiting states](../../docs/03-workflow-engine/execution-model.md)).
    Waiting,
    /// Not executed: every inbound branch was disabled by a guard
    /// ([conditional branching §11](../../docs/03-workflow-engine/workflow-dsl.md#11-conditional-branching)).
    Skipped,
}

impl ActivityState {
    /// Whether `self -> to` is an allowed activity transition
    /// ([spec §9](../../docs/03-workflow-engine/state-machine.md)).
    pub fn can_transition(self, to: ActivityState) -> bool {
        use ActivityState::*;
        matches!(
            (self, to),
            (Created, Ready)
                | (Ready, Scheduled)
                | (Scheduled, Running)
                | (Running, Completed)
                | (Running, Failed)
                | (Failed, Retrying)
                | (Retrying, Scheduled)
                | (Created, Skipped)
                | (Ready, Skipped)
                // A `wait` activity suspends, then completes once signalled.
                | (Created, Waiting)
                | (Ready, Waiting)
                | (Waiting, Waiting)
                | (Waiting, Completed)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_workflow_transitions() {
        assert!(WorkflowState::Created.can_transition(WorkflowState::Validated));
        assert!(WorkflowState::Running.can_transition(WorkflowState::Completed));
        assert!(WorkflowState::Failed.can_transition(WorkflowState::Compensating));
    }

    #[test]
    fn terminal_states_cannot_resume() {
        assert!(WorkflowState::Completed.is_terminal());
        assert!(!WorkflowState::Completed.can_transition(WorkflowState::Running));
        assert!(!WorkflowState::Failed.can_transition(WorkflowState::Running));
        assert!(!WorkflowState::Cancelled.can_transition(WorkflowState::Running));
    }

    #[test]
    fn activity_retry_cycle() {
        assert!(ActivityState::Running.can_transition(ActivityState::Failed));
        assert!(ActivityState::Failed.can_transition(ActivityState::Retrying));
        assert!(ActivityState::Retrying.can_transition(ActivityState::Scheduled));
        assert!(!ActivityState::Completed.can_transition(ActivityState::Running));
    }

    /// RM-GA-P4 API-702: every `WorkflowState` variant round-trips through JSON
    /// as its `snake_case` wire form — a client filtering `?status=completed`
    /// sees the exact same casing a response body's `status` field carries.
    #[test]
    fn workflow_state_round_trips_as_snake_case_json() {
        let cases = [
            (WorkflowState::Created, "\"created\""),
            (WorkflowState::Validated, "\"validated\""),
            (WorkflowState::Scheduled, "\"scheduled\""),
            (WorkflowState::Running, "\"running\""),
            (WorkflowState::Waiting, "\"waiting\""),
            (WorkflowState::Resumed, "\"resumed\""),
            (WorkflowState::Compensating, "\"compensating\""),
            (WorkflowState::Completed, "\"completed\""),
            (WorkflowState::Failed, "\"failed\""),
            (WorkflowState::Cancelled, "\"cancelled\""),
        ];
        for (state, wire) in cases {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, wire, "{state:?} must serialize to {wire}");
            let back: WorkflowState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state, "{wire} must deserialize back to {state:?}");
        }
    }

    /// RM-GA-P4 API-702: every `ActivityState` variant round-trips through JSON
    /// as its `snake_case` wire form, matching [`WorkflowState`]'s policy since
    /// both appear side by side in the same response.
    #[test]
    fn activity_state_round_trips_as_snake_case_json() {
        let cases = [
            (ActivityState::Created, "\"created\""),
            (ActivityState::Ready, "\"ready\""),
            (ActivityState::Scheduled, "\"scheduled\""),
            (ActivityState::Running, "\"running\""),
            (ActivityState::Completed, "\"completed\""),
            (ActivityState::Failed, "\"failed\""),
            (ActivityState::Retrying, "\"retrying\""),
            (ActivityState::Waiting, "\"waiting\""),
            (ActivityState::Skipped, "\"skipped\""),
        ];
        for (state, wire) in cases {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, wire, "{state:?} must serialize to {wire}");
            let back: ActivityState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state, "{wire} must deserialize back to {state:?}");
        }
    }
}
