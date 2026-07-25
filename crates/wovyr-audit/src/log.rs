//! The append-only, hash-chained audit log
//! ([audit §4 Integrity](../../docs/13-security/audit.md#4-integrity-tamper-evidence)).
//!
//! Each [`AuditEntry`] commits to the prior entry's hash, so any deletion or modification
//! of an *interior* record breaks the chain and is detectable by [`AuditLog::verify`].
//! The chain math is deterministic (no clocks/randomness here — timestamps arrive on the
//! event), so the same sequence of events always yields the same hashes.
//!
//! # Tamper resistance vs. consistency (SEC-403)
//!
//! A bare SHA-256 chain is only *consistency* evidence: the hash is public, so any actor
//! who can rewrite `audit.jsonl` can rewrite an entry **and** recompute every downstream
//! hash, and [`verify`](AuditLog::verify) would then pass. Two changes turn the chain into
//! real tamper *resistance*:
//!
//! 1. **Keyed MAC.** When opened with a key ([`open_keyed`](AuditLog::open_keyed)) the
//!    per-entry `hash` is an **HMAC-SHA256** keyed by a secret held *outside* the log file
//!    (sourced like the KMS root key — `WOVYR_AUDIT_MAC_KEY` or an escrowed/generate-once
//!    file, via `wovyr_config::audit`). Without the key an attacker cannot recompute the
//!    chain after editing a record, so an interior edit is detectable even by an actor
//!    with full write access to the file.
//! 2. **Head anchor.** A plain chain cannot detect **tail truncation** — lop off the last
//!    N entries and the shortened chain still verifies. A keyed log therefore persists a
//!    monotonic *head anchor* (highest `seq`, its `hash`, and a keyed MAC over the pair)
//!    to a separate `audit.head` file on every append; `verify` fails closed if the log
//!    is shorter than the anchor commits to, or if the anchor's own MAC doesn't validate.
//!
//! An unkeyed log ([`open`](AuditLog::open) / [`in_memory`](AuditLog::in_memory)) keeps the
//! original plain-SHA-256 chain — it's the test/single-process path and carries no
//! tamper-resistance claim. The `hash`/`prev_hash` field *shapes* are identical either way
//! (hex strings); only their derivation differs, so switching a store from unkeyed to keyed
//! (or changing the key) intentionally invalidates a pre-existing `audit.jsonl` — a
//! breaking on-disk change, acceptable pre-real-deployment (the API-702 stance).
//!
//! [`NotarizationHook`] is the landed interface for the compliance tier's optional
//! external anchor (periodic head-hash to WORM/transparency storage); the concrete
//! publisher is a follow-on.

use crate::event::AuditEvent;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use wovyr_common::{Error, Result};

type HmacSha256 = Hmac<Sha256>;

fn lock_config_err(e: std::io::Error) -> Error {
    Error::config(format!("lock audit log: {e}"))
}

/// Lowercase-hex encode bytes (no `hex` crate dependency — same one-liner
/// `wovyr-events`' signer uses).
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
///
/// With `key = Some(_)` this is a **keyed HMAC-SHA256** (SEC-403) an actor who can
/// rewrite the log cannot recompute without the externally-held key; with `key = None`
/// it's the original plain SHA-256 (the unkeyed test/single-process path). Both produce
/// a lowercase-hex string of the same length, so the `hash`/`prev_hash` field shapes are
/// identical — only the derivation differs.
fn chain_hash(
    key: Option<&[u8; 32]>,
    prev_hash: &str,
    id: &str,
    seq: u64,
    event: &AuditEvent,
) -> String {
    // Canonical body: serde_json preserves struct field order, so this is stable.
    let body = serde_json::to_vec(&(id, seq, event)).expect("audit event serializes");
    match key {
        Some(k) => {
            let mut mac = HmacSha256::new_from_slice(k).expect("HMAC accepts any key length");
            mac.update(prev_hash.as_bytes());
            mac.update(&body);
            hex_lower(&mac.finalize().into_bytes())
        }
        None => {
            let mut h = Sha256::new();
            h.update(prev_hash.as_bytes());
            h.update(&body);
            format!("{:x}", h.finalize())
        }
    }
}

/// Filename of the durable head anchor, beside `audit.jsonl` in the log directory.
const HEAD_FILE: &str = "audit.head";

/// The persisted, keyed **head anchor** (SEC-403): the highest committed `seq`, its
/// entry `hash`, and a keyed MAC binding the two. Written on every append; read by
/// [`AuditLog::verify`] to detect **tail truncation** — a plain chain can't, since a
/// shortened chain still links cleanly.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HeadAnchor {
    seq: u64,
    hash: String,
    /// HMAC-SHA256 over the `(seq, hash)` pair — an attacker who truncates the log
    /// cannot forge a matching anchor without the key.
    mac: String,
}

/// The keyed MAC binding a head anchor's `(seq, hash)` pair (domain-separated so it
/// can't be confused with an entry's chain MAC).
fn head_mac(key: &[u8; 32], seq: u64, hash: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(b"wovyr-audit-head:v1:");
    mac.update(&seq.to_le_bytes());
    mac.update(b":");
    mac.update(hash.as_bytes());
    hex_lower(&mac.finalize().into_bytes())
}

/// Durably write the head anchor for `(seq, hash)` under `dir` (atomic whole-file
/// write, so a crash never leaves a torn anchor).
fn write_anchor(dir: &Path, key: &[u8; 32], seq: u64, hash: &str) -> Result<()> {
    let anchor = HeadAnchor {
        seq,
        hash: hash.to_string(),
        mac: head_mac(key, seq, hash),
    };
    let bytes = serde_json::to_vec(&anchor).map_err(Error::from)?;
    // The anchor is integrity metadata (a seq + hash + keyed MAC), not secret key
    // material — publishing it reveals nothing about the key — so it needs no
    // owner-only lockdown (and skipping it avoids an `icacls` spawn per append on
    // Windows). The key itself lives elsewhere, `0600`, via `wovyr_config::audit`.
    wovyr_common::fs::atomic_write(dir.join(HEAD_FILE), bytes)
        .map_err(|e| Error::config(format!("write audit head anchor: {e}")))
}

/// Read the head anchor under `dir`, or `None` if none has been written yet.
fn read_anchor(dir: &Path) -> Result<Option<HeadAnchor>> {
    match std::fs::read(dir.join(HEAD_FILE)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(Error::from)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::config(format!("read audit head anchor: {e}"))),
    }
}

/// The public projection of a head anchor handed to a [`NotarizationHook`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotarizedHead {
    /// Highest committed sequence number.
    pub seq: u64,
    /// The head entry's chain hash (keyed MAC for a keyed log).
    pub hash: String,
    /// The keyed MAC over `(seq, hash)` — the same value persisted in `audit.head`.
    pub mac: String,
}

/// An optional hook to publish the log's head anchor to durable **external** storage —
/// a WORM bucket, a transparency log, a notary — for the compliance tier (SEC-403).
///
/// This is the *interface*; a concrete publisher is a follow-on. Attach one via
/// [`AuditLog::with_notarization`]; it's invoked after each successful append with the
/// new head. It is **advisory external durability**, not the primary integrity
/// mechanism (that's the keyed chain + local anchor), so a hook error is logged and
/// does **not** fail the audited action — losing an already-recorded action because an
/// external notary is unreachable would be strictly worse than a delayed external anchor.
pub trait NotarizationHook: Send + Sync {
    /// Publish `head` to external storage. Best-effort: an error is logged, not
    /// propagated to the caller recording the event.
    fn notarize(&self, head: &NotarizedHead) -> Result<()>;
}

/// One page of a [`AuditSink::query_page`] read: entries most-recent first, plus a
/// cursor to pass back as `before_seq` for the next page (`None` once exhausted).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditPage {
    /// Matching entries, most-recent first.
    pub entries: Vec<AuditEntry>,
    /// Pass as `before_seq` to continue; `None` when there's nothing more.
    pub next_cursor: Option<u64>,
}

/// Durable storage for audit entries (append-only).
pub trait AuditSink: Send + Sync {
    /// Append an entry (never mutate or delete existing ones).
    fn append(&self, entry: &AuditEntry) -> Result<()>;
    /// All entries in insertion order.
    fn all(&self) -> Result<Vec<AuditEntry>>;

    /// Read one page of entries matching `filter`, most-recent first, continuing from
    /// `before_seq` (exclusive) up to `limit` entries (SEC-301).
    ///
    /// The default implementation reads the whole log via [`all`](Self::all) and
    /// filters/pages in memory — correct for any sink, but exactly the "scan the
    /// whole log per query" cost SEC-301 is about; [`InMemoryAuditSink`] keeps this
    /// default since scanning an in-memory `Vec` isn't the I/O concern this ticket
    /// targets, while [`FileAuditSink`] overrides it with a real bounded read.
    fn query_page(
        &self,
        filter: &AuditFilter,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<AuditPage> {
        let mut entries = self.all()?;
        entries.retain(|e| filter.matches(e) && before_seq.is_none_or(|b| e.seq < b));
        entries.sort_by_key(|e| std::cmp::Reverse(e.seq));
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor = if has_more {
            entries.last().map(|e| e.seq)
        } else {
            None
        };
        Ok(AuditPage {
            entries,
            next_cursor,
        })
    }
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
        wovyr_common::fs::sync_parent_dir(&self.path)
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

    /// Reads `audit.jsonl` backward from the end in bounded chunks, stopping as soon as
    /// `limit` matching entries are found — for the common case (a recent, unfiltered or
    /// lightly-filtered page) this reads only the tail of the file rather than the whole
    /// log every `all()`-based query pays for (SEC-301).
    fn query_page(
        &self,
        filter: &AuditFilter,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<AuditPage> {
        Ok(scan_reverse(&self.path, filter, before_seq, limit, READ_CHUNK_BYTES)?.page)
    }
}

/// Production chunk size for [`scan_reverse`]'s backward read.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// [`scan_reverse`]'s result plus the byte count it actually read from disk — the
/// latter is test-only signal (proving a page read didn't scan the whole file), not
/// part of the public [`AuditSink`] contract.
struct ReverseScan {
    page: AuditPage,
    /// Only the test suite reads this (the production caller wants just the page).
    #[cfg_attr(not(test), allow(dead_code))]
    bytes_read: u64,
}

/// The backward, bounded-chunk scan behind [`FileAuditSink::query_page`]. Since
/// entries are appended in ascending `seq` order, reading the file from its end
/// yields entries in descending `seq` (most-recent-first) order for free — no
/// separate index or sort is needed, unlike [`AuditSink::query_page`]'s default
/// (read-everything-then-sort) implementation.
///
/// `chunk_bytes` is a parameter (not a bare constant) so tests can force many small
/// chunk reads — and so exercise the cross-chunk line-splitting logic — without
/// needing a multi-megabyte fixture file.
fn scan_reverse(
    path: &std::path::Path,
    filter: &AuditFilter,
    before_seq: Option<u64>,
    limit: usize,
    chunk_bytes: usize,
) -> Result<ReverseScan> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReverseScan {
                page: AuditPage::default(),
                bytes_read: 0,
            });
        }
        Err(e) => return Err(Error::config(format!("open audit.jsonl: {e}"))),
    };
    let mut pos = file
        .metadata()
        .map_err(|e| Error::config(format!("stat audit.jsonl: {e}")))?
        .len();

    let mut collected: Vec<AuditEntry> = Vec::new();
    // The head of the previously-read (higher-offset) chunk, up to its first '\n':
    // those bytes continue a line that *starts* somewhere below `chunk_start`, so
    // they're appended after the next (lower-offset) chunk's bytes to complete it.
    let mut carry: Vec<u8> = Vec::new();
    let mut bytes_read: u64 = 0;

    while pos > 0 {
        let read_size = chunk_bytes.min(pos as usize);
        let chunk_start = pos - read_size as u64;
        let mut buf = vec![0u8; read_size];
        file.seek(SeekFrom::Start(chunk_start))
            .map_err(|e| Error::config(format!("seek audit.jsonl: {e}")))?;
        file.read_exact(&mut buf)
            .map_err(|e| Error::config(format!("read audit.jsonl: {e}")))?;
        bytes_read += read_size as u64;
        buf.extend_from_slice(&carry);
        pos = chunk_start;

        // Everything before this buffer's first '\n' continues a line that starts
        // in the not-yet-read file below — defer it to the next iteration, unless
        // we've reached byte 0, where it's a complete line of its own.
        let lines_from = if pos == 0 {
            carry = Vec::new();
            0
        } else if let Some(i) = buf.iter().position(|&b| b == b'\n') {
            carry = buf[..i].to_vec();
            i + 1
        } else {
            // No line boundary anywhere in this chunk: the whole buffer is still
            // a fragment of one line — keep accumulating.
            carry = buf;
            continue;
        };

        let mut lines: Vec<&[u8]> = Vec::new();
        let mut line_start = lines_from;
        for i in lines_from..buf.len() {
            if buf[i] == b'\n' {
                lines.push(&buf[line_start..i]);
                line_start = i + 1;
            }
        }
        // The bytes after the last '\n' are a complete line too: their terminating
        // '\n' was consumed as the carry boundary of the previous (higher-offset)
        // iteration. On the very first iteration this tail is whatever follows the
        // file's final '\n' — empty for a well-formed log, and a parse error
        // (matching `all()`'s behavior) for a torn one.
        if line_start < buf.len() {
            lines.push(&buf[line_start..]);
        }

        for line in lines.iter().rev() {
            if line.trim_ascii().is_empty() {
                continue;
            }
            let entry: AuditEntry = serde_json::from_slice(line).map_err(Error::from)?;
            if before_seq.is_some_and(|b| entry.seq >= b) {
                continue;
            }
            if !filter.matches(&entry) {
                continue;
            }
            collected.push(entry);
            if collected.len() > limit {
                collected.truncate(limit);
                let next_cursor = collected.last().map(|e| e.seq);
                return Ok(ReverseScan {
                    page: AuditPage {
                        entries: collected,
                        next_cursor,
                    },
                    bytes_read,
                });
            }
        }
    }

    Ok(ReverseScan {
        page: AuditPage {
            entries: collected,
            next_cursor: None,
        },
        bytes_read,
    })
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
    /// Restrict to entries at or after this timestamp (epoch ms, inclusive).
    pub after_ms: Option<u64>,
    /// Restrict to entries at or before this timestamp (epoch ms, inclusive).
    pub before_ms: Option<u64>,
}

impl AuditFilter {
    /// Whether `entry` satisfies every set predicate (tenant/principal/action/time-range).
    /// Shared by [`AuditLog::query`] and the paged [`AuditSink::query_page`] path so the
    /// two can never drift on what "matches" means.
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        self.tenant
            .as_ref()
            .is_none_or(|t| &entry.event.actor.tenant == t)
            && self
                .principal
                .as_ref()
                .is_none_or(|p| &entry.event.actor.principal == p)
            && self
                .action
                .as_ref()
                .is_none_or(|a| &entry.event.action == a)
            && self
                .after_ms
                .is_none_or(|from| entry.event.timestamp_ms >= from)
            && self
                .before_ms
                .is_none_or(|to| entry.event.timestamp_ms <= to)
    }
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
    /// When set, entries are chained with a keyed HMAC and a durable head anchor is
    /// maintained under [`anchor_dir`](Self::anchor_dir) (SEC-403). `None` keeps the
    /// original unkeyed SHA-256 chain (test / single-process path).
    key: Option<[u8; 32]>,
    /// Directory holding `audit.head` (typically the same directory as the sink and
    /// the cross-process lock). Only meaningful alongside `key`.
    anchor_dir: Option<PathBuf>,
    /// Optional external-anchor publisher (compliance tier); best-effort.
    notarizer: Option<Arc<dyn NotarizationHook>>,
}

impl AuditLog {
    /// Open a log over `sink` (no cross-process locking — single-process use, or
    /// a purely in-memory sink where there's nothing to protect against another
    /// process). Fails if `sink.all()` itself is broken, so a fundamentally
    /// unreadable store surfaces immediately rather than on first `record()`.
    ///
    /// **Unkeyed** — plain SHA-256 chain, no tamper-resistance claim. Production
    /// deployments use [`open_keyed`](Self::open_keyed).
    pub fn open(sink: Box<dyn AuditSink>) -> Result<Self> {
        sink.all()?;
        Ok(Self {
            sink,
            guard: Mutex::new(()),
            lock_dir: None,
            key: None,
            anchor_dir: None,
            notarizer: None,
        })
    }

    /// Open a log over `sink` with cross-process append safety: each `record()`
    /// holds an advisory lock on `lock_dir` (RM-GA-P2 DUR-403) spanning the
    /// re-derive-tip-then-append sequence, so a second process sharing the same
    /// directory (e.g. the CLI and server both writing `~/.wovyr/audit`) extends
    /// one chain instead of forking it.
    ///
    /// **Unkeyed** — see [`open_keyed`](Self::open_keyed) for the tamper-resistant path.
    pub fn open_with_lock(sink: Box<dyn AuditSink>, lock_dir: impl Into<PathBuf>) -> Result<Self> {
        let mut log = Self::open(sink)?;
        log.lock_dir = Some(lock_dir.into());
        Ok(log)
    }

    /// Open a **keyed, tamper-resistant** log (SEC-403). Entries are chained with an
    /// HMAC-SHA256 under `key` (a secret held outside the log file — sourced like the
    /// KMS root key via `wovyr_config::audit`), and a durable head anchor is maintained
    /// in `dir` so [`verify`](Self::verify) detects tail truncation as well as interior
    /// edits. `dir` also serves as the cross-process lock/anchor directory — pass the
    /// same directory the [`FileAuditSink`] writes `audit.jsonl` into.
    ///
    /// The key must be held durably and *outside* this process's control by an attacker:
    /// the strong path is `WOVYR_AUDIT_MAC_KEY` sourced from escrow. A generate-once
    /// key file beside the log (the dev/local convenience) is only as strong as the
    /// filesystem permissions on that directory — an actor who can also read the key
    /// can forge the chain, exactly as for the KMS root key file.
    pub fn open_keyed(
        sink: Box<dyn AuditSink>,
        key: [u8; 32],
        dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let dir = dir.into();
        let mut log = Self::open(sink)?;
        log.lock_dir = Some(dir.clone());
        log.anchor_dir = Some(dir);
        log.key = Some(key);
        Ok(log)
    }

    /// Attach an optional external-anchor publisher (compliance tier, SEC-403). Called
    /// best-effort after each append; see [`NotarizationHook`].
    pub fn with_notarization(mut self, hook: Arc<dyn NotarizationHook>) -> Self {
        self.notarizer = Some(hook);
        self
    }

    /// A log over a fresh [`InMemoryAuditSink`] (unkeyed).
    pub fn in_memory() -> Self {
        Self::open(Box::new(InMemoryAuditSink::new())).expect("empty in-memory log")
    }

    /// Record `event`, appending a hash-chained entry. Returns the persisted entry.
    ///
    /// For a keyed log this also updates the durable head anchor and (best-effort)
    /// notifies any [`NotarizationHook`], all within the same cross-process lock so a
    /// concurrent writer can never interleave the entry and the anchor.
    pub fn record(&self, event: AuditEvent) -> Result<AuditEntry> {
        let _guard = self.guard.lock().expect("audit log poisoned");
        let _flock = match &self.lock_dir {
            Some(dir) => Some(wovyr_common::fs::FileLock::acquire(dir).map_err(lock_config_err)?),
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
        let hash = chain_hash(self.key.as_ref(), &prev_hash, &id, seq, &event);
        let entry = AuditEntry {
            id,
            seq,
            event,
            prev_hash,
            hash,
        };
        // Append the entry first, then advance the anchor — so a crash between the two
        // leaves the log *ahead* of the anchor (harmless: extra entries are still
        // chain-verified), never behind it (which would look like truncation).
        self.sink.append(&entry)?;
        if let (Some(key), Some(dir)) = (self.key.as_ref(), self.anchor_dir.as_ref()) {
            write_anchor(dir, key, entry.seq, &entry.hash)?;
            if let Some(hook) = &self.notarizer {
                let head = NotarizedHead {
                    seq: entry.seq,
                    hash: entry.hash.clone(),
                    mac: head_mac(key, entry.seq, &entry.hash),
                };
                if let Err(e) = hook.notarize(&head) {
                    tracing::warn!(error = %e, seq = entry.seq, "audit notarization hook failed (advisory)");
                }
            }
        }
        Ok(entry)
    }

    /// Read entries matching `filter` (most-recent first when a limit is set).
    ///
    /// This always reads the entire log (`sink.all()`) and filters in memory — fine for
    /// small logs or a one-off query, but exactly the cost
    /// [`query_page`](Self::query_page) exists to avoid for a paged, possibly
    /// time-ranged read of a large log (SEC-301).
    pub fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>> {
        let mut entries = self.sink.all()?;
        entries.retain(|e| filter.matches(e));
        if let Some(limit) = filter.limit {
            entries.reverse();
            entries.truncate(limit);
        }
        Ok(entries)
    }

    /// Read one page of entries matching `filter`, most-recent first, continuing from
    /// `before_seq` (exclusive — omit for the first page) up to `limit` entries
    /// (SEC-301). Delegates to the sink, which may serve this from a bounded scan
    /// instead of the full-log read `query` always pays for — see
    /// [`FileAuditSink`]'s override.
    pub fn query_page(
        &self,
        filter: &AuditFilter,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<AuditPage> {
        self.sink.query_page(filter, before_seq, limit)
    }

    /// Verify integrity: every entry's hash recomputes and links to its predecessor,
    /// and — for a keyed log (SEC-403) — the durable head anchor confirms the log has
    /// not been **truncated**. Returns an error naming the first break, or `Ok(())`.
    ///
    /// For a keyed log the per-entry recompute is an HMAC an actor without the key
    /// cannot forge, so an interior edit is caught even against a full-write-access
    /// attacker; the head anchor catches tail truncation the chain alone cannot.
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
            let expected = chain_hash(self.key.as_ref(), &e.prev_hash, &e.id, e.seq, &e.event);
            if e.hash != expected {
                return Err(Error::invalid(format!(
                    "audit chain broken at seq {}: hash mismatch (record tampered)",
                    e.seq
                )));
            }
            prev = e.hash.clone();
        }

        // Head-anchor (truncation) check — keyed logs only. A plain chain can't detect
        // a lopped-off tail; the anchor commits to the highest seq ever appended.
        if let (Some(key), Some(dir)) = (self.key.as_ref(), self.anchor_dir.as_ref()) {
            match read_anchor(dir)? {
                Some(anchor) => {
                    if head_mac(key, anchor.seq, &anchor.hash) != anchor.mac {
                        return Err(Error::invalid("audit head anchor tampered: MAC mismatch"));
                    }
                    match entries.last() {
                        Some(last) if last.seq < anchor.seq => {
                            return Err(Error::invalid(format!(
                                "audit log truncated: head anchor commits to seq {} but the log ends at seq {}",
                                anchor.seq, last.seq
                            )));
                        }
                        Some(_) => {
                            // The anchored entry must still be present and unchanged.
                            // (The chain already validates it; this also catches a
                            // forged anchor pointing at a mutated head.)
                            match entries.get(anchor.seq as usize) {
                                Some(anchored) if anchored.hash == anchor.hash => {}
                                _ => {
                                    return Err(Error::invalid(format!(
                                        "audit head anchor mismatch at seq {}: log diverged from the anchored head",
                                        anchor.seq
                                    )));
                                }
                            }
                        }
                        None => {
                            return Err(Error::invalid(format!(
                                "audit log empty but head anchor commits to seq {}",
                                anchor.seq
                            )));
                        }
                    }
                }
                None => {
                    // A keyed log writes an anchor on every append, so a non-empty log
                    // with no anchor means the anchor was removed — treat as tampering.
                    if !entries.is_empty() {
                        return Err(Error::invalid(
                            "audit head anchor missing for a non-empty keyed log (removed?)",
                        ));
                    }
                }
            }
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

    /// A scratch dir unique per test (several tests in this module use the
    /// filesystem within one process, so the pid alone isn't enough).
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wovyr_audit_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A file-backed log under `dir` holding `n` entries with timestamps `1..=n`.
    fn file_log_with(dir: &std::path::Path, n: u64) -> AuditLog {
        let log = AuditLog::open(Box::new(FileAuditSink::new(dir).unwrap())).unwrap();
        for ts in 1..=n {
            log.record(ev(ts, "secret.rotate")).unwrap();
        }
        log
    }

    /// The SEC-301 acceptance criterion: a time-ranged, paged query must not scan
    /// the whole log. `scan_reverse` reports the bytes it actually read, so this
    /// asserts the bounded backward scan stopped after a small tail of the file.
    #[test]
    fn time_ranged_paged_query_reads_only_the_tail_of_the_log() {
        let dir = scratch_dir("tail_read");
        file_log_with(&dir, 400);
        let path = dir.join("audit.jsonl");
        let file_len = std::fs::metadata(&path).unwrap().len();

        let filter = AuditFilter {
            after_ms: Some(380),
            ..Default::default()
        };
        let scan = scan_reverse(&path, &filter, None, 5, 1024).unwrap();

        assert_eq!(scan.page.entries.len(), 5);
        // Most-recent first, and every entry inside the requested window.
        assert_eq!(scan.page.entries[0].event.timestamp_ms, 400);
        assert!(
            scan.page
                .entries
                .iter()
                .all(|e| e.event.timestamp_ms >= 380)
        );
        assert!(
            scan.page.next_cursor.is_some(),
            "window holds more than a page"
        );
        assert!(
            scan.bytes_read < file_len / 4,
            "paged read scanned {} of {} bytes — not a bounded tail read",
            scan.bytes_read,
            file_len
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A chunk size far smaller than one JSON line forces every entry to span
    /// multiple chunks, exercising the carry/reassembly logic: the bytes before a
    /// chunk's first newline belong to a line that starts in an earlier chunk and
    /// must be deferred, never parsed as a line of their own.
    #[test]
    fn reverse_scan_reassembles_lines_split_across_chunks() {
        let dir = scratch_dir("cross_chunk");
        file_log_with(&dir, 10);
        let path = dir.join("audit.jsonl");

        let scan = scan_reverse(&path, &AuditFilter::default(), None, 100, 7).unwrap();
        let seqs: Vec<u64> = scan.page.entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (0..10).rev().collect::<Vec<_>>());
        assert!(scan.page.next_cursor.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Walking the cursor to exhaustion yields every entry exactly once,
    /// most-recent first, with `next_cursor` gone on the final page.
    #[test]
    fn query_page_cursor_walks_the_log_without_gaps_or_overlap() {
        let dir = scratch_dir("cursor_walk");
        let log = file_log_with(&dir, 10);

        let mut cursor = None;
        let mut seqs = Vec::new();
        let mut pages = 0;
        loop {
            let page = log.query_page(&AuditFilter::default(), cursor, 3).unwrap();
            seqs.extend(page.entries.iter().map(|e| e.seq));
            pages += 1;
            assert!(pages <= 10, "cursor failed to make progress");
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seqs, (0..10).rev().collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The trait's default (read-everything) implementation and `FileAuditSink`'s
    /// bounded backward scan must never drift on page contents or cursors —
    /// walk both page by page over identical logs, filtered, and compare.
    #[test]
    fn default_query_page_and_file_override_agree() {
        let dir = scratch_dir("impl_parity");
        let file_log = AuditLog::open(Box::new(FileAuditSink::new(&dir).unwrap())).unwrap();
        let mem_log = AuditLog::in_memory();
        for ts in 1..=9 {
            // Alternate principals so the filter drops some entries.
            let who = if ts % 3 == 0 { "bob" } else { "alice" };
            let e = AuditEvent::new(ts, who, "acme", "secret.rotate", "secret", "secret://k");
            file_log.record(e.clone()).unwrap();
            mem_log.record(e).unwrap();
        }

        let filter = AuditFilter {
            principal: Some("alice".into()),
            ..Default::default()
        };
        let mut cursor = None;
        loop {
            let from_file = file_log.query_page(&filter, cursor, 2).unwrap();
            let from_mem = mem_log.query_page(&filter, cursor, 2).unwrap();
            assert_eq!(from_file, from_mem);
            match from_file.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `after_ms`/`before_ms` are inclusive on both ends.
    #[test]
    fn time_range_bounds_are_inclusive() {
        let log = AuditLog::in_memory();
        for ts in 1..=3 {
            log.record(ev(ts, "secret.rotate")).unwrap();
        }
        let hits = log
            .query(&AuditFilter {
                after_ms: Some(2),
                before_ms: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].event.timestamp_ms, 2);
    }

    #[test]
    fn file_sink_persists_and_continues_chain() {
        let dir = std::env::temp_dir().join(format!("wovyr_audit_test_{}", std::process::id()));
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

    // ---- SEC-403: keyed MAC + tamper-evident head anchor ----

    /// A test key with no special structure — SEC-403 keys the chain, it doesn't
    /// interpret the bytes.
    fn test_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        k
    }

    /// A keyed, file-backed log under `dir` holding entries with timestamps `1..=n`.
    fn keyed_file_log(dir: &std::path::Path, key: [u8; 32], n: u64) -> AuditLog {
        let log =
            AuditLog::open_keyed(Box::new(FileAuditSink::new(dir).unwrap()), key, dir).unwrap();
        for ts in 1..=n {
            log.record(ev(ts, "secret.rotate")).unwrap();
        }
        log
    }

    /// The keyed chain uses an HMAC, so the same event yields a *different* hash than
    /// the unkeyed SHA-256 path — proving the key is actually applied to the chain.
    #[test]
    fn keyed_chain_hash_differs_from_unkeyed() {
        let dir = scratch_dir("keyed_diff");
        let keyed = AuditLog::open_keyed(
            Box::new(FileAuditSink::new(&dir).unwrap()),
            test_key(),
            &dir,
        )
        .unwrap();
        let k = keyed.record(ev(1, "secret.create")).unwrap();

        let unkeyed = AuditLog::in_memory();
        let u = unkeyed.record(ev(1, "secret.create")).unwrap();

        // Same seq (0), same prev_hash (""), same event — only the keying differs.
        assert_eq!(k.seq, u.seq);
        assert_ne!(
            k.hash, u.hash,
            "a keyed HMAC chain must not equal the unkeyed SHA-256 chain"
        );
        keyed.verify().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Acceptance (a): a keyed log detects an interior record rewrite. An attacker
    /// with full write access edits an entry's event but cannot recompute the keyed
    /// MAC, so `verify()` catches the mismatch.
    #[test]
    fn keyed_log_detects_an_interior_edit() {
        let dir = scratch_dir("keyed_interior");
        keyed_file_log(&dir, test_key(), 4);
        let path = dir.join("audit.jsonl");

        // Rewrite entry 0's action directly in the file (leaving its stored hash).
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let mut v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        v["event"]["action"] = serde_json::json!("secret.read");
        lines[0] = serde_json::to_string(&v).unwrap();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let reopened = AuditLog::open_keyed(
            Box::new(FileAuditSink::new(&dir).unwrap()),
            test_key(),
            &dir,
        )
        .unwrap();
        assert!(
            reopened.verify().is_err(),
            "an interior edit must be detected on a keyed log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Acceptance (b): a keyed log detects **tail truncation** — the head anchor
    /// commits to the highest seq ever appended, so lopping off the last entries
    /// (which a plain chain would still accept) fails closed.
    #[test]
    fn keyed_log_detects_tail_truncation() {
        let dir = scratch_dir("keyed_truncate");
        keyed_file_log(&dir, test_key(), 5); // seq 0..=4; anchor commits to seq 4
        let path = dir.join("audit.jsonl");

        // Truncate to the first 3 entries (seq 0..=2); the anchor is left untouched.
        let text = std::fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = text.lines().take(3).collect();
        std::fs::write(&path, kept.join("\n") + "\n").unwrap();

        // The shortened chain links cleanly on its own...
        let unkeyed = AuditLog::open(Box::new(FileAuditSink::new(&dir).unwrap())).unwrap();
        // (unkeyed verify would recompute wrong hashes here since the log was written
        // keyed, so we don't rely on it — the point is the *anchor* catches truncation)
        let _ = unkeyed;

        let reopened = AuditLog::open_keyed(
            Box::new(FileAuditSink::new(&dir).unwrap()),
            test_key(),
            &dir,
        )
        .unwrap();
        let err = reopened.verify().unwrap_err();
        assert!(
            format!("{err}").contains("truncated"),
            "tail truncation must be detected via the head anchor: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The head anchor is itself keyed: editing it (e.g. lowering its committed seq to
    /// hide a truncation) fails the anchor MAC check.
    #[test]
    fn keyed_head_anchor_tamper_is_detected() {
        let dir = scratch_dir("keyed_anchor_tamper");
        let log = keyed_file_log(&dir, test_key(), 3);
        log.verify().unwrap();

        let head_path = dir.join("audit.head");
        let mut a: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&head_path).unwrap()).unwrap();
        a["seq"] = serde_json::json!(0); // forge a lower head without a valid MAC
        std::fs::write(&head_path, serde_json::to_vec(&a).unwrap()).unwrap();

        let reopened = AuditLog::open_keyed(
            Box::new(FileAuditSink::new(&dir).unwrap()),
            test_key(),
            &dir,
        )
        .unwrap();
        let err = reopened.verify().unwrap_err();
        assert!(
            format!("{err}").contains("anchor"),
            "a forged head anchor must fail the MAC check: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verifying a keyed log under the **wrong key** fails — the recomputed HMACs
    /// don't match the stored ones (the "without the MAC key, the log fails" posture).
    #[test]
    fn keyed_log_fails_verify_under_the_wrong_key() {
        let dir = scratch_dir("keyed_wrong_key");
        keyed_file_log(&dir, test_key(), 3);

        let mut wrong = test_key();
        wrong[0] ^= 0xFF;
        let reopened =
            AuditLog::open_keyed(Box::new(FileAuditSink::new(&dir).unwrap()), wrong, &dir).unwrap();
        assert!(
            reopened.verify().is_err(),
            "a keyed log must not verify under a different key"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A keyed log survives a reopen (fresh process): the anchor + chain persist and
    /// the chain continues from the persisted tip.
    #[test]
    fn keyed_log_persists_and_continues_across_reopen() {
        let dir = scratch_dir("keyed_reopen");
        {
            let log = keyed_file_log(&dir, test_key(), 2);
            log.verify().unwrap();
        }
        let log = AuditLog::open_keyed(
            Box::new(FileAuditSink::new(&dir).unwrap()),
            test_key(),
            &dir,
        )
        .unwrap();
        let e = log.record(ev(3, "secret.rotate")).unwrap();
        assert_eq!(e.seq, 2, "seq continues across reopen");
        log.verify().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The notarization hook receives each new head (the landed compliance-tier
    /// interface, SEC-403); a hook error is advisory and does not fail the record.
    #[test]
    fn notarization_hook_receives_each_head() {
        use std::sync::Mutex as StdMutex;
        #[derive(Default)]
        struct RecordingHook {
            heads: StdMutex<Vec<NotarizedHead>>,
        }
        impl NotarizationHook for RecordingHook {
            fn notarize(&self, head: &NotarizedHead) -> Result<()> {
                self.heads.lock().unwrap().push(head.clone());
                Err(Error::config("simulated external notary outage"))
            }
        }

        let dir = scratch_dir("keyed_notarize");
        let hook = Arc::new(RecordingHook::default());
        let log = AuditLog::open_keyed(
            Box::new(FileAuditSink::new(&dir).unwrap()),
            test_key(),
            &dir,
        )
        .unwrap()
        .with_notarization(hook.clone());

        // The hook errors, but the record still succeeds (advisory external durability).
        log.record(ev(1, "secret.create")).unwrap();
        log.record(ev(2, "secret.rotate")).unwrap();

        let heads = hook.heads.lock().unwrap();
        assert_eq!(heads.len(), 2, "the hook sees every head");
        assert_eq!(heads[0].seq, 0);
        assert_eq!(heads[1].seq, 1);
        log.verify().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
