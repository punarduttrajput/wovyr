//! Agent definition and the minimal agent run loop.
//!
//! This crate ties the [provider gateway](apex_provider) and
//! [tool registry](apex_tools) together into the core agent loop described in the
//! [Agent Runtime spec](../../docs/03-workflow-engine/agent-runtime.md):
//! *load context → call model → (run tools → call model)\* → respond*.
//!
//! Agents are declared declaratively in YAML
//! ([agent definition](../../docs/04-agent-framework/agent-definition.md)); the
//! [`AgentDefinition`] loader accepts the Kubernetes-style manifest used by the
//! [hello agent](../../docs/16-examples/hello-agent.md) example.

mod definition;
mod events;
mod memory;
mod runtime;

pub use definition::{AgentDefinition, MemorySpec, Retrieval};
pub use events::{NullSink, RunEvent, RunEventSink};
pub use memory::{ContextRetriever, RetrievedContext};
pub use runtime::{AgentOutput, RunOptions, run_agent, run_agent_with_memory};
