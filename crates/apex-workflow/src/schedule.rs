//! Recurring workflow schedules.
//!
//! Closes [gap closure
//! G2](../../docs/03-workflow-engine/temporal-gap-analysis.md#g2--schedules--cron):
//! a [`Schedule`] starts a workflow on a fixed interval. It reuses the same
//! caller-driven, clock-at-the-boundary design as durable timers (G1) — a
//! [`ScheduleDispatcher`] consults the [`Clock`](crate::timer::Clock) to decide
//! which schedules are due and starts their executions, so the engine itself stays
//! deterministic.
//!
//! Missed ticks (e.g. while the host was down) are **skipped**, not back-filled:
//! the dispatcher fires at most once per schedule per poll and advances the next
//! deadline past `now`. An [`OverlapPolicy`] decides whether a new run may start
//! while the schedule's previous execution is still in flight.

use crate::engine::Engine;
use crate::timer::Clock;
use crate::worker::DefinitionResolver;
use apex_common::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

/// What to do when a schedule is due but its previous execution has not finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// Skip this tick (do not start a concurrent run). The default.
    #[default]
    Skip,
    /// Start a new run regardless of the previous one.
    Allow,
}

/// A recurring schedule that starts a workflow every `interval_ms`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schedule {
    /// Unique schedule id (also the execution-id prefix).
    pub id: String,
    /// Name of the workflow to start (resolved via the dispatcher's resolver).
    pub workflow_name: String,
    /// Run input passed to each started execution.
    #[serde(default)]
    pub input: Value,
    /// Interval between fires, in milliseconds.
    pub interval_ms: u64,
    /// Next fire time, in Unix-epoch milliseconds.
    pub next_fire_ms: u64,
    /// When `true`, the dispatcher skips this schedule entirely.
    #[serde(default)]
    pub paused: bool,
    /// Overlap behavior when the previous run is still active.
    #[serde(default)]
    pub overlap: OverlapPolicy,
    /// Id of the most recently started execution (for overlap checks).
    #[serde(default)]
    pub last_execution_id: Option<String>,
}

impl Schedule {
    /// A schedule firing every `interval_ms`, first due at `first_fire_ms`.
    pub fn every(
        id: impl Into<String>,
        workflow_name: impl Into<String>,
        interval_ms: u64,
        first_fire_ms: u64,
    ) -> Self {
        Self {
            id: id.into(),
            workflow_name: workflow_name.into(),
            input: Value::Null,
            interval_ms,
            next_fire_ms: first_fire_ms,
            paused: false,
            overlap: OverlapPolicy::Skip,
            last_execution_id: None,
        }
    }

    /// Set the run input.
    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    /// Set the overlap policy.
    pub fn with_overlap(mut self, overlap: OverlapPolicy) -> Self {
        self.overlap = overlap;
        self
    }

    /// Advance `next_fire_ms` past `now`, skipping any missed ticks.
    fn advance_past(&mut self, now: u64) {
        self.next_fire_ms = self.next_fire_ms.saturating_add(self.interval_ms);
        while self.next_fire_ms <= now {
            self.next_fire_ms = self.next_fire_ms.saturating_add(self.interval_ms);
        }
    }
}

/// Durable store of schedules.
#[async_trait]
pub trait ScheduleStore: Send + Sync {
    /// Create or replace a schedule.
    async fn save(&self, schedule: &Schedule) -> Result<()>;
    /// Fetch a schedule by id.
    async fn get(&self, id: &str) -> Result<Option<Schedule>>;
    /// List all schedules (deterministic order, by id).
    async fn list(&self) -> Result<Vec<Schedule>>;
    /// Remove a schedule. Idempotent.
    async fn remove(&self, id: &str) -> Result<()>;
}

/// An in-process [`ScheduleStore`]. Cloning shares state.
#[derive(Clone, Default)]
pub struct InMemoryScheduleStore {
    inner: Arc<Mutex<BTreeMap<String, Schedule>>>,
}

impl InMemoryScheduleStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ScheduleStore for InMemoryScheduleStore {
    async fn save(&self, schedule: &Schedule) -> Result<()> {
        self.inner
            .lock()
            .expect("schedule mutex poisoned")
            .insert(schedule.id.clone(), schedule.clone());
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        Ok(self
            .inner
            .lock()
            .expect("schedule mutex poisoned")
            .get(id)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        Ok(self
            .inner
            .lock()
            .expect("schedule mutex poisoned")
            .values()
            .cloned()
            .collect())
    }

    async fn remove(&self, id: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("schedule mutex poisoned")
            .remove(id);
        Ok(())
    }
}

/// A file-backed [`ScheduleStore`]: all schedules live in one JSON file under a
/// directory, surviving restarts. A process-local async mutex serializes writes.
pub struct FileScheduleStore {
    path: PathBuf,
    guard: AsyncMutex<()>,
}

impl FileScheduleStore {
    /// Create a store under `dir`, persisting to `dir/schedules.json`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("schedules.json"),
            guard: AsyncMutex::new(()),
        })
    }

    async fn load_all(&self) -> Result<BTreeMap<String, Schedule>> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    async fn save_all(&self, schedules: &BTreeMap<String, Schedule>) -> Result<()> {
        let json = serde_json::to_string_pretty(schedules)?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }
}

#[async_trait]
impl ScheduleStore for FileScheduleStore {
    async fn save(&self, schedule: &Schedule) -> Result<()> {
        let _g = self.guard.lock().await;
        let mut all = self.load_all().await?;
        all.insert(schedule.id.clone(), schedule.clone());
        self.save_all(&all).await
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        let _g = self.guard.lock().await;
        Ok(self.load_all().await?.get(id).cloned())
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        let _g = self.guard.lock().await;
        Ok(self.load_all().await?.into_values().collect())
    }

    async fn remove(&self, id: &str) -> Result<()> {
        let _g = self.guard.lock().await;
        let mut all = self.load_all().await?;
        if all.remove(id).is_some() {
            self.save_all(&all).await?;
        }
        Ok(())
    }
}

/// Starts due schedules' executions. Caller-driven: a host calls [`poll`](Self::poll)
/// on an interval (the CLI's `workflows tick`, or a worker loop).
pub struct ScheduleDispatcher {
    engine: Engine,
    store: Arc<dyn ScheduleStore>,
    clock: Arc<dyn Clock>,
    resolver: DefinitionResolver,
}

impl ScheduleDispatcher {
    /// Build a dispatcher over an engine, the schedule store, a clock, and a
    /// resolver mapping a workflow name to its definition.
    pub fn new(
        engine: Engine,
        store: Arc<dyn ScheduleStore>,
        clock: Arc<dyn Clock>,
        resolver: DefinitionResolver,
    ) -> Self {
        Self {
            engine,
            store,
            clock,
            resolver,
        }
    }

    /// Start an execution for every schedule due now, honoring its overlap policy.
    /// Returns the ids of the executions started this poll.
    pub async fn poll(&self) -> Result<Vec<String>> {
        let now = self.clock.now_millis();
        let mut started = Vec::new();
        for mut schedule in self.store.list().await? {
            if schedule.paused || schedule.next_fire_ms > now {
                continue;
            }

            // Overlap: skip a new run if the previous one is still active.
            if schedule.overlap == OverlapPolicy::Skip
                && let Some(prev) = &schedule.last_execution_id
                && let Some(state) = self.engine.query(prev).await?
                && !state.status.is_terminal()
            {
                schedule.advance_past(now);
                self.store.save(&schedule).await?;
                continue;
            }

            let Some(def) = (self.resolver)(&schedule.workflow_name) else {
                // Unresolvable here — leave it for a host that has the definition.
                continue;
            };

            let exec_id = format!("{}-{}", schedule.id, schedule.next_fire_ms);
            self.engine
                .run(&def, &exec_id, schedule.input.clone())
                .await?;
            started.push(exec_id.clone());

            schedule.last_execution_id = Some(exec_id);
            schedule.advance_past(now);
            self.store.save(&schedule).await?;
        }
        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advances_past_now_skipping_missed_ticks() {
        let mut s = Schedule::every("s", "w", 1_000, 1_000);
        s.advance_past(1_000); // fired at 1000 → next strictly after 1000
        assert_eq!(s.next_fire_ms, 2_000);

        let mut s = Schedule::every("s", "w", 1_000, 1_000);
        s.advance_past(5_500); // host was down; skip missed ticks
        assert_eq!(s.next_fire_ms, 6_000);
    }
}
