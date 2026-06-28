//! Memory persistence.
//!
//! A [`MemoryStore`] holds records and assigns monotonic sequence numbers + ids.
//! v0.2 ships an [`InMemoryStore`] (tests) and a [`FileStore`] (JSON-lines per
//! namespace) so the CLI can persist across invocations. Postgres + Qdrant
//! adapters ([storage-architecture](../../docs/06-memory-engine/storage-architecture.md))
//! arrive later.

use crate::record::MemoryRecord;
use apex_common::{Error, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Durable store for memory records.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Store a record, assigning its `seq` and `id`; returns the id.
    async fn put(&self, record: MemoryRecord) -> Result<String>;
    /// All records, optionally restricted to a namespace.
    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>>;
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

/// An in-memory store; clones share the same data (via `Arc`).
#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Mutex<Vec<MemoryRecord>>>,
}

impl InMemoryStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn put(&self, mut record: MemoryRecord) -> Result<String> {
        let mut records = self.inner.lock().expect("memory mutex poisoned");
        let seq = records.len() as u64 + 1;
        record.seq = seq;
        record.id = make_id(&record.namespace, seq);
        let id = record.id.clone();
        records.push(record);
        Ok(id)
    }

    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>> {
        let records = self.inner.lock().expect("memory mutex poisoned");
        Ok(filter_ns(records.iter().cloned(), namespace))
    }
}

// ---------------------------------------------------------------------------
// File store
// ---------------------------------------------------------------------------

/// A filesystem store: one JSON-lines file per namespace under a directory.
#[derive(Clone)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// Create a store rooted at `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, namespace: &str) -> PathBuf {
        let stem: String = namespace
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{stem}.jsonl"))
    }

    async fn read_ns(&self, path: &Path) -> Result<Vec<MemoryRecord>> {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => contents
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).map_err(Error::from))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Io(e)),
        }
    }
}

#[async_trait]
impl MemoryStore for FileStore {
    async fn put(&self, mut record: MemoryRecord) -> Result<String> {
        use tokio::io::AsyncWriteExt;

        let path = self.path(&record.namespace);
        let seq = self.read_ns(&path).await?.len() as u64 + 1;
        record.seq = seq;
        record.id = make_id(&record.namespace, seq);
        let id = record.id.clone();

        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(id)
    }

    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>> {
        match namespace {
            Some(ns) => self.read_ns(&self.path(ns)).await,
            None => {
                // Aggregate every namespace file in the directory.
                let mut out = Vec::new();
                let mut entries = tokio::fs::read_dir(&self.dir).await?;
                while let Some(entry) = entries.next_entry().await? {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        out.extend(self.read_ns(&entry.path()).await?);
                    }
                }
                Ok(out)
            }
        }
    }
}

/// Deterministic record id from namespace + sequence.
fn make_id(namespace: &str, seq: u64) -> String {
    format!("mem-{namespace}-{seq}")
}

/// Filter an iterator of records by optional namespace.
fn filter_ns(
    records: impl Iterator<Item = MemoryRecord>,
    namespace: Option<&str>,
) -> Vec<MemoryRecord> {
    match namespace {
        Some(ns) => records.filter(|r| r.namespace == ns).collect(),
        None => records.collect(),
    }
}
