//! Live integration test for the Postgres-backed durable store.
//!
//! Capability-gated like the memory/provider backend tests: reads
//! `APEX_WORKFLOW_POSTGRES_URL` and skips (logging) when unset/unreachable, so
//! offline CI still passes. Verifies the keystone durability property — durable
//! **resume across engine instances** through Postgres, without re-running completed
//! activities — plus the raw event-log/checkpoint round-trip.
//!
//! Only compiled with `--features postgres`. To run locally:
//!
//! ```bash
//! APEX_WORKFLOW_POSTGRES_URL=postgres://apex:apex@127.0.0.1:5433/apex \
//!   cargo test -p apex-workflow --features postgres --test postgres_store -- --nocapture
//! ```
//!
//! Each test uses a per-run nonce execution id, so the shared tables stay isolated
//! across runs.

#![cfg(feature = "postgres")]

use apex_workflow::{
    ActivityError, ActivityState, CheckpointStore, ClosureExecutor, Definition, Engine, EventLog,
    PostgresStore, RunOutcome,
};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Connect a shared `PostgresStore`, or `None` (logging a skip) when unconfigured.
async fn store() -> Option<Arc<PostgresStore>> {
    let url = match std::env::var("APEX_WORKFLOW_POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: APEX_WORKFLOW_POSTGRES_URL not set");
            return None;
        }
    };
    match PostgresStore::connect(&url).await {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!("skipping: postgres unreachable at {url}: {e}");
            None
        }
    }
}

fn linear_abc() -> Definition {
    Definition::from_yaml(
        "metadata:\n  name: durable-pg\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n    - {id: c, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: b, to: c}\n",
    )
    .unwrap()
}

#[tokio::test]
async fn durable_resume_through_postgres_skips_completed_activities() {
    let Some(store) = store().await else { return };
    let def = linear_abc();
    let exec_id = format!("wf-pg-durable-{}", nonce());
    let a_runs = Arc::new(AtomicUsize::new(0));

    // --- Engine 1: completes `a`, then interrupts on `b` (simulated crash). ---
    {
        let events: Arc<dyn EventLog> = store.clone();
        let checkpoints: Arc<dyn CheckpointStore> = store.clone();
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
        let (outcome, state) = engine.run(&def, &exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(state.activities["a"].state, ActivityState::Completed);
    }

    // --- Engine 2: a fresh instance resumes from the durable Postgres checkpoint. ---
    {
        let events: Arc<dyn EventLog> = store.clone();
        let checkpoints: Arc<dyn CheckpointStore> = store.clone();
        let executor = ClosureExecutor::new()
            .on("a", |_| async {
                panic!("completed activity `a` must not be re-executed")
            })
            .on("b", |_| async { Ok(json!({"b": true})) })
            .on("c", |_| async { Ok(json!({"c": true})) });

        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.resume(&def, &exec_id).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(state.activities["c"].state, ActivityState::Completed);
        assert_eq!(state.variables.get("c"), Some(&json!({"c": true})));
    }

    assert_eq!(
        a_runs.load(Ordering::SeqCst),
        1,
        "`a` ran exactly once across the crash + resume"
    );
}

#[tokio::test]
async fn event_log_appends_sequentially_and_checkpoint_upserts() {
    let Some(store) = store().await else { return };
    let def = linear_abc();
    let exec_id = format!("wf-pg-log-{}", nonce());

    let executor = ClosureExecutor::new()
        .on("a", |_| async { Ok(json!({"a": true})) })
        .on("b", |_| async { Ok(json!({"b": true})) })
        .on("c", |_| async { Ok(json!({"c": true})) });
    let engine = Engine::new(store.clone(), store.clone(), Arc::new(executor));
    let (outcome, _) = engine.run(&def, &exec_id, json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    // The event log persisted a non-empty, ordered history for this execution.
    let events = EventLog::load(store.as_ref(), &exec_id).await.unwrap();
    assert!(
        events.len() >= 3,
        "expected at least one event per activity, got {}",
        events.len()
    );

    // The latest checkpoint reflects the completed run.
    let latest = CheckpointStore::latest(store.as_ref(), &exec_id)
        .await
        .unwrap()
        .expect("a checkpoint was saved");
    assert_eq!(latest.execution_id, exec_id);
    assert_eq!(latest.activities["c"].state, ActivityState::Completed);
}
