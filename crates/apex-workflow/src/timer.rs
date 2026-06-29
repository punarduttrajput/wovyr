//! Durable wall-clock timers and the clock abstraction.
//!
//! Closes the timer half of [gap closure
//! G1](../../docs/03-workflow-engine/temporal-gap-analysis.md#g1--durable-wall-clock-timers):
//! a `wait` activity can declare a wall-clock deadline (`{timer: {after: "30d"}}`
//! or `{timer: {at: <epoch_ms>}}`) and the workflow resumes **autonomously** when
//! the deadline passes — no external signal required.
//!
//! Determinism is preserved by isolating the clock at the **dispatcher boundary**.
//! The engine reads the clock exactly once, when it first registers a timer, and
//! records the resulting `fire_at` deadline in the event log + checkpoint; it never
//! recomputes the deadline on resume. The only component that polls "is it due
//! yet?" is the [`TimerDispatcher`]. Core scheduling stays clock-free, so tests
//! drive time deterministically with a [`ManualClock`].

use crate::engine::Engine;
use crate::worker::DefinitionResolver;
use apex_common::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as AsyncMutex;

/// A source of the current time as **Unix-epoch milliseconds**.
///
/// Injected into the [`Engine`](crate::Engine) and [`TimerDispatcher`] so core
/// logic never reads an ambient clock — keeping execution deterministic and
/// testable ([coding-standards §7](../../docs/19-implementation-guide/coding-standards.md)).
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_millis(&self) -> u64;
}

/// The real wall clock.
#[derive(Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A manually-advanced clock for deterministic tests. Cloning shares the same
/// backing instant, so a test and the engine under test see one timeline.
#[derive(Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    /// A clock pinned at `start_ms`.
    pub fn new(start_ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start_ms)))
    }

    /// Advance the clock by `delta_ms`.
    pub fn advance(&self, delta_ms: u64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }

    /// Set the clock to an absolute `ms`.
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A timer registered against a suspended execution, awaiting its deadline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingTimer {
    /// The execution that is waiting.
    pub execution_id: String,
    /// The logical timer id (the wait's `timer.<id>` key).
    pub timer_id: String,
    /// When the timer should fire, in Unix-epoch milliseconds.
    pub fire_at_ms: u64,
}

/// Durable registry of pending wall-clock timers — the queue the dispatcher polls.
///
/// Kept separate from the checkpoint store so "find due timers" is a small, cheap
/// scan over only what is pending, not over every execution.
#[async_trait]
pub trait TimerStore: Send + Sync {
    /// Register (or replace) a pending timer.
    async fn schedule(&self, timer: PendingTimer) -> Result<()>;
    /// Remove a timer (it fired, or its execution ended). Idempotent.
    async fn cancel(&self, execution_id: &str, timer_id: &str) -> Result<()>;
    /// All timers due at `now_ms` (i.e. `fire_at_ms <= now_ms`), in deterministic
    /// order (by deadline, then execution id, then timer id).
    async fn due(&self, now_ms: u64) -> Result<Vec<PendingTimer>>;
}

/// An in-process [`TimerStore`] for tests and single-node use. Cloning shares state.
#[derive(Clone, Default)]
pub struct InMemoryTimerStore {
    inner: Arc<Mutex<BTreeMap<(String, String), PendingTimer>>>,
}

impl InMemoryTimerStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TimerStore for InMemoryTimerStore {
    async fn schedule(&self, timer: PendingTimer) -> Result<()> {
        let mut g = self.inner.lock().expect("timer mutex poisoned");
        g.insert((timer.execution_id.clone(), timer.timer_id.clone()), timer);
        Ok(())
    }

    async fn cancel(&self, execution_id: &str, timer_id: &str) -> Result<()> {
        let mut g = self.inner.lock().expect("timer mutex poisoned");
        g.remove(&(execution_id.to_string(), timer_id.to_string()));
        Ok(())
    }

    async fn due(&self, now_ms: u64) -> Result<Vec<PendingTimer>> {
        let g = self.inner.lock().expect("timer mutex poisoned");
        let mut due: Vec<PendingTimer> = g
            .values()
            .filter(|t| t.fire_at_ms <= now_ms)
            .cloned()
            .collect();
        due.sort_by(|a, b| {
            a.fire_at_ms
                .cmp(&b.fire_at_ms)
                .then_with(|| a.execution_id.cmp(&b.execution_id))
                .then_with(|| a.timer_id.cmp(&b.timer_id))
        });
        Ok(due)
    }
}

/// A file-backed [`TimerStore`]: all pending timers live in one JSON file, so they
/// survive process restarts (what makes the CLI's `workflows tick` meaningful
/// across invocations). A process-local async mutex serializes read-modify-write.
pub struct FileTimerStore {
    path: PathBuf,
    guard: AsyncMutex<()>,
}

impl FileTimerStore {
    /// Create a store under `dir`, persisting to `dir/timers.json`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("timers.json"),
            guard: AsyncMutex::new(()),
        })
    }

    async fn load_all(&self) -> Result<Vec<PendingTimer>> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    async fn save_all(&self, timers: &[PendingTimer]) -> Result<()> {
        let json = serde_json::to_string_pretty(timers)?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }
}

#[async_trait]
impl TimerStore for FileTimerStore {
    async fn schedule(&self, timer: PendingTimer) -> Result<()> {
        let _g = self.guard.lock().await;
        let mut timers = self.load_all().await?;
        timers.retain(|t| !(t.execution_id == timer.execution_id && t.timer_id == timer.timer_id));
        timers.push(timer);
        self.save_all(&timers).await
    }

    async fn cancel(&self, execution_id: &str, timer_id: &str) -> Result<()> {
        let _g = self.guard.lock().await;
        let mut timers = self.load_all().await?;
        let before = timers.len();
        timers.retain(|t| !(t.execution_id == execution_id && t.timer_id == timer_id));
        if timers.len() != before {
            self.save_all(&timers).await?;
        }
        Ok(())
    }

    async fn due(&self, now_ms: u64) -> Result<Vec<PendingTimer>> {
        let _g = self.guard.lock().await;
        let mut due: Vec<PendingTimer> = self
            .load_all()
            .await?
            .into_iter()
            .filter(|t| t.fire_at_ms <= now_ms)
            .collect();
        due.sort_by(|a, b| {
            a.fire_at_ms
                .cmp(&b.fire_at_ms)
                .then_with(|| a.execution_id.cmp(&b.execution_id))
                .then_with(|| a.timer_id.cmp(&b.timer_id))
        });
        Ok(due)
    }
}

/// Drives durable timers: finds the ones due now and resumes their executions.
///
/// Caller-driven — a host calls [`poll`](Self::poll) on an interval (the CLI's
/// `workflows tick`, or a worker loop). This keeps the engine deterministic: the
/// dispatcher is the single place that consults the clock to decide a timer is due.
pub struct TimerDispatcher {
    engine: Engine,
    timers: Arc<dyn TimerStore>,
    clock: Arc<dyn Clock>,
    resolver: DefinitionResolver,
}

impl TimerDispatcher {
    /// Build a dispatcher over an engine, the shared timer store, a clock, and a
    /// resolver that maps a workflow name to its definition.
    pub fn new(
        engine: Engine,
        timers: Arc<dyn TimerStore>,
        clock: Arc<dyn Clock>,
        resolver: DefinitionResolver,
    ) -> Self {
        Self {
            engine,
            timers,
            clock,
            resolver,
        }
    }

    /// Fire every timer due at the current time, resuming each execution. Returns
    /// the ids of the timers fired. Stale timers (execution gone, or its definition
    /// unresolvable) are dropped rather than retried forever.
    pub async fn poll(&self) -> Result<Vec<String>> {
        let now = self.clock.now_millis();
        let due = self.timers.due(now).await?;
        let mut fired = Vec::new();
        for timer in due {
            let Some(state) = self.engine.query(&timer.execution_id).await? else {
                // Execution vanished — drop the orphan timer.
                self.timers
                    .cancel(&timer.execution_id, &timer.timer_id)
                    .await?;
                continue;
            };
            let Some(def) = (self.resolver)(&state.workflow_name) else {
                // No definition for this workflow on this host — leave it for a
                // host that can resolve it.
                continue;
            };
            self.engine
                .fire_timer(&def, &timer.execution_id, &timer.timer_id)
                .await?;
            self.timers
                .cancel(&timer.execution_id, &timer.timer_id)
                .await?;
            fired.push(timer.timer_id);
        }
        Ok(fired)
    }
}

/// Parse a duration value from a `wait` timer's `after` field into milliseconds.
/// Accepts a JSON number (already milliseconds) or a string with a unit suffix
/// (`ms`, `s`, `m`, `h`, `d`), e.g. `"30d"`, `"5m"`, `"500ms"`.
pub(crate) fn parse_duration_ms(value: &serde_json::Value) -> Result<u64> {
    use apex_common::Error;
    if let Some(n) = value.as_u64() {
        return Ok(n);
    }
    let s = value
        .as_str()
        .ok_or_else(|| Error::Invalid("timer `after` must be a number (ms) or a string".into()))?
        .trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| Error::Invalid(format!("timer duration `{s}` needs a unit (ms/s/m/h/d)")))?;
    let (num, unit) = s.split_at(split);
    let n: u64 = num
        .parse()
        .map_err(|_| Error::Invalid(format!("invalid timer duration `{s}`")))?;
    let mult = match unit.trim() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        other => {
            return Err(Error::Invalid(format!(
                "unknown timer duration unit `{other}` (use ms/s/m/h/d)"
            )));
        }
    };
    Ok(n.saturating_mul(mult))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_units() {
        use serde_json::json;
        assert_eq!(parse_duration_ms(&json!("500ms")).unwrap(), 500);
        assert_eq!(parse_duration_ms(&json!("5s")).unwrap(), 5_000);
        assert_eq!(parse_duration_ms(&json!("5m")).unwrap(), 300_000);
        assert_eq!(parse_duration_ms(&json!("2h")).unwrap(), 7_200_000);
        assert_eq!(parse_duration_ms(&json!("30d")).unwrap(), 2_592_000_000);
        assert_eq!(parse_duration_ms(&json!(1234)).unwrap(), 1234);
        assert!(parse_duration_ms(&json!("5x")).is_err());
    }

    #[tokio::test]
    async fn due_returns_only_passed_deadlines_in_order() {
        let store = InMemoryTimerStore::new();
        store
            .schedule(PendingTimer {
                execution_id: "e2".into(),
                timer_id: "t".into(),
                fire_at_ms: 100,
            })
            .await
            .unwrap();
        store
            .schedule(PendingTimer {
                execution_id: "e1".into(),
                timer_id: "t".into(),
                fire_at_ms: 100,
            })
            .await
            .unwrap();
        store
            .schedule(PendingTimer {
                execution_id: "e3".into(),
                timer_id: "t".into(),
                fire_at_ms: 5_000,
            })
            .await
            .unwrap();

        // Nothing due before the first deadline.
        assert!(store.due(99).await.unwrap().is_empty());
        // At 100, the two deadline-100 timers are due, ordered by execution id.
        let due = store.due(100).await.unwrap();
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].execution_id, "e1");
        assert_eq!(due[1].execution_id, "e2");

        store.cancel("e1", "t").await.unwrap();
        assert_eq!(store.due(100).await.unwrap().len(), 1);
    }
}
