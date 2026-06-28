//! Distributed work queue: leased hand-off of executions to workers.
//!
//! For horizontal scaling, multiple [`Worker`](crate::Worker)s pull executions that
//! have pending work from a shared queue. A [`WorkQueue`] hands each ready execution
//! to **one** worker via a time-bounded **lease**; if that worker dies, the lease
//! expires and another worker reclaims it (the engine's idempotent `resume` re-drives
//! from the last checkpoint). Backed by [`InMemoryWorkQueue`] (tests) or the Postgres
//! store (cross-node) — see [`PostgresStore`](crate::PostgresStore).

use apex_common::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A shared queue of executions awaiting work, with per-execution leasing.
#[async_trait]
pub trait WorkQueue: Send + Sync {
    /// Add an execution to the ready queue. Idempotent — a no-op if already queued.
    async fn enqueue(&self, execution_id: &str) -> Result<()>;

    /// Lease the next ready execution (unleased or lease-expired) for `worker`,
    /// holding it for `ttl`. Returns the execution id, or `None` if none are ready.
    async fn lease(&self, worker: &str, ttl: Duration) -> Result<Option<String>>;

    /// Extend `worker`'s lease on `execution_id` by `ttl` (heartbeat for long runs).
    async fn renew(&self, execution_id: &str, worker: &str, ttl: Duration) -> Result<()>;

    /// Remove `execution_id` from the queue — the worker finished this round (the
    /// workflow completed, failed, or suspended awaiting a signal).
    async fn remove(&self, execution_id: &str) -> Result<()>;
}

/// An in-process [`WorkQueue`] for tests and single-node use. Cloning shares state.
#[derive(Clone, Default)]
pub struct InMemoryWorkQueue {
    inner: std::sync::Arc<Mutex<HashMap<String, Lease>>>,
}

#[derive(Clone)]
struct Lease {
    worker: Option<String>,
    expires_at: Option<Instant>,
}

impl InMemoryWorkQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl WorkQueue for InMemoryWorkQueue {
    async fn enqueue(&self, execution_id: &str) -> Result<()> {
        let mut q = self.inner.lock().expect("queue mutex poisoned");
        q.entry(execution_id.to_string()).or_insert(Lease {
            worker: None,
            expires_at: None,
        });
        Ok(())
    }

    async fn lease(&self, worker: &str, ttl: Duration) -> Result<Option<String>> {
        let now = Instant::now();
        let mut q = self.inner.lock().expect("queue mutex poisoned");
        // Deterministic pick: lowest id among free/expired entries.
        let mut ids: Vec<String> = q
            .iter()
            .filter(|(_, l)| l.worker.is_none() || l.expires_at.is_some_and(|e| e <= now))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        let Some(id) = ids.into_iter().next() else {
            return Ok(None);
        };
        q.insert(
            id.clone(),
            Lease {
                worker: Some(worker.to_string()),
                expires_at: Some(now + ttl),
            },
        );
        Ok(Some(id))
    }

    async fn renew(&self, execution_id: &str, worker: &str, ttl: Duration) -> Result<()> {
        let mut q = self.inner.lock().expect("queue mutex poisoned");
        if let Some(lease) = q.get_mut(execution_id) {
            if lease.worker.as_deref() == Some(worker) {
                lease.expires_at = Some(Instant::now() + ttl);
            }
        }
        Ok(())
    }

    async fn remove(&self, execution_id: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("queue mutex poisoned")
            .remove(execution_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lease_is_exclusive_until_removed() {
        let q = InMemoryWorkQueue::new();
        q.enqueue("e1").await.unwrap();

        // One worker leases it; a second worker sees nothing ready.
        let a = q.lease("w1", Duration::from_secs(30)).await.unwrap();
        assert_eq!(a.as_deref(), Some("e1"));
        assert!(
            q.lease("w2", Duration::from_secs(30))
                .await
                .unwrap()
                .is_none()
        );

        // After the worker removes it, the queue is empty.
        q.remove("e1").await.unwrap();
        assert!(
            q.lease("w2", Duration::from_secs(30))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn expired_lease_is_reclaimable() {
        let q = InMemoryWorkQueue::new();
        q.enqueue("e1").await.unwrap();
        // Lease briefly, let it lapse — another worker reclaims it.
        assert_eq!(
            q.lease("w1", Duration::from_millis(40))
                .await
                .unwrap()
                .as_deref(),
            Some("e1")
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            q.lease("w2", Duration::from_secs(30))
                .await
                .unwrap()
                .as_deref(),
            Some("e1"),
            "an expired lease should be reclaimable by another worker"
        );
    }

    #[tokio::test]
    async fn leases_distinct_executions_round_robin() {
        let q = InMemoryWorkQueue::new();
        for id in ["e1", "e2", "e3"] {
            q.enqueue(id).await.unwrap();
        }
        let mut leased = Vec::new();
        for w in ["w1", "w2", "w3"] {
            leased.push(q.lease(w, Duration::from_secs(30)).await.unwrap().unwrap());
        }
        leased.sort();
        assert_eq!(leased, vec!["e1", "e2", "e3"], "each execution leased once");
        assert!(
            q.lease("w4", Duration::from_secs(30))
                .await
                .unwrap()
                .is_none()
        );
    }
}
