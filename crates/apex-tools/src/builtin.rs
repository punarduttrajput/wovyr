//! Built-in tools shipped with v0.1.
//!
//! The [roadmap](../../docs/18-roadmap/v0.1.md) calls for a native sandbox with
//! `fs`/`http`/`shell`. v0.1 implements `fs_read` and `http_get` (read-only,
//! lowest-risk) plus an `echo` utility; `shell` and write access are deferred
//! until the sandbox/permission enforcement lands, since running them unsandboxed
//! would violate the security model
//! ([tool framework §31](../../docs/04-agent-framework/tool-framework.md)).
//!
//! [`ImageGenTool`] (`image_generate`) is a later addition, deliberately kept out
//! of [`crate::ToolRegistry::with_builtins`] — it calls a real, billed external API,
//! unlike the always-free tools above — so a caller registers it explicitly (see its
//! doc comment) rather than getting it for free in every agent.

use crate::sandbox::{NativeSandbox, ResourceLimits, SandboxBackend, SandboxManager};
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

/// Resolve `requested` against the confinement root `root` (`ctx.workdir`),
/// canonicalizing both and rejecting any resolved path that escapes `root`
/// ([RM-GA-P1 SEC-302](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)).
/// Canonicalization resolves symlinks (and `..` segments) *before* the prefix check,
/// so a symlink inside the root pointing outside it is caught too, not just literal
/// `../` traversal. An absolute `requested` path is honored as a candidate location
/// (not auto-rejected) but still must canonicalize to somewhere under `root` — the
/// same bar a relative path is held to.
async fn confine_path(root: &str, requested: &str) -> Result<std::path::PathBuf, ToolError> {
    let root = if root.is_empty() { "." } else { root };
    let root_canonical = tokio::fs::canonicalize(root).await.map_err(|e| {
        ToolError::Internal(format!("could not resolve workspace root `{root}`: {e}"))
    })?;

    let requested_path = std::path::Path::new(requested);
    let candidate = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        root_canonical.join(requested_path)
    };

    let candidate_canonical = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|e| ToolError::Internal(format!("could not read {requested}: {e}")))?;

    if !candidate_canonical.starts_with(&root_canonical) {
        return Err(ToolError::PermissionDenied(format!(
            "path `{requested}` escapes the confined workspace root `{root}`"
        )));
    }
    Ok(candidate_canonical)
}

/// Read a UTF-8 text file from disk, confined to the run's workspace root
/// (`ctx.workdir`) — SEC-302. Never able to read outside it (e.g. `~/.apex`'s
/// platform state — secrets, the KMS root key), symlink escapes included.
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
        ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let path = request
            .parameters
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Validation("missing required string field `path`".into()))?;

        let confined = confine_path(&ctx.workdir, path).await?;
        let content = tokio::fs::read_to_string(&confined)
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

/// Default `User-Agent` sent with every outbound request. Many real-world sites
/// (e.g. Wikipedia's robots policy, https://w.wiki/4wJS) reject unidentified
/// clients with a 403 when no `User-Agent` is set — `reqwest::Client::new()`
/// sends none by default. Not yet configurable per-request/tenant; a fixed
/// default is enough until there's a concrete need to override it.
const DEFAULT_USER_AGENT: &str = "Apex-AI-Platform/0.1 (+https://github.com/apex-ai/apex)";

/// Perform an HTTP GET request and return status, headers count, and a truncated
/// body. A unit struct — unlike before SEC-304, there's no point holding a single
/// pre-built `reqwest::Client`: each call needs its own, with DNS resolution pinned
/// (`.resolve(host, addr)`) to the specific address [`resolve_and_guard`] already
/// vetted for *this* call.
pub struct HttpGetTool;

impl HttpGetTool {
    /// Construct the tool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for HttpGetTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `ip` must never be reached by `http_get`, regardless of any egress
/// allow-list ([RM-GA-P1 SEC-304](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
/// loopback, link-local (which includes the cloud metadata address
/// `169.254.169.254`), private/unique-local, and unspecified. An IPv4-mapped IPv6
/// address (`::ffff:a.b.c.d`) is classified by its embedded IPv4 address.
fn is_blocked_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_private()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped));
            }
            let octets = v6.octets();
            let is_unique_local = (octets[0] & 0xfe) == 0xfc; // fc00::/7
            let is_link_local = octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80; // fe80::/10
            v6.is_loopback() || v6.is_unspecified() || is_unique_local || is_link_local
        }
    }
}

/// Resolve `host:port`, refusing (SEC-304) if *any* candidate address is internal
/// (loopback/link-local/private/metadata) — conservative, so a host with mixed
/// public/internal DNS answers can't be used to reach the internal one — or if an
/// egress allow-list is configured and `host` isn't on it. Returns the first
/// resolved address, to be **pinned** for the actual connection (a second DNS
/// lookup at connect time could return a different address — DNS rebinding).
async fn resolve_and_guard(
    host: &str,
    port: u16,
    egress_allowlist: Option<&[String]>,
) -> Result<std::net::SocketAddr, ToolError> {
    if let Some(allowlist) = egress_allowlist
        && !allowlist.iter().any(|h| h == host)
    {
        return Err(ToolError::PermissionDenied(format!(
            "host `{host}` is not on the egress allow-list"
        )));
    }

    let mut candidates = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ToolError::Network(format!("DNS resolution for `{host}` failed: {e}")))?;
    let first = candidates.next().ok_or_else(|| {
        ToolError::Network(format!("DNS resolution for `{host}` returned no addresses"))
    })?;

    for addr in std::iter::once(first).chain(candidates) {
        if is_blocked_ip(addr.ip()) {
            return Err(ToolError::PermissionDenied(format!(
                "host `{host}` resolves to a blocked internal/metadata address ({})",
                addr.ip()
            )));
        }
    }
    Ok(first)
}

/// Build a client that resolves `host` to exactly `pinned` (the address
/// [`resolve_and_guard`] already vetted), rather than re-resolving DNS at connect
/// time — SEC-304's defeat of a DNS-rebinding attack — carrying the tool's default
/// `User-Agent`.
fn pinned_client(host: &str, pinned: std::net::SocketAddr) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(DEFAULT_USER_AGENT)
        .resolve(host, pinned)
        .build()
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
        ctx: &ToolContext,
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
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| ToolError::Validation(format!("invalid URL `{url}`: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| ToolError::Validation(format!("URL `{url}` has no host")))?
            .to_string();
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| ToolError::Validation(format!("URL `{url}` has no resolvable port")))?;

        let pinned = resolve_and_guard(&host, port, ctx.egress_allowlist.as_deref()).await?;

        // Pin the DNS-resolved address for this request (defeats rebinding: a second
        // lookup at connect time could answer with a different, unsafe address) —
        // this needs a dedicated client, since `resolve` is a client-builder setting.
        let client = pinned_client(&host, pinned)
            .map_err(|e| ToolError::Internal(format!("could not build HTTP client: {e}")))?;

        let resp = client
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

/// Default OpenAI-compatible base URL for image generation. Mirrors
/// `apex-provider`'s `OpenAiProvider` env-var contract (`OPENAI_API_KEY` +
/// `APEX_OPENAI_BASE_URL`) without apex-tools taking a dependency on
/// apex-provider — apex-tools sits below apex-provider in the dependency
/// spine, so this reads the same two env vars independently.
const DEFAULT_IMAGE_BASE_URL: &str = "https://api.openai.com/v1";

/// Generate an image from a text prompt via an OpenAI-compatible
/// `/images/generations` endpoint. Not registered by [`crate::ToolRegistry::with_builtins`]
/// by default — an agent opts in by listing `image_generate` in its manifest
/// `tools:` and the registry that constructs it, since (unlike `http_get`) it
/// needs a real API key and incurs real cost per call.
pub struct ImageGenTool {
    client: reqwest::Client,
}

impl ImageGenTool {
    /// Construct with a default HTTP client. Reads `OPENAI_API_KEY`/
    /// `APEX_OPENAI_BASE_URL` per call (in [`Tool::execute`]), not at
    /// construction, so a missing key fails a single tool call rather than
    /// registry setup.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ImageGenTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "image_generate",
            "1.0.0",
            "media",
            "Generate an image from a text prompt, returning image URLs or base64 data.",
        )
        .with_permissions(["net.egress"])
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": { "type": "string", "description": "Text description of the desired image." },
                "size": { "type": "string", "description": "Image dimensions, e.g. `1024x1024` (default `1024x1024`)." },
                "n": { "type": "integer", "description": "Number of images to generate (default 1)." }
            }
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let prompt = request
            .parameters
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::Validation("missing required string field `prompt`".into())
            })?;
        let size = request
            .parameters
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("1024x1024");
        let n = request
            .parameters
            .get("n")
            .and_then(Value::as_u64)
            .unwrap_or(1);

        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            ToolError::Validation(
                "OPENAI_API_KEY is not set; image_generate needs an OpenAI-compatible API key"
                    .into(),
            )
        })?;
        let base_url = std::env::var("APEX_OPENAI_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_IMAGE_BASE_URL.to_string());
        let base_url = base_url.trim_end_matches('/');

        let resp = self
            .client
            .post(format!("{base_url}/images/generations"))
            .bearer_auth(&api_key)
            .json(&json!({ "prompt": prompt, "size": size, "n": n }))
            .send()
            .await
            .map_err(|e| ToolError::Network(format!("image generation request failed: {e}")))?;

        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| {
            ToolError::Network(format!("reading image generation response failed: {e}"))
        })?;

        if !status.is_success() {
            let message = body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(ToolError::Network(format!(
                "image generation failed ({status}): {message}"
            )));
        }

        let images = body.get("data").cloned().unwrap_or(Value::Array(vec![]));

        Ok(ToolResponse::success(json!({
            "prompt": prompt,
            "images": images,
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
/// Backend selection is driven by the caller's real `ctx.trust_class`
/// ([RM-GA-P1 SEC-305](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md)):
/// a first-party run resolves to the native sandbox (process + timeout + output
/// cap); a `Verified`/`Untrusted` run is floored to `Container`/`Gvisor` and fails
/// closed here (`SandboxManager::native_only()` advertises only `Native`), since
/// this node's default capability set doesn't include those stronger backends —
/// there is no silent downgrade to native for untrusted provenance.
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

        // Resolve the isolation backend from the caller's real trust class
        // (SEC-305) — an untrusted/verified-but-not-first-party run is floored to a
        // stronger backend than this node's native-only capability set offers, so
        // selection fails closed rather than silently running natively.
        SandboxManager::native_only()
            .select(SandboxBackend::Native, ctx.trust_class)
            .map_err(|e| ToolError::PermissionDenied(e.to_string()))?;

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

    // --- RM-GA-P1 SEC-302: fs_read confinement --------------------------------------

    /// A scratch workspace root with a file inside it, and a sibling file *outside*
    /// it — the confinement boundary a test needs both sides of.
    fn workspace_fixture() -> (tempfile_dir::TempDir, std::path::PathBuf) {
        let root = tempfile_dir::TempDir::new();
        std::fs::write(root.path().join("inside.txt"), "inside content").unwrap();
        let outside_dir = tempfile_dir::TempDir::new();
        std::fs::write(outside_dir.path().join("secret.txt"), "outside content").unwrap();
        let outside_file = outside_dir.path().join("secret.txt");
        // Keep `outside_dir` alive for the caller by leaking it deliberately — a test
        // fixture, not long-lived process state.
        std::mem::forget(outside_dir);
        (root, outside_file)
    }

    /// A minimal `TempDir` (create on `new`, `rmdir -rf` on `Drop`) — avoids adding a
    /// dev-dependency just for this test module.
    mod tempfile_dir {
        pub(super) struct TempDir(std::path::PathBuf);
        impl TempDir {
            pub(super) fn new() -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "apex-fs-read-test-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }
            pub(super) fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[tokio::test]
    async fn fs_read_allows_a_path_inside_the_workspace_root() {
        let (root, _outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let resp = FsReadTool
            .execute(&ctx, ToolRequest::new(json!({"path": "inside.txt"})))
            .await
            .unwrap();
        assert_eq!(resp.payload["content"], "inside content");
    }

    #[tokio::test]
    async fn fs_read_denies_dot_dot_traversal_out_of_the_root() {
        let (root, outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        // Reach the outside file via `../<outside-dir-name>/secret.txt`.
        let traversal = format!(
            "../{}/{}",
            outside
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            outside.file_name().unwrap().to_string_lossy()
        );
        let err = FsReadTool
            .execute(&ctx, ToolRequest::new(json!({"path": traversal})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn fs_read_denies_an_absolute_path_outside_the_root() {
        let (root, outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsReadTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": outside.to_string_lossy()})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_read_denies_a_symlink_that_escapes_the_root() {
        let (root, outside) = workspace_fixture();
        let link = root.path().join("escape-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsReadTool
            .execute(&ctx, ToolRequest::new(json!({"path": "escape-link"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    /// The concrete regression the ticket calls out: confined to a scratch
    /// workspace, `fs_read` cannot reach `~/.apex/kms/root.key` regardless of how
    /// the path is spelled.
    #[tokio::test]
    async fn fs_read_cannot_reach_the_kms_root_key() {
        let root = tempfile_dir::TempDir::new();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let kms_key = format!("{home}/.apex/kms/root.key");
        let err = FsReadTool
            .execute(&ctx, ToolRequest::new(json!({"path": kms_key})))
            .await
            .unwrap_err();
        // Denied one way or another: PermissionDenied if it resolves and escapes,
        // Internal (not found) if this machine has no such file — never a success.
        assert!(
            matches!(err, ToolError::PermissionDenied(_) | ToolError::Internal(_)),
            "{err:?}"
        );
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

    // --- RM-GA-P1 SEC-304: SSRF guard -------------------------------------------------

    #[test]
    fn is_blocked_ip_classifies_internal_and_metadata_ranges() {
        let blocked = [
            "127.0.0.1",        // loopback
            "169.254.169.254",  // cloud metadata (link-local)
            "169.254.0.1",      // link-local
            "10.0.0.1",         // private
            "172.16.0.1",       // private
            "192.168.1.1",      // private
            "0.0.0.0",          // unspecified
            "::1",              // loopback v6
            "fe80::1",          // link-local v6
            "fc00::1",          // unique-local v6 ("private" equivalent)
            "::ffff:127.0.0.1", // IPv4-mapped loopback
            "::ffff:10.0.0.1",  // IPv4-mapped private
        ];
        for ip in blocked {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} should be blocked");
        }

        let allowed = [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ];
        for ip in allowed {
            assert!(
                !is_blocked_ip(ip.parse().unwrap()),
                "{ip} should not be blocked"
            );
        }
    }

    #[tokio::test]
    async fn http_get_denies_a_loopback_url() {
        let t = HttpGetTool::new();
        let ctx = ToolContext::default();
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"url": "http://127.0.0.1:1/"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn http_get_denies_the_cloud_metadata_address() {
        let t = HttpGetTool::new();
        let ctx = ToolContext::default();
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"url": "http://169.254.169.254/latest/meta-data/"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn http_get_denies_a_private_range_url() {
        let t = HttpGetTool::new();
        let ctx = ToolContext::default();
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"url": "http://10.1.2.3/"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn http_get_egress_allowlist_narrows_to_listed_public_hosts() {
        let t = HttpGetTool::new();
        // A public IP that isn't on the allow-list is denied even though it isn't
        // internal.
        let ctx = ToolContext {
            egress_allowlist: Some(vec!["allowed.example.com".to_string()]),
            ..ToolContext::default()
        };
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"url": "http://8.8.8.8/"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn http_get_egress_allowlist_does_not_override_the_internal_range_block() {
        let t = HttpGetTool::new();
        // Even an explicitly allow-listed host is still refused if it resolves to an
        // internal address — the allow-list narrows public egress, it never
        // legitimizes reaching an internal one ("regardless" — SEC-304).
        let ctx = ToolContext {
            egress_allowlist: Some(vec!["127.0.0.1".to_string()]),
            ..ToolContext::default()
        };
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"url": "http://127.0.0.1:1/"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn image_generate_missing_prompt_is_validation_error() {
        let t = ImageGenTool::new();
        let ctx = ToolContext::default();
        let err = t
            .execute(&ctx, ToolRequest::new(json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn http_get_sends_default_user_agent() {
        // Exercises `pinned_client` (the client-construction logic `execute` uses)
        // directly against a real loopback listener, rather than through
        // `HttpGetTool::execute` — loopback is unconditionally refused there now
        // (SEC-304), which this test deliberately isn't exercising.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });

        let client = pinned_client("example.invalid", addr).unwrap();
        let resp = client
            .get(format!("http://example.invalid:{}/", addr.port()))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let request = server.await.unwrap();
        let user_agent_line = request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
            .expect("request must include a User-Agent header");
        assert!(user_agent_line.contains(DEFAULT_USER_AGENT));
    }

    // --- RM-GA-P1 SEC-305: sandbox selection driven by real TrustClass ---------------

    #[tokio::test]
    async fn shell_runs_natively_for_first_party_trust_class() {
        let t = ShellTool;
        let ctx = ToolContext {
            trust_class: crate::sandbox::TrustClass::FirstParty,
            ..ToolContext::default()
        };
        let resp = t
            .execute(&ctx, ToolRequest::new(json!({"command": "echo ok"})))
            .await
            .unwrap();
        assert!(resp.success);
    }

    #[tokio::test]
    async fn shell_denies_untrusted_provenance_rather_than_running_natively() {
        let t = ShellTool;
        let ctx = ToolContext {
            trust_class: crate::sandbox::TrustClass::Untrusted,
            ..ToolContext::default()
        };
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"command": "echo ok"})))
            .await
            .unwrap_err();
        // Floored to Gvisor, which this node's native-only capability set doesn't
        // support — fails closed, never silently falls back to Native.
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn shell_denies_verified_but_not_first_party_provenance() {
        let t = ShellTool;
        let ctx = ToolContext {
            trust_class: crate::sandbox::TrustClass::Verified,
            ..ToolContext::default()
        };
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"command": "echo ok"})))
            .await
            .unwrap_err();
        // Floored to Container, also unsupported by native_only() — same fail-closed
        // behavior.
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
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
