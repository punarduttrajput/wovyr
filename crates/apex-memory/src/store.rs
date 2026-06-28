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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A retrieval hit pushed down to the store: a record id and its normalized
/// relevance in `[0,1]`, ordered best-first.
pub type ScoredId = (String, f32);

/// Durable store for memory records.
///
/// Beyond `put`/`all`, a store may **push retrieval down** (vector ANN, keyword
/// search) when it has a purpose-built index — see [`Self::supports_pushdown`].
/// Stores that don't (the in-memory/file stores) leave the defaults, and the engine
/// ranks in-process over [`Self::all`].
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Store a record, assigning its `seq` and `id`; returns the id.
    async fn put(&self, record: MemoryRecord) -> Result<String>;
    /// All records, optionally restricted to a namespace.
    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>>;

    /// Fetch records by id (any order). Defaults to scanning [`Self::all`]; stores
    /// with primary-key lookup should override.
    async fn get(&self, ids: &[String]) -> Result<Vec<MemoryRecord>> {
        let want: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        Ok(self
            .all(None)
            .await?
            .into_iter()
            .filter(|r| want.contains(r.id.as_str()))
            .collect())
    }

    /// Delete records by id (missing ids are ignored). Used by compaction to retire
    /// records that have been consolidated into a summary. Defaults to unsupported.
    async fn delete(&self, _ids: &[String]) -> Result<()> {
        Err(Error::invalid("this store does not support deletion"))
    }

    /// Whether this store implements retrieval pushdown ([`Self::vector_search`] /
    /// [`Self::keyword_search`]). When `false`, the engine ranks in-process.
    fn supports_pushdown(&self) -> bool {
        false
    }

    /// Approximate-nearest-neighbour search over embeddings → up to `k` hits
    /// best-first. `None` means the store does not support vector pushdown.
    async fn vector_search(
        &self,
        _namespace: Option<&str>,
        _query: &[f32],
        _k: usize,
    ) -> Result<Option<Vec<ScoredId>>> {
        Ok(None)
    }

    /// Keyword/full-text search → up to `k` hits best-first. `None` means the store
    /// does not support keyword pushdown.
    async fn keyword_search(
        &self,
        _namespace: Option<&str>,
        _query: &str,
        _k: usize,
    ) -> Result<Option<Vec<ScoredId>>> {
        Ok(None)
    }
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

    async fn delete(&self, ids: &[String]) -> Result<()> {
        let want: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let mut records = self.inner.lock().expect("memory mutex poisoned");
        records.retain(|r| !want.contains(r.id.as_str()));
        Ok(())
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

    async fn delete(&self, ids: &[String]) -> Result<()> {
        let want: HashSet<&str> = ids.iter().map(String::as_str).collect();
        // Rewrite each namespace file that holds a targeted id, keeping the rest.
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let records = self.read_ns(&path).await?;
            if !records.iter().any(|r| want.contains(r.id.as_str())) {
                continue;
            }
            let kept: Vec<String> = records
                .into_iter()
                .filter(|r| !want.contains(r.id.as_str()))
                .map(|r| serde_json::to_string(&r))
                .collect::<std::result::Result<_, _>>()?;
            let mut body = kept.join("\n");
            if !body.is_empty() {
                body.push('\n');
            }
            // Atomic replace: write a temp file then rename.
            let tmp = path.with_extension("jsonl.tmp");
            tokio::fs::write(&tmp, body).await?;
            tokio::fs::rename(&tmp, &path).await?;
        }
        Ok(())
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
