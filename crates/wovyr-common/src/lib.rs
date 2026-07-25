//! Shared types and primitives for the Wovyr AI Platform.
//!
//! This crate is intentionally dependency-light: every other crate in the
//! workspace depends on it, so it only holds cross-cutting concerns — the
//! platform [`Error`] type, the [`Result`] alias, and token/cost
//! [`Usage`] accounting.

mod error;
mod usage;

pub mod fs;

pub use error::{Error, Result};
pub use usage::Usage;
