//! The audit record schema ([audit §3](../../docs/13-security/audit.md#3-record-schema)).
//!
//! Records are structured, correlation-linked (`request_id`), and PII/secret-masked —
//! sensitive resources are referenced **by id, never by value** (e.g. a secret read
//! audits the `secret://…` reference, not the secret). It is the caller's responsibility
//! to pass references rather than values; this type stores only the strings it is given.

use serde::{Deserialize, Serialize};

/// What kind of principal acted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    /// A human user.
    User,
    /// A service / workload identity.
    Service,
    /// An API key subject.
    ApiKey,
    /// The platform itself (background/system action).
    System,
}

/// Who performed the action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// Principal id (user id / api-key subject / service identity). Empty = anonymous.
    pub principal: String,
    /// The principal's kind.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// The tenant the action was performed in.
    pub tenant: String,
}

/// The resource an action targeted, referenced by type + id (never a value).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// Resource kind (e.g. `secret`, `agent`, `execution`).
    #[serde(rename = "type")]
    pub resource_type: String,
    /// Resource id / reference (e.g. `secret://acme/token`).
    pub id: String,
}

/// The outcome of an audited action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The action was authorized and performed.
    Allowed,
    /// The action was denied (e.g. authorization failure).
    Denied,
    /// The action was authorized but failed during execution.
    Error,
}

/// A single audited action — the structured "who did what, when, with what outcome".
/// The `timestamp_ms` is supplied by the caller (read at the request boundary) so the
/// core stays clock-free and deterministic in tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Wall-clock time of the action, epoch milliseconds (set at the boundary).
    pub timestamp_ms: u64,
    /// Who acted.
    pub actor: Actor,
    /// The action, dotted (`secret.create`, `workflow.execution.cancel`).
    pub action: String,
    /// What was acted on.
    pub resource: Resource,
    /// The outcome.
    pub outcome: Outcome,
    /// Why, for denials/errors (e.g. the missing scope). `None` when allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Correlation id linking to the originating request, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl AuditEvent {
    /// A new allowed event at `timestamp_ms` for `principal`/`tenant` performing `action`
    /// on `(resource_type, resource_id)`.
    pub fn new(
        timestamp_ms: u64,
        principal: impl Into<String>,
        tenant: impl Into<String>,
        action: impl Into<String>,
        resource_type: impl Into<String>,
        resource_id: impl Into<String>,
    ) -> Self {
        Self {
            timestamp_ms,
            actor: Actor {
                principal: principal.into(),
                actor_type: ActorType::User,
                tenant: tenant.into(),
            },
            action: action.into(),
            resource: Resource {
                resource_type: resource_type.into(),
                id: resource_id.into(),
            },
            outcome: Outcome::Allowed,
            reason: None,
            request_id: None,
        }
    }

    /// Set the actor kind (default [`ActorType::User`]).
    pub fn with_actor_type(mut self, actor_type: ActorType) -> Self {
        self.actor.actor_type = actor_type;
        self
    }

    /// Mark the outcome a denial with a reason.
    pub fn denied(mut self, reason: impl Into<String>) -> Self {
        self.outcome = Outcome::Denied;
        self.reason = Some(reason.into());
        self
    }

    /// Mark the outcome an execution error with a reason.
    pub fn errored(mut self, reason: impl Into<String>) -> Self {
        self.outcome = Outcome::Error;
        self.reason = Some(reason.into());
        self
    }

    /// Attach the originating request id.
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}
