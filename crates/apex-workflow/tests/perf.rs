//! Performance baselines for the workflow engine and work queue (G6 scaling).
//!
//! These are **assertion-style** baselines (like
//! [`apex-provider`'s perf test](../../apex-provider/tests/perf.rs)): they measure
//! throughput on the in-process stores and assert a conservative floor with large
//! headroom (so they stay green in debug/CI), while printing the measured numbers
//! that feed the published **scaling envelope**
//! ([distributed execution §scaling envelope](../../../docs/03-workflow-engine/distributed-execution.md)).
//!
//! Methodology: trivial work (one no-op activity), in-memory stores, single process —
//! this isolates the engine/queue overhead from activity and I/O latency. The numbers
//! are a per-core software ceiling, not a distributed-throughput figure; the durable
//! Postgres queue is the horizontal-scale path.

use apex_workflow::{
    CheckpointStore, ClosureExecutor, Definition, Engine, EventLog, InMemoryStore,
    InMemoryWorkQueue, RunOutcome, WorkQueue,
};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn trivial_engine() -> (Engine, Definition) {
    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!("ok")) });
    let engine = Engine::new(events, checkpoints, Arc::new(executor));
    let def = Definition::from_yaml(
        "metadata:\n  name: bench\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .unwrap();
    (engine, def)
}

#[tokio::test]
async fn executions_per_second_baseline() {
    const N: usize = 3_000;
    // Conservative floor with large headroom; the measured number is the real signal.
    const FLOOR_PER_SEC: f64 = 300.0;

    let (engine, def) = trivial_engine();

    let start = Instant::now();
    for i in 0..N {
        let (outcome, _) = engine.run(&def, &format!("e{i}"), json!({})).await.unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
    }
    let elapsed = start.elapsed();
    let per_sec = N as f64 / elapsed.as_secs_f64();

    eprintln!(
        "engine throughput: {N} executions in {:?} = {per_sec:.0} executions/sec \
         (single core, in-memory store)",
        elapsed
    );
    assert!(
        per_sec > FLOOR_PER_SEC,
        "engine throughput {per_sec:.0}/sec below the {FLOOR_PER_SEC}/sec floor"
    );
}

#[tokio::test]
async fn lease_queue_throughput_baseline() {
    const N: usize = 3_000;
    const FLOOR_PER_SEC: f64 = 1_000.0;
    let ttl = Duration::from_secs(30);

    let queue = InMemoryWorkQueue::new();
    for i in 0..N {
        queue.enqueue(&format!("e{i:06}")).await.unwrap();
    }

    let start = Instant::now();
    let mut leased = 0usize;
    while let Some(id) = queue.lease("w", ttl).await.unwrap() {
        queue.remove(&id).await.unwrap();
        leased += 1;
    }
    let elapsed = start.elapsed();
    let per_sec = leased as f64 / elapsed.as_secs_f64();

    assert_eq!(leased, N, "every enqueued execution leased exactly once");
    eprintln!(
        "lease+remove throughput: {N} ops in {:?} = {per_sec:.0} ops/sec \
         (in-memory queue; ordered scan with early-exit, near-O(1) when leases are \
         removed promptly)",
        elapsed
    );
    assert!(
        per_sec > FLOOR_PER_SEC,
        "lease throughput {per_sec:.0}/sec below the {FLOOR_PER_SEC}/sec floor"
    );
}
