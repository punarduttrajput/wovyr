//! Integration tests for the Temporal gap-closure features
//! ([scope](../../../docs/03-workflow-engine/temporal-gap-analysis.md)):
//! durable wall-clock timers (G1), recurring schedules (G2), side-effect-free
//! queries (G3), and definition pinning (G7). All are exercised deterministically
//! with a `ManualClock` and in-memory stores — no wall-clock sleeps.

use apex_workflow::{
    ActivityState, CheckpointStore, ClosureExecutor, Definition, DefinitionResolver, Engine,
    EventLog, InMemoryScheduleStore, InMemoryStore, InMemoryTimerStore, ManualClock, OverlapPolicy,
    RunOutcome, Schedule, ScheduleDispatcher, ScheduleStore, TimerDispatcher, TimerStore,
    WorkflowState,
};
use serde_json::json;
use std::sync::Arc;

fn resolver_for(def: &Definition) -> DefinitionResolver {
    let name = def.metadata.name.clone();
    let def = def.clone();
    Arc::new(move |want: &str| (want == name).then(|| def.clone()))
}

// ---------------------------------------------------------------------------
// G3 — queries (read live state without side effects)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_reports_status_without_side_effects() {
    let def = Definition::from_yaml(
        "metadata:\n  name: q\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .unwrap();

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store.clone());
    let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!({"ok": true})) });
    let engine = Engine::new(events, checkpoints, Arc::new(executor));

    // Unknown execution → None.
    assert!(engine.status("missing").await.unwrap().is_none());

    let (outcome, _) = engine.run(&def, "q-1", json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    // Querying does not append events or change the checkpoint.
    let events_before = store.load("q-1").await.unwrap().len();
    let summary = engine.status("q-1").await.unwrap().expect("exists");
    let events_after = store.load("q-1").await.unwrap().len();

    assert_eq!(summary.status, WorkflowState::Completed);
    assert_eq!(summary.activities["a"], ActivityState::Completed);
    assert!(summary.waiting_on.is_empty());
    assert_eq!(
        events_before, events_after,
        "query must be side-effect free"
    );
}

#[tokio::test]
async fn query_surfaces_pending_waits() {
    let def = Definition::from_yaml(
        "metadata:\n  name: qw\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n",
    )
    .unwrap();
    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let engine = Engine::new(events, checkpoints, Arc::new(ClosureExecutor::new()));

    let (outcome, _) = engine.run(&def, "qw-1", json!({})).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));

    let summary = engine.status("qw-1").await.unwrap().unwrap();
    assert_eq!(summary.waiting_on, vec!["hold".to_string()]);
}

// ---------------------------------------------------------------------------
// G7 — definition pinning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_rejects_a_drifted_definition() {
    let original = "metadata:\n  name: pinned\n  version: 1.0.0\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n";
    let def = Definition::from_yaml(original).unwrap();

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let engine = Engine::new(events, checkpoints, Arc::new(ClosureExecutor::new()));

    // Start and suspend on the event wait.
    let (outcome, _) = engine.run(&def, "pin-1", json!({})).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));

    // A definition whose content drifted (extra activity) is rejected on resume.
    let drifted = Definition::from_yaml(
        "metadata:\n  name: pinned\n  version: 1.0.0\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: go}}\n    - {id: extra, type: function}\n",
    )
    .unwrap();
    let err = engine.resume(&drifted, "pin-1").await.unwrap_err();
    assert!(err.to_string().contains("changed since execution"));

    // A version bump is rejected too.
    let bumped = Definition::from_yaml(&original.replace("1.0.0", "2.0.0")).unwrap();
    assert!(engine.resume(&bumped, "pin-1").await.is_err());

    // The original definition still resumes (after delivering the event).
    let (outcome, _) = engine
        .signal_event(&def, "pin-1", "go", json!("released"))
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Completed);
}

// ---------------------------------------------------------------------------
// G1 — durable wall-clock timers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn durable_timer_fires_when_the_deadline_passes() {
    let def = Definition::from_yaml(
        "metadata:\n  name: timed\nspec:\n  activities:\n    - {id: wait_a, type: wait, inputs: {timer: {after: \"5m\"}}}\n    - {id: after, type: function}\n  transitions:\n    - {from: wait_a, to: after}\n",
    )
    .unwrap();

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let timers = Arc::new(InMemoryTimerStore::new());
    let clock = ManualClock::new(0);
    let executor = ClosureExecutor::new().on("after", |_| async { Ok(json!("done")) });

    let engine = Engine::new(events, checkpoints, Arc::new(executor))
        .with_clock(Arc::new(clock.clone()))
        .with_timer_store(timers.clone());

    // Start: the wall-clock wait suspends and registers a timer for t=300_000.
    let (outcome, _) = engine.run(&def, "tm-1", json!({})).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Interrupted(_)));
    let summary = engine.status("tm-1").await.unwrap().unwrap();
    assert_eq!(summary.activities["wait_a"], ActivityState::Waiting);
    assert_eq!(summary.activities["after"], ActivityState::Created);

    let dispatcher = TimerDispatcher::new(
        engine.clone(),
        timers.clone() as Arc<dyn TimerStore>,
        Arc::new(clock.clone()),
        resolver_for(&def),
    );

    // Before the deadline nothing fires.
    assert!(dispatcher.poll().await.unwrap().is_empty());
    assert_eq!(
        engine.status("tm-1").await.unwrap().unwrap().activities["wait_a"],
        ActivityState::Waiting
    );

    // Advance past the deadline → the timer fires and the workflow completes.
    clock.advance(300_000);
    let fired = dispatcher.poll().await.unwrap();
    assert_eq!(fired.len(), 1);

    let summary = engine.status("tm-1").await.unwrap().unwrap();
    assert_eq!(summary.status, WorkflowState::Completed);
    assert_eq!(summary.activities["after"], ActivityState::Completed);

    // The fired timer left no pending registration behind.
    assert!(timers.due(u64::MAX).await.unwrap().is_empty());
}

#[tokio::test]
async fn wall_clock_timer_without_a_store_is_a_clear_error() {
    let def = Definition::from_yaml(
        "metadata:\n  name: notimers\nspec:\n  activities:\n    - {id: w, type: wait, inputs: {timer: {after: \"1s\"}}}\n",
    )
    .unwrap();
    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let engine = Engine::new(events, checkpoints, Arc::new(ClosureExecutor::new()));

    let err = engine.run(&def, "nt-1", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("no timer store"));
}

// ---------------------------------------------------------------------------
// G2 — recurring schedules
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schedule_starts_executions_on_its_interval() {
    let def = Definition::from_yaml(
        "metadata:\n  name: cron_wf\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .unwrap();

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!("tick")) });
    let engine = Engine::new(events, checkpoints, Arc::new(executor));

    let schedules = InMemoryScheduleStore::new();
    schedules
        .save(&Schedule::every("nightly", "cron_wf", 1_000, 1_000))
        .await
        .unwrap();
    let clock = ManualClock::new(0);
    let dispatcher = ScheduleDispatcher::new(
        engine.clone(),
        Arc::new(schedules.clone()) as Arc<dyn ScheduleStore>,
        Arc::new(clock.clone()),
        resolver_for(&def),
    );

    // Not due yet.
    assert!(dispatcher.poll().await.unwrap().is_empty());

    // At t=1000 it fires once and the started execution completes.
    clock.set(1_000);
    let started = dispatcher.poll().await.unwrap();
    assert_eq!(started, vec!["nightly-1000".to_string()]);
    assert_eq!(
        engine.status("nightly-1000").await.unwrap().unwrap().status,
        WorkflowState::Completed
    );

    // Next deadline advanced to 2000; nothing new at t=1500.
    clock.set(1_500);
    assert!(dispatcher.poll().await.unwrap().is_empty());

    // A long gap fires once (missed ticks skipped) and re-aligns the deadline.
    clock.set(4_200);
    let started = dispatcher.poll().await.unwrap();
    assert_eq!(started, vec!["nightly-2000".to_string()]);
    assert_eq!(
        schedules
            .get("nightly")
            .await
            .unwrap()
            .unwrap()
            .next_fire_ms,
        5_000
    );
}

#[tokio::test]
async fn cron_schedule_fires_at_cron_instants() {
    let def = Definition::from_yaml(
        "metadata:\n  name: cron_wf\nspec:\n  activities:\n    - {id: a, type: function}\n",
    )
    .unwrap();

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let executor = ClosureExecutor::new().on("a", |_| async { Ok(json!("tick")) });
    let engine = Engine::new(events, checkpoints, Arc::new(executor));

    let schedules = InMemoryScheduleStore::new();
    // Every 5 minutes (UTC); first fire after t=0 is minute 5 (300_000 ms).
    schedules
        .save(&Schedule::cron("five", "cron_wf", "*/5 * * * *", 0).unwrap())
        .await
        .unwrap();
    let clock = ManualClock::new(0);
    let dispatcher = ScheduleDispatcher::new(
        engine.clone(),
        Arc::new(schedules.clone()) as Arc<dyn ScheduleStore>,
        Arc::new(clock.clone()),
        resolver_for(&def),
    );

    // Not due before the first cron instant.
    clock.set(299_000);
    assert!(dispatcher.poll().await.unwrap().is_empty());

    // At minute 5 it fires and re-aligns to minute 10.
    clock.set(300_000);
    assert_eq!(
        dispatcher.poll().await.unwrap(),
        vec!["five-300000".to_string()]
    );
    assert_eq!(
        schedules.get("five").await.unwrap().unwrap().next_fire_ms,
        600_000
    );

    // At minute 10 it fires again.
    clock.set(600_000);
    assert_eq!(
        dispatcher.poll().await.unwrap(),
        vec!["five-600000".to_string()]
    );
}

#[tokio::test]
async fn schedule_skip_overlap_does_not_start_concurrent_runs() {
    // A workflow that suspends (waits for an event it never gets) stays non-terminal.
    let def = Definition::from_yaml(
        "metadata:\n  name: slow_wf\nspec:\n  activities:\n    - {id: hold, type: wait, inputs: {event: never}}\n",
    )
    .unwrap();

    let store = InMemoryStore::new();
    let events: Arc<dyn EventLog> = Arc::new(store.clone());
    let checkpoints: Arc<dyn CheckpointStore> = Arc::new(store);
    let engine = Engine::new(events, checkpoints, Arc::new(ClosureExecutor::new()));

    let schedules = InMemoryScheduleStore::new();
    schedules
        .save(&Schedule::every("hourly", "slow_wf", 1_000, 1_000).with_overlap(OverlapPolicy::Skip))
        .await
        .unwrap();
    let clock = ManualClock::new(0);
    let dispatcher = ScheduleDispatcher::new(
        engine.clone(),
        Arc::new(schedules.clone()) as Arc<dyn ScheduleStore>,
        Arc::new(clock.clone()),
        resolver_for(&def),
    );

    // First tick starts a run that suspends (stays Running).
    clock.set(1_000);
    let started = dispatcher.poll().await.unwrap();
    assert_eq!(started, vec!["hourly-1000".to_string()]);
    assert_eq!(
        engine.status("hourly-1000").await.unwrap().unwrap().status,
        WorkflowState::Running
    );

    // Second tick is due, but the previous run is still active → skipped.
    clock.set(2_000);
    assert!(
        dispatcher.poll().await.unwrap().is_empty(),
        "Skip overlap must not start a concurrent run"
    );
    // The deadline still advanced so it does not busy-loop.
    assert_eq!(
        schedules.get("hourly").await.unwrap().unwrap().next_fire_ms,
        3_000
    );
}
