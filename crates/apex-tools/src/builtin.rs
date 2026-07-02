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

/// Printed as the last line of every shell invocation so the caller can learn the
/// command's ending working directory (e.g. after a `cd`) without a persistent shell
/// session. Deliberately plain text rather than a control character so it survives
/// every shell's quoting rules; a real command printing this exact token as its own
/// last line is astronomically unlikely.
const CWD_MARKER: &str = ">>>APEX_CWD>>>";

/// Wrap `command` so it unconditionally prints `CWD_MARKER<ending dir>` as its very
/// last line while preserving the *original* command's exit code (not the marker
/// print's) — each shell has its own way to run a follow-up statement regardless of
/// the prior one's success and to recover that prior exit code.
fn wrap_with_cwd_marker(command: &str, shell: &str) -> String {
    match shell {
        "powershell" => format!(
            "{command}\n\
             $__apexExit = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}\n\
             Write-Output \"{CWD_MARKER}$($PWD.Path)\"\n\
             exit $__apexExit"
        ),
        "cmd" => format!(
            "{command} & set __apexExit=!ERRORLEVEL! & echo {CWD_MARKER}!CD! & exit /b !__apexExit!"
        ),
        _ => format!(
            "{command}\n\
             __apex_exit=$?\n\
             printf '%s%s\\n' '{CWD_MARKER}' \"$PWD\"\n\
             exit \"$__apex_exit\""
        ),
    }
}

/// Pull the trailing `CWD_MARKER<dir>` line out of `stdout`, returning the cleaned
/// output (marker removed) and the reported directory, if the marker is present.
/// Absent when the process was killed (e.g. timeout) before reaching it — callers
/// should then leave the working directory unchanged. Only the marker itself through
/// its line end is stripped, so a command whose output has no trailing newline (the
/// marker then shares its line) keeps that output intact.
fn extract_cwd_marker(stdout: &str) -> (String, Option<String>) {
    let Some(marker_at) = stdout.rfind(CWD_MARKER) else {
        return (stdout.to_string(), None);
    };
    let line_end = stdout[marker_at..]
        .find('\n')
        .map_or(stdout.len(), |p| marker_at + p + 1);
    let cwd = stdout[marker_at + CWD_MARKER.len()..line_end]
        .trim_end_matches(['\r', '\n'])
        .to_string();
    let mut cleaned = String::with_capacity(stdout.len() - (line_end - marker_at));
    cleaned.push_str(&stdout[..marker_at]);
    cleaned.push_str(&stdout[line_end..]);
    (cleaned, Some(cwd).filter(|c| !c.is_empty()))
}

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
            "Run a shell command and return its stdout, stderr, exit code, and ending \
             working directory (`cwd`, reflecting any `cd` the command performed).",
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
                },
                "shell": {
                    "type": "string",
                    "enum": ["powershell", "cmd", "sh"],
                    "description": "Which shell to run the command through. Windows: `powershell` \
                        (default) or `cmd`. Non-Windows: `sh` (the only supported value there)."
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

        let shell = request.parameters.get("shell").and_then(Value::as_str);

        // Resolve the isolation backend (first-party → native on this node).
        SandboxManager::native_only()
            .select(SandboxBackend::Native, TrustClass::FirstParty)
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let limits = ResourceLimits {
            timeout: Duration::from_secs(timeout_secs),
            ..ResourceLimits::default()
        };
        let sandbox = NativeSandbox::with_limits(limits);

        // Run via the requested (or platform-default) shell so users can write normal
        // command lines. PowerShell is the Windows default — it's what an interactive
        // session actually uses, unlike bare `cmd.exe`. Each command is wrapped so it
        // reports its ending working directory (see `CWD_MARKER`), letting the caller
        // observe a `cd` without a persistent shell session.
        let run = if cfg!(windows) {
            match shell.unwrap_or("powershell") {
                "powershell" => {
                    let wrapped = wrap_with_cwd_marker(command, "powershell");
                    sandbox
                        .run(
                            "powershell",
                            &["-NoProfile", "-NonInteractive", "-Command", &wrapped],
                            workdir,
                        )
                        .await
                }
                "cmd" => {
                    let wrapped = wrap_with_cwd_marker(command, "cmd");
                    // `/V:ON` enables the delayed `!ERRORLEVEL!`/`!CD!` expansion the
                    // wrapper relies on.
                    sandbox
                        .run("cmd", &["/V:ON", "/C", &wrapped], workdir)
                        .await
                }
                other => {
                    return Err(ToolError::Validation(format!(
                        "unsupported `shell` value `{other}` on Windows; expected \
                         `powershell` or `cmd`"
                    )));
                }
            }
        } else {
            match shell {
                None | Some("sh") => {
                    let wrapped = wrap_with_cwd_marker(command, "sh");
                    sandbox.run("sh", &["-c", &wrapped], workdir).await
                }
                Some(other) => {
                    return Err(ToolError::Validation(format!(
                        "unsupported `shell` value `{other}` on this platform; expected `sh`"
                    )));
                }
            }
        };
        let outcome = run.map_err(|e| ToolError::Internal(e.to_string()))?;

        // Strip the marker from the visible output; `cwd` is null when the process
        // died before printing it (e.g. timeout) — the directory is then unknown.
        let (stdout, cwd) = extract_cwd_marker(&outcome.stdout);

        Ok(ToolResponse {
            // A non-zero exit or timeout is reported as success=false so the model
            // can react, while still receiving the captured output.
            success: outcome.exit_code == Some(0) && !outcome.timed_out,
            payload: json!({
                "exit_code": outcome.exit_code,
                "stdout": stdout,
                "stderr": outcome.stderr,
                "timed_out": outcome.timed_out,
                "cwd": cwd,
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

    #[test]
    fn extract_cwd_marker_strips_marker_line() {
        let (out, cwd) = extract_cwd_marker(&format!("hi\n{CWD_MARKER}/tmp\n"));
        assert_eq!(out, "hi\n");
        assert_eq!(cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn extract_cwd_marker_absent_leaves_output_unchanged() {
        let (out, cwd) = extract_cwd_marker("plain output\n");
        assert_eq!(out, "plain output\n");
        assert!(cwd.is_none());
    }

    #[test]
    fn extract_cwd_marker_preserves_unterminated_last_line() {
        // A command whose output has no trailing newline shares the marker's line;
        // only the marker is removed, never the command's own text.
        let (out, cwd) = extract_cwd_marker(&format!("partial{CWD_MARKER}/home/x\n"));
        assert_eq!(out, "partial");
        assert_eq!(cwd.as_deref(), Some("/home/x"));
    }

    #[test]
    fn extract_cwd_marker_empty_dir_is_none() {
        let (out, cwd) = extract_cwd_marker(&format!("{CWD_MARKER}\n"));
        assert_eq!(out, "");
        assert!(cwd.is_none());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn shell_reports_ending_working_directory_after_cd() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        let resp = t
            .execute(&ctx, ToolRequest::new(json!({"command": "cd /tmp"})))
            .await
            .unwrap();
        assert!(resp.success, "{resp:?}");
        assert_eq!(resp.payload["cwd"], "/tmp");
        // The marker never leaks into the model-visible output.
        assert!(
            !resp.payload["stdout"]
                .as_str()
                .unwrap()
                .contains(CWD_MARKER)
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn shell_preserves_command_exit_code_despite_cwd_marker() {
        // The wrapper's marker print must not mask the command's own failure code.
        // (`false` fails without exiting the shell, so the wrapper's follow-up lines
        // still run — unlike an explicit `exit`, which skips the marker entirely.)
        let t = ShellTool;
        let ctx = ToolContext::default();
        let resp = t
            .execute(&ctx, ToolRequest::new(json!({"command": "false"})))
            .await
            .unwrap();
        assert!(!resp.success);
        assert_eq!(resp.payload["exit_code"], 1);
        // The marker still ran, so the ending directory is known even on failure.
        assert!(resp.payload["cwd"].is_string());
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

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_defaults_to_powershell_on_windows() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        // `if ($true) { ... }` is PowerShell syntax; cmd.exe would fail to parse it.
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"command": "if ($true) { Write-Output ps_marker }"})),
            )
            .await
            .unwrap();
        assert!(resp.success, "{resp:?}");
        assert!(
            resp.payload["stdout"]
                .as_str()
                .unwrap()
                .contains("ps_marker")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_can_request_cmd_explicitly() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        // `if 1==1 echo ...` is cmd.exe syntax; PowerShell would fail to parse it.
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"command": "if 1==1 echo cmd_marker", "shell": "cmd"})),
            )
            .await
            .unwrap();
        assert!(resp.success, "{resp:?}");
        assert!(
            resp.payload["stdout"]
                .as_str()
                .unwrap()
                .contains("cmd_marker")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_rejects_unknown_shell_value_on_windows() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"command": "echo hi", "shell": "bash"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn shell_rejects_unknown_shell_value_on_unix() {
        let t = ShellTool;
        let ctx = ToolContext::default();
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"command": "echo hi", "shell": "cmd"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
