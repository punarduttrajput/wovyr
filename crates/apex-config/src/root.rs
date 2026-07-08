//! The one `HOME`/`USERPROFILE` resolution both `apex-server` and `apex-cli`
//! build on.

use apex_common::{Error, Result};
use std::path::PathBuf;

/// The `~/.apex` directory both binaries treat as the single source of truth
/// for durable local state. Resolves `HOME` (Unix) or `USERPROFILE` (Windows)
/// — every other function in this crate builds on this instead of resolving
/// either variable itself.
pub fn apex_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".apex"))
        .ok_or_else(|| {
            Error::config("could not determine home directory (set HOME or USERPROFILE)")
        })
}
