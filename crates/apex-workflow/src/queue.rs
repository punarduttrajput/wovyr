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
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The partition (shard) an execution belongs to over `total` partitions — a stable,
/// dependency-free FNV-1a hash of the id, modulo `total`. Used to spread executions
/// across partitions so worker pools serving disjoint partitions never contend on the
/// same rows ([G6 scaling](../../docs/03-workflow-engine/temporal-gap-analysis.md#g6--horizontal-scaling-story-honest-tiering)).
pub fn shard_of(execution_id: &str, total: u32) -> u32 {
    if total <= 1 {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in execution_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h % total as u64) as u32
}

/// Which queue partitions a worker pool serves. Pools with disjoint `owned` sets
/// lease non-overlapping executions, removing cross-pool contention.
#[derive(Clone, Debug)]
pub struct PartitionAssignment {
    /// Total partitions the queue is sharded into. Must agree across all pools (and,
    /// for the Postgres queue, with the store's configured partition count).
    pub total: u32,
    /// The partition indices this pool serves (a subset of `0..total`).
    pub owned: Vec<u32>,
}

impl PartitionAssignment {
    /// The assignment for pool `pool_index` of `pool_count`, evenly striping `total`
    /// partitions across the pools (partition `p` is served by pool `p % pool_count`).
    pub fn for_pool(pool_index: u32, pool_count: u32, total: u32) -> Self {
        let owned = (0..total)
            .filter(|p| pool_count == 0 || p % pool_count == pool_index)
            .collect();
        Self { total, owned }
    }

    /// Whether this assignment serves `shard`.
    pub fn owns(&self, shard: u32) -> bool {
        self.owned.contains(&shard)
    }
}

/// A shared queue of executions awaiting work, with per-execution leasing.
#[async_trait]
pub trait WorkQueue: Send + Sync {
    /// Add an execution to the ready queue. Idempotent — a no-op if already queued.
    async fn enqueue(&self, execution_id: &str) -> Result<()>;

    /// Lease the next ready execution (unleased or lease-expired) for `worker`,
    /// holding it for `ttl`. Returns the execution id, or `None` if none are ready.
    async fn lease(&self, worker: &str, ttl: Duration) -> Result<Option<String>>;

    /// Like [`lease`](Self::lease) but only considers executions in partitions owned
    /// by `assignment` — so worker pools serving disjoint partitions never contend on
    /// the same rows (G6 scaling). The default serves all partitions (single pool).
    async fn lease_sharded(
        &self,
        worker: &str,
        assignment: &PartitionAssignment,
        ttl: Duration,
    ) -> Result<Option<String>>;

    /// Extend `worker`'s lease on `execution_id` by `ttl` (heartbeat for long runs).
    async fn renew(&self, execution_id: &str, worker: &str, ttl: Duration) -> Result<()>;

    /// Remove `execution_id` from the queue — the worker finished this round (the
    /// workflow completed, failed, or suspended awaiting a signal).
    async fn remove(&self, execution_id: &str) -> Result<()>;
}

/// An in-process [`WorkQueue`] for tests and single-node use. Cloning shares state.
/// Entries are kept in a `BTreeMap` keyed by execution id, so a lease finds the
/// lowest-id ready entry by an early-exiting ordered scan (no per-call sort).
#[derive(Clone, Default)]
pub struct InMemoryWorkQueue {
    inner: std::sync::Arc<Mutex<BTreeMap<String, Lease>>>,
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

    /// Lease the lowest-id ready execution accepted by `owns`, claiming it for
    /// `worker`. Shared by [`lease`](WorkQueue::lease) (accept all) and
    /// [`lease_sharded`](WorkQueue::lease_sharded) (accept owned partitions).
    fn lease_where(
        &self,
        worker: &str,
        ttl: Duration,
        owns: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let now = Instant::now();
        let mut q = self.inner.lock().expect("queue mutex poisoned");
        // The BTreeMap iterates by ascending id; take the first free/expired entry in
        // scope and stop — deterministic (lowest id) without sorting every call.
        let id = q
            .iter()
            .find(|(id, l)| {
                (l.worker.is_none() || l.expires_at.is_some_and(|e| e <= now)) && owns(id)
            })
            .map(|(id, _)| id.clone())?;
        q.insert(
            id.clone(),
            Lease {
                worker: Some(worker.to_string()),
                expires_at: Some(now + ttl),
            },
        );
        Some(id)
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
        Ok(self.lease_where(worker, ttl, |_| true))
    }

    async fn lease_sharded(
        &self,
        worker: &str,
        assignment: &PartitionAssignment,
        ttl: Duration,
    ) -> Result<Option<String>> {
        Ok(self.lease_where(worker, ttl, |id| {
            assignment.owns(shard_of(id, assignment.total))
        }))
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
    async fn sharded_pools_lease_disjoint_executions() {
        let q = InMemoryWorkQueue::new();
        let total = 4;
        for i in 0..20 {
            q.enqueue(&format!("e{i:02}")).await.unwrap();
        }
        // Two pools over four partitions: pool 0 owns {0,2}, pool 1 owns {1,3}.
        let a0 = PartitionAssignment::for_pool(0, 2, total);
        let a1 = PartitionAssignment::for_pool(1, 2, total);

        let mut p0 = Vec::new();
        while let Some(id) = q
            .lease_sharded("w0", &a0, Duration::from_secs(30))
            .await
            .unwrap()
        {
            p0.push(id);
        }
        let mut p1 = Vec::new();
        while let Some(id) = q
            .lease_sharded("w1", &a1, Duration::from_secs(30))
            .await
            .unwrap()
        {
            p1.push(id);
        }

        // Each pool only leased executions in its own partitions...
        for id in &p0 {
            assert!(a0.owns(shard_of(id, total)), "{id} not owned by pool 0");
        }
        for id in &p1 {
            assert!(a1.owns(shard_of(id, total)), "{id} not owned by pool 1");
        }
        // ...and together they covered every execution exactly once (no contention).
        let mut all: Vec<String> = p0.into_iter().chain(p1).collect();
        let leased = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 20, "every execution leased");
        assert_eq!(leased, 20, "no execution leased by two pools");
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
