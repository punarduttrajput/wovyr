//! Integration tests for the workflow engine: DAG execution, retry, and — the
//! keystone — durable resume across engine instances without re-running completed
//! activities ([recovery model](../../../docs/03-workflow-engine/execution-model.md#16-recovery-model)).

use apex_workflow::{
    ActivityError, ActivityState, CheckpointStore, ClosureExecutor, Definition, Engine, EventLog,
    FileStore, InMemoryStore, RunOutcome, WorkflowEvent, WorkflowState,
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

#[tokio::test]
async fn conditional_branch_takes_one_edge_and_skips_the_other() {
    // triage routes to `refund` or `reply` by the ticket intent.
    let def = Definition::from_yaml(
        "metadata:\n  name: branch\nspec:\n  activities:\n    - {id: triage, type: function}\n    - {id: refund, type: function}\n    - {id: reply, type: function}\n  transitions:\n    - {from: triage, to: refund, when: \"input.intent == 'refund'\"}\n    - {from: triage, to: reply, when: \"input.intent != 'refund'\"}\n",
    )
    .unwrap();

    let ran = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut executor = ClosureExecutor::new();
    for id in ["triage", "refund", "reply"] {
        let ran = ran.clone();
        executor = executor.on(id, move |ctx| {
            let ran = ran.clone();
            async move {
                ran.lock().unwrap().push(ctx.id.clone());
                Ok(Value::Null)
            }
        });
    }

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine
        .run(&def, "wf-branch-1", json!({"intent": "refund"}))
        .await
        .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.activities["refund"].state, ActivityState::Completed);
    assert_eq!(state.activities["reply"].state, ActivityState::Skipped);
    let ran = ran.lock().unwrap().clone();
    assert_eq!(
        ran,
        vec!["triage", "refund"],
        "the reply branch must not run"
    );
}

#[tokio::test]
async fn human_task_suspends_and_resumes_durably() {
    // start -> approve (human) -> finish. `approve` interrupts until a decision is
    // injected into the durable checkpoint, modelling human-in-the-loop approval.
    let def = Definition::from_yaml(
        "metadata:\n  name: approval\nspec:\n  activities:\n    - {id: start, type: function}\n    - {id: approve, type: human}\n    - {id: finish, type: function}\n  transitions:\n    - {from: start, to: approve}\n    - {from: approve, to: finish}\n",
    )
    .unwrap();

    fn executor() -> ClosureExecutor {
        ClosureExecutor::new()
            .on("start", |_| async { Ok(json!({"ok": true})) })
            .on("approve", |ctx| async move {
                match ctx.variables.get("decision") {
                    Some(d) => Ok(d.clone()),
                    None => Err(ActivityError::Interrupted("awaiting approval".into())),
                }
            })
            .on("finish", |_| async { Ok(json!({"sent": true})) })
    }

    let dir = std::env::temp_dir().join(format!("apex-wf-human-{}", std::process::id()));
    let store = FileStore::new(&dir).unwrap();
    let exec_id = "wf-approval-1";

    // First run suspends on the human task.
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let engine = Engine::new(events, checkpoints, Arc::new(executor()));
        let (outcome, state) = engine.run(&def, exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(state.activities["start"].state, ActivityState::Completed);
        assert_ne!(state.activities["finish"].state, ActivityState::Completed);
    }

    // A human approves: inject the decision into the durable checkpoint.
    {
        let mut cp = store.latest(exec_id).await.unwrap().unwrap();
        cp.variables
            .insert("decision".to_string(), json!("approved"));
        store.save(&cp).await.unwrap();
    }

    // A fresh engine (simulating a restart) resumes and completes.
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let engine = Engine::new(events, checkpoints, Arc::new(executor()));
        let (outcome, state) = engine.resume(&def, exec_id).await.unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(state.activities["approve"].state, ActivityState::Completed);
        assert_eq!(state.activities["finish"].state, ActivityState::Completed);
        assert_eq!(state.variables.get("approve"), Some(&json!("approved")));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn event_wait_suspends_then_resumes_on_signal() {
    // start -> gate (wait for event 'approval') -> finish. The wait is handled by
    // the engine: it suspends durably until `signal_event` delivers the event.
    let def = Definition::from_yaml(
        "metadata:\n  name: eventwait\nspec:\n  activities:\n    - {id: start, type: function}\n    - {id: gate, type: wait, inputs: {event: approval}}\n    - {id: finish, type: function}\n  transitions:\n    - {from: start, to: gate}\n    - {from: gate, to: finish}\n",
    )
    .unwrap();

    fn executor() -> ClosureExecutor {
        // The engine handles `gate` itself; only the function activities need handlers.
        ClosureExecutor::new()
            .on("start", |_| async { Ok(json!({"ok": true})) })
            .on("finish", |_| async { Ok(json!({"sent": true})) })
    }

    let dir = std::env::temp_dir().join(format!("apex-wf-event-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FileStore::new(&dir).unwrap();
    let exec_id = "wf-event-1";

    // First run suspends at the wait.
    {
        let engine = Engine::new(
            Arc::new(store.clone()) as Arc<dyn EventLog>,
            Arc::new(store.clone()) as Arc<dyn CheckpointStore>,
            Arc::new(executor()),
        );
        let (outcome, state) = engine.run(&def, exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(state.activities["gate"].state, ActivityState::Waiting);
        assert_ne!(state.activities["finish"].state, ActivityState::Completed);
    }

    // A fresh engine delivers the event → resumes and completes.
    {
        let engine = Engine::new(
            Arc::new(store.clone()) as Arc<dyn EventLog>,
            Arc::new(store.clone()) as Arc<dyn CheckpointStore>,
            Arc::new(executor()),
        );
        let (outcome, state) = engine
            .signal_event(&def, exec_id, "approval", json!({"by": "alice"}))
            .await
            .unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(state.activities["gate"].state, ActivityState::Completed);
        assert_eq!(state.activities["finish"].state, ActivityState::Completed);
        // The wait exposes the delivered payload as its output.
        assert_eq!(state.variables.get("gate"), Some(&json!({"by": "alice"})));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn timer_wait_suspends_then_resumes_on_fire() {
    let def = Definition::from_yaml(
        "metadata:\n  name: timerwait\nspec:\n  activities:\n    - {id: gate, type: wait, inputs: {timer: deadline}}\n    - {id: finish, type: function}\n  transitions:\n    - {from: gate, to: finish}\n",
    )
    .unwrap();

    let executor = || ClosureExecutor::new().on("finish", |_| async { Ok(json!({"done": true})) });
    let store = InMemoryStore::new();
    let exec_id = "wf-timer-1";

    let engine = Engine::new(
        Arc::new(store.clone()) as Arc<dyn EventLog>,
        Arc::new(store.clone()) as Arc<dyn CheckpointStore>,
        Arc::new(executor()),
    );

    // Starts blocked on the timer.
    let (outcome, state) = engine.run(&def, exec_id, json!({})).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert_eq!(state.activities["gate"].state, ActivityState::Waiting);

    // Firing the timer resumes and completes the workflow.
    let (outcome, state) = engine.fire_timer(&def, exec_id, "deadline").await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.activities["finish"].state, ActivityState::Completed);
}

/// The two independent branches of a diamond run **concurrently**, not one-after-the-
/// other. A `Barrier(2)` that both `b` and `c` must reach can only be cleared if they
/// overlap in time — a sequential scheduler would deadlock (caught by the timeout).
#[tokio::test]
async fn parallel_branches_run_concurrently() {
    let def = Definition::from_yaml(
        "metadata:\n  name: diamond\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n    - {id: c, type: function}\n    - {id: d, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: a, to: c}\n    - {from: b, to: d}\n    - {from: c, to: d}\n",
    )
    .unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut executor = ClosureExecutor::new()
        .on("a", |_| async { Ok(Value::Null) })
        .on("d", |_| async { Ok(Value::Null) });
    for id in ["b", "c"] {
        let barrier = barrier.clone();
        executor = executor.on(id, move |ctx| {
            let barrier = barrier.clone();
            async move {
                // Rendezvous: both branches must be in-flight at once to pass.
                barrier.wait().await;
                Ok(json!({"id": ctx.id}))
            }
        });
    }

    let engine = engine_with(InMemoryStore::new(), executor);
    let run = engine.run(&def, "wf-parallel-1", json!({}));
    let (outcome, _) = tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("branches must run concurrently (sequential would deadlock on the barrier)")
        .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
}

/// Concurrent execution, deterministic commit: even when `c` finishes well before
/// `b`, the engine commits the batch in **declaration order**, so `completed_order`
/// (and thus the event log / resume behavior) is reproducible regardless of timing.
#[tokio::test]
async fn parallel_commit_order_is_deterministic() {
    let def = Definition::from_yaml(
        "metadata:\n  name: diamond\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n    - {id: c, type: function}\n    - {id: d, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: a, to: c}\n    - {from: b, to: d}\n    - {from: c, to: d}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new()
        .on("a", |_| async { Ok(Value::Null) })
        .on("d", |_| async { Ok(Value::Null) })
        // `b` is the slow branch; `c` returns immediately and finishes first.
        .on("b", |_| async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok(json!({"id": "b"}))
        })
        .on("c", |_| async { Ok(json!({"id": "c"})) });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine.run(&def, "wf-parallel-2", json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    // Declaration order, not completion order (c finished before b).
    assert_eq!(state.completed_order, vec!["a", "b", "c", "d"]);
}

/// When one branch of a concurrent batch fails, the batch still commits the sibling
/// that completed (so its effects are durable), then the workflow compensates —
/// rolling back the committed activities in reverse order.
#[tokio::test]
async fn parallel_branch_failure_compensates_completed_siblings() {
    // a -> {b, c} -> d. `b` fails permanently; `c` (and `a`) completed and must roll back.
    let def = Definition::from_yaml(
        "metadata:\n  name: diamond\nspec:\n  activities:\n    - {id: a, type: function, compensate: undo_a}\n    - {id: b, type: function}\n    - {id: c, type: function, compensate: undo_c}\n    - {id: d, type: function}\n    - {id: undo_a, type: function}\n    - {id: undo_c, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: a, to: c}\n    - {from: b, to: d}\n    - {from: c, to: d}\n",
    )
    .unwrap();

    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut executor = ClosureExecutor::new();
    for id in ["a", "c", "d", "undo_a", "undo_c"] {
        let log = log.clone();
        executor = executor.on(id, move |ctx| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(ctx.id.clone());
                Ok(Value::Null)
            }
        });
    }
    executor = executor.on("b", |_| async {
        Err(ActivityError::Permanent("branch b rejected".into()))
    });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine
        .run(&def, "wf-parallel-fail-1", json!({}))
        .await
        .unwrap();

    assert!(
        matches!(outcome, RunOutcome::Compensated(_)),
        "got {outcome:?}"
    );
    assert_eq!(state.activities["b"].state, ActivityState::Failed);
    // c committed despite the sibling failure; rollback runs in reverse completed order.
    let order = log.lock().unwrap().clone();
    assert_eq!(order, vec!["a", "c", "undo_c", "undo_a"]);
}

// ---------------------------------------------------------------------------
// EXE-603 — Engine::cancel (no more fake 202s from the server's DELETE route)
// ---------------------------------------------------------------------------

/// Cancelling a suspended execution transitions it to `Cancelled`, records a
/// `WorkflowCancelled` event in history, and marks the still-pending activity
/// `Skipped` rather than leaving it `Waiting` forever.
#[tokio::test]
async fn cancel_transitions_to_cancelled_and_skips_pending_activities() {
    let def = Definition::from_yaml(
        "metadata:\n  name: cancel-me\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n  transitions:\n    - {from: a, to: b}\n",
    )
    .unwrap();
    let executor = ClosureExecutor::new()
        .on("a", |_| async { Ok(json!({"a": true})) })
        .on("b", |_| async {
            Err(ActivityError::Interrupted("paused".into()))
        });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine.run(&def, "wf-cancel-1", json!({})).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    assert_eq!(state.activities["a"].state, ActivityState::Completed);
    assert_eq!(state.activities["b"].state, ActivityState::Ready);

    let cancelled = engine.cancel("wf-cancel-1").await.unwrap();
    assert_eq!(cancelled.status, WorkflowState::Cancelled);
    // `a` already completed — untouched. `b` was pending, not yet failed/completed —
    // skipped rather than left dangling.
    assert_eq!(cancelled.activities["a"].state, ActivityState::Completed);
    assert_eq!(cancelled.activities["b"].state, ActivityState::Skipped);

    let history = engine.history("wf-cancel-1").await.unwrap();
    assert!(
        matches!(history.last(), Some(WorkflowEvent::WorkflowCancelled)),
        "expected a trailing WorkflowCancelled event, got {history:?}"
    );

    // The cancellation is durable: a query (no side effects) reflects it too.
    let queried = engine.query("wf-cancel-1").await.unwrap().unwrap();
    assert_eq!(queried.status, WorkflowState::Cancelled);
}

/// Cancelling an execution that doesn't exist, or one already in a terminal state,
/// fails closed rather than silently succeeding — the acceptance bar EXE-603 sets
/// against the old handler's unconditional `202`.
#[tokio::test]
async fn cancel_fails_closed_on_unknown_or_already_terminal_executions() {
    let def = Definition::from_yaml(
        "metadata:\n  name: cancel-terminal\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .unwrap();
    let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!({"a": true})) });
    let engine = engine_with(InMemoryStore::new(), executor);

    assert!(engine.cancel("does-not-exist").await.is_err());

    let (outcome, _) = engine.run(&def, "wf-cancel-2", json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        engine.cancel("wf-cancel-2").await.is_err(),
        "a completed execution must not be cancellable"
    );

    // A second cancel of an already-cancelled execution is not idempotent success
    // either — it's still a terminal state.
    let def2 = Definition::from_yaml(
        "metadata:\n  name: cancel-twice\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .unwrap();
    let executor2 = ClosureExecutor::new().on("a", |_| async {
        Err(ActivityError::Interrupted("paused".into()))
    });
    let engine2 = engine_with(InMemoryStore::new(), executor2);
    engine2.run(&def2, "wf-cancel-3", json!({})).await.unwrap();
    engine2.cancel("wf-cancel-3").await.unwrap();
    assert!(
        engine2.cancel("wf-cancel-3").await.is_err(),
        "cancelling an already-cancelled execution must not silently succeed again"
    );
}

// ---------------------------------------------------------------------------
// WFL-301/302 — engine-native for_each/map fan-out
// ---------------------------------------------------------------------------

/// A referenced collection (`${fetch}`) expands into one instance per element, and
/// the joined output preserves **item order** regardless of anything else.
#[tokio::test]
async fn for_each_expands_a_referenced_collection_and_joins_outputs_in_order() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-basic\nspec:\n  activities:\n    - {id: fetch, type: function}\n    - {id: doubled, type: for_each, inputs: {items: \"${fetch}\", activity: {type: function}}}\n  transitions:\n    - {from: fetch, to: doubled}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new()
        .on("fetch", |_| async { Ok(json!([1, 2, 3])) })
        .on("doubled[0]", |ctx| async move {
            Ok(json!(ctx.variables["item"].as_i64().unwrap() * 2))
        })
        .on("doubled[1]", |ctx| async move {
            Ok(json!(ctx.variables["item"].as_i64().unwrap() * 2))
        })
        .on("doubled[2]", |ctx| async move {
            Ok(json!(ctx.variables["item"].as_i64().unwrap() * 2))
        });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine
        .run(&def, "wf-foreach-basic-1", json!({}))
        .await
        .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.variables.get("doubled"), Some(&json!([2, 4, 6])));
    assert_eq!(state.activities["doubled"].state, ActivityState::Completed);
    for i in 0..3 {
        assert_eq!(
            state.activities[&format!("doubled[{i}]")].state,
            ActivityState::Completed
        );
    }
}

/// `items` may also be a literal array (no upstream activity involved), and
/// `item_index` is exposed alongside `item` to each instance.
#[tokio::test]
async fn for_each_accepts_a_literal_array_and_exposes_item_index() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-literal\nspec:\n  activities:\n    - {id: tagged, type: for_each, inputs: {items: [\"a\", \"b\"], activity: {type: function}}}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new()
        .on("tagged[0]", |ctx| async move {
            Ok(json!(format!(
                "{}-{}",
                ctx.variables["item_index"],
                ctx.variables["item"].as_str().unwrap()
            )))
        })
        .on("tagged[1]", |ctx| async move {
            Ok(json!(format!(
                "{}-{}",
                ctx.variables["item_index"],
                ctx.variables["item"].as_str().unwrap()
            )))
        });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine
        .run(&def, "wf-foreach-literal-1", json!({}))
        .await
        .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.variables.get("tagged"), Some(&json!(["0-a", "1-b"])));
}

/// An empty collection completes the `for_each` immediately with an empty joined
/// output — no item instances are ever created.
#[tokio::test]
async fn for_each_over_an_empty_collection_completes_immediately() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-empty\nspec:\n  activities:\n    - {id: none, type: for_each, inputs: {items: [], activity: {type: function}}}\n",
    )
    .unwrap();

    // No handlers registered — any spawned instance would panic on lookup.
    let engine = engine_with(InMemoryStore::new(), ClosureExecutor::new());
    let (outcome, state) = engine
        .run(&def, "wf-foreach-empty-1", json!({}))
        .await
        .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.variables.get("none"), Some(&json!([])));
    assert_eq!(state.activities["none"].state, ActivityState::Completed);
}

/// A resolved collection larger than `max_items` fails the `for_each` closed,
/// before any item instance is created — no unbounded fan-out.
#[tokio::test]
async fn for_each_fails_closed_when_the_collection_exceeds_max_items() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-bound\nspec:\n  activities:\n    - {id: bounded, type: for_each, inputs: {items: [1, 2, 3], max_items: 2, activity: {type: function}}}\n",
    )
    .unwrap();

    // No handlers registered — if any instance were spawned, it would panic.
    let engine = engine_with(InMemoryStore::new(), ClosureExecutor::new());
    let (outcome, _) = engine
        .run(&def, "wf-foreach-bound-1", json!({}))
        .await
        .unwrap();

    match outcome {
        RunOutcome::Failed(msg) => assert!(msg.contains("max_items"), "{msg}"),
        other => panic!("expected Failed(..), got {other:?}"),
    }
}

/// `items` resolving to something other than an array (an object, here) fails
/// closed with a clear message rather than silently treating it as zero/one item.
#[tokio::test]
async fn for_each_fails_closed_when_items_does_not_resolve_to_an_array() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-badtype\nspec:\n  activities:\n    - {id: fetch, type: function}\n    - {id: loopy, type: for_each, inputs: {items: \"${fetch}\", activity: {type: function}}}\n  transitions:\n    - {from: fetch, to: loopy}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new().on("fetch", |_| async { Ok(json!({"not": "an array"})) });
    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, _) = engine
        .run(&def, "wf-foreach-badtype-1", json!({}))
        .await
        .unwrap();

    match outcome {
        RunOutcome::Failed(msg) => assert!(msg.contains("array"), "{msg}"),
        other => panic!("expected Failed(..), got {other:?}"),
    }
}

/// At most `max_concurrent` item instances run at once — proven the same way
/// `parallel_branches_run_concurrently` proves DAG-level concurrency: an
/// in-flight counter that must never exceed the cap, wrapped in a timeout so a
/// scheduler bug that serializes everything (or over-parallelizes) is caught
/// rather than silently passing.
#[tokio::test]
async fn for_each_respects_the_max_concurrent_cap() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-cap\nspec:\n  activities:\n    - {id: capped, type: for_each, inputs: {items: [1, 2, 3, 4, 5, 6], max_concurrent: 2, activity: {type: function}}}\n",
    )
    .unwrap();

    let in_flight = Arc::new(AtomicUsize::new(0));
    let observed_max = Arc::new(AtomicUsize::new(0));
    let mut executor = ClosureExecutor::new();
    for i in 0..6 {
        let in_flight = in_flight.clone();
        let observed_max = observed_max.clone();
        executor = executor.on(format!("capped[{i}]"), move |_| {
            let in_flight = in_flight.clone();
            let observed_max = observed_max.clone();
            async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                observed_max.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(Value::Null)
            }
        });
    }

    let engine = engine_with(InMemoryStore::new(), executor);
    let run = engine.run(&def, "wf-foreach-cap-1", json!({}));
    let (outcome, _) = tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("must not hang")
        .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        observed_max.load(Ordering::SeqCst) <= 2,
        "max_concurrent: 2 was exceeded (observed {})",
        observed_max.load(Ordering::SeqCst)
    );
    assert_eq!(
        observed_max.load(Ordering::SeqCst),
        2,
        "the cap should actually be reached with 6 items, not just never exceeded"
    );
}

/// One item failing permanently fails the `for_each` (and the workflow), but the
/// *other* items launched in the same phase still commit their completed outputs
/// durably — a partial success is not silently discarded.
#[tokio::test]
async fn for_each_item_failure_fails_the_for_each_but_commits_completed_siblings() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-itemfail\nspec:\n  activities:\n    - {id: risky, type: for_each, inputs: {items: [1, 2, 3], activity: {type: function}}}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new()
        .on("risky[0]", |_| async { Ok(json!("ok-0")) })
        .on("risky[1]", |_| async {
            Err(ActivityError::Permanent("item 1 rejected".into()))
        })
        .on("risky[2]", |_| async { Ok(json!("ok-2")) });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, state) = engine
        .run(&def, "wf-foreach-itemfail-1", json!({}))
        .await
        .unwrap();

    match outcome {
        RunOutcome::Failed(msg) => assert!(msg.contains("item 1"), "{msg}"),
        other => panic!("expected Failed(..), got {other:?}"),
    }
    assert_eq!(state.activities["risky[0]"].state, ActivityState::Completed);
    assert_eq!(state.activities["risky[1]"].state, ActivityState::Failed);
    assert_eq!(state.activities["risky[2]"].state, ActivityState::Completed);
    assert_eq!(state.activities["risky"].state, ActivityState::Failed);
}

/// Durable resume re-drives only the item instances that never completed — the
/// `for_each` analogue of `durable_resume_does_not_reexecute_completed_activities`.
/// Engine 1 completes items 0 and 2 but is interrupted on item 1 (simulating a
/// crash mid-fan-out); a fresh engine over the same store resumes and must not
/// re-run 0 or 2 (registering no handlers for them — any call would panic).
#[tokio::test]
async fn for_each_resume_reexecutes_only_incomplete_items() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-resume\nspec:\n  activities:\n    - {id: items3, type: for_each, inputs: {items: [10, 20, 30], activity: {type: function}}}\n",
    )
    .unwrap();

    let dir = std::env::temp_dir().join(format!("apex-wf-foreach-resume-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let exec_id = "wf-foreach-resume-1";

    // --- Engine 1: items 0 and 2 complete; item 1 interrupts (simulated crash). ---
    {
        let store = FileStore::new(&dir).unwrap();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let executor = ClosureExecutor::new()
            .on("items3[0]", |ctx| async move {
                Ok(json!(ctx.variables["item"].as_i64().unwrap() * 10))
            })
            .on("items3[1]", |_| async {
                Err(ActivityError::Interrupted("worker crash mid-item".into()))
            })
            .on("items3[2]", |ctx| async move {
                Ok(json!(ctx.variables["item"].as_i64().unwrap() * 10))
            });

        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.run(&def, exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(
            state.activities["items3[0]"].state,
            ActivityState::Completed
        );
        assert_ne!(
            state.activities["items3[1]"].state,
            ActivityState::Completed
        );
        assert_eq!(
            state.activities["items3[2]"].state,
            ActivityState::Completed
        );
    }

    // --- Engine 2: a fresh instance resumes; items 0/2 must not re-run. ---
    {
        let store = FileStore::new(&dir).unwrap();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let executor = ClosureExecutor::new().on("items3[1]", |ctx| async move {
            Ok(json!(ctx.variables["item"].as_i64().unwrap() * 10))
        });
        // No handlers for items3[0]/items3[2] — a re-run would panic on lookup.

        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.resume(&def, exec_id).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(
            state.variables.get("items3"),
            Some(&json!([100, 200, 300])),
            "joined output must preserve item order regardless of which item resumed"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The resolved collection is pinned into the checkpoint on first encounter and
/// never recomputed — even if the referenced source variable is mutated directly
/// in the durable checkpoint before a resume (the same "recovery model" property
/// [`human_task_suspends_and_resumes_durably`] exercises for a decision variable).
#[tokio::test]
async fn for_each_pins_the_resolved_collection_across_resume() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-pin\nspec:\n  activities:\n    - {id: fetch, type: function}\n    - {id: pinned, type: for_each, inputs: {items: \"${fetch}\", activity: {type: function}}}\n  transitions:\n    - {from: fetch, to: pinned}\n",
    )
    .unwrap();

    let dir = std::env::temp_dir().join(format!("apex-wf-foreach-pin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let exec_id = "wf-foreach-pin-1";
    let store = FileStore::new(&dir).unwrap();

    // Engine 1: fetch resolves [1, 2], then the for_each interrupts on item 1.
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let executor = ClosureExecutor::new()
            .on("fetch", |_| async { Ok(json!([1, 2])) })
            .on("pinned[0]", |ctx| async move {
                Ok(json!(ctx.variables["item"].as_i64().unwrap()))
            })
            .on("pinned[1]", |_| async {
                Err(ActivityError::Interrupted("pause before item 1".into()))
            });
        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, _) = engine.run(&def, exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    }

    // Mutate the durable checkpoint's `fetch` variable directly — simulating a
    // source that would resolve differently if the for_each ever recomputed it.
    {
        let mut cp = store.latest(exec_id).await.unwrap().unwrap();
        cp.variables
            .insert("fetch".to_string(), json!([99, 98, 97, 96, 95]));
        store.save(&cp).await.unwrap();
    }

    // Engine 2: resumes. If the collection were recomputed from the mutated
    // `fetch`, this would expand to 5 items (and `pinned[1]`'s item would be 98,
    // not 2) — instead the pinned 2-item collection from engine 1 must be used.
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let executor = ClosureExecutor::new().on("pinned[1]", |ctx| async move {
            assert_eq!(
                ctx.variables["item"],
                json!(2),
                "must resume against the originally pinned collection, not a recomputed one"
            );
            Ok(json!(ctx.variables["item"].as_i64().unwrap()))
        });
        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.resume(&def, exec_id).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(state.variables.get("pinned"), Some(&json!([1, 2])));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// WFL-303 — checkpoint size cap: an oversized activity output is rejected
// fail-closed rather than silently bloating every future checkpoint.
// ---------------------------------------------------------------------------

/// An activity output over the configured cap fails the activity (and, absent
/// compensation, the workflow) instead of being merged into `state.variables` —
/// where it would re-serialize into every future checkpoint of the execution.
#[tokio::test]
async fn oversized_activity_output_fails_closed_instead_of_bloating_the_checkpoint() {
    let def = Definition::from_yaml(
        "metadata:\n  name: oversized\nspec:\n  activities:\n    - {id: dump, type: function}\n",
    )
    .unwrap();

    // Comfortably over a tiny cap, so the test doesn't need to build a real 1 MiB
    // payload to exercise the guard.
    let executor = ClosureExecutor::new().on("dump", |_| async { Ok(json!("x".repeat(1000))) });

    let engine = engine_with(InMemoryStore::new(), executor).with_max_activity_output_bytes(100);
    let (outcome, state) = engine.run(&def, "wf-oversized-1", json!({})).await.unwrap();

    match outcome {
        RunOutcome::Failed(msg) => {
            assert!(msg.contains("dump"), "{msg}");
            assert!(msg.contains("bytes"), "{msg}");
        }
        other => panic!("expected Failed(..), got {other:?}"),
    }
    // The oversized output must never have reached the execution's variables.
    assert!(!state.variables.contains_key("dump"));
}

/// An output at or under the cap is unaffected — the guard doesn't false-positive
/// on ordinary-sized activity outputs.
#[tokio::test]
async fn activity_output_under_the_cap_is_unaffected() {
    let def = Definition::from_yaml(
        "metadata:\n  name: small\nspec:\n  activities:\n    - {id: ok, type: function}\n",
    )
    .unwrap();
    let executor = ClosureExecutor::new().on("ok", |_| async { Ok(json!({"fine": true})) });

    let engine = engine_with(InMemoryStore::new(), executor).with_max_activity_output_bytes(100);
    let (outcome, state) = engine.run(&def, "wf-small-1", json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(state.variables.get("ok"), Some(&json!({"fine": true})));
}

/// The cap also guards a `for_each`'s **joined** output — many small, individually
/// fine item outputs can still aggregate into an oversized whole.
#[tokio::test]
async fn for_each_joined_output_over_the_cap_fails_closed() {
    let def = Definition::from_yaml(
        "metadata:\n  name: foreach-oversized\nspec:\n  activities:\n    - {id: loop, type: for_each, inputs: {items: [1, 2, 3], activity: {type: function}}}\n",
    )
    .unwrap();
    let executor = ClosureExecutor::new()
        .on("loop[0]", |_| async { Ok(json!("x".repeat(50))) })
        .on("loop[1]", |_| async { Ok(json!("x".repeat(50))) })
        .on("loop[2]", |_| async { Ok(json!("x".repeat(50))) });

    let engine = engine_with(InMemoryStore::new(), executor).with_max_activity_output_bytes(100);
    let (outcome, _) = engine
        .run(&def, "wf-foreach-oversized-1", json!({}))
        .await
        .unwrap();

    match outcome {
        RunOutcome::Failed(msg) => assert!(msg.contains("loop"), "{msg}"),
        other => panic!("expected Failed(..), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// WFL-307 — activity progress events: a long-running activity can report
// incremental progress via `ActivityContext::progress`, durably recorded as
// `ActivityProgress` events.
// ---------------------------------------------------------------------------

/// A long activity that reports several progress updates gets each one
/// recorded, in order, as a durable `ActivityProgress` event in the history —
/// interleaved with real `.await` points, so the reports are genuinely
/// received (and emitted) as the activity runs, not just recovered from a
/// leftover buffer after it already returned.
#[tokio::test]
async fn a_long_activity_emits_progress_events_as_it_runs() {
    let def = Definition::from_yaml(
        "metadata:\n  name: progress\nspec:\n  activities:\n    - {id: slow, type: function}\n",
    )
    .unwrap();

    let executor = ClosureExecutor::new().on("slow", |ctx| async move {
        let tx = ctx
            .progress
            .as_ref()
            .expect("progress sink present on the sequential path");
        for pct in ["25%", "50%", "75%"] {
            tx.send(pct.to_string()).unwrap();
            // Yield back to the runtime so the engine's concurrent drain loop
            // genuinely gets a chance to observe and emit each report as it
            // arrives, rather than the whole closure running to completion in
            // one uninterrupted poll.
            tokio::task::yield_now().await;
        }
        Ok(json!("done"))
    });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, _) = engine.run(&def, "wf-progress-1", json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let history = engine.history("wf-progress-1").await.unwrap();
    let messages: Vec<String> = history
        .iter()
        .filter_map(|e| match e {
            WorkflowEvent::ActivityProgress { id, message, .. } if id == "slow" => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(messages, vec!["25%", "50%", "75%"]);

    // Progress events land *before* the terminal ActivityCompleted event, not
    // interleaved after it or out of order.
    let progress_positions: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, WorkflowEvent::ActivityProgress { .. }))
        .map(|(i, _)| i)
        .collect();
    let completed_position = history
        .iter()
        .position(|e| matches!(e, WorkflowEvent::ActivityCompleted { .. }))
        .unwrap();
    assert!(
        progress_positions.iter().all(|&i| i < completed_position),
        "every progress event must precede the activity's completion event"
    );
}

/// An activity that never reports progress is completely unaffected — no
/// spurious `ActivityProgress` events, no change in behavior.
#[tokio::test]
async fn an_activity_with_no_progress_reports_behaves_exactly_as_before() {
    let def = Definition::from_yaml(
        "metadata:\n  name: quiet\nspec:\n  activities:\n    - {id: fast, type: function}\n",
    )
    .unwrap();
    let executor = ClosureExecutor::new().on("fast", |_| async { Ok(json!("ok")) });

    let engine = engine_with(InMemoryStore::new(), executor);
    let (outcome, _) = engine.run(&def, "wf-quiet-1", json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let history = engine.history("wf-quiet-1").await.unwrap();
    assert!(
        !history
            .iter()
            .any(|e| matches!(e, WorkflowEvent::ActivityProgress { .. })),
        "an activity that never reports progress must emit no progress events"
    );
}
