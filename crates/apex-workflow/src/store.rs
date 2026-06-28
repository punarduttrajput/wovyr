//! Durable persistence: the event log and checkpoint store.
//!
//! Two ports back the engine's durability
//! ([persistence §10](../../docs/03-workflow-engine/overview.md),
//! [checkpointing §25](../../docs/03-workflow-engine/checkpointing-specification.md)):
//! an append-only [`EventLog`] (source of truth) and a [`CheckpointStore`] (latest
//! full snapshot for fast recovery). Two implementations ship in v0.2: an
//! [`InMemoryStore`] for tests and a [`FileStore`] for real cross-process
//! durability. Postgres/S3/RocksDB adapters arrive later.

use crate::engine::ExecutionState;
use crate::event::WorkflowEvent;
use apex_common::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
}

impl FileStore {
    /// Create a store rooted at `dir`, creating the directory if needed.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
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
        file.flush().await?;

        // Sequence = line count after append.
        let seq = load_lines(&path).await?.len() as u64;
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
        // Write to a temp file then rename for atomicity (no partial snapshots,
        // per checkpointing §17).
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, &path).await?;
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
}

/// Read a file into lines, treating "not found" as empty.
async fn load_lines(path: &Path) -> Result<Vec<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(contents.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Io(e)),
    }
}
