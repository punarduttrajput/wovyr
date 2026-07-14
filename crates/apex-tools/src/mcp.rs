//! MCP (Model Context Protocol) client tool-source (RM-AIM-P3 ECO-301).
//!
//! Connects to an external MCP server, discovers the tools it serves
//! (`initialize` handshake → `tools/list`), and proxies each one into the
//! [`ToolRegistry`](crate::ToolRegistry) as an ordinary [`Tool`] impl — so an
//! agent calls a remote MCP tool exactly like a built-in, and the registry's
//! **permission enforcement applies unchanged**: every proxied tool declares
//! `mcp:<server>` (overridable via [`McpClient::with_tool_permissions`]), and
//! `ToolRegistry::execute` denies an ungranted call *before* anything reaches
//! the wire.
//!
//! Two transports ([`McpTransport`]) ship: [`StdioTransport`] (spawn a child
//! process, newline-delimited JSON-RPC 2.0 over stdin/stdout — the dominant
//! local-server convention) and [`HttpTransport`] (JSON-RPC POSTs to a
//! streamable-HTTP endpoint, echoing the server's `Mcp-Session-Id`; the SSE
//! response mode is deliberately out of scope — a server that answers
//! `text/event-stream` gets a clear error, never a hang or a mangled parse).
//!
//! Proxied tool ids are namespaced `mcp__<server>__<tool>` so a remote tool
//! can never silently shadow a built-in or plugin tool in the registry.
//! Protocol-level failures map onto the standard [`ToolError`] categories the
//! rest of the platform already classifies (`Validation` = permanent,
//! `Network`/`Internal` = retryable), and every request is bounded by a
//! timeout ([`McpClient::with_timeout`], default 30 s) so a hung server can't
//! stall an agent run indefinitely.

use crate::registry::ToolRegistry;
use crate::tool::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// The MCP protocol revision this client speaks.
pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// Default per-request timeout — a hung MCP server fails the call, not the run.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on `tools/list` pagination rounds, so a server that keeps returning a
/// cursor can't spin discovery forever (fail-closed).
const MAX_LIST_PAGES: usize = 64;

/// A JSON-RPC 2.0 transport to an MCP server.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a request and return its `result` (a JSON-RPC `error` maps onto
    /// [`ToolError`] by code — see [`rpc_error_to_tool_error`]).
    async fn request(&self, method: &str, params: Value) -> Result<Value, ToolError>;

    /// Send a notification (no id, no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<(), ToolError>;
}

/// Map a JSON-RPC error object onto the platform's [`ToolError`] categories:
/// request-shaped rejections (`-32600` invalid request / `-32602` invalid
/// params — the code MCP servers answer an unknown tool name with) are
/// permanent `Validation` errors, everything else is a retryable `Internal`.
fn rpc_error_to_tool_error(error: &Value) -> ToolError {
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    match code {
        -32600 | -32602 => ToolError::Validation(format!(
            "MCP server rejected the request ({code}): {message}"
        )),
        _ => ToolError::Internal(format!("MCP server error ({code}): {message}")),
    }
}

/// Pull the `result` out of a JSON-RPC response envelope, mapping an `error`.
fn extract_result(response: Value) -> Result<Value, ToolError> {
    if let Some(error) = response.get("error") {
        return Err(rpc_error_to_tool_error(error));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| ToolError::Internal("JSON-RPC response has neither result nor error".into()))
}

// --- stdio transport --------------------------------------------------------

/// Newline-delimited JSON-RPC over a spawned child process's stdin/stdout —
/// the standard local MCP server transport. One request/response exchange at a
/// time (serialized behind a lock); unsolicited server notifications are
/// skipped and server→client *requests* (sampling etc.) are answered with
/// `method not found` so a compliant server isn't left hanging on us.
pub struct StdioTransport {
    inner: Mutex<StdioInner>,
    next_id: AtomicU64,
    /// Keeps the child alive for the transport's lifetime; `kill_on_drop` reaps
    /// it when the transport goes away.
    _child: std::sync::Mutex<Option<tokio::process::Child>>,
}

struct StdioInner {
    reader: BufReader<Box<dyn AsyncRead + Send + Unpin>>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
}

impl StdioTransport {
    /// Spawn `program args…` as the MCP server, wiring its stdin/stdout. The
    /// child's stderr is inherited (server logs stay visible); it is killed
    /// when the transport drops.
    pub fn spawn(
        program: &str,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    ) -> Result<Self, ToolError> {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ToolError::Network(format!("could not spawn MCP server {program}: {e}"))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ToolError::Internal("spawned MCP server has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Internal("spawned MCP server has no stdout".into()))?;
        let mut transport = Self::from_streams(stdout, stdin);
        transport._child = std::sync::Mutex::new(Some(child));
        Ok(transport)
    }

    /// Build over arbitrary streams — how tests drive the framing with an
    /// in-memory duplex, and how a caller with its own process management
    /// plugs in.
    pub fn from_streams(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        Self {
            inner: Mutex::new(StdioInner {
                reader: BufReader::new(Box::new(reader)),
                writer: Box::new(writer),
            }),
            next_id: AtomicU64::new(1),
            _child: std::sync::Mutex::new(None),
        }
    }
}

impl StdioInner {
    async fn write_message(&mut self, message: &Value) -> Result<(), ToolError> {
        let mut line = message.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ToolError::Network(format!("MCP stdio write failed: {e}")))?;
        self.writer
            .flush()
            .await
            .map_err(|e| ToolError::Network(format!("MCP stdio flush failed: {e}")))
    }

    async fn read_message(&mut self) -> Result<Value, ToolError> {
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| ToolError::Network(format!("MCP stdio read failed: {e}")))?;
            if n == 0 {
                return Err(ToolError::Network(
                    "MCP server closed the connection".into(),
                ));
            }
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(&line).map_err(|e| {
                ToolError::Internal(format!("MCP server sent invalid JSON: {e}: {line}"))
            });
        }
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut inner = self.inner.lock().await;
        inner.write_message(&request).await?;
        loop {
            let message = inner.read_message().await?;
            if message.get("method").is_some() {
                // A server→client request or notification, not our response.
                // Answer a request with `method not found` (this client hosts
                // no server-callable methods) so the server isn't left waiting;
                // skip notifications outright.
                if let Some(server_id) = message.get("id") {
                    let refusal = json!({
                        "jsonrpc": "2.0",
                        "id": server_id,
                        "error": { "code": -32601, "message": "method not supported by this client" }
                    });
                    inner.write_message(&refusal).await?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return extract_result(message);
            }
            // A response to an id we didn't issue in this exchange — requests
            // are serialized, so this is server misbehavior; skip rather than
            // misattribute it.
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), ToolError> {
        let notification = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.inner.lock().await.write_message(&notification).await
    }
}

// --- HTTP transport ----------------------------------------------------------

/// JSON-RPC POSTs to a streamable-HTTP MCP endpoint. Plain-JSON responses
/// only: a server that answers with `text/event-stream` (the spec's optional
/// SSE mode) gets a clear "not supported" error rather than a hang. The
/// server-assigned `Mcp-Session-Id` is captured from any response and echoed
/// on every subsequent request, per the streamable-HTTP session rules.
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    session: std::sync::Mutex<Option<String>>,
    next_id: AtomicU64,
}

impl HttpTransport {
    /// A transport POSTing to `url` (the server's single MCP endpoint).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: url.into(),
            session: std::sync::Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    /// POST one JSON-RPC message; `None` for an empty-body ack (the 202 a
    /// notification gets).
    async fn post(&self, message: &Value) -> Result<Option<Value>, ToolError> {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(message);
        let session = self.session.lock().expect("session lock").clone();
        if let Some(session_id) = session {
            request = request.header("mcp-session-id", session_id);
        }
        let response = request
            .send()
            .await
            .map_err(|e| ToolError::Network(format!("MCP HTTP request failed: {e}")))?;
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().expect("session lock") = Some(session_id.to_string());
        }
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response
            .text()
            .await
            .map_err(|e| ToolError::Network(format!("MCP HTTP body read failed: {e}")))?;
        if !status.is_success() {
            return Err(ToolError::Network(format!(
                "MCP server returned HTTP {status}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        if content_type.starts_with("text/event-stream") {
            return Err(ToolError::Internal(
                "MCP server answered with an SSE stream, which this client does not support — \
                 configure the server for plain JSON responses"
                    .into(),
            ));
        }
        if body.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| ToolError::Internal(format!("MCP server sent invalid JSON: {e}")))
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let response = self.post(&request).await?.ok_or_else(|| {
            ToolError::Internal(format!("MCP server sent no response body to `{method}`"))
        })?;
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(ToolError::Internal(
                "MCP server answered with a mismatched JSON-RPC id".into(),
            ));
        }
        extract_result(response)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), ToolError> {
        let notification = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        // A notification's ack is an empty 202; a body (e.g. an error envelope
        // some servers send anyway) is tolerated and ignored.
        self.post(&notification).await.map(|_| ())
    }
}

// --- client + tool proxy ------------------------------------------------------

/// A discovered MCP tool's identity, as served by `tools/list`.
#[derive(Clone, Debug)]
pub struct McpToolInfo {
    /// The server-side tool name (what `tools/call` takes).
    pub name: String,
    /// Human/model-readable description (may be empty).
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
}

/// A connected MCP server: handshake done, ready to list and call tools.
pub struct McpClient {
    server: String,
    server_version: String,
    transport: Box<dyn McpTransport>,
    timeout: Duration,
    tool_permissions: Vec<String>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server", &self.server)
            .field("server_version", &self.server_version)
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// Connect: run the `initialize` handshake and send
    /// `notifications/initialized`. `server` names this connection — it
    /// namespaces the proxied tool ids (`mcp__<server>__<tool>`) and the
    /// default permission (`mcp:<server>`), so it must be a simple
    /// `[A-Za-z0-9_-]+` identifier (fail-closed otherwise).
    pub async fn connect(
        server: impl Into<String>,
        transport: impl McpTransport + 'static,
    ) -> Result<Self, ToolError> {
        let server = server.into();
        if server.is_empty()
            || !server
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(ToolError::Validation(format!(
                "MCP server name `{server}` must be a non-empty [A-Za-z0-9_-]+ identifier — \
                 it namespaces tool ids and permissions"
            )));
        }
        let mut client = Self {
            tool_permissions: vec![format!("mcp:{server}")],
            server,
            server_version: "0".to_string(),
            transport: Box::new(transport),
            timeout: DEFAULT_REQUEST_TIMEOUT,
        };
        let result = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "apex", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        if let Some(version) = result
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
        {
            client.server_version = version.to_string();
        }
        client
            .transport
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    /// Override the per-request timeout (default 30 s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the permissions every proxied tool declares (default:
    /// `["mcp:<server>"]` — one grant covers the server's whole tool set).
    /// The registry enforces these against the caller's grants; an empty list
    /// makes the server's tools unpermissioned, an operator decision.
    pub fn with_tool_permissions(
        mut self,
        permissions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.tool_permissions = permissions.into_iter().map(Into::into).collect();
        self
    }

    /// The connection's namespace name.
    pub fn server_name(&self) -> &str {
        &self.server
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        tokio::time::timeout(self.timeout, self.transport.request(method, params))
            .await
            .map_err(|_| {
                ToolError::Network(format!(
                    "MCP request `{method}` to `{}` timed out after {:?}",
                    self.server, self.timeout
                ))
            })?
    }

    /// List the server's tools, draining `tools/list` cursor pagination.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, ToolError> {
        let mut infos = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params).await?;
            let tools = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ToolError::Internal("tools/list response has no `tools` array".into())
                })?;
            for tool in tools {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::Internal("MCP tool with no name".into()))?;
                infos.push(McpToolInfo {
                    name: name.to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(String::from);
            if cursor.is_none() {
                return Ok(infos);
            }
        }
        Err(ToolError::Internal(format!(
            "tools/list still paginating after {MAX_LIST_PAGES} pages — refusing to spin"
        )))
    }

    /// Invoke a tool by its server-side name. The MCP result object (content
    /// blocks, `structuredContent`, …) passes through as the payload;
    /// `isError: true` becomes an unsuccessful [`ToolResponse`] — a *tool*
    /// failure the model should see, distinct from a protocol error.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResponse, ToolError> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(ToolResponse {
            success: !is_error,
            payload: result,
        })
    }

    /// Discover the server's tools as registry-ready [`Tool`] impls.
    pub async fn discover_tools(self: &Arc<Self>) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self
            .list_tools()
            .await?
            .into_iter()
            .map(|info| {
                Arc::new(McpTool {
                    client: Arc::clone(self),
                    info,
                }) as Arc<dyn Tool>
            })
            .collect())
    }

    /// Discover and register every served tool, returning the registered ids
    /// (`mcp__<server>__<tool>`). Registration overwrites an existing id, the
    /// registry's standard collision behavior.
    pub async fn register_into(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
    ) -> Result<Vec<String>, ToolError> {
        let mut ids = Vec::new();
        for tool in self.discover_tools().await? {
            ids.push(tool.metadata().id.clone());
            registry.register(tool);
        }
        Ok(ids)
    }
}

/// A registry-resident proxy for one remote MCP tool. Permission checks run in
/// [`ToolRegistry::execute`] — the platform's permission-checked entry point —
/// before `execute` is ever reached.
struct McpTool {
    client: Arc<McpClient>,
    info: McpToolInfo,
}

/// Registry ids must be model-callable function names — map anything outside
/// `[A-Za-z0-9_-]` to `_`.
fn sanitize_id(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[async_trait]
impl Tool for McpTool {
    fn metadata(&self) -> ToolMetadata {
        let description = if self.info.description.is_empty() {
            format!(
                "MCP tool `{}` served by `{}`",
                self.info.name, self.client.server
            )
        } else {
            self.info.description.clone()
        };
        ToolMetadata {
            id: format!(
                "mcp__{}__{}",
                self.client.server,
                sanitize_id(&self.info.name)
            ),
            version: self.client.server_version.clone(),
            category: "mcp".to_string(),
            description,
            permissions: self.client.tool_permissions.clone(),
        }
    }

    fn input_schema(&self) -> Value {
        self.info.input_schema.clone()
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        request: ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        self.client
            .call_tool(&self.info.name, request.parameters)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// Drive a scripted MCP server over an in-memory duplex "stdio" pair:
    /// reads newline-delimited JSON-RPC and answers per `handle`.
    fn scripted_stdio_server(
        handle: impl Fn(&str, u64, &Value) -> Option<Value> + Send + 'static,
    ) -> StdioTransport {
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let message: Value = serde_json::from_str(&line).expect("client sent valid JSON");
                let method = message
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let Some(id) = message.get("id").and_then(Value::as_u64) else {
                    continue; // notification — nothing to answer
                };
                let params = message.get("params").cloned().unwrap_or(json!({}));
                if let Some(result) = handle(&method, id, &params) {
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                    let mut out = response.to_string();
                    out.push('\n');
                    writer.write_all(out.as_bytes()).await.unwrap();
                    writer.flush().await.unwrap();
                }
            }
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        StdioTransport::from_streams(client_read, client_write)
    }

    fn default_handler(method: &str, _id: u64, params: &Value) -> Option<Value> {
        match method {
            "initialize" => Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "scripted", "version": "1.2.3" },
            })),
            "tools/list" => {
                // Two pages, driven by the cursor.
                if params.get("cursor").is_none() {
                    Some(json!({
                        "tools": [{
                            "name": "get_weather",
                            "description": "Current weather for a city",
                            "inputSchema": { "type": "object", "properties": { "city": { "type": "string" } } }
                        }],
                        "nextCursor": "page-2",
                    }))
                } else {
                    Some(json!({
                        "tools": [{ "name": "get_time" }],
                    }))
                }
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let city = params
                    .pointer("/arguments/city")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                Some(json!({
                    "content": [{ "type": "text", "text": format!("{name}: sunny in {city}") }],
                    "isError": false,
                }))
            }
            _ => Some(json!({})),
        }
    }

    #[tokio::test]
    async fn stdio_handshake_paginated_list_and_call_round_trip() {
        let transport = scripted_stdio_server(default_handler);
        let client = Arc::new(McpClient::connect("weather", transport).await.unwrap());

        // Pagination drained: both pages' tools present, in order.
        let tools = client.list_tools().await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["get_weather", "get_time"]
        );

        let response = client
            .call_tool("get_weather", json!({ "city": "Pune" }))
            .await
            .unwrap();
        assert!(response.success);
        assert_eq!(
            response.payload.pointer("/content/0/text").unwrap(),
            "get_weather: sunny in Pune"
        );
    }

    #[tokio::test]
    async fn proxied_tool_metadata_is_namespaced_and_permissioned() {
        let transport = scripted_stdio_server(default_handler);
        let client = Arc::new(McpClient::connect("weather", transport).await.unwrap());
        let tools = client.discover_tools().await.unwrap();
        let meta = tools[0].metadata();
        assert_eq!(meta.id, "mcp__weather__get_weather");
        assert_eq!(meta.category, "mcp");
        assert_eq!(meta.version, "1.2.3"); // the server's declared version
        assert_eq!(meta.permissions, vec!["mcp:weather".to_string()]);
        assert_eq!(
            tools[0].input_schema().pointer("/properties/city/type"),
            Some(&json!("string"))
        );
        // A tool the server described with no description gets a synthesized one.
        assert!(tools[1].metadata().description.contains("get_time"));
    }

    #[tokio::test]
    async fn unsolicited_notifications_and_server_requests_do_not_derail_a_response() {
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let message: Value = serde_json::from_str(&line).unwrap();
                let Some(id) = message.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                if message.get("method").is_none() {
                    continue; // the client's method-not-found reply to our request
                }
                // Before every response: a notification AND a server→client
                // request, both of which the client must skip past.
                let noise = format!(
                    "{}\n{}\n",
                    json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": {} }),
                    json!({ "jsonrpc": "2.0", "id": 999, "method": "sampling/createMessage", "params": {} }),
                );
                writer.write_all(noise.as_bytes()).await.unwrap();
                let result = match message.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        json!({ "serverInfo": { "name": "noisy", "version": "0" } })
                    }
                    _ => json!({ "tools": [] }),
                };
                let mut out = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                out.push('\n');
                writer.write_all(out.as_bytes()).await.unwrap();
                writer.flush().await.unwrap();
            }
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        let transport = StdioTransport::from_streams(client_read, client_write);
        let client = Arc::new(McpClient::connect("noisy", transport).await.unwrap());
        assert!(client.list_tools().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn json_rpc_errors_map_onto_tool_error_categories() {
        // -32602 (how servers answer an unknown tool) → permanent Validation.
        assert!(matches!(
            rpc_error_to_tool_error(&json!({ "code": -32602, "message": "unknown tool" })),
            ToolError::Validation(_)
        ));
        // Anything else → retryable Internal.
        assert!(matches!(
            rpc_error_to_tool_error(&json!({ "code": -32603, "message": "boom" })),
            ToolError::Internal(_)
        ));

        // End to end: a server that answers `initialize` with a JSON-RPC error
        // envelope fails connect cleanly (never a hang or a mangled parse).
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let id = serde_json::from_str::<Value>(&line).unwrap()["id"].clone();
            let mut out = json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32603, "message": "server exploded" }
            })
            .to_string();
            out.push('\n');
            writer.write_all(out.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        let transport = StdioTransport::from_streams(client_read, client_write);
        let err = McpClient::connect("dead", transport).await.unwrap_err();
        assert!(
            matches!(&err, ToolError::Internal(m) if m.contains("server exploded")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn call_tool_maps_is_error_to_an_unsuccessful_response() {
        let transport = scripted_stdio_server(|method, _id, _params| match method {
            "initialize" => Some(json!({ "serverInfo": { "name": "s", "version": "0" } })),
            "tools/call" => Some(json!({
                "content": [{ "type": "text", "text": "the API key is invalid" }],
                "isError": true,
            })),
            _ => Some(json!({})),
        });
        let client = Arc::new(McpClient::connect("s", transport).await.unwrap());
        let response = client.call_tool("anything", json!({})).await.unwrap();
        // A tool-level failure is a result the model should see, not a ToolError.
        assert!(!response.success);
        assert_eq!(
            response.payload.pointer("/content/0/text").unwrap(),
            "the API key is invalid"
        );
    }

    #[tokio::test]
    async fn server_names_are_validated_fail_closed() {
        for bad in ["", "has space", "dot.dot", "semi;colon"] {
            let transport = scripted_stdio_server(default_handler);
            let err = McpClient::connect(bad, transport).await.unwrap_err();
            assert!(matches!(err, ToolError::Validation(_)), "{bad}: {err}");
        }
    }

    #[tokio::test]
    async fn runaway_pagination_is_refused() {
        let transport = scripted_stdio_server(|method, _id, _params| match method {
            "initialize" => Some(json!({ "serverInfo": { "name": "s", "version": "0" } })),
            // Always another cursor — a spin the client must refuse.
            "tools/list" => Some(json!({ "tools": [], "nextCursor": "again" })),
            _ => Some(json!({})),
        });
        let client = Arc::new(McpClient::connect("s", transport).await.unwrap());
        let err = client.list_tools().await.unwrap_err();
        assert!(err.to_string().contains("refusing to spin"), "{err}");
    }

    #[test]
    fn tool_ids_are_sanitized_to_model_callable_names() {
        assert_eq!(sanitize_id("get_weather"), "get_weather");
        assert_eq!(sanitize_id("files.read/write"), "files_read_write");
    }
}
