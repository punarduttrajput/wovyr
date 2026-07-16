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

use crate::sandbox::{
    CommandOutcome, ContainerSandbox, NativeSandbox, NetworkPolicy, ResourceLimits, Sandbox,
    SandboxBackend, SandboxCommand, SandboxError, SandboxManager,
};
use crate::tool::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
use apex_provider::{Gateway, ImageGenRequest};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
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

/// Resolve `requested` against the confinement root `root` for a **write** target
/// (SBX-301) — the write-side counterpart of [`confine_path`], which can't be reused
/// directly because it canonicalizes the *whole* candidate, and a brand-new file
/// naturally doesn't exist yet for `canonicalize` to resolve. Instead: split off the
/// file name, canonicalize just the *parent directory* (which must already exist) the
/// same way `confine_path` canonicalizes a read target, and reject if that escapes
/// `root`. If the target itself already exists as a **symlink**, its resolved
/// destination is checked too — otherwise a write would silently follow a symlink
/// planted inside the confined root out to a target outside it, a write-specific
/// escape a read-only confinement check doesn't need to consider (a symlink `fs_read`
/// might follow only leaks data already reachable by read; a symlink `fs_write`
/// follows could **overwrite** an arbitrary file outside the root).
async fn confine_path_for_write(
    root: &str,
    requested: &str,
) -> Result<std::path::PathBuf, ToolError> {
    let root = if root.is_empty() { "." } else { root };
    let root_canonical = tokio::fs::canonicalize(root).await.map_err(|e| {
        ToolError::Internal(format!("could not resolve workspace root `{root}`: {e}"))
    })?;

    let requested_path = std::path::Path::new(requested);
    let file_name = requested_path.file_name().ok_or_else(|| {
        ToolError::Validation(format!("path `{requested}` has no file name component"))
    })?;
    if file_name == ".." || file_name == "." {
        return Err(ToolError::Validation(format!(
            "path `{requested}` must name a file, not `.`/`..`"
        )));
    }

    let parent = requested_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let parent_candidate = if parent.as_os_str().is_empty() {
        root_canonical.clone()
    } else if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        root_canonical.join(parent)
    };

    let parent_canonical = tokio::fs::canonicalize(&parent_candidate)
        .await
        .map_err(|e| {
            ToolError::Internal(format!(
                "could not resolve parent directory of `{requested}`: {e}"
            ))
        })?;

    if !parent_canonical.starts_with(&root_canonical) {
        return Err(ToolError::PermissionDenied(format!(
            "path `{requested}` escapes the confined workspace root `{root}`"
        )));
    }

    let candidate = parent_canonical.join(file_name);
    if let Ok(metadata) = tokio::fs::symlink_metadata(&candidate).await
        && metadata.file_type().is_symlink()
    {
        match tokio::fs::canonicalize(&candidate).await {
            Ok(resolved) if resolved.starts_with(&root_canonical) => {}
            _ => {
                return Err(ToolError::PermissionDenied(format!(
                    "path `{requested}` is a symlink that escapes the confined workspace root \
                     `{root}`"
                )));
            }
        }
    }
    Ok(candidate)
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

/// Write UTF-8 text content to a file, confined to the run's workspace root
/// (`ctx.workdir`) — SBX-301, the write-side counterpart of `fs_read`'s SEC-302
/// confinement (via [`confine_path_for_write`], including a check against a
/// planted symlink escaping the root). Deliberately **not** registered by
/// [`crate::ToolRegistry::with_builtins`] — like `shell`, write access is an
/// explicit opt-in ([`crate::ToolRegistry::with_fs_write`]/
/// [`crate::ToolRegistry::with_privileged_builtins`]), since handing every agent
/// unconditional write access by default is a much bigger blast radius than the
/// read-only builtins (SEC-301's stance extended to writes).
pub struct FsWriteTool;

#[async_trait]
impl Tool for FsWriteTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "fs_write",
            "1.0.0",
            "filesystem",
            "Write UTF-8 text content to a file at the given path, creating or \
             overwriting it (or appending, with `append: true`).",
        )
        .with_permissions(["filesystem.write"])
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write." },
                "content": { "type": "string", "description": "UTF-8 text content to write." },
                "append": {
                    "type": "boolean",
                    "description": "Append to the file instead of overwriting it (default false)."
                }
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
        let content = request
            .parameters
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::Validation("missing required string field `content`".into())
            })?;
        let append = request
            .parameters
            .get("append")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let confined = confine_path_for_write(&ctx.workdir, path).await?;

        if append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&confined)
                .await
                .map_err(|e| ToolError::Internal(format!("could not open {path}: {e}")))?;
            file.write_all(content.as_bytes())
                .await
                .map_err(|e| ToolError::Internal(format!("could not write {path}: {e}")))?;
        } else {
            tokio::fs::write(&confined, content)
                .await
                .map_err(|e| ToolError::Internal(format!("could not write {path}: {e}")))?;
        }

        Ok(ToolResponse::success(json!({
            "path": path,
            "bytes_written": content.len(),
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
pub(crate) async fn resolve_and_guard(
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
pub(crate) fn pinned_client(
    host: &str,
    pinned: std::net::SocketAddr,
) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(DEFAULT_USER_AGENT)
        .resolve(host, pinned)
        .build()
}

/// Parse an `http(s)://` URL into `(host, port)` — the shared first half of
/// SEC-304's guard, factored out so [`HttpGetTool`] and the MCP `Http`-transport
/// connection guard (RM-MCX-P1-104) can never diverge on what counts as a
/// validatable target.
pub(crate) fn parse_http_host_port(url: &str) -> Result<(String, u16), ToolError> {
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
    Ok((host, port))
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

        let (host, port) = parse_http_host_port(url)?;

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

/// Generate an image from a text prompt via the shared [`Gateway`]
/// (RM-GA-P4 HLTH-904) — retry/failover/circuit-breaking and cost metering
/// apply the same as any other gateway call, instead of this tool holding its
/// own bare HTTP client and reading `OPENAI_API_KEY`/`APEX_OPENAI_BASE_URL`
/// independently of `apex-provider`'s identical env-var contract. Not
/// registered by [`crate::ToolRegistry::with_builtins`] by default — an agent
/// opts in by listing `image_generate` in its manifest `tools:` and the
/// registry that constructs it, since (unlike `http_get`) it incurs real cost
/// per call.
pub struct ImageGenTool {
    gateway: Arc<Gateway>,
}

impl ImageGenTool {
    /// Construct over the run's shared gateway.
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
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

        let request = ImageGenRequest::new(prompt).with_size(size).with_n(n);
        let response = self
            .gateway
            .generate_image(request)
            .await
            .map_err(|e| match e {
                apex_common::Error::Invalid(m) | apex_common::Error::Config(m) => {
                    ToolError::Validation(m)
                }
                other => ToolError::Network(other.to_string()),
            })?;

        Ok(ToolResponse::success(json!({
            "prompt": prompt,
            "images": response.images,
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
        "cmd" => {
            // cmd.exe's parser treats `<`/`>`/`|`/`&` as redirection/control
            // operators wherever they appear on the line — including inside a
            // bare `echo` argument, unlike PowerShell's `"..."` string literals
            // or a Unix shell's quoting. `CWD_MARKER`'s `>>>` would otherwise be
            // read as output redirection ("> was unexpected at this time."),
            // verified live against a real cmd.exe. `^` escapes each one to a
            // literal character; the escaped form only affects parsing, so the
            // actual printed (and later `extract_cwd_marker`-matched) text is
            // still the plain, unescaped marker.
            let escaped_marker = CWD_MARKER.replace('>', "^>");
            format!(
                "{command} & set __apexExit=!ERRORLEVEL! & echo {escaped_marker}!CD! & exit /b !__apexExit!"
            )
        }
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

/// Default OCI image untrusted/verified shell commands run inside when a container
/// backend is selected. Overridable via `APEX_SANDBOX_IMAGE`; small + has `/bin/sh`.
const DEFAULT_SANDBOX_IMAGE: &str = "alpine:3.20";

/// Run a shell command through the sandbox.
///
/// Backend selection is driven by the caller's real `ctx.trust_class`
/// ([RM-GA-P1 SEC-305](../../docs/18-roadmap/v1.0/phase1-security-floor-tickets.md))
/// against this tool's [`SandboxManager`] — the node's *detected* capabilities
/// (RM-AIM-P1 SBX-101), not a hardcoded native-only set. A first-party run resolves
/// to the native sandbox (host shell: process + timeout + output cap); a
/// `Verified`/`Untrusted` run is floored to `Container`/`Gvisor` and, **when the node
/// actually has Docker/gVisor**, runs the command inside a network-isolated Linux
/// container via `sh -c` instead of failing closed. On a node with no strong backend
/// (e.g. [`ShellTool::native_only`], the CLI/local default), such a run still fails
/// closed — there is never a silent downgrade to native for untrusted provenance.
pub struct ShellTool {
    /// The node's backend capabilities, used to resolve the trust-class floor.
    manager: SandboxManager,
    /// OCI image used when a container/gVisor backend is selected.
    image: String,
}

impl ShellTool {
    /// A shell tool for a **trusted first-party / local** context: native-only
    /// capabilities, so a verified/untrusted run fails closed (no strong backend to
    /// run it in). This is the CLI's `agents run --local` and every test's default.
    pub fn native_only() -> Self {
        Self {
            manager: SandboxManager::native_only(),
            image: default_sandbox_image(),
        }
    }

    /// A shell tool driven by the node's **detected** backend capabilities
    /// (RM-AIM-P1 SBX-101): a verified/untrusted run uses the strongest available
    /// backend (container/gVisor) rather than failing closed, if the node has one.
    pub fn with_manager(manager: SandboxManager) -> Self {
        Self {
            manager,
            image: default_sandbox_image(),
        }
    }

    /// Override the container image (builder-style; default [`DEFAULT_SANDBOX_IMAGE`]).
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Run the command on the native host shell (first-party path). Returns the raw
    /// outcome; the caller strips the cwd marker and shapes the response.
    async fn run_native(
        &self,
        command: &str,
        shell: Option<&str>,
        workdir: &str,
        limits: ResourceLimits,
    ) -> Result<CommandOutcome, ToolError> {
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
        run.map_err(|e| ToolError::Internal(e.to_string()))
    }

    /// Run the command inside a network-isolated Linux container (verified/untrusted
    /// path). The container is Linux, so only `sh` applies; a Windows-shell request is
    /// rejected rather than silently ignored.
    async fn run_container(
        &self,
        backend: SandboxBackend,
        command: &str,
        shell: Option<&str>,
        workdir: &str,
        limits: ResourceLimits,
    ) -> Result<CommandOutcome, ToolError> {
        if let Some(other) = shell.filter(|s| *s != "sh") {
            return Err(ToolError::Validation(format!(
                "shell `{other}` is unavailable in the isolated container sandbox used \
                 for verified/untrusted runs; only `sh` is supported there"
            )));
        }
        let wrapped = wrap_with_cwd_marker(command, "sh");
        let sandbox = match backend {
            SandboxBackend::Gvisor => ContainerSandbox::gvisor(&self.image),
            _ => ContainerSandbox::docker(&self.image),
        };
        let cmd = SandboxCommand {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), wrapped],
            workdir: workdir.to_string(),
            env: Vec::new(),
            limits,
        };
        sandbox
            .execute(&cmd)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))
    }
}

/// The container image for isolated shell runs: `APEX_SANDBOX_IMAGE` or a default.
fn default_sandbox_image() -> String {
    std::env::var("APEX_SANDBOX_IMAGE").unwrap_or_else(|_| DEFAULT_SANDBOX_IMAGE.to_string())
}

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

        // Resolve the isolation backend from the caller's real trust class (SEC-305)
        // against the node's *detected* capabilities (SBX-101). A first-party run
        // resolves to Native (host shell); a verified/untrusted run is floored to
        // Container/Gvisor and runs in an isolated Linux container when the node has
        // one — else selection fails closed (never a silent native downgrade).
        let backend = self
            .manager
            .select(SandboxBackend::Native, ctx.trust_class)
            .map_err(|e| ToolError::PermissionDenied(e.to_string()))?;

        let limits = ResourceLimits {
            timeout: Duration::from_secs(timeout_secs),
            ..ResourceLimits::default()
        };

        let outcome = match backend {
            SandboxBackend::Native => self.run_native(command, shell, workdir, limits).await?,
            SandboxBackend::Container | SandboxBackend::Gvisor => {
                self.run_container(backend, command, shell, workdir, limits)
                    .await?
            }
            other => {
                return Err(ToolError::Internal(format!(
                    "the shell tool cannot run on the `{other}` backend"
                )));
            }
        };

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

/// Default execution timeout for [`CodeExecuteTool`], in seconds.
const CODE_DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum allowed code-execution timeout, in seconds.
const CODE_MAX_TIMEOUT_SECS: u64 = 300;

/// Resolve a declared `language` to its interpreter binary and the source file
/// extension to write the snippet under (some interpreters, notably Python,
/// behave differently invoked as `-c '<code>'` vs. a real `.py` file on disk —
/// writing a real file with the right extension is the more faithful, more
/// debuggable execution mode, and it's what makes a stack trace point at a real
/// path). `python3` is preferred on non-Windows (the standard modern binary
/// name); Windows commonly ships only the bare `python` launcher.
fn interpreter_for(language: &str) -> Result<(&'static str, &'static str), ToolError> {
    match language {
        "python" | "python3" => Ok((if cfg!(windows) { "python" } else { "python3" }, "py")),
        "node" | "javascript" | "js" => Ok(("node", "js")),
        other => Err(ToolError::Validation(format!(
            "unsupported language `{other}` (expected `python` or `node`)"
        ))),
    }
}

/// A process-unique, collision-free file name stem — the same "counter, not just
/// a timestamp" fix `builtin.rs`'s own test `TempDir` needed (coarse clock
/// resolution under concurrent load can otherwise collide).
fn unique_suffix() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Run a source-code snippet in a sandboxed language runtime (SBX-302) — the
/// code-execution counterpart of [`ShellTool`], for a language snippet rather
/// than a raw shell command line. Routed through the identical sandbox backend
/// selection (SBX-101/SEC-305, see [`ShellTool`]'s doc comment): a first-party
/// run executes natively; a verified/untrusted run is floored to a
/// network-isolated container when one is available, else fails closed — never a
/// silent native fallback. Resource limits (timeout, memory, CPU, output cap)
/// apply on every backend; an optional egress allow-list applies on the
/// container path (a native run always has full host network access, the same
/// as `ShellTool`'s native path — isolating a *trusted* first-party run's
/// network was never this tool's job).
///
/// The interpreter must actually exist in the execution environment: the
/// default sandbox image (`alpine:3.20`, [`ShellTool`]'s same default) has
/// neither Python nor Node installed — override via [`Self::with_image`] or
/// `APEX_SANDBOX_IMAGE` to a runtime-appropriate image for container/gVisor
/// runs; a native run uses whatever's on the host `PATH`.
pub struct CodeExecuteTool {
    /// The node's backend capabilities, used to resolve the trust-class floor.
    manager: SandboxManager,
    /// OCI image used when a container/gVisor backend is selected.
    image: String,
}

impl CodeExecuteTool {
    /// A code-execution tool for a **trusted first-party / local** context:
    /// native-only capabilities, so a verified/untrusted run fails closed. Mirrors
    /// [`ShellTool::native_only`].
    pub fn native_only() -> Self {
        Self {
            manager: SandboxManager::native_only(),
            image: default_sandbox_image(),
        }
    }

    /// A code-execution tool driven by the node's **detected** backend
    /// capabilities (SBX-101). Mirrors [`ShellTool::with_manager`].
    pub fn with_manager(manager: SandboxManager) -> Self {
        Self {
            manager,
            image: default_sandbox_image(),
        }
    }

    /// Override the container image (builder-style; default [`DEFAULT_SANDBOX_IMAGE`]).
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }
}

#[async_trait]
impl Tool for CodeExecuteTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "code_execute",
            "1.0.0",
            "system",
            "Run a source-code snippet in a sandboxed language runtime (`python` or \
             `node`) and return its stdout, stderr, and exit code.",
        )
        .with_permissions(["code.execute"])
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["language", "code"],
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python", "node"],
                    "description": "Language runtime to execute the snippet in."
                },
                "code": { "type": "string", "description": "Source code to execute." },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional execution timeout in seconds (default 30, max 300)."
                },
                "network": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Egress allow-list (container/gVisor runs only; a native \
                        run always has full host network access). Omitted or empty means no \
                        outbound network access on an isolated run."
                }
            }
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        let language = request
            .parameters
            .get("language")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::Validation("missing required string field `language`".into())
            })?;
        let code = request
            .parameters
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Validation("missing required string field `code`".into()))?;
        let (interpreter, ext) = interpreter_for(language)?;

        let timeout_secs = request
            .parameters
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(CODE_DEFAULT_TIMEOUT_SECS)
            .min(CODE_MAX_TIMEOUT_SECS);

        let network: Vec<String> = request
            .parameters
            .get("network")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let workdir = if ctx.workdir.is_empty() {
            "."
        } else {
            ctx.workdir.as_str()
        };

        // We pick the file name ourselves (never derived from caller input), so
        // there's no path-confinement concern the way `fs_write`'s caller-supplied
        // path needs — it's always a fresh file directly under `workdir`.
        let file_name = format!(".apex-codeexec-{}.{ext}", unique_suffix());
        let file_path = std::path::Path::new(workdir).join(&file_name);
        tokio::fs::write(&file_path, code)
            .await
            .map_err(|e| ToolError::Internal(format!("could not stage the snippet: {e}")))?;

        // Resolved once as a `SandboxError` throughout (never converted to
        // `ToolError` and back) so the final classification below sees the real
        // failure kind — in particular, a selection failure must surface as
        // `PermissionDenied`, matching `ShellTool`'s behavior, not the generic
        // `Internal` a lossy round-trip through `ToolError` would collapse it to.
        let backend = self.manager.select(SandboxBackend::Native, ctx.trust_class);

        let limits = ResourceLimits {
            timeout: Duration::from_secs(timeout_secs),
            ..ResourceLimits::default()
        };

        let result: Result<CommandOutcome, SandboxError> = match backend {
            Ok(SandboxBackend::Native) => {
                NativeSandbox::with_limits(limits)
                    .run(interpreter, &[file_name.as_str()], workdir)
                    .await
            }
            Ok(backend @ (SandboxBackend::Container | SandboxBackend::Gvisor)) => {
                let network_policy = NetworkPolicy {
                    default_deny: true,
                    outbound_allow: network,
                };
                let sandbox: ContainerSandbox = match backend {
                    SandboxBackend::Gvisor => ContainerSandbox::gvisor(&self.image),
                    _ => ContainerSandbox::docker(&self.image),
                }
                .with_network(network_policy);
                let cmd = SandboxCommand {
                    program: interpreter.to_string(),
                    args: vec![format!("/workspace/{file_name}")],
                    workdir: workdir.to_string(),
                    env: Vec::new(),
                    limits,
                };
                sandbox.execute(&cmd).await
            }
            Ok(other) => Err(SandboxError::Unsupported(other)),
            Err(e) => Err(e),
        };

        // Best-effort cleanup regardless of outcome — never leave the snippet
        // behind in the caller's workspace.
        let _ = tokio::fs::remove_file(&file_path).await;

        let outcome = result.map_err(|e| match e {
            SandboxError::Unsupported(_) => ToolError::PermissionDenied(e.to_string()),
            _ => ToolError::Internal(e.to_string()),
        })?;

        Ok(ToolResponse {
            // A non-zero exit or timeout is reported as success=false so the model
            // can react, while still receiving the captured output.
            success: outcome.exit_code == Some(0) && !outcome.timed_out,
            payload: json!({
                "language": language,
                "exit_code": outcome.exit_code,
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
                "timed_out": outcome.timed_out,
                "truncated": outcome.truncated,
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
        use std::sync::atomic::{AtomicU64, Ordering};

        pub(super) struct TempDir(std::path::PathBuf);
        impl TempDir {
            pub(super) fn new() -> Self {
                // A pid+nanos name alone can collide: `workspace_fixture` makes two
                // `TempDir`s per test and several `fs_read_*` tests run concurrently
                // (`cargo test`'s default multi-threaded runner), and Windows'
                // `SystemTime::now()` resolution is coarser than a nanosecond under
                // load — two calls landing on the same tick get the identical path,
                // so one test's `Drop` deletes the directory the other is still
                // using ("the system cannot find the file specified"). Reproduced
                // live: ~1-in-5 runs of `cargo test -p apex-tools --lib --
                // --test-threads=8`. A process-wide counter guarantees uniqueness
                // regardless of clock resolution.
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let dir = std::env::temp_dir().join(format!(
                    "apex-fs-read-test-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
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

    // --- SBX-301: fs_write confinement -----------------------------------------------

    #[tokio::test]
    async fn fs_write_missing_fields_are_validation_errors() {
        let (root, _outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsWriteTool
            .execute(&ctx, ToolRequest::new(json!({"path": "new.txt"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
        let err = FsWriteTool
            .execute(&ctx, ToolRequest::new(json!({"content": "hi"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn fs_write_creates_a_new_file_inside_the_workspace_root() {
        let (root, _outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let resp = FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": "new.txt", "content": "hello"})),
            )
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(resp.payload["bytes_written"], 5);
        assert_eq!(
            std::fs::read_to_string(root.path().join("new.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn fs_write_overwrites_an_existing_file_by_default() {
        let (root, _outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": "inside.txt", "content": "replaced"})),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("inside.txt")).unwrap(),
            "replaced"
        );
    }

    #[tokio::test]
    async fn fs_write_appends_when_requested() {
        let (root, _outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(
                    json!({"path": "inside.txt", "content": " appended", "append": true}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("inside.txt")).unwrap(),
            "inside content appended"
        );
    }

    #[tokio::test]
    async fn fs_write_denies_dot_dot_traversal_out_of_the_root() {
        let (root, outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
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
        let err = FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": traversal, "content": "pwned"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn fs_write_denies_an_absolute_path_outside_the_root() {
        let (root, outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": outside.to_string_lossy(), "content": "pwned"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_write_denies_a_symlinked_file_that_escapes_the_root() {
        // A symlink planted *inside* the root, pointing to a file *outside* it — the
        // write-specific escape confine_path_for_write's extra symlink check guards
        // against (writing through the link would otherwise silently overwrite the
        // external target).
        let (root, outside) = workspace_fixture();
        let link = root.path().join("escape-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": "escape-link", "content": "pwned"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
        // The external target must be untouched.
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "outside content"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_write_denies_writing_through_a_symlinked_parent_directory() {
        // The parent *directory* itself is a symlink escaping the root — caught by
        // the parent-canonicalization check (the same class of escape `fs_read`
        // already guards against, just applied to the parent instead of the target).
        let (root, outside_dir) = {
            let root = tempfile_dir::TempDir::new();
            let outside = tempfile_dir::TempDir::new();
            let outside_path = outside.path().to_path_buf();
            std::mem::forget(outside); // keep alive; workspace_fixture-style leak
            (root, outside_path)
        };
        let link = root.path().join("escape-dir");
        std::os::unix::fs::symlink(&outside_dir, &link).unwrap();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": "escape-dir/pwned.txt", "content": "pwned"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
        assert!(!outside_dir.join("pwned.txt").exists());
    }

    #[tokio::test]
    async fn fs_write_rejects_a_path_with_no_file_name() {
        let (root, _outside) = workspace_fixture();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        let err = FsWriteTool
            .execute(
                &ctx,
                ToolRequest::new(json!({"path": "..", "content": "x"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)), "{err:?}");
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
        let gateway = Arc::new(Gateway::new(Box::new(apex_provider::MockProvider::new())));
        let t = ImageGenTool::new(gateway);
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
    async fn shell_with_container_capability_routes_verified_run_off_native() {
        // A manager advertising Container capability must NOT fail closed for a
        // verified run the way `native_only` does (SBX-101) — it routes to the
        // container backend. Docker may be absent on this host, so execution then
        // errors as `Internal` (a spawn failure), never `PermissionDenied` (the
        // fail-closed selection error) and never the native host-shell path.
        let manager = SandboxManager::new(
            vec![SandboxBackend::Native, SandboxBackend::Container],
            None,
        );
        let t = ShellTool::with_manager(manager);
        let ctx = ToolContext {
            trust_class: crate::sandbox::TrustClass::Verified,
            ..ToolContext::default()
        };
        let result = t
            .execute(&ctx, ToolRequest::new(json!({"command": "echo ok"})))
            .await;
        match result {
            // No container runtime here → routed to the container backend, which then
            // fails to spawn `docker` (Internal). On a Docker host it would be `Ok`.
            // Either way it did not fail closed and did not run natively.
            Err(ToolError::Internal(_)) | Ok(_) => {}
            other => panic!("expected container routing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_runs_command_and_reports_success() {
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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
        let t = ShellTool::native_only();
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

    // --- SBX-302: code_execute ---------------------------------------------------

    /// Whether `cmd --version` runs at all — the capability gate the `code_execute`
    /// tests below use to skip cleanly on a machine without that language runtime
    /// installed, the same "skip, don't fail" pattern this workspace uses for
    /// Postgres/Docker/wasm-toolchain-gated tests elsewhere.
    fn interpreter_missing(cmd: &str) -> bool {
        match std::process::Command::new(cmd).arg("--version").output() {
            // A non-zero exit isn't necessarily a spawn failure: Windows'
            // "app execution alias" stub for `python`/`python3` spawns
            // successfully but exits non-zero with a "Python was not found;
            // run without arguments to install from the Microsoft Store…"
            // message when no real interpreter is installed behind it — the
            // spawn succeeding is not evidence the interpreter is real.
            Ok(out) => !out.status.success(),
            Err(_) => true,
        }
    }

    #[tokio::test]
    async fn code_execute_missing_fields_are_validation_errors() {
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext::default();
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"code": "print(1)"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
        let err = t
            .execute(&ctx, ToolRequest::new(json!({"language": "python"})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn code_execute_rejects_an_unsupported_language() {
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext::default();
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"language": "ruby", "code": "puts 1"})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)), "{err:?}");
    }

    #[tokio::test]
    async fn code_execute_runs_a_python_snippet_and_captures_stdout_and_exit_code() {
        let interpreter = if cfg!(windows) { "python" } else { "python3" };
        if interpreter_missing(interpreter) {
            eprintln!("skipping: `{interpreter}` not available on this machine");
            return;
        }
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext::default();
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({
                    "language": "python",
                    "code": "print('apex_code_exec_ok')",
                })),
            )
            .await
            .unwrap();
        assert!(resp.success, "{resp:?}");
        assert_eq!(resp.payload["exit_code"], 0);
        assert!(
            resp.payload["stdout"]
                .as_str()
                .unwrap()
                .contains("apex_code_exec_ok")
        );
    }

    #[tokio::test]
    async fn code_execute_runs_a_node_snippet_and_captures_stdout_and_exit_code() {
        if interpreter_missing("node") {
            eprintln!("skipping: `node` not available on this machine");
            return;
        }
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext::default();
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({
                    "language": "node",
                    "code": "console.log('apex_code_exec_ok')",
                })),
            )
            .await
            .unwrap();
        assert!(resp.success, "{resp:?}");
        assert_eq!(resp.payload["exit_code"], 0);
        assert!(
            resp.payload["stdout"]
                .as_str()
                .unwrap()
                .contains("apex_code_exec_ok")
        );
    }

    #[tokio::test]
    async fn code_execute_reports_a_nonzero_exit_code_as_unsuccessful() {
        let interpreter = if cfg!(windows) { "python" } else { "python3" };
        if interpreter_missing(interpreter) {
            eprintln!("skipping: `{interpreter}` not available on this machine");
            return;
        }
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext::default();
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({
                    "language": "python",
                    "code": "import sys; sys.exit(7)",
                })),
            )
            .await
            .unwrap();
        assert!(!resp.success);
        assert_eq!(resp.payload["exit_code"], 7);
    }

    /// SBX-302's resource-limits acceptance bar: a snippet that runs longer than
    /// `timeout_secs` is killed and reported `timed_out` rather than left to run
    /// unbounded.
    #[tokio::test]
    async fn code_execute_timeout_resource_limit_applies() {
        let interpreter = if cfg!(windows) { "python" } else { "python3" };
        if interpreter_missing(interpreter) {
            eprintln!("skipping: `{interpreter}` not available on this machine");
            return;
        }
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext::default();
        let resp = t
            .execute(
                &ctx,
                ToolRequest::new(json!({
                    "language": "python",
                    "code": "import time; time.sleep(30)",
                    "timeout_secs": 1,
                })),
            )
            .await
            .unwrap();
        assert!(!resp.success, "{resp:?}");
        assert_eq!(resp.payload["timed_out"], true);
    }

    #[tokio::test]
    async fn code_execute_cleans_up_the_staged_snippet_file() {
        let interpreter = if cfg!(windows) { "python" } else { "python3" };
        if interpreter_missing(interpreter) {
            eprintln!("skipping: `{interpreter}` not available on this machine");
            return;
        }
        let root = tempfile_dir::TempDir::new();
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext {
            workdir: root.path().to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        t.execute(
            &ctx,
            ToolRequest::new(json!({"language": "python", "code": "print('hi')"})),
        )
        .await
        .unwrap();
        let leftover: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            leftover.is_empty(),
            "the staged snippet must not linger in the workspace: {leftover:?}"
        );
    }

    #[tokio::test]
    async fn code_execute_denies_untrusted_provenance_rather_than_running_natively() {
        let t = CodeExecuteTool::native_only();
        let ctx = ToolContext {
            trust_class: crate::sandbox::TrustClass::Untrusted,
            ..ToolContext::default()
        };
        let err = t
            .execute(
                &ctx,
                ToolRequest::new(json!({"language": "python", "code": "print(1)"})),
            )
            .await
            .unwrap_err();
        // Floored to Gvisor, unsupported by this native-only manager — fails
        // closed rather than silently running natively (SEC-305).
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err:?}");
    }
}
