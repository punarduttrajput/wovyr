//! Crash-safe whole-file writes and append durability.
//!
//! Every single-document JSON store in the workspace holds its state as a
//! whole-file rewrite: read the old file, mutate in memory, write out the
//! result. A `std::fs::write` in place truncates the target *before* the new
//! bytes land, so a crash mid-write leaves a zero-length or partial file —
//! for the KMS root key or tenant-key catalog, that's unrecoverable
//! crypto-shredding of every sealed secret/memory record. `atomic_write`
//! closes that window: write to a temp file in the same directory, `fsync`
//! it, `rename` over the target (atomic on the same filesystem), then
//! `fsync` the parent directory so the rename itself survives a crash.
//!
//! [`sync_parent_dir`] is also exposed standalone for append-only logs (the
//! workflow event log, the audit chain): after appending to and `fsync`ing a
//! file, a caller that also cares about the file's *existence* surviving a
//! crash (e.g. its very first append, which creates the file) syncs the
//! containing directory the same way `atomic_write`'s rename does.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Atomically overwrite `path` with `bytes`.
///
/// On any failure before the rename, `path` is left exactly as it was — the
/// temp file may remain on disk for inspection, but the target never
/// observes a truncated or partial write.
pub fn atomic_write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    let tmp = tmp_path(path);

    let mut file = File::create(&tmp)?;
    file.write_all(bytes.as_ref())?;
    file.sync_data()?;
    drop(file);

    fs::rename(&tmp, path)?;
    sync_parent_dir(path)?;
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// `fsync` the directory containing `path` (a no-op on non-Unix, where there
/// is no directory-handle fsync equivalent — NTFS's metadata journaling makes
/// a rename/create durable without one).
pub fn sync_parent_dir(path: impl AsRef<Path>) -> io::Result<()> {
    match path.as_ref().parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => sync_dir(parent),
        None => Ok(()),
    }
}

/// `fsync` `dir` itself (a no-op on non-Unix — see [`sync_parent_dir`]).
/// Callers that create or rename an entry *within* a directory (an
/// append-only log's first write, an `atomic_write` rename) sync the
/// directory afterward so that directory-entry change survives a crash too,
/// not just the file's own `fsync`.
#[cfg(unix)]
pub fn sync_dir(dir: impl AsRef<Path>) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// `fsync` `dir` itself (a no-op on non-Unix — see [`sync_parent_dir`]).
#[cfg(not(unix))]
pub fn sync_dir(_dir: impl AsRef<Path>) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("apex_atomic_write_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_and_overwrites_atomically() {
        let dir = scratch_dir("basic");
        let path = dir.join("state.json");

        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");

        atomic_write(&path, b"second-longer-payload").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second-longer-payload");

        // No leftover temp file after a successful write.
        assert!(!tmp_path(&path).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_before_rename_leaves_the_previous_file_intact() {
        let dir = scratch_dir("crash");
        let path = dir.join("state.json");

        atomic_write(&path, b"committed").unwrap();

        // Simulate a crash between the temp-file write and the rename:
        // perform atomic_write's first phase (write the temp file) and stop
        // there, without renaming over the target.
        fs::write(tmp_path(&path), b"torn-write-in-progress").unwrap();

        // The live file must still read back the last *committed* value —
        // parseable, not truncated or torn.
        assert_eq!(fs::read(&path).unwrap(), b"committed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_a_new_file_that_did_not_exist_before() {
        let dir = scratch_dir("create");
        let path = dir.join("new.json");
        assert!(!path.exists());

        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");

        let _ = fs::remove_dir_all(&dir);
    }
}
