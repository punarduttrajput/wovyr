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

use crate::cron::Cron;
use crate::engine::Engine;
use crate::timer::Clock;
use crate::worker::DefinitionResolver;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use wovyr_common::{Error, Result};

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

/// A recurring schedule that starts a workflow on a cadence — either a fixed
/// `interval_ms` or, when `cron` is set, a cron expression (UTC).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schedule {
    /// Unique schedule id (also the execution-id prefix).
    pub id: String,
    /// Name of the workflow to start (resolved via the dispatcher's resolver).
    pub workflow_name: String,
    /// Run input passed to each started execution.
    #[serde(default)]
    pub input: Value,
    /// Interval between fires, in milliseconds. Ignored when `cron` is set.
    pub interval_ms: u64,
    /// Cron expression (5-field or `@macro`, UTC). When present it drives the
    /// cadence instead of `interval_ms` ([G2 cron](../../docs/03-workflow-engine/temporal-gap-analysis.md#g2--schedules--cron)).
    #[serde(default)]
    pub cron: Option<String>,
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
            cron: None,
            next_fire_ms: first_fire_ms,
            paused: false,
            overlap: OverlapPolicy::Skip,
            last_execution_id: None,
        }
    }

    /// A schedule driven by a cron `expr` (UTC), with its first fire computed as the
    /// next cron instant strictly after `now_ms`. Validates the expression.
    pub fn cron(
        id: impl Into<String>,
        workflow_name: impl Into<String>,
        expr: impl Into<String>,
        now_ms: u64,
    ) -> Result<Self> {
        let expr = expr.into();
        let parsed = Cron::parse(&expr)?;
        let first_fire = parsed
            .next_after(now_ms)
            .ok_or_else(|| Error::Invalid(format!("cron `{expr}` has no upcoming fire time")))?;
        Ok(Self {
            id: id.into(),
            workflow_name: workflow_name.into(),
            input: Value::Null,
            interval_ms: 0,
            cron: Some(expr),
            next_fire_ms: first_fire,
            paused: false,
            overlap: OverlapPolicy::Skip,
            last_execution_id: None,
        })
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

    /// Advance `next_fire_ms` past `now`, skipping any missed ticks. For a cron
    /// schedule the next fire is the next cron instant strictly after `now`; for an
    /// interval schedule it is the next multiple of `interval_ms` past `now`.
    fn advance_past(&mut self, now: u64) -> Result<()> {
        if let Some(expr) = &self.cron {
            self.next_fire_ms = Cron::parse(expr)?
                .next_after(now)
                .ok_or_else(|| Error::Invalid(format!("cron `{expr}` has no further fire time")))?;
        } else {
            self.next_fire_ms = self.next_fire_ms.saturating_add(self.interval_ms);
            while self.next_fire_ms <= now {
                self.next_fire_ms = self.next_fire_ms.saturating_add(self.interval_ms);
            }
        }
        Ok(())
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

    /// The earliest `next_fire_ms` among every non-paused schedule (WFL-306) —
    /// `None` if there are none. Lets a dispatcher sleep exactly until the next
    /// deadline instead of polling on a fixed interval.
    async fn next_deadline(&self) -> Result<Option<u64>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|s| !s.paused)
            .map(|s| s.next_fire_ms)
            .min())
    }
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
                schedule.advance_past(now)?;
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
            schedule.advance_past(now)?;
            self.store.save(&schedule).await?;
        }
        Ok(started)
    }

    /// The earliest pending deadline, in Unix-epoch milliseconds — see
    /// [`ScheduleStore::next_deadline`].
    pub async fn next_deadline_ms(&self) -> Result<Option<u64>> {
        self.store.next_deadline().await
    }

    /// Run indefinitely: [`poll`](Self::poll), then sleep exactly until the next
    /// pending deadline instead of a fixed interval (WFL-306), capped at
    /// `max_interval` so the loop still wakes up periodically to notice a
    /// schedule registered by another process in the meantime. Checked once per
    /// iteration, `should_stop` lets a caller shut the loop down cleanly (e.g.
    /// on server shutdown); returns `Ok(())` the first time it reports `true`.
    pub async fn run_adaptive(
        &self,
        max_interval: std::time::Duration,
        mut should_stop: impl FnMut() -> bool,
    ) -> Result<()> {
        loop {
            if should_stop() {
                return Ok(());
            }
            self.poll().await?;
            let now = self.clock.now_millis();
            let sleep_for = match self.next_deadline_ms().await? {
                Some(next) => std::time::Duration::from_millis(next.saturating_sub(now)),
                None => max_interval,
            }
            .min(max_interval);
            tokio::time::sleep(sleep_for).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advances_past_now_skipping_missed_ticks() {
        let mut s = Schedule::every("s", "w", 1_000, 1_000);
        s.advance_past(1_000).unwrap(); // fired at 1000 → next strictly after 1000
        assert_eq!(s.next_fire_ms, 2_000);

        let mut s = Schedule::every("s", "w", 1_000, 1_000);
        s.advance_past(5_500).unwrap(); // host was down; skip missed ticks
        assert_eq!(s.next_fire_ms, 6_000);
    }

    /// WFL-306: `next_deadline` is the minimum `next_fire_ms` among non-paused
    /// schedules, ignoring paused ones entirely.
    #[tokio::test]
    async fn next_deadline_ignores_paused_schedules_and_tracks_the_minimum() {
        let store = InMemoryScheduleStore::new();
        assert_eq!(store.next_deadline().await.unwrap(), None);

        store
            .save(&Schedule::every("a", "w", 1_000, 5_000))
            .await
            .unwrap();
        assert_eq!(store.next_deadline().await.unwrap(), Some(5_000));

        // A closer, but paused, schedule must not win.
        let mut paused = Schedule::every("b", "w", 1_000, 100);
        paused.paused = true;
        store.save(&paused).await.unwrap();
        assert_eq!(
            store.next_deadline().await.unwrap(),
            Some(5_000),
            "a paused schedule's deadline must not be considered"
        );

        // Unpausing it makes it the new minimum.
        paused.paused = false;
        store.save(&paused).await.unwrap();
        assert_eq!(store.next_deadline().await.unwrap(), Some(100));
    }

    #[test]
    fn cron_schedule_computes_fire_times() {
        // Every 5 minutes; first fire strictly after t=0 is minute 5.
        let mut s = Schedule::cron("c", "w", "*/5 * * * *", 0).unwrap();
        assert_eq!(s.next_fire_ms, 300_000);
        s.advance_past(300_000).unwrap();
        assert_eq!(s.next_fire_ms, 600_000);

        // A macro is accepted and validated.
        assert!(Schedule::cron("d", "w", "@daily", 0).is_ok());
        // An invalid expression is rejected at construction.
        assert!(Schedule::cron("e", "w", "nope", 0).is_err());
    }
}
