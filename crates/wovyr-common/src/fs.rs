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
//!
//! [`FileLock`] closes a gap `atomic_write` alone doesn't: two callers racing
//! `atomic_write` on the *same* target share one fixed temp-file name, so
//! without external synchronization their writes can interleave into that
//! shared temp file before either renames — and, one level up, a
//! read-modify-write cycle (load the whole document, mutate a field, write it
//! back) can silently lose one side's update even when each individual write
//! is torn-write-safe. The CLI and server share every `~/.wovyr` store
//! directory by design, so this is a real cross-*process* hazard, not just a
//! hypothetical one (RM-GA-P2 DUR-403).

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

/// Restrict `path` to the owning user's exclusive access — the shared
/// primitive behind every owner-only file in the workspace (`wovyr-kms`'s
/// `root.key` and `kms.json`, the CLI's `credentials.json`), previously three
/// independent copies of the same `#[cfg(unix)]`/`#[cfg(not(unix))]` pair.
/// Unix: `chmod 0600`. Windows: no permission bits exist to `chmod` — access
/// control is a full ACL, and std has no API for editing one. Rather than add
/// a Windows-ACL dependency for a single call, this shells out to `icacls`
/// (bundled with every Windows install since XP), the same
/// external-tool-via-`Command` pattern `wovyr-tools`' egress lockdown already
/// uses for `iptables`/`nsenter`: `/inheritance:r` strips inherited ACEs, then
/// `/grant:r <user>:F` grants Full Control to the invoking user only,
/// replacing (`:r`) any prior explicit grant rather than stacking onto it.
pub fn restrict_to_owner(path: impl AsRef<Path>) -> io::Result<()> {
    restrict_to_owner_impl(path.as_ref())
}

#[cfg(unix)]
fn restrict_to_owner_impl(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict_to_owner_impl(path: &Path) -> io::Result<()> {
    // Windows always sets USERNAME for the process's own session; there is no
    // sandboxed/service context in this workspace's deployment story where it
    // would be absent.
    let user = std::env::var("USERNAME")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "USERNAME is not set"))?;
    // `.output()`, not `.status()`: icacls prints a "Successfully processed N
    // files" line to stdout on every call, which would otherwise spam the
    // server/CLI's own console on every KMS write. Captured and only surfaced
    // on failure.
    let output = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:F"))
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "icacls exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

#[cfg(not(any(unix, windows)))]
fn restrict_to_owner_impl(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// A held cross-process advisory exclusive lock on `<dir>/.lock` — `flock` on
/// Unix, `LockFileEx` on Windows, via the `fs2` crate. Acquiring **blocks**
/// the calling thread until the lock is held (a store's read-modify-write
/// cycle should just wait its turn, not error out), and releases on `Drop`
/// (or immediately if the process dies, so a crash can never leave a store
/// permanently wedged).
///
/// Each open file description gets its own lock state, so this also
/// correctly serializes concurrent callers *within* the same process (two
/// threads each calling `acquire` on the same directory), not only across
/// processes — there is no need to additionally guard it with an in-process
/// `Mutex` for correctness, though callers may still keep one for other
/// reasons (e.g. avoiding a syscall on an already-known-uncontended path).
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Block until an exclusive lock on `<dir>/.lock` is held, creating `dir`
    /// (and the lock file) if needed.
    pub fn acquire(dir: impl AsRef<Path>) -> io::Result<FileLock> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))?;
        // Fully qualified rather than `use fs2::FileExt` + method-call syntax:
        // this crate's MSRV (1.85) predates std's own `File::lock`/`unlock`
        // (stabilized in 1.89), but on a newer toolchain the inherent std
        // method of the same name would silently win method resolution and
        // make the `use` look unused — naming the trait explicitly always
        // picks fs2's impl, on any supported toolchain.
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(FileLock { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Best-effort — closing the handle (right after this returns) releases
        // the OS-level lock regardless of whether unlock() itself succeeds.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wovyr_atomic_write_{name}_{}", std::process::id()));
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

    #[test]
    fn a_second_acquire_blocks_until_the_first_is_dropped() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let dir = scratch_dir("filelock");
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let lock1 = FileLock::acquire(&dir).unwrap();

        let dir2 = dir.clone();
        let order2 = order.clone();
        let handle = std::thread::spawn(move || {
            // Blocks here until `lock1` is dropped below.
            let _lock2 = FileLock::acquire(&dir2).unwrap();
            order2.lock().unwrap().push("second-acquired");
        });

        std::thread::sleep(Duration::from_millis(50));
        order.lock().unwrap().push("first-still-held");
        drop(lock1);
        handle.join().unwrap();

        assert_eq!(
            *order.lock().unwrap(),
            vec!["first-still-held", "second-acquired"],
            "the second lock must not be acquired while the first is still held"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dropped_lock_can_be_reacquired_immediately() {
        let dir = scratch_dir("filelock_reacquire");
        {
            let _lock = FileLock::acquire(&dir).unwrap();
        }
        // No hang, no error — the prior lock was released on drop.
        let _lock = FileLock::acquire(&dir).unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restrict_to_owner_locks_the_file_down() {
        let dir = scratch_dir("restrict_to_owner");
        let path = dir.join("secret.key");
        fs::write(&path, b"secret").unwrap();

        restrict_to_owner(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        #[cfg(windows)]
        {
            // icacls prints one line per ACE; after `/inheritance:r` +
            // `/grant:r <user>:F` there must be one naming the invoking user,
            // and no "(I)" marker (which flags a surviving *inherited* ACE).
            let user = std::env::var("USERNAME").unwrap();
            let output = std::process::Command::new("icacls")
                .arg(&path)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                text.contains(&user),
                "icacls output should list the owning user: {text}"
            );
            assert!(
                !text.contains("(I)"),
                "no ACE should remain inherited after /inheritance:r: {text}"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
