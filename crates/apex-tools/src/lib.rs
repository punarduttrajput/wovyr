//! Tool framework for the Apex AI Platform.
//!
//! Implements the core of the
//! [Tool Framework spec](../../docs/04-agent-framework/tool-framework.md): the
//! [`Tool`] trait (§42), [`ToolMetadata`], [`ToolContext`], request/response
//! types, and a [`ToolRegistry`] (§57). v0.1 ships three native built-in tools —
//! [`EchoTool`], [`FsReadTool`], and [`HttpGetTool`] — running in-process.
//!
//! Out of scope for v0.1 (deferred per the [roadmap](../../docs/18-roadmap/v0.1.md)):
//! sandboxing, permissions enforcement, distributed execution, and streaming. The
//! `permissions` a tool declares are surfaced but not yet enforced.

mod builtin;
mod registry;
mod tool;

pub use builtin::{EchoTool, FsReadTool, HttpGetTool};
pub use registry::ToolRegistry;
pub use tool::{Tool, ToolContext, ToolError, ToolMetadata, ToolRequest, ToolResponse};
