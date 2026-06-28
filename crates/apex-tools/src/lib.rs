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
//! Still deferred (per the [roadmap](../../docs/18-roadmap/v0.2.md)): an egress
//! proxy for per-host network allow-listing, a Firecracker guest agent for in-VM
//! execution, warm pooling, distributed execution, and streaming. The `permissions`
//! a tool declares are surfaced but not yet enforced; command-executing tools are
//! gated by an agent's explicit allowed-tools list.

mod builtin;
mod pool;
mod registry;
mod sandbox;
mod tool;

pub use builtin::{EchoTool, FsReadTool, HttpGetTool, ShellTool};
pub use pool::{AutoscalePolicy, PooledSandbox, SandboxFactory, SandboxPool};
pub use registry::ToolRegistry;
#[cfg(feature = "wasi")]
pub use sandbox::WasiSandbox;
pub use sandbox::{
    CommandOutcome, ContainerSandbox, FirecrackerConfig, FirecrackerSandbox, NativeSandbox,
    NetworkPolicy, ResourceLimits, Sandbox, SandboxBackend, SandboxCommand, SandboxError,
    SandboxManager, TrustClass, select_backend,
};
pub use tool::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
