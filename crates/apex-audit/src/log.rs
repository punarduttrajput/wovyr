//! The append-only, hash-chained audit log
//! ([audit §4 Integrity](../../docs/13-security/audit.md#4-integrity-tamper-evidence)).
//!
//! Each [`AuditEntry`] commits to the prior entry's hash, so any deletion or modification
//! of a record breaks the chain and is detectable by [`AuditLog::verify`]. The chain math
//! is deterministic (no clocks/randomness here — timestamps arrive on the event), so the
//! same sequence of events always yields the same hashes.

use crate::event::AuditEvent;
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;

fn lock_config_err(e: std::io::Error) -> Error {
    Error::config(format!("lock audit log: {e}"))
}

/// A persisted audit record: the event plus its position and hash-chain links.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Derived id, `aud-<seq>`.
    pub id: String,
    /// Monotonic sequence number (0-based).
    pub seq: u64,
    /// The audited event.
    pub event: AuditEvent,
    /// Hex sha256 of the previous entry (empty string for the genesis entry).
    pub prev_hash: String,
    /// Hex sha256 over `prev_hash` + the canonical `(id, seq, event)` bytes.
    pub hash: String,
}

/// Compute the chained hash for an entry's `(id, seq, event)` given `prev_hash`.
fn chain_hash(prev_hash: &str, id: &str, seq: u64, event: &AuditEvent) -> String {
    // Canonical body: serde_json preserves struct field order, so this is stable.
    let body = serde_json::to_vec(&(id, seq, event)).expect("audit event serializes");
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(&body);
    format!("{:x}", h.finalize())
}

/// Durable storage for audit entries (append-only).
pub trait AuditSink: Send + Sync {
    /// Append an entry (never mutate or delete existing ones).
    fn append(&self, entry: &AuditEntry) -> Result<()>;
    /// All entries in insertion order.
    fn all(&self) -> Result<Vec<AuditEntry>>;
}

/// In-process sink (tests / single process).
#[derive(Default)]
pub struct InMemoryAuditSink {
    entries: Mutex<Vec<AuditEntry>>,
}

impl InMemoryAuditSink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AuditSink for InMemoryAuditSink {
    fn append(&self, entry: &AuditEntry) -> Result<()> {
        self.entries
            .lock()
            .expect("audit sink poisoned")
            .push(entry.clone());
        Ok(())
    }

    fn all(&self) -> Result<Vec<AuditEntry>> {
        Ok(self.entries.lock().expect("audit sink poisoned").clone())
    }
}

/// Filesystem sink: one JSON object per line in `audit.jsonl` (append-only).
pub struct FileAuditSink {
    path: PathBuf,
}

impl FileAuditSink {
    /// Open (or create) the log under `dir` (`<dir>/audit.jsonl`).
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::config(format!("create audit dir: {e}")))?;
        Ok(Self {
            path: dir.join("audit.jsonl"),
        })
    }
}

impl AuditSink for FileAuditSink {
    fn append(&self, entry: &AuditEntry) -> Result<()> {
        use std::io::Write as _;
        let mut line = serde_json::to_vec(entry).map_err(Error::from)?;
        line.push(b'\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| Error::config(format!("open audit.jsonl: {e}")))?;
        f.write_all(&line)
            .map_err(|e| Error::config(format!("write audit.jsonl: {e}")))?;
        f.sync_data()
            .map_err(|e| Error::config(format!("fsync audit.jsonl: {e}")))?;
        drop(f);
        apex_common::fs::sync_parent_dir(&self.path)
            .map_err(|e| Error::config(format!("fsync audit dir: {e}")))
    }

    fn all(&self) -> Result<Vec<AuditEntry>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).map_err(Error::from))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::config(format!("read audit.jsonl: {e}"))),
        }
    }
}

/// A filter for reading audit history ([audit §6](../../docs/13-security/audit.md#6-access--search)).
#[derive(Clone, Debug, Default)]
pub struct AuditFilter {
    /// Restrict to a tenant.
    pub tenant: Option<String>,
    /// Restrict to an acting principal.
    pub principal: Option<String>,
    /// Restrict to an exact action.
    pub action: Option<String>,
    /// Cap the number returned (most-recent first when set).
    pub limit: Option<usize>,
}

/// The audit log: chains events onto a [`AuditSink`] and reads them back.
///
/// `record()` always re-derives the chain tip (next sequence + last hash) from
/// `sink.all()` rather than trusting an in-memory cache across calls — for a
/// file-backed sink shared with another process (the CLI racing the server), a
/// cached tip would let a second writer append onto a stale predecessor,
/// **forking** the chain; `verify()` would then report tampering that never
/// happened (RM-GA-P2 DUR-403). A process-local `Mutex` still serializes
/// concurrent `record()` calls within this instance, and — when opened via
/// [`open_with_lock`](Self::open_with_lock) — a cross-process advisory file lock
/// additionally spans the re-derive-then-append sequence so a second *process*
/// extends the same chain instead of forking it too.
pub struct AuditLog {
    sink: Box<dyn AuditSink>,
    guard: Mutex<()>,
    lock_dir: Option<PathBuf>,
}

impl AuditLog {
    /// Open a log over `sink` (no cross-process locking — single-process use, or
    /// a purely in-memory sink where there's nothing to protect against another
    /// process). Fails if `sink.all()` itself is broken, so a fundamentally
    /// unreadable store surfaces immediately rather than on first `record()`.
    pub fn open(sink: Box<dyn AuditSink>) -> Result<Self> {
        sink.all()?;
        Ok(Self {
            sink,
            guard: Mutex::new(()),
            lock_dir: None,
        })
    }

    /// Open a log over `sink` with cross-process append safety: each `record()`
    /// holds an advisory lock on `lock_dir` (RM-GA-P2 DUR-403) spanning the
    /// re-derive-tip-then-append sequence, so a second process sharing the same
    /// directory (e.g. the CLI and server both writing `~/.apex/audit`) extends
    /// one chain instead of forking it.
    pub fn open_with_lock(sink: Box<dyn AuditSink>, lock_dir: impl Into<PathBuf>) -> Result<Self> {
        let mut log = Self::open(sink)?;
        log.lock_dir = Some(lock_dir.into());
        Ok(log)
    }

    /// A log over a fresh [`InMemoryAuditSink`].
    pub fn in_memory() -> Self {
        Self::open(Box::new(InMemoryAuditSink::new())).expect("empty in-memory log")
    }

    /// Record `event`, appending a hash-chained entry. Returns the persisted entry.
    pub fn record(&self, event: AuditEvent) -> Result<AuditEntry> {
        let _guard = self.guard.lock().expect("audit log poisoned");
        let _flock = match &self.lock_dir {
            Some(dir) => Some(apex_common::fs::FileLock::acquire(dir).map_err(lock_config_err)?),
            None => None,
        };

        // Re-derive the tip fresh under the lock — never trust a cached value,
        // since another process may have appended since we last looked.
        let existing = self.sink.all()?;
        let (seq, prev_hash) = existing
            .last()
            .map(|e| (e.seq + 1, e.hash.clone()))
            .unwrap_or((0, String::new()));

        let id = format!("aud-{seq}");
        let hash = chain_hash(&prev_hash, &id, seq, &event);
        let entry = AuditEntry {
            id,
            seq,
            event,
            prev_hash,
            hash,
        };
        self.sink.append(&entry)?;
        Ok(entry)
    }

    /// Read entries matching `filter` (most-recent first when a limit is set).
    pub fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>> {
        let mut entries = self.sink.all()?;
        entries.retain(|e| {
            filter
                .tenant
                .as_ref()
                .is_none_or(|t| &e.event.actor.tenant == t)
                && filter
                    .principal
                    .as_ref()
                    .is_none_or(|p| &e.event.actor.principal == p)
                && filter.action.as_ref().is_none_or(|a| &e.event.action == a)
        });
        if let Some(limit) = filter.limit {
            entries.reverse();
            entries.truncate(limit);
        }
        Ok(entries)
    }

    /// Verify the hash chain: every entry's hash recomputes and links to its predecessor.
    /// Returns the offending `seq` on the first break, or `Ok(())` if intact.
    pub fn verify(&self) -> Result<()> {
        let entries = self.sink.all()?;
        let mut prev = String::new();
        for (i, e) in entries.iter().enumerate() {
            if e.seq != i as u64 {
                return Err(Error::invalid(format!(
                    "audit chain: entry {i} has out-of-order seq {}",
                    e.seq
                )));
            }
            if e.prev_hash != prev {
                return Err(Error::invalid(format!(
                    "audit chain broken at seq {}: prev_hash mismatch",
                    e.seq
                )));
            }
            let expected = chain_hash(&e.prev_hash, &e.id, e.seq, &e.event);
            if e.hash != expected {
                return Err(Error::invalid(format!(
                    "audit chain broken at seq {}: hash mismatch (record tampered)",
                    e.seq
                )));
            }
            prev = e.hash.clone();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AuditEvent;

    fn ev(seq_ts: u64, action: &str) -> AuditEvent {
        AuditEvent::new(
            seq_ts,
            "alice",
            "acme",
            action,
            "secret",
            "secret://acme/token",
        )
    }

    #[test]
    fn records_chain_and_verify_passes() {
        let log = AuditLog::in_memory();
        let a = log.record(ev(1, "secret.create")).unwrap();
        let b = log.record(ev(2, "secret.rotate")).unwrap();
        assert_eq!(a.seq, 0);
        assert_eq!(a.prev_hash, "");
        assert_eq!(b.seq, 1);
        // The second entry links to the first.
        assert_eq!(b.prev_hash, a.hash);
        log.verify().unwrap();
    }

    #[test]
    fn tampering_breaks_the_chain() {
        let sink = std::sync::Arc::new(InMemoryAuditSink::new());
        // Build a log over a sink we can also mutate behind its back.
        let log = AuditLog::open(Box::new(CloneSink(sink.clone()))).unwrap();
        log.record(ev(1, "secret.create")).unwrap();
        log.record(ev(2, "secret.delete")).unwrap();
        log.verify().unwrap();

        // Tamper with a stored record's action.
        {
            let mut entries = sink.entries.lock().unwrap();
            entries[0].event.action = "secret.read".to_string();
        }
        let reopened = AuditLog::open(Box::new(CloneSink(sink.clone()))).unwrap();
        assert!(reopened.verify().is_err(), "tampering must be detected");
    }

    /// A sink view that shares the same underlying `InMemoryAuditSink` (for tamper tests).
    struct CloneSink(std::sync::Arc<InMemoryAuditSink>);
    impl AuditSink for CloneSink {
        fn append(&self, entry: &AuditEntry) -> Result<()> {
            self.0.append(entry)
        }
        fn all(&self) -> Result<Vec<AuditEntry>> {
            self.0.all()
        }
    }

    #[test]
    fn query_filters_and_limits() {
        let log = AuditLog::in_memory();
        log.record(ev(1, "secret.create")).unwrap();
        log.record(AuditEvent::new(
            2,
            "bob",
            "beta",
            "secret.delete",
            "secret",
            "secret://beta/k",
        ))
        .unwrap();
        log.record(ev(3, "secret.rotate")).unwrap();

        assert_eq!(log.query(&AuditFilter::default()).unwrap().len(), 3);
        assert_eq!(
            log.query(&AuditFilter {
                tenant: Some("acme".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            log.query(&AuditFilter {
                principal: Some("bob".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
        // Limit returns most-recent first.
        let recent = log
            .query(&AuditFilter {
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].event.action, "secret.rotate");
    }

    #[test]
    fn file_sink_persists_and_continues_chain() {
        let dir = std::env::temp_dir().join(format!("apex_audit_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let log = AuditLog::open(Box::new(FileAuditSink::new(&dir).unwrap())).unwrap();
            log.record(ev(1, "secret.create")).unwrap();
        }
        // Reopen: the chain continues from the persisted tip and still verifies.
        let log = AuditLog::open(Box::new(FileAuditSink::new(&dir).unwrap())).unwrap();
        let e = log.record(ev(2, "secret.rotate")).unwrap();
        assert_eq!(e.seq, 1, "seq continues across reopen");
        log.verify().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
