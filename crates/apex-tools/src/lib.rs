//! Tool framework for the Apex AI Platform.
//!
//! Implements the core of the
//! [Tool Framework spec](../../docs/04-agent-framework/tool-framework.md): the
//! [`Tool`] trait (§42), [`ToolMetadata`], [`ToolContext`], request/response
//! types, and a [`ToolRegistry`] (§57). It ships four built-in tools —
//! [`EchoTool`], [`FsReadTool`], [`HttpGetTool`], and [`ShellTool`] — plus the
//! sandbox isolation backends: a resource-enforcing [`NativeSandbox`], a
//! [`ContainerSandbox`] (Docker/Podman, and gVisor via `runsc`), a capability-gated
//! [`FirecrackerSandbox`], and (under the `wasi` cargo feature) a `WasiSandbox`
//! that runs `wasm32-wasi` modules in an in-process Wasmtime VM.
//!
//! The allow-listing egress proxy ([`EgressProxy`]) enforces per-host egress at the
//! application layer; the container backend additionally closes the L3 bypass a
//! workload could otherwise use to skip the proxy — a host-side network-namespace
//! `iptables` lockdown (`egress_lockdown`, Linux/Docker-specific) restricts a
//! sandboxed container's `OUTPUT` chain to loopback + the proxy address before the
//! real command ever runs, so ignoring `HTTPS_PROXY` no longer reaches anything.

mod builtin;
mod egress;
mod egress_lockdown;
mod mcp;
mod pool;
mod registry;
mod sandbox;
mod scheduler;
mod tool;

pub use builtin::{EchoTool, FsReadTool, HttpGetTool, ImageGenTool, ShellTool};
pub use egress::EgressProxy;
pub use mcp::{
    HttpTransport, MCP_PROTOCOL_VERSION, McpClient, McpToolInfo, McpTransport, StdioTransport,
};
pub use pool::{AutoscalePolicy, PooledSandbox, SandboxFactory, SandboxPool};
pub use registry::ToolRegistry;
#[cfg(feature = "wasi")]
pub use sandbox::WasiSandbox;
pub use sandbox::{
    CommandOutcome, ContainerSandbox, FirecrackerConfig, FirecrackerSandbox, NativeSandbox,
    NetworkPolicy, ResourceLimits, Sandbox, SandboxBackend, SandboxCommand, SandboxError,
    SandboxManager, TrustClass, select_backend,
};
pub use scheduler::FairScheduler;
pub use tool::{
    Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse, check_permissions,
};
