//! Core tool trait and supporting types.
//!
//! Mirrors the Rust interfaces in the
//! [Tool Framework spec §42–47](../../docs/04-agent-framework/tool-framework.md),
//! trimmed to what v0.1 needs.

use async_trait::async_trait;
use serde_json::Value;
use std::fmt;

/// Immutable, declarative metadata describing a tool
/// ([spec §43](../../docs/04-agent-framework/tool-framework.md)).
#[derive(Clone, Debug)]
pub struct ToolMetadata {
    /// Unique tool id the model calls (e.g. `echo`).
    pub id: String,
    /// Semantic version of the tool.
    pub version: String,
    /// Tool category (e.g. `utility`, `filesystem`, `network`).
    pub category: String,
    /// Human/model-readable description, advertised to the model.
    pub description: String,
    /// Permissions the tool requires, enforced against the caller's grants by
    /// [`check_permissions`] / [`ToolRegistry::execute`](crate::ToolRegistry::execute).
    pub permissions: Vec<String>,
}

impl ToolMetadata {
    /// Convenience constructor with no declared permissions.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        category: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            category: category.into(),
            description: description.into(),
            permissions: Vec::new(),
        }
    }

    /// Builder-style setter for required permissions.
    pub fn with_permissions(mut self, perms: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.permissions = perms.into_iter().map(Into::into).collect();
        self
    }
}

/// Immutable execution context injected by the runtime
/// ([spec §44](../../docs/04-agent-framework/tool-framework.md)).
///
/// v0.1 carries only correlation ids and the working directory; secrets,
/// metrics, and tracing handles are added with their respective subsystems.
#[derive(Clone, Debug, Default)]
pub struct ToolContext {
    /// Unique id for this tool invocation.
    pub execution_id: String,
    /// Id of the agent that requested the tool.
    pub agent_id: String,
    /// Base directory tools may operate within.
    pub workdir: String,
    /// Permissions granted to the caller. `None` means **unrestricted** (no policy);
    /// `Some(set)` enforces that a tool's declared permissions are a subset of it
    /// ([spec §47](../../docs/04-agent-framework/tool-framework.md)).
    pub granted_permissions: Option<Vec<String>>,
}

/// Parameters passed to a tool, as a validated-at-the-boundary JSON value.
#[derive(Clone, Debug)]
pub struct ToolRequest {
    /// Tool-specific parameters (shape declared by [`Tool::input_schema`]).
    pub parameters: Value,
}

impl ToolRequest {
    /// Wrap a JSON value as a tool request.
    pub fn new(parameters: Value) -> Self {
        Self { parameters }
    }
}

/// A standardized tool result ([spec §46](../../docs/04-agent-framework/tool-framework.md)).
#[derive(Clone, Debug)]
pub struct ToolResponse {
    /// Whether the tool considers the invocation successful.
    pub success: bool,
    /// Tool output payload.
    pub payload: Value,
}

impl ToolResponse {
    /// A successful response carrying `payload`.
    pub fn success(payload: Value) -> Self {
        Self {
            success: true,
            payload,
        }
    }
}

/// Standardized tool error categories
/// ([spec §47 / §39](../../docs/04-agent-framework/tool-framework.md)).
#[derive(Clone, Debug)]
pub enum ToolError {
    /// Input failed validation (not retryable).
    Validation(String),
    /// Caller lacked permission (not retryable).
    PermissionDenied(String),
    /// A dependency/network call failed (retryable).
    Network(String),
    /// An unexpected internal failure (retryable).
    Internal(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Validation(m) => write!(f, "validation error: {m}"),
            ToolError::PermissionDenied(m) => write!(f, "permission denied: {m}"),
            ToolError::Network(m) => write!(f, "network error: {m}"),
            ToolError::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// Enforce that every permission a tool `required` is granted by `ctx`. Returns
/// [`ToolError::PermissionDenied`] (fail-closed) listing the missing permissions, or
/// `Ok(())` when the context is unrestricted or grants them all.
pub fn check_permissions(required: &[String], ctx: &ToolContext) -> Result<(), ToolError> {
    let Some(granted) = &ctx.granted_permissions else {
        return Ok(()); // unrestricted: no permission policy in force
    };
    let missing: Vec<&str> = required
        .iter()
        .filter(|p| !granted.contains(p))
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ToolError::PermissionDenied(format!(
            "tool requires ungranted permission(s): {}",
            missing.join(", ")
        )))
    }
}

/// A capability an agent can invoke.
///
/// The runtime calls [`Tool::execute`] after validation/authorization. Every
/// tool also advertises an [`input_schema`](Tool::input_schema) so the model
/// knows how to call it.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Immutable metadata for this tool.
    fn metadata(&self) -> ToolMetadata;

    /// JSON Schema for the tool's parameters (advertised to the model).
    fn input_schema(&self) -> Value;

    /// Execute the tool.
    async fn execute(
        &self,
        ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError>;
}
