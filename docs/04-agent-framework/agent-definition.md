<!--
File: docs/04-agent-framework/agent-definition.md
Document ID: AGENT-010
-->

# Agent Definition Specification

**Document ID:** AGENT-010  
**File Path:** `docs/04-agent-framework/agent-definition.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-26

---

# 1. Purpose

The Agent Definition specification describes how an AI agent is declared, configured, versioned, and executed within the Wovyr AI Platform.

An agent is a self-contained execution unit composed of:

- Instructions (system behavior)
- Capabilities (tools, APIs, models)
- Memory access rules
- Policy constraints
- Planning strategy
- Runtime configuration

---

# 2. Objectives

The Agent Definition system shall provide:

- Declarative agent configuration
- Version-controlled agent behavior
- Capability-based execution
- Tool access control
- Memory access boundaries
- Model/provider selection rules
- Runtime isolation
- Multi-agent compatibility

---

# 3. Design Principles

1. Agents are declarative, not imperative.
2. Every agent is versioned.
3. Behavior is reproducible.
4. Agents are stateless between executions (state is externalized).
5. Capabilities are explicitly declared.
6. Security is enforced via policy integration.
7. Agents are composable.

---

# 4. High-Level Structure

```text
Agent Definition
│
├── Identity
├── Instructions
├── Capabilities
├── Tools
├── Memory Access
├── Model Policy
├── Planning Strategy
├── Runtime Constraints
└── Metadata
```

---

# 5. Agent Schema

```yaml
agentId:
name:
version:
description:
owner:
tenantId:
status:
createdAt:
updatedAt:
```

## 5.1 Unknown fields are tolerated here — and only here (VAL-401)

The agent manifest parser (`AgentDefinition`, `crates/wovyr-agent/src/definition.rs`)
deliberately **does not** set `#[serde(deny_unknown_fields)]`. An unrecognized key is
silently ignored rather than rejected.

This is intentional, not an oversight: this document specifies a richer schema than the
implementation currently consumes (see §7–§12), and tolerating the difference lets a
manifest written against the full spec load and run against a build that only
understands part of it.

**It is also the single exception in this codebase.** Every other YAML/JSON DSL here
rejects unknown fields fail-closed:

| Surface | Unknown fields |
|---|---|
| Agent manifest (`AgentDefinition`) | **tolerated** (this section) |
| Workflow DSL (`Definition`) | rejected |
| UI frame protocol (`UiFrame`/`UiNode`, `crates/wovyr-ui/src/frame.rs`) | rejected |
| MCP connections (`McpConnection`, `crates/wovyr-tools/src/mcp_store.rs`) | rejected |
| UI trust policies (`UiPolicy`, `crates/wovyr-ui-guard`) | rejected |

The practical consequence, confirmed by the 2026-07-27 internal red-team assessment:
an attacker-supplied manifest carrying an extra field parses and runs, with that field
ignored. That is not itself an escalation — the ignored key grants nothing — but it does
mean **the manifest is not a place to detect tampering**. Authorization decisions must
come from the fields the runtime actually reads (`spec.tools`, `spec.permissions`,
`spec.mcp_servers`), enforced at the points that read them:

- tool availability — the registry's permission check (SEC-303), which defaults to
  deny-all for a hosted run;
- privileged local tools — the explicit opt-in described in
  [security-isolation §5.2](../07-tool-runtime/security-isolation.md#52-privileged-builtins-need-an-explicit-opt-in-under---local) (SBX-305);
- MCP tools — the per-connection allow-list resolved at run time.

If you need strict manifest validation for your own deployment, validate before
submission; the loader will not do it for you.

---

# 6. Instructions Layer

The instructions define the agent’s behavior.

Example:

```yaml
instructions: |
  You are a software engineering agent.
  Your job is to generate production-grade code,
  follow architecture rules, and ensure correctness.
```

Instructions are injected into the Context Manager.

---

# 7. Capability Model

Capabilities define what an agent can do.

```yaml
capabilities:

  tools:

    - code-generator

    - git

    - docker

  models:

    - gpt-5

    - claude-opus

  memory:

    read: true

    write: true
```

Capabilities are enforced by the Policy Engine.

---

# 8. Tool Binding

Agents explicitly declare allowed tools.

```yaml
tools:

  allowed:

    - filesystem

    - compiler

    - test-runner

  denied:

    - shell
```

Tool access is strictly enforced.

---

# 9. Memory Permissions

Memory access rules:

```yaml
memory:

  working: true

  episodic: true

  semantic: true

  shared: false

  organizational: true
```

---

# 10. Model Policy

Defines model usage rules.

```yaml
modelPolicy:

  preferred: gpt-5

  fallback:

    - claude-sonnet

    - gemini-pro

  maxTokens: 8000

  temperature: 0.2
```

---

# 11. Planning Strategy

```yaml
planning:

  strategy: hierarchical

  maxDepth: 20

  replanning: true
```

---

# 12. Runtime Constraints

```yaml
runtime:

  timeout: 300s

  maxRetries: 3

  maxConcurrency: 10

  region: ap-south-1
```

---

# 13. Security Model

All agents operate under strict security policies:

- No direct secret access
- No raw system access
- No unrestricted network calls
- No filesystem escape
- Policy engine enforcement mandatory

---

# 14. Versioning Strategy

Agents follow semantic versioning:

```text
MAJOR.MINOR.PATCH
```

- Major: breaking behavior changes
- Minor: feature additions
- Patch: bug fixes

---

# 15. Lifecycle

```text
Draft
  ↓
Validated
  ↓
Published
  ↓
Active
  ↓
Deprecated
  ↓
Archived
```

---

# 16. Execution Flow

```text
Agent Request
     ↓
Policy Engine
     ↓
Context Manager
     ↓
Planner
     ↓
Tool Execution
     ↓
Memory System
     ↓
Provider SDK
     ↓
Response
```

---

# 17. Agent Types

| Type | Description |
|------|-------------|
| Stateless | No persistent memory |
| Stateful | Uses memory system |
| Tool-based | Heavy tool usage |
| LLM-only | Pure reasoning agent |
| Multi-agent | Delegates tasks |

---

# 18. Agent Composition

Agents can be composed:

```text
Planner Agent
   ↓
Developer Agent
   ↓
QA Agent
   ↓
Deployment Agent
```

---

# 19. Metadata

```yaml
metadata:

  tags:

    - backend

    - rust

    - ai-agent

  costTier: high

  priority: medium
```

---

# 20. Rust Interface

```rust
pub trait Agent {

    fn id(&self) -> &str;

    fn version(&self) -> &str;

    fn capabilities(&self) -> AgentCapabilities;

    fn execute(&self, input: AgentInput) -> AgentOutput;
}
```

---

# 21. Module Organization

```text
engine-agents/
├── definition/
├── registry/
├── executor/
├── capabilities/
├── runtime/
├── planner/
├── memory/
├── tools/
├── policies/
└── mod.rs
```

---

# 22. Testing Strategy

## Unit Tests

- Capability validation
- Tool restrictions
- Memory access rules
- Model fallback logic

## Integration Tests

- Workflow execution
- Tool framework integration
- Memory system integration
- Provider SDK routing

## Security Tests

- Unauthorized tool access
- Memory leakage prevention
- Policy enforcement validation

---

# 23. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Agent initialization | < 20 ms |
| Capability validation | < 5 ms |
| Execution dispatch | < 10 ms |
| Memory access | < 30 ms |
| Availability | 99.99% |

---

# 24. Dependencies

- `docs/04-agent-framework/policy-engine.md`
- `docs/04-agent-framework/context-manager.md`
- `docs/04-agent-framework/tool-framework.md`
- `docs/04-agent-framework/memory-system.md`

---

# 25. Related Documents

- `docs/04-agent-framework/provider-sdk.md`
- `docs/04-agent-framework/planning-engine.md`
- `docs/04-agent-framework/multi-agent-coordination.md`
- `docs/04-agent-framework/agent-runtime-protocol.md`

---

# 26. Future Enhancements

- Self-modifying agents (governed)
- Reinforcement learning agents
- Federated agent registries
- AI-generated agent definitions
- Cost-aware autonomous agents
- Cross-platform agent portability

---

# 27. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Agent Definition Specification |