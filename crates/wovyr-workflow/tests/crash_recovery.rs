//! RM-GA-P2 DUR-402 acceptance: the event log and checkpoint are now `fsync`ed
//! (not just page-cache-durable), so an acknowledged append or checkpoint save
//! must survive a crash and be visible to a freshly opened `FileStore` — the
//! same durable-resume guarantee `postgres_store.rs` proves for the Postgres
//! backend, exercised here against the file backend DUR-402 actually touched.

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wovyr_workflow::{
    ActivityError, ActivityState, CheckpointStore, ClosureExecutor, Definition, Engine, EventLog,
    FileStore, RunOutcome,
};

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wovyr_workflow_crash_recovery_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn linear_abc() -> Definition {
    Definition::from_yaml(
        "metadata:\n  name: durable-file\nspec:\n  activities:\n    - {id: a, type: function}\n    - {id: b, type: function}\n    - {id: c, type: function}\n  transitions:\n    - {from: a, to: b}\n    - {from: b, to: c}\n",
    )
    .unwrap()
}

/// The keystone durability property: an interrupted run's completed activity
/// is not re-executed after a fresh `FileStore`/`Engine` (a new process, in
/// spirit) resumes from the durable checkpoint + event log alone.
#[tokio::test]
async fn a_freshly_opened_store_resumes_without_rerunning_completed_activities() {
    let dir = scratch_dir("resume");
    let def = linear_abc();
    let exec_id = "wf-file-durable";
    let a_runs = Arc::new(AtomicUsize::new(0));

    // --- Instance 1: completes `a`, then interrupts on `b` (simulated crash). ---
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
                Err(ActivityError::Interrupted("simulated crash".into()))
            });

        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.run(&def, exec_id, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Interrupted(_)));
        assert_eq!(state.activities["a"].state, ActivityState::Completed);
    }

    // --- Instance 2: a brand-new `FileStore` opened over the same directory —
    // no in-memory state carried over, exactly what a restarted process sees. ---
    {
        let store = FileStore::new(&dir).unwrap();
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
        let executor = ClosureExecutor::new()
            .on("a", |_| async {
                panic!("completed activity `a` must not be re-executed")
            })
            .on("b", |_| async { Ok(json!({"b": true})) })
            .on("c", |_| async { Ok(json!({"c": true})) });

        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        let (outcome, state) = engine.resume(&def, exec_id).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(state.activities["c"].state, ActivityState::Completed);
    }

    assert_eq!(
        a_runs.load(Ordering::SeqCst),
        1,
        "`a` ran exactly once across the crash + resume"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The event log's sequence numbers must stay correct across the process
/// boundary too: a fresh `FileStore`'s first append for an execution it has
/// never seen (this process's cache is empty) re-derives the next sequence
/// from the file itself, rather than colliding with or skipping past what a
/// prior instance already wrote.
#[tokio::test]
async fn sequence_numbers_continue_correctly_across_a_freshly_opened_store() {
    let dir = scratch_dir("sequence");
    let exec_id = "wf-file-sequence";

    let first_seq = {
        let store = FileStore::new(&dir).unwrap();
        store.append(exec_id, dummy_event()).await.unwrap();
        store.append(exec_id, dummy_event()).await.unwrap()
    };
    assert_eq!(first_seq, 2);

    // A fresh store (empty in-process cache) continues the sequence rather
    // than restarting it at 1.
    let store = FileStore::new(&dir).unwrap();
    let seq = store.append(exec_id, dummy_event()).await.unwrap();
    assert_eq!(seq, 3, "sequence continues from the file's true length");

    let events = EventLog::load(&store, exec_id).await.unwrap();
    assert_eq!(events.len(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

fn dummy_event() -> wovyr_workflow::WorkflowEvent {
    wovyr_workflow::WorkflowEvent::WorkflowStarted
}

/// DUR-401/402: the checkpoint save path now goes through `atomic_write`
/// (temp file + fsync + rename + directory fsync). A crash between the
/// temp-file write and the rename — reproduced directly, the same way the
/// wovyr-kms and wovyr-common tests do — must leave the last *committed*
/// checkpoint intact and loadable, not torn or missing.
#[tokio::test]
async fn a_torn_checkpoint_temp_file_does_not_disturb_the_last_committed_checkpoint() {
    let dir = scratch_dir("checkpoint");
    let def = linear_abc();
    let exec_id = "wf-file-checkpoint";

    let store = FileStore::new(&dir).unwrap();
    {
        let events: Arc<dyn EventLog> = Arc::new(store.clone());
        let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
        let executor = ClosureExecutor::new()
            .on("a", |_| async { Ok(json!({"a": true})) })
            .on("b", |_| async {
                Err(ActivityError::Interrupted("simulated crash".into()))
            });
        let engine = Engine::new(events, checkpoints, Arc::new(executor));
        engine.run(&def, exec_id, json!({})).await.unwrap();
    }
    let committed = CheckpointStore::latest(&store, exec_id)
        .await
        .unwrap()
        .expect("a checkpoint was saved");
    assert_eq!(committed.activities["a"].state, ActivityState::Completed);

    // Simulate a crash mid-*next* checkpoint save: atomic_write's temp-file
    // write happened, the rename never did.
    let tmp = dir.join(format!("{exec_id}.checkpoint.json.tmp"));
    std::fs::write(&tmp, b"{ torn checkpoint write, not valid json").unwrap();

    // The last committed checkpoint is unaffected — read via a fresh store,
    // mirroring a restarted process.
    let reopened = FileStore::new(&dir).unwrap();
    let latest = CheckpointStore::latest(&reopened, exec_id)
        .await
        .unwrap()
        .expect("the committed checkpoint survives the torn temp file");
    assert_eq!(latest.activities["a"].state, ActivityState::Completed);

    let _ = std::fs::remove_dir_all(&dir);
}
