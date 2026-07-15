//! Stage 1 smoke test: connect Apex's MCP client to a real, locally-spawned
//! `@modelcontextprotocol/server-filesystem` (via `npx`), list its tools, and
//! call one directly — no agent loop yet, just proving the raw MCP round trip
//! against a real external server (not a scripted mock).
//!
//! Run: `cargo run -p apex-agent --example mcp_filesystem_stage1 -- <scratch-dir>`

use apex_tools::{McpClient, StdioTransport};
use serde_json::json;
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: mcp_filesystem_stage1 <allowed-directory>");
    println!("Spawning @modelcontextprotocol/server-filesystem for: {dir}");

    // `npx` resolves to `npx.cmd` on Windows, which `CreateProcess` cannot
    // invoke directly (a well-known Rust std::process::Command limitation) —
    // route it through `cmd.exe /C` there. Elsewhere, `npx` is a real
    // executable and can be spawned directly.
    #[cfg(windows)]
    let transport = StdioTransport::spawn(
        "cmd",
        [
            "/C",
            "npx",
            "-y",
            "@modelcontextprotocol/server-filesystem",
            &dir,
        ],
    )
    .expect("failed to spawn npx via cmd /C");
    #[cfg(not(windows))]
    let transport = StdioTransport::spawn(
        "npx",
        ["-y", "@modelcontextprotocol/server-filesystem", &dir],
    )
    .expect("failed to spawn npx");
    let client = Arc::new(
        McpClient::connect("fs", transport)
            .await
            .expect("MCP initialize handshake failed"),
    );
    println!("Connected. Handshake OK.");

    let tools = client.list_tools().await.expect("tools/list failed");
    println!("\nDiscovered {} tools:", tools.len());
    for t in &tools {
        println!("  - {} :: {}", t.name, t.description);
        println!("      schema: {}", t.input_schema);
    }

    let read_tool = tools
        .iter()
        .find(|t| {
            let n = t.name.to_lowercase();
            n.contains("read") && !n.contains("multiple")
        })
        .unwrap_or_else(|| panic!("no single-file read tool found among: {tools:?}"));
    println!("\nUsing read tool: {}", read_tool.name);

    let file_path = format!("{dir}/greeting.txt");
    let response = client
        .call_tool(&read_tool.name, json!({ "path": file_path }))
        .await
        .expect("tools/call failed");

    println!(
        "\ncall_tool(\"{}\") success={}",
        read_tool.name, response.success
    );
    println!(
        "payload: {}",
        serde_json::to_string_pretty(&response.payload).unwrap()
    );
}
