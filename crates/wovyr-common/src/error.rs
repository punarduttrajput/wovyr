//! The platform-wide error type.
//!
//! Per the [coding standards](../../docs/19-implementation-guide/coding-standards.md),
//! service code uses typed `Result`s rather than `unwrap`/`panic`. Each crate maps
//! its failures onto these variants so callers — and eventually the API error
//! envelope — see a single, stable error shape.

use thiserror::Error;

/// Convenience alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// The canonical error type for Wovyr core crates.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid or missing configuration (env, files, flags).
    #[error("configuration error: {0}")]
    Config(String),

    /// A requested resource (agent, tool, model) does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// Caller-supplied input failed validation.
    #[error("invalid input: {0}")]
    Invalid(String),

    /// The principal lacks the scope/role required for the operation (authorization).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The operation conflicts with existing state (e.g. a duplicate name).
    #[error("conflict: {0}")]
    Conflict(String),

    /// A tenant/project quota would be exceeded by the operation.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),

    /// An LLM provider/gateway call failed.
    #[error("provider error: {message}")]
    Provider {
        /// Human-readable detail.
        message: String,
        /// Milliseconds to wait before retrying, when the provider specified
        /// one (RM-AIM-P2 PRV-205 — e.g. a `Retry-After` header on an HTTP
        /// 429/503). `None` when no hint was given, or the failure wasn't
        /// HTTP-shaped (e.g. a connection error); the resilience layer falls
        /// back to jittered exponential backoff in that case.
        retry_after_ms: Option<u64>,
    },

    /// A tool invocation failed.
    #[error("tool error: {0}")]
    Tool(String),

    /// The agent run loop hit an unrecoverable condition.
    #[error("runtime error: {0}")]
    Runtime(String),

    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Construct a [`Error::Config`] from anything string-like.
    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }

    /// Construct a [`Error::Invalid`] from anything string-like.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    /// Construct a [`Error::Provider`] from anything string-like, with no
    /// server-specified retry hint.
    pub fn provider(msg: impl Into<String>) -> Self {
        Error::Provider {
            message: msg.into(),
            retry_after_ms: None,
        }
    }

    /// Construct a [`Error::Provider`] carrying a server-specified retry delay
    /// (RM-AIM-P2 PRV-205 — e.g. parsed from a `Retry-After` header), which the
    /// gateway's retry loop honors in place of its own jittered backoff.
    pub fn provider_with_retry_after(msg: impl Into<String>, retry_after_ms: u64) -> Self {
        Error::Provider {
            message: msg.into(),
            retry_after_ms: Some(retry_after_ms),
        }
    }

    /// Construct a [`Error::Forbidden`] from anything string-like.
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Error::Forbidden(msg.into())
    }

    /// Construct a [`Error::Conflict`] from anything string-like.
    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into())
    }

    /// Construct a [`Error::QuotaExceeded`] from anything string-like.
    pub fn quota_exceeded(msg: impl Into<String>) -> Self {
        Error::QuotaExceeded(msg.into())
    }
}
