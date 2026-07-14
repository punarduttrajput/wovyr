<!--
File: docs/03-workflow-engine/agent-runtime.md
Document ID: WF-013
-->

# Agent Runtime Specification

**Document ID:** WF-013  
**File Path:** `docs/03-workflow-engine/agent-runtime.md`  
**Version:** 1.2.0  
**Status:** Draft  
**Owner:** Workflow Engine Team  
**Last Updated:** 2026-07-14

---

# 1. Purpose

This document defines the Agent Runtime responsible for executing AI-powered agents inside the Apex Workflow Engine.

The Agent Runtime is the execution environment that enables autonomous and semi-autonomous agents to participate in workflow execution while maintaining deterministic orchestration.

Unlike a traditional LLM wrapper, the Agent Runtime is a complete execution platform responsible for:

- Agent lifecycle
- Context management
- Tool execution
- Memory
- Planning
- Reasoning
- Multi-agent collaboration
- Human-in-the-loop interactions
- Workflow integration

---

# 2. Goals

The Agent Runtime shall provide:

- Pluggable AI providers
- Deterministic workflow integration
- Long-running conversations
- Secure tool execution
- Persistent memory
- Agent isolation
- Distributed execution
- Event-driven communication
- Full observability

---

# 3. Scope

The runtime is responsible for:

- Agent creation
- Agent scheduling
- Prompt execution
- Context loading
- Tool orchestration
- Memory retrieval
- Planning
- Reflection
- Conversation persistence
- Result generation

The runtime is **not** responsible for:

- Workflow scheduling
- DAG execution
- Checkpoint persistence
- Distributed coordination

Those responsibilities belong to other engine components.

---

# 4. High-Level Architecture

```text
                        Workflow Engine
                               │
                               ▼
                       Agent Runtime
                               │
     ┌───────────────┬──────────┼───────────────┬───────────────┐
     ▼               ▼          ▼               ▼
 Planner        Context Manager Memory      Tool Executor
     │               │          │               │
     └───────────────┼──────────┼───────────────┘
                     ▼
              LLM Provider Layer
                     │
     ┌───────────────┼───────────────────────────────┐
     ▼               ▼               ▼               ▼
  OpenAI         Anthropic      Local LLM      Azure OpenAI
```

---

# 5. Runtime Components

| Component | Responsibility |
|------------|----------------|
| Agent Manager | Agent lifecycle |
| Planner | Task decomposition |
| Context Manager | Prompt assembly |
| Memory Manager | Long-term memory |
| Tool Executor | Tool invocation |
| Reflection Engine | Self-evaluation |
| Conversation Manager | Chat history |
| Policy Engine | Guardrails |
| Provider Adapter | AI model abstraction |

---

# 6. Agent Lifecycle

```text
Created

↓

Initialized

↓

Context Loaded

↓

Planning

↓

Executing

↓

Waiting

↓

Resumed

↓

Completed

↓

Archived
```

---

# 7. Agent States

| State | Description |
|---------|-------------|
| Created | Agent registered |
| Initialized | Runtime initialized |
| Planning | Creating execution plan |
| Executing | Running tasks |
| Waiting | Awaiting input |
| Resumed | Continuing execution |
| Completed | Finished successfully |
| Failed | Execution failed |
| Cancelled | User cancelled |

---

# 8. Agent Definition

Example:

```yaml
agent:

  id: contract-generator

  model: gpt-5

  systemPrompt: |
      You are a senior Solidity engineer.

  temperature: 0.2

  maxTokens: 16000

  memory:
      enabled: true

  tools:
      - rust_compiler
      - solidity_generator
      - filesystem
```

---

# 9. Runtime Lifecycle

```text
Workflow Activity

↓

Create Agent

↓

Load Context

↓

Load Memory

↓

Plan

↓

Execute

↓

Call Tools

↓

Generate Response

↓

Persist State

↓

Return Result
```

**Loop recovery (AIC-201).** The run loop bounds model/tool iterations by a step
budget (`RunOptions::max_steps` → manifest `spec.max_steps` → built-in default) and
always tries to end with an answer rather than an error:

- A **transient** model-step failure (`Error::Provider` — e.g. a stream that errors
  or truncates mid-flight, which the gateway's per-call retry cannot cover) re-issues
  the step up to `RunOptions::step_retries` times (default 2). Each re-issue passes
  back through the gateway's full retry/failover/circuit-breaker pipeline, which owns
  backoff pacing. Permanent errors (`Invalid`/`Config`/…) abort immediately.
- The **last budgeted step** advertises no tools and injects a system instruction to
  answer from the information already gathered — so a run that spent its budget on
  tool calls returns a final answer within `max_steps` model calls instead of a
  `Runtime` error. A zero budget, or a provider that returns tool calls despite none
  being advertised, still fails with the budget error.

---

# 10. Context Management

Context is assembled from:

- System prompts
- User instructions
- Workflow variables
- Memory
- Previous messages
- Retrieved documents
- Tool results

The Context Manager optimizes token usage before model invocation.

---

# 11. Memory Model

Memory layers:

```text
Working Memory

↓

Conversation Memory

↓

Workflow Memory

↓

Long-Term Memory

↓

Knowledge Store
```

Each layer has different retention policies and retrieval strategies.

---

# 12. Planning Engine

The Planner decomposes complex objectives into executable tasks.

Example:

```text
User Goal

↓

Analyze Goal

↓

Create Task Graph

↓

Assign Tools

↓

Execute

↓

Validate Output
```

Planning is deterministic within the workflow context.

---

# 13. Tool Execution

The Tool Executor manages all external interactions.

Supported tool categories:

- File System
- Web Search
- Database
- Rust Compiler
- Docker
- Kubernetes
- Git
- REST APIs
- GraphQL APIs
- Blockchain Nodes
- Vector Databases
- Email
- Slack
- GitHub

Tool execution is sandboxed and audited.

---

# 14. Tool Invocation Flow

```text
LLM Response

↓

Tool Request

↓

Permission Check

↓

Execute Tool

↓

Validate Result

↓

Return Output

↓

Continue Reasoning
```

---

# 15. Multi-Agent Collaboration

Agents may collaborate.

Example:

```text
Project Manager Agent

        │

────────┼────────

│               │

Developer   QA Agent

│               │

────────┼────────

        ▼

 Documentation Agent
```

Communication occurs through structured messages.

---

# 16. Conversation Management

Conversation history includes:

- User messages
- Agent responses
- Tool calls
- Reflection notes
- Errors
- Context snapshots

Conversation history is persisted separately from workflow state.

---

# 17. Reflection Engine

Reflection enables quality improvement.

Execution flow:

```text
Response

↓

Evaluate

↓

Detect Weaknesses

↓

Improve

↓

Finalize
```

Reflection policies are configurable.

---

# 18. Provider Abstraction

Supported providers:

| Provider | Supported |
|------------|-----------|
| OpenAI | Yes |
| Anthropic | Yes |
| Google Gemini | Yes |
| Ollama | Yes |
| llama.cpp | Yes |
| Azure OpenAI | Yes |
| OpenRouter | Yes |
| HuggingFace | Yes |

Provider adapters expose a common interface.

---

# 19. Prompt Management

Prompt components:

- System prompt
- Workflow prompt
- User prompt
- Retrieved context
- Tool output
- Memory
- Policies

Prompt templates are version-controlled.

---

# 20. Human-in-the-Loop

Agents may pause for approval.

Example:

```text
Need Approval

↓

Pause Workflow

↓

Human Decision

↓

Resume Agent
```

The Agent Runtime integrates with the workflow waiting state.

---

# 21. Persistence

Persisted data includes:

```yaml
agentId:
workflowId:
executionId:
conversationId:
currentState:
contextVersion:
memoryReference:
provider:
model:
tokenUsage:
timestamps:
```

---

# 22. Security

The runtime enforces:

- Prompt isolation
- Secret masking
- Tool authorization
- Tenant isolation
- API credential management
- Output validation

Sensitive information is never exposed to unauthorized tools.

**Content-safety guardrails (RM-AIM-P2 SAF-201).** A pluggable `Guardrail`
trait runs on the user's input (before retrieval or any model call) and on the
final answer (before it reaches the caller): each guardrail allows, redacts
(passes a transformed replacement), or blocks (the run fails `Forbidden` —
permanent, never retried). Guardrails are off by default — a run with none
configured behaves exactly as before — and **fail closed**: a guardrail that
errors fails the run rather than admitting unchecked content. When any
configured guardrail checks the output stage, the run buffers streaming (no
raw model deltas reach the sink) and emits the checked final answer as a
single delta, so unchecked content can't leak through the streaming side
channel. Shipped implementations: a keyword blocklist, a dependency-free
PII redactor (emails + long digit runs — a documented heuristic, not a DLP
engine), and an LLM moderator (one schema-constrained gateway call, on its
own gateway to avoid self-moderation bias).

---

# 23. Observability

Metrics:

- Active agents
- Completed agents
- Failed agents
- Average execution time
- Token usage
- Tool invocations
- Context size
- Memory hits
- Reflection success rate

---

# 24. Rust Interfaces

```rust
pub trait AgentRuntime {
    fn create(
        &self,
        definition: AgentDefinition,
    ) -> Result<AgentId>;

    fn execute(
        &self,
        request: AgentRequest,
    ) -> Result<AgentResponse>;

    fn resume(
        &self,
        execution: AgentExecutionId,
    ) -> Result<AgentResponse>;

    fn cancel(
        &self,
        execution: AgentExecutionId,
    ) -> Result<()>;
}
```

---

# 25. Module Organization

```text
engine-agent/
├── runtime/
│   ├── runtime.rs
│   ├── manager.rs
│   ├── lifecycle.rs
│   └── mod.rs
│
├── planner/
│   ├── planner.rs
│   ├── task_graph.rs
│   └── mod.rs
│
├── context/
│   ├── context_manager.rs
│   ├── prompt_builder.rs
│   └── mod.rs
│
├── memory/
│   ├── working_memory.rs
│   ├── conversation_memory.rs
│   ├── long_term_memory.rs
│   └── mod.rs
│
├── tools/
│   ├── executor.rs
│   ├── registry.rs
│   ├── sandbox.rs
│   └── mod.rs
│
├── providers/
│   ├── openai.rs
│   ├── anthropic.rs
│   ├── gemini.rs
│   ├── ollama.rs
│   ├── azure.rs
│   └── mod.rs
│
├── reflection/
│   ├── evaluator.rs
│   └── mod.rs
│
├── conversation/
├── policy/
├── metrics/
└── mod.rs
```

---

# 26. Testing Strategy

## Unit Tests

- Prompt generation
- Context assembly
- Memory retrieval
- Tool execution
- Provider adapters

## Integration Tests

- Workflow integration
- Multi-agent collaboration
- Tool invocation
- Human approval
- Memory persistence

## Performance Tests

- Thousands of concurrent agents
- Large context windows
- High token throughput
- Multi-provider routing

## Chaos Tests

- LLM timeout
- Provider outage
- Tool failure
- Memory corruption
- Network partition

---

# 27. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Agent startup | < 100 ms |
| Context assembly | < 20 ms |
| Tool invocation overhead | < 10 ms |
| Resume after checkpoint | < 500 ms |
| Memory retrieval | < 15 ms |
| Horizontal scalability | Unlimited |

---

# 28. Dependencies

This module depends on:

- `docs/03-workflow-engine/execution-model.md`
- `docs/03-workflow-engine/scheduler.md`
- `docs/03-workflow-engine/state-machine.md`
- `docs/03-workflow-engine/checkpointing.md`
- `docs/03-workflow-engine/event-bus.md`
- `docs/03-workflow-engine/persistence-layer.md`
- `docs/03-workflow-engine/distributed-execution.md`

---

# 29. Related Documents

- `docs/04-agent-framework/agent-definition.md`
- `docs/04-agent-framework/tool-framework.md`
- `docs/04-agent-framework/memory-system.md`
- `docs/04-agent-framework/planning-engine.md`
- `docs/04-agent-framework/provider-sdk.md`

---

# 30. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Agent Runtime Specification |
| 1.1.0 | 2026-07-13 | §9: loop recovery — transient step-error retry + forced final answer on the last budgeted step (RM-AIM-P2 AIC-201) |
| 1.2.0 | 2026-07-14 | §22: pluggable content-safety guardrails — block/redact on input and output, fail-closed, buffered streaming (RM-AIM-P2 SAF-201) |