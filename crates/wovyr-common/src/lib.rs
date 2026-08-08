//! Shared types and primitives for the Wovyr AI Platform.
//!
//! This crate is intentionally dependency-light: every other crate in the
//! workspace depends on it, so it only holds cross-cutting concerns — the
//! platform [`Error`] type, the [`Result`] alias, and token/cost
//! [`Usage`] accounting.

mod error;
mod usage;

// Filesystem primitives (atomic_write, sync_dir, restrict_to_owner, FileLock)
// are meaningless on wasm32-unknown-unknown, which has no filesystem — and
// `fs2` does not compile there. Gating the module keeps `wovyr-ui`/
// `wovyr-ui-guard` buildable for the browser, since neither touches `fs`.
#[cfg(not(target_arch = "wasm32"))]
pub mod fs;

pub use error::{Error, Result};
pub use usage::Usage;
