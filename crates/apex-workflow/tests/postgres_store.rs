//! Live integration test for the Postgres-backed durable store.
//!
//! Capability-gated like the memory/provider backend tests: reads
//! `APEX_WORKFLOW_POSTGRES_URL` and skips (logging) when unset/unreachable, so
//! offline CI still passes. Verifies the keystone durability property — durable
//! **resume across engine instances** through Postgres, without re-running completed
//! activities — plus the raw event-log/checkpoint round-trip.
//!
//! Only compiled with `--features postgres`. `connect` only ever *reads* the
//! schema version (RM-GA-P3 MIG-A1) — migrate first, or every test here skips
//! with a "not migrated" reason instead of running. To run locally:
//!
//! ```bash
//! cargo run -p apex-cli --features postgres -- admin migrate --target workflow \
//!   --database-url postgres://apex:apex@127.0.0.1:5433/apex
//! APEX_WORKFLOW_POSTGRES_URL=postgres://apex:apex@127.0.0.1:5433/apex \
//!   cargo test -p apex-workflow --features postgres --test postgres_store -- --nocapture
//! ```
//!
//! Each test uses a per-run nonce execution id, so the shared tables stay isolated
//! across runs.

#![cfg(feature = "postgres")]

use apex_workflow::{
    ActivityError, ActivityState, CheckpointStore, ClosureExecutor, Definition, DefinitionResolver,
    Engine, EventLog, PostgresStore, RunOutcome, WorkQueue, Worker, WorkflowState,
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

/// A counting executor for the linear a→b→c workflow (3 activities).
fn counting_abc(runs: Arc<AtomicUsize>) -> ClosureExecutor {
    let mut ex = ClosureExecutor::new();
    for id in ["a", "b", "c"] {
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
async fn distributed_workers_over_postgres_process_each_exactly_once() {
    let Some(store1) = store().await else { return };
    // A second, independent connection — the realistic two-node setup.
    let url = std::env::var("APEX_WORKFLOW_POSTGRES_URL").unwrap();
    let store2 = match PostgresStore::connect(&url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("skipping: second postgres connection failed: {e}");
            return;
        }
    };

    const N: usize = 8;
    let def = linear_abc();
    let runs = Arc::new(AtomicUsize::new(0));
    let n = nonce();
    let ids: Vec<String> = (0..N).map(|i| format!("wf-pg-dist-{n}-{i}")).collect();

    // Submit N executions: durably start (no activities run) + enqueue.
    let submitter = Engine::new(
        store1.clone() as Arc<dyn EventLog>,
        store1.clone() as Arc<dyn CheckpointStore>,
        Arc::new(counting_abc(runs.clone())),
    );
    for id in &ids {
        submitter.start(&def, id, json!({})).await.unwrap();
        WorkQueue::enqueue(store1.as_ref(), id).await.unwrap();
    }

    let resolver: DefinitionResolver = {
        let def = def.clone();
        Arc::new(move |name: &str| (name == "durable-pg").then(|| def.clone()))
    };
    let worker = |id: &str, store: Arc<PostgresStore>| {
        Worker::new(
            id,
            Engine::new(
                store.clone() as Arc<dyn EventLog>,
                store.clone() as Arc<dyn CheckpointStore>,
                Arc::new(counting_abc(runs.clone())),
            ),
            store.clone() as Arc<dyn WorkQueue>,
            store.clone() as Arc<dyn CheckpointStore>,
            resolver.clone(),
        )
    };
    let w1 = worker("w1", store1.clone());
    let w2 = worker("w2", store2.clone());

    // Both workers drain concurrently; FOR UPDATE SKIP LOCKED keeps leases disjoint.
    let (n1, n2) = tokio::join!(w1.run_until_idle(), w2.run_until_idle());
    let processed = n1.unwrap() + n2.unwrap();

    assert_eq!(
        processed, N,
        "each execution leased + driven by exactly one worker"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        N * 3,
        "3 activities per workflow, none run twice"
    );
    for id in &ids {
        let st = CheckpointStore::latest(store1.as_ref(), id)
            .await
            .unwrap()
            .expect("checkpoint");
        assert_eq!(
            st.status,
            WorkflowState::Completed,
            "{id} should be completed"
        );
    }
}

/// RM-GA-P3 MIG-A1's version-skew acceptance criterion: "an old binary refuses
/// a newer schema rather than corrupting it." Simulates the situation a
/// partial fleet rollout creates — a migration this binary's embedded set
/// doesn't know about already applied to the database — by inserting a fake
/// future row directly into the tracking table, then asserting `connect`
/// fails closed instead of silently proceeding against a schema shape it
/// doesn't understand. `apex-memory`/`apex-marketplace`'s `PostgresStore`
/// share this exact fail-closed mechanism (`assert_schema_version`), just
/// against their own distinct tracking tables.
#[tokio::test]
async fn connect_refuses_a_schema_newer_than_this_binary_understands() {
    // Also proves the schema is actually migrated (a prerequisite for the
    // fake-future-row insert below to land in a real, already-existing table).
    let Some(_) = store().await else { return };
    let url = std::env::var("APEX_WORKFLOW_POSTGRES_URL").unwrap();

    // This test needs a raw connection to inject a fake future-version row. It uses
    // NoTls, so against a TLS-only host (e.g. a managed remote with `sslmode=require`)
    // the raw connect can't be established — skip rather than fail, since the
    // version-skew behavior under test is orthogonal to transport security.
    let (raw, connection) = match tokio_postgres::connect(&url, tokio_postgres::NoTls).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping: raw NoTls admin connection unavailable (TLS-only host?): {e}");
            return;
        }
    };
    tokio::spawn(async move {
        let _ = connection.await;
    });
    raw.execute(
        "INSERT INTO apex_workflow_schema_history (version, name, applied_on, checksum)
         VALUES (999999, 'future', '2999-01-01T00:00:00.000000000+00:00', '0')
         ON CONFLICT (version) DO NOTHING",
        &[],
    )
    .await
    .unwrap();

    let result = PostgresStore::connect(&url).await;

    // Clean up the fake row before asserting, so a failed assertion doesn't
    // permanently poison every later run of this suite against this database.
    raw.execute(
        "DELETE FROM apex_workflow_schema_history WHERE version = 999999",
        &[],
    )
    .await
    .unwrap();

    let err = match result {
        Ok(_) => panic!("connect must refuse a schema newer than this binary's own"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("newer than this binary"),
        "expected a version-skew error, got: {err}"
    );
}

/// RM-AIM-P1 WFL-104: concurrent appends to *one* execution get distinct, contiguous
/// seqs — no `(execution_id, seq)` primary-key violation and no lost writes — proving
/// the atomic counter-table allocation replaced the old racy `MAX(seq)+1`. This is the
/// overlapping-workers race the ticket calls out, exercised directly at the event log.
#[tokio::test]
async fn concurrent_appends_to_one_execution_get_distinct_contiguous_seqs() {
    use apex_workflow::WorkflowEvent;
    let Some(store) = store().await else { return };
    let exec_id = format!("wf-pg-seq-{}", nonce());
    const N: u64 = 24;

    let mut handles = Vec::new();
    for i in 0..N {
        let store = store.clone();
        let exec = exec_id.clone();
        handles.push(tokio::spawn(async move {
            store
                .append(
                    &exec,
                    WorkflowEvent::ActivityStarted {
                        id: format!("a{i}"),
                        attempt: 1,
                    },
                )
                .await
        }));
    }
    let mut seqs = Vec::new();
    for h in handles {
        seqs.push(
            h.await
                .unwrap()
                .expect("append must not PK-collide under concurrency"),
        );
    }
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        (1..=N).collect::<Vec<u64>>(),
        "concurrent appends must yield exactly the distinct contiguous seqs 1..=N"
    );
    // The log actually persisted all N events in order.
    assert_eq!(store.load(&exec_id).await.unwrap().len() as u64, N);
}

/// RM-AIM-P1 WFL-101: concurrent store calls are served without deadlock or
/// serialization failure — the pool hands out distinct connections (a single shared
/// `Client` would force every call through one socket). Uses distinct executions so
/// the calls are genuinely independent.
#[tokio::test]
async fn concurrent_store_calls_are_served_by_the_pool() {
    use apex_workflow::WorkflowEvent;
    let Some(store) = store().await else { return };
    let run = nonce();
    const N: u64 = 16;

    let mut handles = Vec::new();
    for i in 0..N {
        let store = store.clone();
        let exec = format!("wf-pg-pool-{run}-{i}");
        handles.push(tokio::spawn(async move {
            // A pair of round-trips per task, so several connections are in flight at once.
            store
                .append(
                    &exec,
                    WorkflowEvent::ActivityStarted {
                        id: "a".into(),
                        attempt: 1,
                    },
                )
                .await?;
            store.load(&exec).await.map(|evs| evs.len())
        }));
    }
    for h in handles {
        assert_eq!(
            h.await.unwrap().unwrap(),
            1,
            "each independent execution has one event"
        );
    }
}
