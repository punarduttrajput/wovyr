//! Performance baselines for the workflow engine and work queue (G6 scaling).
//!
//! These are **assertion-style** baselines (like
//! [`wovyr-provider`'s perf test](../../wovyr-provider/tests/perf.rs)): they measure
//! throughput on the in-process stores and assert a conservative floor with large
//! headroom (so they stay green in debug/CI), while printing the measured numbers
//! that feed the published **scaling envelope**
//! ([distributed execution §scaling envelope](../../../docs/03-workflow-engine/distributed-execution.md)).
//!
//! Methodology: trivial work (one no-op activity), in-memory stores, single process —
//! this isolates the engine/queue overhead from activity and I/O latency. The numbers
//! are a per-core software ceiling, not a distributed-throughput figure; the durable
//! Postgres queue is the horizontal-scale path.

use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wovyr_workflow::{
    CheckpointStore, ClosureExecutor, Definition, Engine, EventLog, FileStore, InMemoryStore,
    InMemoryWorkQueue, RunOutcome, WorkQueue, WorkflowEvent,
};

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

/// RM-GA-P2 DUR-402: `FileStore::append` used to recompute its sequence number
/// by re-reading and re-splitting the *entire* event file on every single
/// call — O(events) per append, O(N^2) over an execution's lifetime. It now
/// keeps an in-process monotonic counter and only re-reads the file once
/// (per execution, per `FileStore` instance) to seed it. This asserts the
/// fix held: appending a fixed-size batch late in a long-lived log must not
/// cost meaningfully more than the same-sized batch early on.
#[tokio::test]
async fn file_event_log_append_latency_does_not_grow_with_log_length() {
    let dir =
        std::env::temp_dir().join(format!("wovyr_workflow_perf_append_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FileStore::new(&dir).unwrap();
    let exec_id = "perf-append";

    const WARMUP: usize = 50;
    const BATCH: usize = 200;
    const GROWTH: usize = 2_000;

    for _ in 0..WARMUP {
        store
            .append(exec_id, WorkflowEvent::WorkflowStarted)
            .await
            .unwrap();
    }

    let early_start = Instant::now();
    for _ in 0..BATCH {
        store
            .append(exec_id, WorkflowEvent::WorkflowStarted)
            .await
            .unwrap();
    }
    let early = early_start.elapsed();

    // Grow the log well past its earlier size, then re-measure a same-sized batch.
    for _ in 0..GROWTH {
        store
            .append(exec_id, WorkflowEvent::WorkflowStarted)
            .await
            .unwrap();
    }

    let late_start = Instant::now();
    for _ in 0..BATCH {
        store
            .append(exec_id, WorkflowEvent::WorkflowStarted)
            .await
            .unwrap();
    }
    let late = late_start.elapsed();

    eprintln!(
        "event log append: {BATCH} appends at ~{WARMUP} prior events = {early:?}, \
         {BATCH} appends at ~{} prior events = {late:?}",
        WARMUP + BATCH + GROWTH
    );

    // A conservative ratio with real headroom for fsync jitter: with the old
    // O(N) re-read-the-whole-file-every-append behavior, the log is ~45x
    // longer by the second batch, so append cost would grow roughly
    // proportionally. With the O(1) warm path, it should not grow at all
    // beyond fsync noise.
    assert!(
        late.as_secs_f64() < early.as_secs_f64() * 5.0 + 0.05,
        "append latency grew from {early:?} (early, ~{WARMUP} events) to {late:?} \
         (late, ~{} events) — looks quadratic again",
        WARMUP + BATCH + GROWTH
    );

    let _ = std::fs::remove_dir_all(&dir);
}
