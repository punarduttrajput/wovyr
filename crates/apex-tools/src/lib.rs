//! Tool framework for the Apex AI Platform.
//!
//! Implements the core of the
//! [Tool Framework spec](../../docs/04-agent-framework/tool-framework.md): the
//! [`Tool`] trait (§42), [`ToolMetadata`], [`ToolContext`], request/response
//! types, and a [`ToolRegistry`] (§57). v0.1 ships four native built-in tools —
//! [`EchoTool`], [`FsReadTool`], [`HttpGetTool`], and [`ShellTool`] — running
//! in-process, plus a minimal native-process [`NativeSandbox`] (timeout-enforced).
//!
//! Out of scope for v0.1 (deferred per the [roadmap](../../docs/18-roadmap/v0.1.md)):
//! stronger sandbox isolation (WASI/Docker/microVM), permission *enforcement*,
//! distributed execution, and streaming. The `permissions` a tool declares are
//! surfaced but not yet enforced; command-executing tools are gated only by an
//! agent's explicit allowed-tools list.

mod builtin;
mod registry;
mod sandbox;
mod tool;

pub use builtin::{EchoTool, FsReadTool, HttpGetTool, ShellTool};
pub use registry::ToolRegistry;
pub use sandbox::{
    CommandOutcome, NativeSandbox, NetworkPolicy, ResourceLimits, Sandbox, SandboxBackend,
    SandboxCommand, SandboxError, SandboxManager, TrustClass, select_backend,
};
pub use tool::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
