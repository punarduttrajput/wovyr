//! Pluggable activity execution.
//!
//! The engine orchestrates; the actual work of an activity (calling a tool, an
//! LLM, an HTTP service, …) is delegated to an [`ActivityExecutor`]. This keeps the
//! engine deterministic and free of side effects
//! ([execution model §14](../../docs/03-workflow-engine/execution-model.md)) while
//! activities perform the non-deterministic work.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// What an activity needs to run: its identity, type, static inputs, and a
/// read-only view of the current workflow variables.
#[derive(Debug, Clone)]
pub struct ActivityContext {
    /// Activity id within the workflow.
    pub id: String,
    /// Activity type (`function`, `tool`, …).
    pub activity_type: String,
    /// Activity name (e.g. tool id), if any.
    pub name: Option<String>,
    /// Static inputs from the definition.
    pub inputs: Value,
    /// Current workflow variables.
    pub variables: BTreeMap<String, Value>,
    /// Current attempt number (1-based).
    pub attempt: u32,
}

/// The outcome of a failed activity, used by the retry classifier
/// ([retry §8](../../docs/03-workflow-engine/retry-engine.md)).
#[derive(Debug, Clone)]
pub enum ActivityError {
    /// A transient failure that should be retried (network, 429/503, …).
    Retryable(String),
    /// A permanent failure that must not be retried (validation, permission, …).
    Permanent(String),
    /// The worker is yielding/crashing: execution is durable and resumable, and
    /// the workflow stays `Running` rather than failing. Models the recovery hook
    /// in [state machine §21](../../docs/03-workflow-engine/state-machine.md).
    Interrupted(String),
}

impl ActivityError {
    /// Whether the retry engine should attempt this failure again.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ActivityError::Retryable(_))
    }
}

impl fmt::Display for ActivityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActivityError::Retryable(m) => write!(f, "retryable: {m}"),
            ActivityError::Permanent(m) => write!(f, "permanent: {m}"),
            ActivityError::Interrupted(m) => write!(f, "interrupted: {m}"),
        }
    }
}

impl std::error::Error for ActivityError {}

/// Executes a single activity, returning its output value.
#[async_trait]
pub trait ActivityExecutor: Send + Sync {
    /// Run the activity described by `ctx`.
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError>;
}

/// Boxed async closure used by [`ClosureExecutor`].
type Handler = Box<
    dyn Fn(ActivityContext) -> Pin<Box<dyn Future<Output = Result<Value, ActivityError>> + Send>>
        + Send
        + Sync,
>;

/// An executor backed by a per-activity-id closure map — handy for tests and for
/// composing simple in-process workflows.
#[derive(Default)]
pub struct ClosureExecutor {
    handlers: BTreeMap<String, Handler>,
}

impl ClosureExecutor {
    /// Create an empty executor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for an activity id.
    pub fn on<F, Fut>(mut self, id: impl Into<String>, handler: F) -> Self
    where
        F: Fn(ActivityContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ActivityError>> + Send + 'static,
    {
        self.handlers
            .insert(id.into(), Box::new(move |ctx| Box::pin(handler(ctx))));
        self
    }
}

#[async_trait]
impl ActivityExecutor for ClosureExecutor {
    async fn execute(&self, ctx: &ActivityContext) -> Result<Value, ActivityError> {
        match self.handlers.get(&ctx.id) {
            Some(handler) => handler(ctx.clone()).await,
            None => Err(ActivityError::Permanent(format!(
                "no handler registered for activity `{}`",
                ctx.id
            ))),
        }
    }
}
