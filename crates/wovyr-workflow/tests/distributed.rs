//! Distributed workers over the in-memory queue (offline): two workers sharing a
//! queue + store must process each submitted execution exactly once.

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use wovyr_workflow::{
    CheckpointStore, ClosureExecutor, Definition, DefinitionResolver, Engine, InMemoryStore,
    InMemoryWorkQueue, RunOutcome, WorkQueue, Worker, WorkflowState,
};

fn linear_def() -> Definition {
    Definition::from_yaml(
        "metadata:\n  name: dist\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n  transitions:\n    - {from: a, to: b}\n",
    )
    .unwrap()
}

/// An executor whose activities bump a shared counter (so double-execution is visible).
fn counting_executor(runs: Arc<AtomicUsize>) -> ClosureExecutor {
    let mut ex = ClosureExecutor::new();
    for id in ["a", "b"] {
        let runs = runs.clone();
        ex = ex.on(id, move |_| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(json!({}))
            }
        });
    }
    ex
}

#[tokio::test]
async fn two_workers_process_each_execution_exactly_once() {
    const N: usize = 8;
    let def = linear_def();
    let store = InMemoryStore::new();
    let queue = Arc::new(InMemoryWorkQueue::new());
    let runs = Arc::new(AtomicUsize::new(0));

    // Submit N executions: durably start each (no activities run yet), then enqueue.
    let submitter = Engine::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        Arc::new(counting_executor(runs.clone())),
    );
    for i in 0..N {
        let id = format!("wf-{i}");
        submitter.start(&def, &id, json!({})).await.unwrap();
        queue.enqueue(&id).await.unwrap();
    }

    let resolver: DefinitionResolver = {
        let def = def.clone();
        Arc::new(move |name: &str| (name == "dist").then(|| def.clone()))
    };
    let make_worker = |id: &str| {
        Worker::new(
            id,
            Engine::new(
                Arc::new(store.clone()),
                Arc::new(store.clone()),
                Arc::new(counting_executor(runs.clone())),
            ),
            queue.clone() as Arc<dyn WorkQueue>,
            Arc::new(store.clone()),
            resolver.clone(),
        )
    };
    let w1 = make_worker("w1");
    let w2 = make_worker("w2");

    // Two workers drain the queue concurrently.
    let (n1, n2) = tokio::join!(w1.run_until_idle(), w2.run_until_idle());
    let processed = n1.unwrap() + n2.unwrap();

    assert_eq!(
        processed, N,
        "every execution processed exactly once across workers"
    );
    // Two activities per workflow, each run exactly once.
    assert_eq!(runs.load(Ordering::SeqCst), N * 2, "no activity ran twice");

    // The queue is fully drained and every execution reached a terminal Completed state.
    assert!(
        queue
            .lease("w3", std::time::Duration::from_secs(1))
            .await
            .unwrap()
            .is_none(),
        "queue drained"
    );
    for i in 0..N {
        let state = CheckpointStore::latest(&store, &format!("wf-{i}"))
            .await
            .unwrap()
            .expect("checkpoint exists");
        assert_eq!(state.status, WorkflowState::Completed);
    }
}

#[tokio::test]
async fn crashed_worker_lease_is_reclaimed_and_completed() {
    let def = linear_def();
    let store = InMemoryStore::new();
    let queue = Arc::new(InMemoryWorkQueue::new());
    let runs = Arc::new(AtomicUsize::new(0));

    let submitter = Engine::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        Arc::new(counting_executor(runs.clone())),
    );
    submitter.start(&def, "wf-crash", json!({})).await.unwrap();
    queue.enqueue("wf-crash").await.unwrap();

    // A worker leases it with a short ttl, then "crashes": it never drives or removes.
    let leased = queue
        .lease("dead", Duration::from_millis(40))
        .await
        .unwrap();
    assert_eq!(leased.as_deref(), Some("wf-crash"));
    tokio::time::sleep(Duration::from_millis(60)).await; // lease lapses

    // A live worker reclaims the expired lease and completes the execution.
    let resolver: DefinitionResolver = {
        let def = def.clone();
        Arc::new(move |name: &str| (name == "dist").then(|| def.clone()))
    };
    let live = Worker::new(
        "live",
        Engine::new(
            Arc::new(store.clone()),
            Arc::new(store.clone()),
            Arc::new(counting_executor(runs.clone())),
        ),
        queue.clone() as Arc<dyn WorkQueue>,
        Arc::new(store.clone()),
        resolver,
    );
    let (id, outcome) = live
        .step()
        .await
        .unwrap()
        .expect("reclaimed the expired lease");
    assert_eq!(id, "wf-crash");
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(runs.load(Ordering::SeqCst), 2, "a,b ran once after reclaim");
}
