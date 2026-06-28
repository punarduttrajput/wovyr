//! Integration tests for the workflow engine: DAG execution, retry, and — the
//! keystone — durable resume across engine instances without re-running completed
//! activities ([recovery model](../../../docs/03-workflow-engine/execution-model.md#16-recovery-model)).

use apex_workflow::{
    ActivityError, CheckpointStore, ClosureExecutor, Definition, Engine, EventLog, FileStore,
    InMemoryStore, RunOutcome,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

fn engine_with(store: InMemoryStore, executor: ClosureExecutor) -> Engine {
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    Engine::new(events, checkpoints, Arc::new(executor))
}

#[tokio::test]
async fn runs_linear_workflow_to_completion() {
    let def = Definition::from_yaml(
        "metadata:\n  name: linear\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n  transitions:\n    - {from: a, to: b}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new()
        .on("a", |_| async { Ok(json!({"step": "a"})) })
        .on("b", |ctx| async move {
            // b can see a's output via variables.
            assert_eq!(ctx.variables.get("a"), Some(&json!({"step": "a"})));
            Ok(json!({"step": "b"}))
        });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine.run(&def, "wf-linear-1", json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.variables.get("b"), Some(&json!({"step": "b"})));
}

#[tokio::test]
async fn diamond_respects_dependency_order() {
    // a -> {b, c} -> d
    let def = Definition::from_yaml(
        "metadata:\n  name: diamond\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n    - {id: c, type: function}\n    - {id: d, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: a, to: c}\n    - {from: b, to: d}\n    - {from: c, to: d}\n",
    )
    .unwrap();

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut executor = ClosureExecutor::new();
    for id in ["a", "b", "c", "d"] {
        let order = order.clone();
        executor = executor.on(id, move |ctx| {
            let order = order.clone();
            async move {
                order.lock().unwrap().push(ctx.id.clone());
                Ok(Value::Null)
            }
        });
    }

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, _) = engine.run(&def, "wf-diamond-1", json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let order = order.lock().unwrap().clone();
    assert_eq!(order.len(), 4);
    assert_eq!(order[0], "a", "a must run first");
    assert_eq!(order[3], "d", "d must run last");
    // b and c run after a and before d.
    assert!(order[1..3].contains(&"b".to_string()));
    assert!(order[1..3].contains(&"c".to_string()));
}

#[tokio::test]
async fn retries_transient_failures_then_succeeds() {
    let def = Definition::from_yaml(
        "metadata:\n  name: retry\nspec:\n  retry: {maxAttempts: 5, strategy: fixed, initialDelayMs: 1, maxDelayMs: 10}\n  activities:\n    - {id: flaky, type: function}\n",
    )
    .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let executor = ClosureExecutor::new().on("flaky", {
        let calls = calls.clone();
        move |_| {
            let calls = calls.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(ActivityError::Retryable("transient outage".into()))
                } else {
                    Ok(json!({"ok": true}))
                }
            }
        }
    });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine.run(&def, "wf-retry-1", json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "failed twice, succeeded on third"
    );
    assert_eq!(state.activities["flaky"].attempts, 3);
}

#[tokio::test]
async fn permanent_failure_fails_the_workflow() {
    let def = Definition::from_yaml(
        "metadata:\n  name: boom\nspec:\n  activities:\n    - {id: x, type: function}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new().on("x", |_| async {
        Err(ActivityError::Permanent("bad input".into()))
    });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, _) = engine.run(&def, "wf-fail-1", json!({})).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Failed(_)));
}

#[tokio::test]
async fn failure_rolls_back_completed_activities_in_reverse_order() {
    // reserve -> charge -> ship(fails). Completed reserve+charge must be
    // compensated in reverse: refund (for charge) then release (for reserve).
    let def = Definition::from_yaml(
        "metadata:\n  name: saga\nspec:\n  activities:\n    - {id: reserve, type: function, compensate: release}\n    - {id: charge, type: function, compensate: refund}\n    - {id: ship, type: function}\n    - {id: release, type: function}\n    - {id: refund, type: function}\n  transitions:\n    - {from: reserve, to: charge}\n    - {from: charge, to: ship}\n",
    )
    .unwrap();

    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut executor = ClosureExecutor::new();
    for id in ["reserve", "charge", "release", "refund"] {
        let log = log.clone();
        executor = executor.on(id, move |ctx| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(ctx.id.clone());
                Ok(Value::Null)
            }
        });
    }
    // `ship` fails permanently, triggering rollback.
    executor = executor.on("ship", |_| async {
        Err(ActivityError::Permanent("carrier rejected shipment".into()))
    });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine.run(&def, "wf-saga-1", json!({})).await.unwrap();

    assert!(
        matches!(outcome, RunOutcome::Compensated(_)),
        "got {outcome:?}"
    );
    // Forward work + reverse-order compensation.
    let order = log.lock().unwrap().clone();
    assert_eq!(order, vec!["reserve", "charge", "refund", "release"]);
    // After a clean rollback the workflow ends Completed (compensating -> completed).
    assert_eq!(state.status, apex_workflow::WorkflowState::Completed);
}

#[tokio::test]
async fn durable_resume_does_not_reexecute_completed_activities() {
    // a -> b -> c. The first engine completes `a`, then is "interrupted" on `b`
    // (simulating a worker crash). A fresh engine resumes from the on-disk
    // checkpoint and finishes b, c — without re-running the already-completed `a`.
    let def = Definition::from_yaml(
        "metadata:\n  name: durable\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n    - {id: c, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: b, to: c}\n",
    )
    .unwrap();

    let dir = std::env::temp_dir().join(format!("apex-wf-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let exec_id = "wf-durable-1";

    let a_runs = Arc::new(AtomicUsize::new(0));

    // --- Engine 1: completes `a`, interrupts on `b`. ---
    {
        let store = FileStore::new(&dir).unwrap();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let executor = ClosureExecutor::new()
            .on("a", {
                let a_runs = a_runs.clone();
                move |_| {
                    let a_runs = a_runs.clone();
                    async move {
                        a_runs.fetch_add(1, Ordering::SeqCst);
                        Ok(json!({"a": true}))
                    }
                }
            })
            .on("b", |_| async {
                Err(ActivityError::Interrupted("worker crash".into()))
            });

        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.run(&def, exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(
            state.activities["a"].state,
            apex_workflow::ActivityState::Completed
        );
    }

    // --- Engine 2: a fresh instance resumes from the durable checkpoint. ---
    {
        let store = FileStore::new(&dir).unwrap();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        // `a` would panic the test if re-run; it must not be.
        let executor = ClosureExecutor::new()
            .on("a", |_| async {
                panic!("completed activity `a` must not be re-executed")
            })
            .on("b", |_| async { Ok(json!({"b": true})) })
            .on("c", |_| async { Ok(json!({"c": true})) });

        let engine = Engine::new(events, checkpoints.clone(), Arc::new(executor));
        let (outcome, state) = engine.resume(&def, exec_id).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(
            state.activities["c"].state,
            apex_workflow::ActivityState::Completed
        );

        // Durable audit trail survived the "restart".
        let log = checkpoints.latest(exec_id).await.unwrap().unwrap();
        assert_eq!(log.status, apex_workflow::WorkflowState::Completed);
    }

    assert_eq!(
        a_runs.load(Ordering::SeqCst),
        1,
        "completed activity re-executed!"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
