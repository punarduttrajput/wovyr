//! The platform domain event
//! ([event structure §7](../../docs/02-architecture/event-driven-architecture.md#7-event-structure)).
//!
//! Events are **past-tense** facts (`project.created`, `plugin.enabled`,
//! `workflow.completed`) emitted on mutations and mirrored to webhook subscribers. The
//! struct is a plain serializable record; the producer supplies the id and timestamp
//! (read at the service boundary), keeping this crate free of ambient clocks/randomness.

use serde::{Deserialize, Serialize};

/// The current event schema version.
pub const EVENT_SCHEMA_VERSION: &str = "1";

/// A domain event delivered to webhook subscribers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unique event id (producer-assigned).
    pub id: String,
    /// Dotted, past-tense event type, e.g. `project.created`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Event schema version.
    pub version: String,
    /// The tenant the event belongs to (the isolation boundary for delivery).
    pub tenant: String,
    /// Unix epoch milliseconds at emission.
    pub timestamp_ms: u64,
    /// The event payload (type-specific).
    pub payload: serde_json::Value,
}

impl Event {
    /// A new event with the current schema version.
    pub fn new(
        id: impl Into<String>,
        event_type: impl Into<String>,
        tenant: impl Into<String>,
        timestamp_ms: u64,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            event_type: event_type.into(),
            version: EVENT_SCHEMA_VERSION.to_string(),
            tenant: tenant.into(),
            timestamp_ms,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_with_renamed_type_and_version() {
        let e = Event::new(
            "evt-1",
            "project.created",
            "acme",
            1_700_000_000_000,
            json!({"id":"prj-x"}),
        );
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(v["type"], "project.created");
        assert_eq!(v["version"], "1");
        assert_eq!(v["tenant"], "acme");
        assert_eq!(v["payload"]["id"], "prj-x");
    }
}
