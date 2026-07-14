//! ECO-301 acceptance: connect to a mock MCP server over HTTP, list its tools,
//! and invoke one **through `ToolRegistry`** — with the registry's permission
//! enforcement proven to deny an ungranted call before anything reaches the
//! wire.
//!
//! The mock server is a hand-rolled HTTP/1.1 loop over a `TcpListener` (one
//! request per connection, `Connection: close`) speaking the streamable-HTTP
//! plain-JSON mode: JSON-RPC POSTs answered with `application/json`, a
//! notification acknowledged `202` with no body, and an `Mcp-Session-Id`
//! assigned at `initialize` that every later request must echo.

use apex_tools::{HttpTransport, McpClient, ToolContext, ToolRegistry, ToolRequest};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the mock server saw: JSON-RPC method names, in arrival order.
type SeenLog = Arc<Mutex<Vec<String>>>;

/// Serve the mock MCP endpoint; returns its URL and the request log.
async fn spawn_mock_mcp_server() -> (String, SeenLog) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen: SeenLog = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                // Read headers + body (one request per connection).
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (headers, mut body) = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = find_header_end(&buf) {
                        let headers = String::from_utf8_lossy(&buf[..end]).to_string();
                        break (headers, buf[end + 4..].to_vec());
                    }
                };
                let content_length: usize = headers
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .map(String::from)
                    })
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while body.len() < content_length {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    body.extend_from_slice(&chunk[..n]);
                }
                let message: Value = serde_json::from_slice(&body).expect("valid JSON-RPC body");
                let method = message
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                log.lock().unwrap().push(method.clone());

                let has_session = headers
                    .lines()
                    .any(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:"));

                let response = match (method.as_str(), message.get("id")) {
                    // A notification: 202, no body (the streamable-HTTP ack).
                    (_, None) => http_response(202, None, false),
                    ("initialize", Some(id)) => http_response(
                        200,
                        Some(json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": {
                                "protocolVersion": "2025-03-26",
                                "capabilities": { "tools": {} },
                                "serverInfo": { "name": "mock-mcp", "version": "9.9.9" },
                            }
                        })),
                        true, // assign the session id here
                    ),
                    // Every post-initialize request must echo the session id.
                    (_, Some(id)) if !has_session => http_response(
                        200,
                        Some(json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32600, "message": "missing Mcp-Session-Id" }
                        })),
                        false,
                    ),
                    ("tools/list", Some(id)) => http_response(
                        200,
                        Some(json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "tools": [
                                {
                                    "name": "lookup_order",
                                    "description": "Look up an order by id",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": { "order_id": { "type": "string" } },
                                        "required": ["order_id"]
                                    }
                                }
                            ] }
                        })),
                        false,
                    ),
                    ("tools/call", Some(id)) => {
                        let order = message
                            .pointer("/params/arguments/order_id")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        http_response(
                            200,
                            Some(json!({
                                "jsonrpc": "2.0", "id": id,
                                "result": {
                                    "content": [{ "type": "text", "text": format!("order {order}: shipped") }],
                                    "isError": false,
                                }
                            })),
                            false,
                        )
                    }
                    (_, Some(id)) => http_response(
                        200,
                        Some(json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32601, "message": "method not found" }
                        })),
                        false,
                    ),
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    (format!("http://{addr}/mcp"), seen)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn http_response(status: u16, body: Option<Value>, assign_session: bool) -> String {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        _ => "Error",
    };
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let session = if assign_session {
        "Mcp-Session-Id: sess-mock-1\r\n"
    } else {
        ""
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn ctx(granted: Option<&[&str]>) -> ToolContext {
    ToolContext {
        granted_permissions: granted.map(|g| g.iter().map(ToString::to_string).collect()),
        ..ToolContext::default()
    }
}

#[tokio::test]
async fn discovers_and_invokes_an_mcp_tool_through_the_registry() {
    let (url, seen) = spawn_mock_mcp_server().await;
    let client = Arc::new(
        McpClient::connect("orders", HttpTransport::new(&url))
            .await
            .expect("handshake"),
    );

    // Discovery registers the proxied tool under its namespaced id.
    let mut registry = ToolRegistry::with_builtins();
    let ids = client.register_into(&mut registry).await.expect("discover");
    assert_eq!(ids, vec!["mcp__orders__lookup_order".to_string()]);
    assert!(registry.contains("mcp__orders__lookup_order"));

    // The proxied metadata is discoverable like any tool's.
    let meta = registry
        .get("mcp__orders__lookup_order")
        .expect("registered")
        .metadata();
    assert_eq!(meta.category, "mcp");
    assert_eq!(meta.version, "9.9.9");
    assert_eq!(meta.permissions, vec!["mcp:orders".to_string()]);

    // Invoke through the registry with the permission granted.
    let response = registry
        .execute(
            "mcp__orders__lookup_order",
            &ctx(Some(&["mcp:orders"])),
            ToolRequest::new(json!({ "order_id": "A-42" })),
        )
        .await
        .expect("call succeeds");
    assert!(response.success);
    assert_eq!(
        response.payload.pointer("/content/0/text").unwrap(),
        "order A-42: shipped"
    );

    // The wire saw the full protocol sequence — incl. the session id echoed
    // after initialize (the mock rejects any sessionless follow-up).
    let methods = seen.lock().unwrap().clone();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call"
        ]
    );
}

#[tokio::test]
async fn ungranted_call_is_denied_before_anything_reaches_the_wire() {
    let (url, seen) = spawn_mock_mcp_server().await;
    let client = Arc::new(
        McpClient::connect("orders", HttpTransport::new(&url))
            .await
            .expect("handshake"),
    );
    let mut registry = ToolRegistry::new();
    client.register_into(&mut registry).await.expect("discover");
    let calls_after_discovery = seen.lock().unwrap().len();

    // A caller granted an unrelated permission is denied fail-closed…
    let err = registry
        .execute(
            "mcp__orders__lookup_order",
            &ctx(Some(&["net.egress"])),
            ToolRequest::new(json!({ "order_id": "A-42" })),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("mcp:orders"), "{err}");

    // …and the denial happened *before* the proxy touched the network: the
    // mock server saw no `tools/call`.
    let methods = seen.lock().unwrap().clone();
    assert_eq!(methods.len(), calls_after_discovery, "{methods:?}");
    assert!(!methods.iter().any(|m| m == "tools/call"));
}
