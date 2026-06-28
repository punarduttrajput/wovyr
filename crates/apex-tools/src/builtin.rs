//! Built-in tools shipped with v0.1.
//!
//! The [roadmap](../../docs/18-roadmap/v0.1.md) calls for a native sandbox with
//! `fs`/`http`/`shell`. v0.1 implements `fs_read` and `http_get` (read-only,
//! lowest-risk) plus an `echo` utility; `shell` and write access are deferred
//! until the sandbox/permission enforcement lands, since running them unsandboxed
//! would violate the security model
//! ([tool framework §31](../../docs/04-agent-framework/tool-framework.md)).

use crate::sandbox::{NativeSandbox, ResourceLimits, SandboxBackend, SandboxManager, TrustClass};
use crate::tool::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;

/// Read the `parameters` of the request and return them unchanged. Useful for
/// smoke-testing the tool-calling loop.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "echo",
            "1.0.0",
            "utility",
            "Echo the given parameters back unchanged.",
        )
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        Ok(ToolResponse::success(request.parameters))
    }
}

/// Read a UTF-8 text file from disk.
pub struct FsReadTool;

#[async_trait]
impl Tool for FsReadTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "fs_read",
            "1.0.0",
            "filesystem",
            "Read the contents of a UTF-8 text file at the given path.",
        )
        .with_permissions(["filesystem.read"])
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read." }
            }
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let path = request
            .parameters
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Validation("missing required string field `path`".into()))?;

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ToolError::Internal(format!("could not read {path}: {e}")))?;

        Ok(ToolResponse::success(json!({
            "path": path,
            "content": content,
        })))
    }
}

/// Maximum number of body bytes returned by [`HttpGetTool`] to avoid flooding the
/// model context with large pages.
const MAX_BODY_BYTES: usize = 16 * 1024;

/// Perform an HTTP GET request and return status, headers count, and a truncated body.
pub struct HttpGetTool {
    client: reqwest::Client,
}

impl HttpGetTool {
    /// Construct with a default HTTP client.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for HttpGetTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for HttpGetTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "http_get",
            "1.0.0",
            "network",
            "Perform an HTTP GET request and return the status and response body.",
        )
        .with_permissions(["net.egress"])
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": { "type": "string", "description": "Absolute http(s) URL to fetch." }
            }
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let url = request
            .parameters
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Validation("missing required string field `url`".into()))?;

        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ToolError::Validation(
                "url must start with http:// or https://".into(),
            ));
        }

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::Network(format!("GET {url} failed: {e}")))?;

        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::Network(format!("reading body of {url} failed: {e}")))?;

        let truncated = body.len() > MAX_BODY_BYTES;
        let body = if truncated {
            body.chars().take(MAX_BODY_BYTES).collect::<String>()
        } else {
            body
        };

        Ok(ToolResponse::success(json!({
            "url": url,
            "status": status,
            "truncated": truncated,
            "body": body,
        })))
    }
}

/// Default execution timeout for the shell tool, in seconds.
const SHELL_DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum allowed shell timeout, in seconds.
const SHELL_MAX_TIMEOUT_SECS: u64 = 300;

/// Run a shell command through the sandbox.
///
/// `shell` is a first-party tool, so backend selection resolves to the native
/// sandbox (process + timeout + output cap). Stronger confinement (CPU/memory/
/// network/filesystem) requires the container/microVM backends, which are not yet
/// available; the gate today is that an agent must list `shell` in its allowed tools.
pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "shell",
            "1.0.0",
            "system",
            "Run a shell command and return its stdout, stderr, and exit code.",
        )
        .with_permissions(["shell.execute"])
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "Shell command line to execute." },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional execution timeout in seconds (default 30, max 300)."
                }
            }
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let command = request
            .parameters
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::Validation("missing required string field `command`".into())
            })?;

        let timeout_secs = request
            .parameters
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(SHELL_DEFAULT_TIMEOUT_SECS)
            .min(SHELL_MAX_TIMEOUT_SECS);

        let workdir = if ctx.workdir.is_empty() {
            "."
        } else {
            ctx.workdir.as_str()
        };

        // Resolve the isolation backend (first-party → native on this node).
        SandboxManager::native_only()
            .select(SandboxBackend::Native, TrustClass::FirstParty)
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let limits = ResourceLimits {
            timeout: Duration::from_secs(timeout_secs),
            ..ResourceLimits::default()
        };
        let sandbox = NativeSandbox::with_limits(limits);

        // Run via the platform shell so users can write normal command lines.
        let run = if cfg!(windows) {
            sandbox.run("cmd", &["/C", command], workdir).await
        } else {
            sandbox.run("sh", &["-c", command], workdir).await
        };
        let outcome = run.map_err(|e| ToolError::Internal(e.to_string()))?;

        Ok(ToolResponse {
            // A non-zero exit or timeout is reported as success=false so the model
            // can react, while still receiving the captured output.
            success: outcome.exit_code == Some(0) && !outcome.timed_out,
            payload: json!({
                "exit_code": outcome.exit_code,
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
                "timed_out": outcome.timed_out,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_returns_input() {
        let t = EchoTool;
        let ctx = ToolContext::default();
        let resp = t
            .execute(&ctx, ToolRequest::new(json!({"hello": "world"})))
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(resp.payload, json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn fs_read_missing_path_is_validation_error() {
        let t = FsReadTool;
        let ctx = ToolContext::default();
        let err = t
            .execute(&ctx, ToolRequest::new(json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn http_get_rejects_non_http_scheme() {
        let t = HttpGetTool::new();
        let ctx = ToolContext::default();
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"url": "ftp://example.com"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn shell_runs_command_and_reports_success() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"command": "echo apex_shell_ok"})),
            )
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(resp.payload["exit_code"], 0);
        assert!(
            resp.payload["stdout"]
                .as_str()
                .unwrap()
                .contains("apex_shell_ok")
        );
    }

    #[tokio::test]
    async fn shell_missing_command_is_validation_error() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        let err = t
            .execute(&ctx, ToolRequest::new(json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
