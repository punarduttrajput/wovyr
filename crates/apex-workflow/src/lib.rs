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
//! approval), timer/event waiting states, parallel/distributed workers, and durable
//! [`store`]s (in-memory + file, with Postgres behind a feature). Activity work is
//! pluggable via the [`ActivityExecutor`] trait.
//!
//! Temporal gap-closure additions
//! ([next-phase scope](../../docs/03-workflow-engine/temporal-gap-analysis.md)):
//! **durable wall-clock timers** ([`TimerDispatcher`] over a [`TimerStore`] +
//! [`Clock`], G1), **recurring schedules** ([`ScheduleDispatcher`] over a
//! [`ScheduleStore`], G2), side-effect-free **queries** ([`Engine::query`]/
//! [`Engine::status`], G3), and **definition pinning** so `resume` rejects a drifted
//! definition (G7).

mod condition;
mod definition;
mod engine;
mod event;
mod executor;
#[cfg(feature = "postgres")]
mod postgres;
mod queue;
mod retry;
mod schedule;
mod state;
mod store;
mod timer;
mod worker;

pub use definition::{ActivityDef, Definition};
pub use engine::{Engine, ExecutionState, ExecutionSummary, RunOutcome};
pub use event::WorkflowEvent;
pub use executor::{ActivityContext, ActivityError, ActivityExecutor, ClosureExecutor};
#[cfg(feature = "postgres")]
pub use postgres::PostgresStore;
pub use queue::{InMemoryWorkQueue, WorkQueue};
pub use retry::{RetryPolicy, RetryStrategy};
pub use schedule::{
    FileScheduleStore, InMemoryScheduleStore, OverlapPolicy, Schedule, ScheduleDispatcher,
    ScheduleStore,
};
pub use state::{ActivityState, WorkflowState};
pub use store::{CheckpointStore, EventLog, FileStore, InMemoryStore};
pub use timer::{
    Clock, FileTimerStore, InMemoryTimerStore, ManualClock, PendingTimer, SystemClock,
    TimerDispatcher, TimerStore,
};
pub use worker::{DefinitionResolver, Worker};
