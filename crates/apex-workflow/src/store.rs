//! Durable persistence: the event log and checkpoint store.
//!
//! Two ports back the engine's durability
//! ([persistence §10](../../docs/03-workflow-engine/overview.md),
//! [checkpointing §25](../../docs/03-workflow-engine/checkpointing-specification.md)):
//! an append-only [`EventLog`] (source of truth) and a [`CheckpointStore`] (latest
//! full snapshot for fast recovery). Two implementations ship in v0.2: an
//! [`InMemoryStore`] for tests and a [`FileStore`] for real cross-process
//! durability. Postgres/S3/RocksDB adapters arrive later.

use crate::engine::{ExecutionFilter, ExecutionState};
use crate::event::WorkflowEvent;
use apex_common::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// Append-only log of workflow events.
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Append an event, returning its 1-based sequence number.
    async fn append(&self, execution_id: &str, event: WorkflowEvent) -> Result<u64>;
    /// Load all events for an execution, in order.
    async fn load(&self, execution_id: &str) -> Result<Vec<WorkflowEvent>>;
}

/// Stores the latest full execution snapshot for fast recovery.
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Persist a snapshot (overwriting any previous one for this execution).
    async fn save(&self, snapshot: &ExecutionState) -> Result<()>;
    /// Load the latest snapshot for an execution, if any.
    async fn latest(&self, execution_id: &str) -> Result<Option<ExecutionState>>;
    /// List snapshots matching `filter`, ordered deterministically by execution id,
    /// honoring `filter.limit` (G4 visibility). The default scans all snapshots;
    /// indexed stores may override for efficiency.
    async fn list(&self, filter: &ExecutionFilter) -> Result<Vec<ExecutionState>>;
}

/// Order, filter, and cap a set of snapshots per an [`ExecutionFilter`] — the shared
/// logic behind the scanning stores' `list`.
fn apply_filter(
    mut snapshots: Vec<ExecutionState>,
    filter: &ExecutionFilter,
) -> Vec<ExecutionState> {
    snapshots.retain(|s| filter.matches(s));
    snapshots.sort_by(|a, b| a.execution_id.cmp(&b.execution_id));
    if let Some(limit) = filter.limit {
        snapshots.truncate(limit);
    }
    snapshots
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

/// An in-memory event log + checkpoint store. Cloning shares the same backing
/// data (via `Arc`), so two `Engine`s built from the same `InMemoryStore` observe
/// each other's writes — useful for simulating recovery within a test.
#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    events: Mutex<HashMap<String, Vec<WorkflowEvent>>>,
    checkpoints: Mutex<HashMap<String, ExecutionState>>,
}

impl InMemoryStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl EventLog for InMemoryStore {
    async fn append(&self, execution_id: &str, event: WorkflowEvent) -> Result<u64> {
        let mut events = self.inner.events.lock().expect("events mutex poisoned");
        let log = events.entry(execution_id.to_string()).or_default();
        log.push(event);
        Ok(log.len() as u64)
    }

    async fn load(&self, execution_id: &str) -> Result<Vec<WorkflowEvent>> {
        let events = self.inner.events.lock().expect("events mutex poisoned");
        Ok(events.get(execution_id).cloned().unwrap_or_default())
    }
}

#[async_trait]
impl CheckpointStore for InMemoryStore {
    async fn save(&self, snapshot: &ExecutionState) -> Result<()> {
        let mut cps = self
            .inner
            .checkpoints
            .lock()
            .expect("checkpoint mutex poisoned");
        cps.insert(snapshot.execution_id.clone(), snapshot.clone());
        Ok(())
    }

    async fn latest(&self, execution_id: &str) -> Result<Option<ExecutionState>> {
        let cps = self
            .inner
            .checkpoints
            .lock()
            .expect("checkpoint mutex poisoned");
        Ok(cps.get(execution_id).cloned())
    }

    async fn list(&self, filter: &ExecutionFilter) -> Result<Vec<ExecutionState>> {
        let snapshots: Vec<ExecutionState> = self
            .inner
            .checkpoints
            .lock()
            .expect("checkpoint mutex poisoned")
            .values()
            .cloned()
            .collect();
        Ok(apply_filter(snapshots, filter))
    }
}

// ---------------------------------------------------------------------------
// File store
// ---------------------------------------------------------------------------

/// A filesystem-backed store: events as JSON lines (`<id>.events.jsonl`) and the
/// latest checkpoint as JSON (`<id>.checkpoint.json`) under a directory. Durable
/// across process restarts, which is what makes `resume` meaningful.
#[derive(Clone)]
pub struct FileStore {
    dir: PathBuf,
    /// In-process cache of each execution's last-appended sequence number, so a
    /// warm append is O(1) instead of re-reading the whole event file to count
    /// lines. Seeded lazily (once per execution id, per `FileStore` instance) from
    /// the file's actual line count — the source of truth stays the file itself,
    /// this is purely an amortization of repeated appends within one process's
    /// lifetime. A fresh `FileStore` opened later (e.g. after a restart) reseeds
    /// from disk on its own first append, so this cache never needs to survive a
    /// process boundary.
    seqs: Arc<AsyncMutex<HashMap<String, u64>>>,
}

impl FileStore {
    /// Create a store rooted at `dir`, creating the directory if needed.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            seqs: Arc::new(AsyncMutex::new(HashMap::new())),
        })
    }

    /// Sanitize an execution id into a safe filename stem.
    fn stem(execution_id: &str) -> String {
        execution_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn events_path(&self, execution_id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.events.jsonl", Self::stem(execution_id)))
    }

    fn checkpoint_path(&self, execution_id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.checkpoint.json", Self::stem(execution_id)))
    }
}

#[async_trait]
impl EventLog for FileStore {
    async fn append(&self, execution_id: &str, event: WorkflowEvent) -> Result<u64> {
        use tokio::io::AsyncWriteExt;

        let path = self.events_path(execution_id);
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        // Durability, not just page-cache durability (DUR-402): an
        // acknowledged append must survive a crash immediately after it
        // returns, since `resume` trusts the event log as the source of
        // truth.
        file.sync_data().await?;
        drop(file);
        sync_dir(&self.dir).await?;

        let mut seqs = self.seqs.lock().await;
        let seq = match seqs.get(execution_id) {
            // Warm path: O(1), no re-read of the file.
            Some(&last) => last + 1,
            // Cold path (first append this `FileStore` has seen for this
            // execution): seed the counter from the file's line count, which
            // already includes the append above. Paid once per execution per
            // process lifetime, not once per append — the fix for the O(N^2)
            // total-append cost across an execution's lifetime.
            None => load_lines(&path).await?.len() as u64,
        };
        seqs.insert(execution_id.to_string(), seq);
        Ok(seq)
    }

    async fn load(&self, execution_id: &str) -> Result<Vec<WorkflowEvent>> {
        let path = self.events_path(execution_id);
        let lines = load_lines(&path).await?;
        lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(Error::from))
            .collect()
    }
}

#[async_trait]
impl CheckpointStore for FileStore {
    async fn save(&self, snapshot: &ExecutionState) -> Result<()> {
        let path = self.checkpoint_path(&snapshot.execution_id);
        let json = serde_json::to_string_pretty(snapshot)?;
        // Temp file + fsync + rename + fsync-the-directory (no partial
        // snapshots, per checkpointing §17, and DUR-402: the rename itself
        // must survive a crash, not just land in the page cache).
        tokio::task::spawn_blocking(move || apex_common::fs::atomic_write(&path, json))
            .await
            .map_err(|e| Error::Runtime(format!("checkpoint save task panicked: {e}")))??;
        Ok(())
    }

    async fn latest(&self, execution_id: &str) -> Result<Option<ExecutionState>> {
        let path = self.checkpoint_path(execution_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    async fn list(&self, filter: &ExecutionFilter) -> Result<Vec<ExecutionState>> {
        let mut snapshots = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Io(e)),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.ends_with(".checkpoint.json") {
                continue;
            }
            // Read execution_id from the snapshot itself (the filename stem is
            // sanitized and not a reliable inverse).
            let contents = tokio::fs::read_to_string(entry.path()).await?;
            snapshots.push(serde_json::from_str(&contents)?);
        }
        Ok(apply_filter(snapshots, filter))
    }
}

/// Read a file into lines, treating "not found" as empty.
async fn load_lines(path: &Path) -> Result<Vec<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(contents.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// `fsync` `dir` on a blocking thread (a directory `fsync` is a syscall that
/// can block, so it shouldn't run inline on the async executor).
async fn sync_dir(dir: &Path) -> Result<()> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || apex_common::fs::sync_dir(&dir))
        .await
        .map_err(|e| Error::Runtime(format!("fsync task panicked: {e}")))??;
    Ok(())
}
