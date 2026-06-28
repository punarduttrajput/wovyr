//! Durable workflow engine.
//!
//! Implements the v0.2 core of the
//! [Workflow Engine](../../docs/03-workflow-engine/overview.md): workflows are
//! executed as **durable, event-sourced state machines**
//! ([execution model](../../docs/03-workflow-engine/execution-model.md)). Progress
//! survives process restarts via [checkpointing](../../docs/03-workflow-engine/checkpointing-specification.md),
//! and transient activity failures are recovered by the
//! [retry engine](../../docs/03-workflow-engine/retry-engine.md).
//!
//! v0.2 slice scope: a [`Definition`] (YAML DSL) compiled and validated into a DAG,
//! a deterministic scheduler ([`Engine`]) that runs ready activities, conditional
//! branching (guarded transitions with branch skipping), per-activity retry, saga
//! compensation, durable suspend/resume (the `Interrupted` waiting state, e.g. human
//! approval), and durable [`store`]s (in-memory + file). Activity work is pluggable
//! via the [`ActivityExecutor`] trait. **Deferred:** timer/event waiting states,
//! parallel/distributed workers, and Postgres-backed persistence.

mod condition;
mod definition;
mod engine;
mod event;
mod executor;
#[cfg(feature = "postgres")]
mod postgres;
mod queue;
mod retry;
mod state;
mod store;
mod worker;

pub use definition::{ActivityDef, Definition};
pub use engine::{Engine, ExecutionState, RunOutcome};
pub use event::WorkflowEvent;
pub use executor::{ActivityContext, ActivityError, ActivityExecutor, ClosureExecutor};
#[cfg(feature = "postgres")]
pub use postgres::PostgresStore;
pub use queue::{InMemoryWorkQueue, WorkQueue};
pub use retry::{RetryPolicy, RetryStrategy};
pub use state::{ActivityState, WorkflowState};
pub use store::{CheckpointStore, EventLog, FileStore, InMemoryStore};
pub use worker::{DefinitionResolver, Worker};
