//! The one `HOME`/`USERPROFILE` resolution both `wovyr-server` and `wovyr-cli`
//! build on.

use std::path::PathBuf;
use wovyr_common::{Error, Result};

/// The `~/.wovyr` directory both binaries treat as the single source of truth
/// for durable local state. Resolves `HOME` (Unix) or `USERPROFILE` (Windows)
/// — every other function in this crate builds on this instead of resolving
/// either variable itself.
pub fn wovyr_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".wovyr"))
        .ok_or_else(|| {
            Error::config("could not determine home directory (set HOME or USERPROFILE)")
        })
}
