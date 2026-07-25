//! `apex admin backup` / `apex admin restore` — snapshot and restore the local
//! `~/.apex` state directory (RM-GA-P2 DR-1001).
//!
//! No backup/restore tooling existed anywhere before this: no CLI command, no
//! `pg_dump` hook, no `~/.apex` snapshot — and a naive `tar` of a live directory
//! wasn't even safe, since half the stores under it used to rewrite in place.
//! `atomic_write` (DUR-401) and fsync'd appends (DUR-402) now make every
//! individual file torn-write-safe, and the directories shared between the CLI
//! and server hold a cross-process advisory lock across their read-modify-write
//! cycles (DUR-403). `backup` quiesces every such directory by acquiring its
//! lock for the duration of the copy, so a snapshot taken during concurrent
//! writes still reads a self-consistent point-in-time copy rather than racing
//! an in-flight mutation.
//!
//! The core copy/verify logic (`backup_dir`/`restore_dir`) is parameterized by
//! explicit source/destination paths rather than reading `~/.apex` itself, so
//! it's unit-testable against a scratch directory without mutating the
//! process-wide `HOME`/`USERPROFILE` environment variables `config::config_dir`
//! reads — a shared global no concurrently-running test can safely rewrite.
//! `backup_cmd`/`restore_cmd` are the thin CLI-facing wrappers that resolve the
//! real `~/.apex`.

use apex_common::fs::FileLock;
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

use crate::config;
use crate::s3::{S3Config, S3Uri};

/// Written at the root of a backup directory, alongside the copied store tree.
/// Named to make an accidental collision with a real `~/.apex` entry
/// essentially impossible.
const MANIFEST_FILE: &str = "apex_backup_manifest.json";

/// DUR-403's per-directory lock file — lock-machinery state, not user data.
/// Restoring a stale one has no effect on `flock` state anyway (that lives in
/// the kernel, not the file's bytes), so it's simplest to leave it out of the
/// manifest and the copy entirely.
const LOCK_FILE: &str = ".lock";

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// The `apex-cli` crate version that produced this backup — informational
    /// only; restore doesn't reject a manifest from a different version, since
    /// no on-disk format has changed since this was introduced.
    apex_version: String,
    files: Vec<FileEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileEntry {
    /// Path relative to the backed-up directory, always `/`-separated so a
    /// backup taken on one OS restores cleanly on another.
    path: String,
    sha256: String,
    size: u64,
}

/// `apex admin backup <dest>` — snapshot `~/.apex` into `<dest>`: a local
/// directory, or an `s3://bucket/prefix` URI (GA-002 §4.1) for a remote
/// object-storage destination. The s3:// path stages the identical local
/// backup (via [`backup_dir`], unchanged — same manifest, same checksums) into
/// a scratch directory first, then uploads it, so the well-tested local logic
/// stays the single source of truth for what a backup actually contains.
pub async fn backup_cmd(dest: &str) -> Result<()> {
    let source = config::config_dir()?;
    if !source.exists() {
        return Err(Error::config(format!(
            "nothing to back up: {} does not exist",
            source.display()
        )));
    }
    let count = run_backup(&source, dest).await?;
    println!(
        "backed up {count} file(s) from {} to {dest}",
        source.display()
    );
    Ok(())
}

/// The backup core, parameterized on an explicit local `source` — unlike
/// `backup_cmd`, which always resolves this to the real `~/.apex` via
/// `config::config_dir()`. This is what lets a test exercise the real `s3://`
/// upload path (QA-403) against a scratch source directory and a live
/// MinIO/S3 endpoint without mutating the process-global `HOME`/`USERPROFILE`
/// env vars `config::config_dir` reads — the same parameterization rationale
/// `backup_dir`/`restore_dir` already use for the local-destination case.
async fn run_backup(source: &Path, dest: &str) -> Result<usize> {
    if S3Uri::is_s3(dest) {
        let uri = S3Uri::parse(dest)?;
        let staging = staging_dir("backup")?;
        let count = backup_dir(source, &staging)?;
        let upload_result = crate::s3::upload_dir(S3Config::from_env()?, &uri, &staging).await;
        let _ = fs::remove_dir_all(&staging);
        upload_result?;
        Ok(count)
    } else {
        backup_dir(source, Path::new(dest))
    }
}

/// `apex admin restore <src> --yes` — restore `~/.apex` from a backup made by
/// `apex admin backup`: a local directory, or an `s3://bucket/prefix` URI
/// matching a remote backup's destination. Overwrites the live `~/.apex` —
/// irreversible for anything written there since the backup was taken, hence
/// `--yes`. The s3:// path downloads into a scratch directory first, then
/// hands it to [`restore_dir`] unchanged, so the same checksum-before-write
/// verification applies regardless of where the backup came from.
pub async fn restore_cmd(src: &str, confirmed: bool) -> Result<()> {
    if !confirmed {
        eprintln!(
            "refusing to restore without --yes: this OVERWRITES the live ~/.apex \
             directory (secrets, memory, workflows, tenancy, kms keys, ...) with \
             the backup's contents"
        );
        return Err(Error::invalid("missing --yes confirmation"));
    }
    let dest = config::config_dir()?;
    fs::create_dir_all(&dest)?;
    let count = run_restore(src, &dest).await?;
    println!(
        "restored {count} file(s) from {src} into {}",
        dest.display()
    );
    Ok(())
}

/// The restore core, parameterized on an explicit local `dest` — the
/// restore-side mirror of [`run_backup`], for the identical QA-403
/// testability reason: `restore_cmd` always resolves `dest` to the real
/// `~/.apex`, but a test needs a scratch directory instead.
async fn run_restore(src: &str, dest: &Path) -> Result<usize> {
    if S3Uri::is_s3(src) {
        let uri = S3Uri::parse(src)?;
        let staging = staging_dir("restore")?;
        let download_result = crate::s3::download_dir(S3Config::from_env()?, &uri, &staging).await;
        let restore_result = download_result.and_then(|_| restore_dir(&staging, dest));
        let _ = fs::remove_dir_all(&staging);
        restore_result
    } else {
        restore_dir(Path::new(src), dest)
    }
}

/// A guaranteed-unique local scratch directory for staging an `s3://` backup's
/// local copy before uploading (or after downloading). Named with a pid +
/// nanosecond timestamp *and* a process-wide counter, not the timestamp alone —
/// a timestamp-only name is exactly the shape that caused a real, reproduced
/// flaky-test collision elsewhere in this workspace (`apex-tools`'
/// `tempfile_dir` test helper) when two calls landed on the same clock tick.
fn staging_dir(label: &str) -> Result<std::path::PathBuf> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "apex_admin_{label}_{}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `apex admin migrate --target <workflow|memory|marketplace> --database-url
/// <url>` — apply a Postgres-backed backend's versioned schema migrations
/// (RM-GA-P3 MIG-A1). The only place any of these three backends' schema is
/// ever created or altered: `serve`/CLI query paths only ever *read* the
/// resulting schema version and refuse to run against an unmigrated or
/// newer-than-expected one, so this is the one command that needs DDL
/// privilege on the target database.
pub async fn migrate_cmd(target: &str, database_url: &str) -> Result<()> {
    match target {
        "workflow" => run_workflow_migrations(database_url).await,
        "memory" => run_memory_migrations(database_url).await,
        "marketplace" => run_marketplace_migrations(database_url),
        other => Err(Error::invalid(format!(
            "unknown migration target `{other}` (expected one of: workflow, memory, marketplace)"
        ))),
    }
}

#[cfg(feature = "postgres")]
async fn run_workflow_migrations(database_url: &str) -> Result<()> {
    apex_workflow::PostgresStore::run_migrations(database_url).await?;
    println!("workflow schema migrated");
    Ok(())
}
#[cfg(not(feature = "postgres"))]
async fn run_workflow_migrations(_database_url: &str) -> Result<()> {
    Err(Error::config(
        "migrating the workflow schema needs a --features postgres build",
    ))
}

#[cfg(feature = "tiered-memory")]
async fn run_memory_migrations(database_url: &str) -> Result<()> {
    apex_memory::PostgresStore::run_migrations(database_url).await?;
    println!("memory schema migrated");
    Ok(())
}
#[cfg(not(feature = "tiered-memory"))]
async fn run_memory_migrations(_database_url: &str) -> Result<()> {
    Err(Error::config(
        "migrating the memory schema needs a --features tiered-memory build",
    ))
}

#[cfg(feature = "postgres")]
fn run_marketplace_migrations(database_url: &str) -> Result<()> {
    apex_marketplace::PostgresRegistryStore::run_migrations(database_url)?;
    println!("marketplace schema migrated");
    Ok(())
}
#[cfg(not(feature = "postgres"))]
fn run_marketplace_migrations(_database_url: &str) -> Result<()> {
    Err(Error::config(
        "migrating the marketplace schema needs a --features postgres build",
    ))
}

/// Snapshot every file under `source` into `dest` (created if missing) plus a
/// manifest recording each file's relative path, size, and sha256 digest.
/// Quiesces every existing immediate subdirectory of `source` (and `source`
/// itself) for the duration of the copy. Returns the number of files backed up.
fn backup_dir(source: &Path, dest: &Path) -> Result<usize> {
    fs::create_dir_all(dest)?;

    // Held until this function returns — a concurrent DUR-403-locked writer
    // blocks until the snapshot completes, rather than racing it.
    let _locks = acquire_all_locks(source)?;

    let mut files = Vec::new();
    copy_tree(source, source, dest, &mut files)?;
    let count = files.len();

    let manifest = Manifest {
        apex_version: env!("CARGO_PKG_VERSION").to_string(),
        files,
    };
    fs::write(
        dest.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(count)
}

/// Restore a backup made by `backup_dir` from `src` into `dest`. Every entry's
/// digest is verified against the manifest *before* anything is written into
/// `dest`, so a corrupt or truncated backup fails closed without touching live
/// state. Writes go through `atomic_write` so an interruption mid-restore never
/// leaves a torn file in `dest`, even though the restore as a whole isn't
/// atomic across the full tree. Returns the number of files restored.
fn restore_dir(src: &Path, dest: &Path) -> Result<usize> {
    let manifest_path = src.join(MANIFEST_FILE);
    let contents = fs::read_to_string(&manifest_path).map_err(|e| {
        Error::config(format!(
            "could not read backup manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    let manifest: Manifest = serde_json::from_str(&contents)?;

    let mut validated = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        let path = src.join(&entry.path);
        let bytes = fs::read(&path).map_err(|e| {
            Error::invalid(format!(
                "backup entry `{}` missing or unreadable: {e}",
                entry.path
            ))
        })?;
        if bytes.len() as u64 != entry.size {
            return Err(Error::invalid(format!(
                "backup entry `{}` size mismatch (manifest says {}, found {})",
                entry.path,
                entry.size,
                bytes.len()
            )));
        }
        if hex::encode(Sha256::digest(&bytes)) != entry.sha256 {
            return Err(Error::invalid(format!(
                "backup entry `{}` failed checksum verification (backup may be corrupt)",
                entry.path
            )));
        }
        validated.push((entry.path.clone(), bytes));
    }

    // Quiesce the destination too, in case a server or another CLI invocation
    // is mid read-modify-write against the directory being restored into.
    let _locks = acquire_all_locks(dest)?;

    for (rel, bytes) in &validated {
        let to = dest.join(rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        apex_common::fs::atomic_write(&to, bytes)?;
    }

    Ok(validated.len())
}

/// Acquire an advisory lock on `dir` itself (the CLI's `credentials.json` locks
/// at that level) plus every existing immediate subdirectory (each
/// `~/.apex/<store>` locks independently, DUR-403). Generic over whatever
/// subdirectories currently exist, so a future store that adopts `FileLock`
/// is automatically quiesced by backup/restore without an `admin.rs` change.
fn acquire_all_locks(dir: &Path) -> Result<Vec<FileLock>> {
    let mut locks = vec![FileLock::acquire(dir)?];
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(locks),
        Err(e) => return Err(Error::Io(e)),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            locks.push(FileLock::acquire(entry.path())?);
        }
    }
    Ok(locks)
}

/// Recursively copy every regular file under `dir` (relative to `root`) into
/// `dest`, recording a manifest entry for each. Skips [`LOCK_FILE`]s.
fn copy_tree(root: &Path, dir: &Path, dest: &Path, files: &mut Vec<FileEntry>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            copy_tree(root, &path, dest, files)?;
            continue;
        }
        if entry.file_name() == LOCK_FILE {
            continue;
        }

        let rel = relative_slash_path(root, &path);
        let bytes = fs::read(&path)?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;

        let out_path = dest.join(&rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out_path, &bytes)?;

        files.push(FileEntry {
            path: rel,
            sha256,
            size,
        });
    }
    Ok(())
}

/// `path`'s position relative to `root`, joined with `/` regardless of the
/// host platform's separator, so a manifest is portable across operating
/// systems.
fn relative_slash_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("path is under root")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("apex_cli_admin_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn backup_then_restore_round_trips_every_file() {
        let source = scratch_dir("roundtrip_source");
        let dest = scratch_dir("roundtrip_backup");
        let restored = scratch_dir("roundtrip_restored");

        fs::create_dir_all(source.join("kms")).unwrap();
        fs::write(source.join("kms/root.key"), b"deadbeef").unwrap();
        fs::create_dir_all(source.join("secrets")).unwrap();
        fs::write(source.join("secrets/secrets.json"), b"{\"a\":1}").unwrap();
        fs::create_dir_all(source.join("workflows/definitions")).unwrap();
        fs::write(source.join("workflows/agents.json"), b"{}").unwrap();
        fs::write(
            source.join("workflows/definitions/wf.yaml"),
            b"metadata:\n  name: wf\n",
        )
        .unwrap();
        fs::write(source.join("credentials.json"), b"{\"token\":\"x\"}").unwrap();

        let count = backup_dir(&source, &dest).unwrap();
        assert_eq!(count, 5);
        assert!(dest.join(MANIFEST_FILE).exists());
        // The lock files acquired during backup must not be copied into it.
        assert!(!dest.join("kms").join(LOCK_FILE).exists());

        let restored_count = restore_dir(&dest, &restored).unwrap();
        assert_eq!(restored_count, 5);

        assert_eq!(
            fs::read(restored.join("kms/root.key")).unwrap(),
            b"deadbeef"
        );
        assert_eq!(
            fs::read(restored.join("secrets/secrets.json")).unwrap(),
            b"{\"a\":1}"
        );
        assert_eq!(
            fs::read(restored.join("workflows/agents.json")).unwrap(),
            b"{}"
        );
        assert_eq!(
            fs::read(restored.join("workflows/definitions/wf.yaml")).unwrap(),
            b"metadata:\n  name: wf\n"
        );
        assert_eq!(
            fs::read(restored.join("credentials.json")).unwrap(),
            b"{\"token\":\"x\"}"
        );
        // The manifest itself is backup/restore metadata, not a restored store file.
        assert!(!restored.join(MANIFEST_FILE).exists());

        for dir in [&source, &dest, &restored] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn restore_rejects_a_manifest_with_a_tampered_file() {
        let source = scratch_dir("tamper_source");
        let dest = scratch_dir("tamper_backup");
        let restored = scratch_dir("tamper_restored");

        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("secrets.json"), b"original").unwrap();
        backup_dir(&source, &dest).unwrap();

        // Corrupt the backed-up file after the manifest recorded its digest —
        // simulating bit rot on the backup medium. Same length as the original
        // so this specifically exercises the checksum check, not the (also
        // real, but separately covered) size-mismatch check.
        fs::write(dest.join("secrets.json"), b"ORIGINAL").unwrap();

        let err = restore_dir(&dest, &restored).unwrap_err();
        assert!(
            err.to_string().contains("checksum"),
            "expected a checksum-verification error, got: {err}"
        );
        // Nothing should have been written into the restore target.
        assert!(!restored.exists() || fs::read_dir(&restored).unwrap().next().is_none());

        for dir in [&source, &dest, &restored] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn restore_rejects_a_missing_manifest() {
        let empty_src = scratch_dir("no_manifest_src");
        let restored = scratch_dir("no_manifest_restored");
        fs::create_dir_all(&empty_src).unwrap();

        let err = restore_dir(&empty_src, &restored).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "got: {err:?}");

        for dir in [&empty_src, &restored] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    /// A backup taken mid-write (simulated: a writer holds a subdirectory's
    /// lock on a background thread while `backup_dir` runs) must still block
    /// until the writer releases it, so the snapshot never observes a
    /// half-written file — DR-1001's "internally consistent, no torn files"
    /// acceptance criterion for concurrent writes.
    #[test]
    fn backup_blocks_until_a_concurrent_writer_releases_its_lock() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let source = scratch_dir("quiesce_source");
        let dest = scratch_dir("quiesce_backup");
        fs::create_dir_all(source.join("secrets")).unwrap();
        fs::write(source.join("secrets/secrets.json"), b"{}").unwrap();

        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let writer_lock = FileLock::acquire(source.join("secrets")).unwrap();

        let order2 = order.clone();
        let source2 = source.clone();
        let dest2 = dest.clone();
        let handle = std::thread::spawn(move || {
            backup_dir(&source2, &dest2).unwrap();
            order2.lock().unwrap().push("backup-completed");
        });

        std::thread::sleep(Duration::from_millis(50));
        order.lock().unwrap().push("writer-still-held");
        drop(writer_lock);
        handle.join().unwrap();

        assert_eq!(
            *order.lock().unwrap(),
            vec!["writer-still-held", "backup-completed"],
            "backup must not complete while a store's lock is held by another writer"
        );

        for dir in [&source, &dest] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    /// QA-403: the `s3://` backup→restore round trip, exercised through the real
    /// `run_backup`/`run_restore` cores (the same code `backup_cmd`/`restore_cmd`
    /// drive) against a **live** S3-compatible endpoint — closing the gap the
    /// module's own doc comment used to admit ("not validated against a live
    /// S3-compatible endpoint"). Capability-gated like every other live-backend
    /// test in this workspace (`tenancy::redis_tests`, `sandbox_backends.rs`):
    /// skip cleanly when `APEX_S3_ENDPOINT` isn't set, so this suite still passes
    /// offline; CI's `services-integration` job points it at a real MinIO
    /// service container.
    #[tokio::test]
    async fn s3_backup_restore_round_trips_against_a_live_endpoint() {
        if std::env::var("APEX_S3_ENDPOINT").is_err() {
            eprintln!("skipping: APEX_S3_ENDPOINT not set");
            return;
        }
        // `APEX_S3_TEST_BUCKET` must already exist (this client only ever does
        // PUT/GET/ListObjectsV2 against an existing bucket, matching production
        // scope — bucket creation is a one-time CI setup step, not something the
        // backup/restore path itself does).
        let bucket =
            std::env::var("APEX_S3_TEST_BUCKET").unwrap_or_else(|_| "apex-ci-backups".to_string());
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dest = format!("s3://{bucket}/qa403-{}-{nonce}", std::process::id());

        let source = scratch_dir("s3_roundtrip_source");
        let restored = scratch_dir("s3_roundtrip_restored");
        fs::create_dir_all(source.join("kms")).unwrap();
        fs::write(source.join("kms/root.key"), b"s3-roundtrip-deadbeef").unwrap();
        fs::create_dir_all(source.join("secrets")).unwrap();
        fs::write(source.join("secrets/secrets.json"), b"{\"a\":1}").unwrap();

        let uploaded = run_backup(&source, &dest)
            .await
            .expect("s3 backup must succeed against a live endpoint");
        assert_eq!(uploaded, 2);

        let downloaded = run_restore(&dest, &restored)
            .await
            .expect("s3 restore must succeed against a live endpoint");
        assert_eq!(downloaded, 2);

        assert_eq!(
            fs::read(restored.join("kms/root.key")).unwrap(),
            b"s3-roundtrip-deadbeef"
        );
        assert_eq!(
            fs::read(restored.join("secrets/secrets.json")).unwrap(),
            b"{\"a\":1}"
        );

        for dir in [&source, &restored] {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
