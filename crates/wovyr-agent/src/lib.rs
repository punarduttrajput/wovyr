//! Agent definition and the minimal agent run loop.
//!
//! This crate ties the [provider gateway](wovyr_provider) and
//! [tool registry](wovyr_tools) together into the core agent loop described in the
//! [Agent Runtime spec](../../docs/03-workflow-engine/agent-runtime.md):
//! *load context → call model → (run tools → call model)\* → respond*.
//!
//! Agents are declared declaratively in YAML
//! ([agent definition](../../docs/04-agent-framework/agent-definition.md)); the
//! [`AgentDefinition`] loader accepts the Kubernetes-style manifest used by the
//! [hello agent](../../docs/16-examples/hello-agent.md) example.

mod context;
mod definition;
mod events;
mod guardrail;
mod memory;
mod prompt;
mod runtime;

pub use context::{CompactionStrategy, ContextPolicy};
pub use definition::{AgentDefinition, MemorySpec, Retrieval};
pub use events::{NullSink, RunEvent, RunEventSink};
pub use guardrail::{
    BlocklistGuardrail, Guardrail, GuardrailDecision, GuardrailStage, Guardrails, LlmModerator,
    PiiRedactor,
};
pub use memory::{ContextRetriever, RetrievedContext};
pub use prompt::{AbArm, PromptRegistry, PromptSpec, PromptTemplate, VariableSpec, VariableType};
pub use runtime::{AgentOutput, RunOptions, run_agent, run_agent_with_memory};
