//! The one `HOME`/`USERPROFILE` resolution both `wovyr-server` and `wovyr-cli`
//! build on.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use wovyr_common::{Error, Result};

/// Process-wide redirect of the state root, set at most once via
/// [`set_root_override`]. Consulted before `HOME`/`USERPROFILE`.
static ROOT_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Point every `~/.wovyr` path at `path` for the rest of this process, and
/// return the root now in effect.
///
/// **First call wins**, and the winner is returned to every caller — so
/// concurrent callers converge on one directory instead of racing. Idempotent
/// for the winner; a later call with a different path is ignored rather than
/// being an error, because the only sane response to "two components disagree
/// about the state root" is to keep the one already in use by whatever has
/// already read or written through it.
///
/// This exists for **tests and embedders**, not operators: a normal
/// deployment sets `HOME`/`USERPROFILE` (or runs as a user that has one).
/// A test process that calls this gets a scratch root, so a test suite no
/// longer reads or writes the developer's real `~/.wovyr` — which it did,
/// leaving test tenancy quotas and workflow executions in live state and
/// appending test entries to the real tamper-evident audit chain. Redirection
/// is per **process**, not per test: every path in this crate resolves through
/// one global root, so tests in the same binary share the scratch root exactly
/// as they previously shared the real one — isolated from the user, not from
/// each other.
pub fn set_root_override(path: impl Into<PathBuf>) -> PathBuf {
    ROOT_OVERRIDE.get_or_init(|| path.into()).clone()
}

/// The override currently in effect, if any.
pub fn root_override() -> Option<&'static Path> {
    ROOT_OVERRIDE.get().map(PathBuf::as_path)
}

/// Redirect the state root at a scratch directory unique to this process, and
/// return it. Safe to call repeatedly and from several threads — like
/// [`set_root_override`], the first call wins and every caller gets the winner.
///
/// The test-support entry point: a test binary calls this before building
/// anything that resolves a `~/.wovyr` path, and the whole binary is then
/// isolated from the developer's real state directory. `tag` only labels the
/// directory (`wovyr-test-<tag>-<pid>`), so a failed run is identifiable; the
/// pid is what makes it unique, and the directory is wiped on the first call so
/// a reused pid can't inherit a previous run's files.
pub fn redirect_to_scratch(tag: &str) -> PathBuf {
    // Everything happens inside `get_or_init` so the wipe-and-create runs exactly
    // once even when several threads race here. Doing it outside would let two
    // callers that computed the same path (same tag, same pid) both "win" by
    // equality and have one wipe the directory the other is already using.
    ROOT_OVERRIDE
        .get_or_init(|| {
            let scratch = std::env::temp_dir()
                .join(format!("wovyr-test-{tag}-{}", std::process::id()))
                .join(".wovyr");
            let _ = std::fs::remove_dir_all(&scratch);
            let _ = std::fs::create_dir_all(&scratch);
            scratch
        })
        .clone()
}

/// The `~/.wovyr` directory both binaries treat as the single source of truth
/// for durable local state. Resolves [`set_root_override`] first, then `HOME`
/// (Unix) or `USERPROFILE` (Windows) — every other function in this crate
/// builds on this instead of resolving either variable itself, which is what
/// makes the override cover every resource directory at once.
pub fn wovyr_dir() -> Result<PathBuf> {
    if let Some(root) = ROOT_OVERRIDE.get() {
        return Ok(root.clone());
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".wovyr"))
        .ok_or_else(|| {
            Error::config("could not determine home directory (set HOME or USERPROFILE)")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The redirect is process-wide and first-call-wins, and every resource
    /// directory follows it — that single indirection is what isolates a test
    /// binary from the developer's real `~/.wovyr` without threading a root
    /// parameter through `paths::*`.
    ///
    /// One test covers all of it deliberately: the override is a process-global
    /// `OnceLock`, so separate `#[test]` functions would race for the first call
    /// and only one could assert on it.
    #[test]
    fn the_root_override_wins_over_the_environment_and_covers_every_path() {
        assert!(
            root_override().is_none(),
            "no test may set the override first"
        );

        let scratch = redirect_to_scratch("config-unit");
        assert!(scratch.exists(), "the scratch root must be created");
        assert_eq!(wovyr_dir().unwrap(), scratch);
        assert_eq!(root_override(), Some(scratch.as_path()));

        // Every resource directory resolves under it, not under HOME.
        for dir in [
            crate::paths::secrets_dir().unwrap(),
            crate::paths::memory_dir().unwrap(),
            crate::paths::kms_dir().unwrap(),
            crate::paths::audit_dir().unwrap(),
            crate::paths::tenancy_dir().unwrap(),
            crate::paths::workflows_dir().unwrap(),
            crate::paths::server_state_dir().unwrap(),
        ] {
            assert!(
                dir.starts_with(&scratch),
                "{dir:?} must resolve under the overridden root {scratch:?}"
            );
        }

        // First call wins: a later, different path is ignored rather than
        // silently moving state out from under whatever is already using it.
        let second = set_root_override(std::env::temp_dir().join("wovyr-test-someone-else"));
        assert_eq!(second, scratch);
        assert_eq!(wovyr_dir().unwrap(), scratch);
        // ...and asking for a scratch root again is idempotent.
        assert_eq!(redirect_to_scratch("another-tag"), scratch);

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
