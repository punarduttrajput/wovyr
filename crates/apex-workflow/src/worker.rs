//! Distributed workflow worker: lease an execution, drive it, release it.
//!
//! Multiple workers share a [`WorkQueue`] over the same durable store. Each leases a
//! ready execution, resolves its [`Definition`] (by workflow name), drives it with the
//! engine's idempotent [`resume`](crate::Engine::resume) to its next stop (completion
//! or a suspend point), and removes it from the queue. Because completed activities
//! are never re-run, a worker that dies mid-execution loses only its lease — another
//! worker reclaims the execution once the lease expires and resumes from the last
//! checkpoint. This gives **at-least-once dispatch with exactly-once activity effects**.

use crate::definition::Definition;
use crate::engine::{Engine, RunOutcome};
use crate::queue::WorkQueue;
use crate::store::CheckpointStore;
use apex_common::{Error, Result};
use std::sync::Arc;
use std::time::Duration;

/// Resolves a workflow [`Definition`] by name — workers carry one so they can drive
/// any execution they lease.
pub type DefinitionResolver = Arc<dyn Fn(&str) -> Option<Definition> + Send + Sync>;

/// A worker that drives leased executions from a shared [`WorkQueue`].
pub struct Worker {
    id: String,
    engine: Engine,
    queue: Arc<dyn WorkQueue>,
    checkpoints: Arc<dyn CheckpointStore>,
    resolver: DefinitionResolver,
    lease_ttl: Duration,
}

impl Worker {
    /// Build a worker identified by `id` over `engine`/`queue`, reading execution
    /// metadata from `checkpoints` and resolving definitions via `resolver`.
    pub fn new(
        id: impl Into<String>,
        engine: Engine,
        queue: Arc<dyn WorkQueue>,
        checkpoints: Arc<dyn CheckpointStore>,
        resolver: DefinitionResolver,
    ) -> Self {
        Self {
            id: id.into(),
            engine,
            queue,
            checkpoints,
            resolver,
            lease_ttl: Duration::from_secs(30),
        }
    }

    /// Override the lease duration (default 30s).
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    /// Lease and drive one ready execution. Returns `Some((execution_id, outcome))`
    /// if it processed one, or `None` if the queue was empty.
    pub async fn step(&self) -> Result<Option<(String, RunOutcome)>> {
        let Some(execution_id) = self.queue.lease(&self.id, self.lease_ttl).await? else {
            return Ok(None);
        };

        // Resolve the definition for the leased execution from its checkpoint.
        let state = self
            .checkpoints
            .latest(&execution_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("no checkpoint for `{execution_id}`")))?;
        let def = (self.resolver)(&state.workflow_name).ok_or_else(|| {
            Error::NotFound(format!(
                "worker has no definition for workflow `{}`",
                state.workflow_name
            ))
        })?;

        // Drive to the next stop. On error we leave the lease to expire so another
        // worker retries; on success (terminal or suspended) we leave the ready queue.
        let (outcome, _) = self.engine.resume(&def, &execution_id).await?;
        self.queue.remove(&execution_id).await?;
        Ok(Some((execution_id, outcome)))
    }

    /// Drive ready executions until the queue is drained; returns how many were
    /// processed. A long-running worker calls this on a poll interval.
    pub async fn run_until_idle(&self) -> Result<usize> {
        let mut processed = 0;
        while self.step().await?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }
}
