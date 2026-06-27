<!--
File: docs/04-agent-framework/context-manager.md
Document ID: AGENT-005
-->

# Context Manager Specification

**Document ID:** AGENT-005  
**File Path:** `docs/04-agent-framework/context-manager.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-26

---

# 1. Purpose

The Context Manager is responsible for constructing, optimizing, securing, and delivering the execution context used by AI agents.

Rather than simply concatenating prompts, the Context Manager intelligently assembles context from multiple sources while respecting model token limits, security policies, tenant boundaries, and workflow state.

It is the "compiler" that converts platform data into an optimized LLM prompt.

---

# 2. Objectives

The Context Manager shall provide:

- Prompt composition
- Context aggregation
- Token optimization
- Memory retrieval
- Workflow context injection
- Policy enforcement
- Multi-model optimization
- Context versioning
- Context replay
- Secure prompt generation

---

# 3. Design Principles

1. Context is immutable after creation.
2. Context generation is deterministic.
3. Sensitive information is masked before prompt generation.
4. Context is versioned.
5. Context supports replay.
6. Token limits are always respected.
7. Context generation is observable.

---

# 4. High-Level Architecture

```text
                     Agent Runtime
                           │
                           ▼
                    Context Manager
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
 Workflow State     Memory Manager      Policy Engine
        │                  │                  │
        └──────────────────┼──────────────────┘
                           ▼
                    Prompt Builder
                           │
                           ▼
                   Token Optimizer
                           │
                           ▼
                      LLM Provider
```

---

# 5. Context Sources

The Context Manager assembles information from:

- System prompts
- Agent definition
- User prompt
- Workflow variables
- Workflow state
- Conversation history
- Working memory
- Episodic memory
- Semantic memory
- Retrieved documents
- Tool outputs
- Policies
- Human feedback

---

# 6. Context Layers

```text
System Prompt

↓

Organization Policies

↓

Agent Instructions

↓

Workflow Context

↓

Conversation History

↓

Retrieved Memory

↓

Tool Results

↓

User Prompt

↓

Execution Metadata
```

Each layer has a defined priority.

---

# 7. Context Object

```yaml
contextId:
workflowId:
agentId:
conversationId:
tenantId:
version:
model:
messages:
variables:
memory:
documents:
metadata:
tokenCount:
createdAt:
```

---

# 8. Prompt Assembly Pipeline

```text
Receive Request

↓

Load Context Sources

↓

Retrieve Memory

↓

Merge Context

↓

Apply Policies

↓

Optimize Tokens

↓

Validate Context

↓

Generate Prompt
```

---

# 9. Prompt Templates

Prompt templates are version-controlled.

Example:

```yaml
template:

  id: code-review

  version: 2.0

  sections:

    - system

    - workflow

    - memory

    - conversation

    - user
```

Templates allow reusable prompt structures.

---

# 10. Token Budgeting

Each context section receives a configurable token budget.

Example:

| Section | Token Budget |
|----------|-------------:|
| System Prompt | 1,500 |
| Workflow | 2,000 |
| Memory | 8,000 |
| Conversation | 10,000 |
| User Prompt | 2,000 |
| Tool Results | 8,000 |

The total budget must not exceed the model's context window.

---

# 11. Context Prioritization

Priority order:

1. System prompt
2. Security policies
3. User request
4. Workflow state
5. Retrieved memory
6. Tool outputs
7. Historical conversation

Lower-priority sections may be truncated when necessary.

---

# 12. Context Compression

Compression strategies include:

- Summarization
- Duplicate removal
- Semantic clustering
- Sliding window
- Importance scoring
- Token-aware trimming

Compression preserves critical information while reducing token usage.

---

# 13. Context Versioning

Every generated context is versioned.

```text
Context

↓

Version 1

↓

Version 2

↓

Version 3
```

Historical contexts support replay and debugging.

---

# 14. Conversation Window Management

Strategies:

- Fixed window
- Sliding window
- Hierarchical summaries
- Semantic recall
- Importance-based retention

The strategy is configurable per agent.

---

# 15. Workflow Context

Workflow context includes:

- Variables
- Current activity
- Execution state
- Checkpoint information
- Previous decisions
- Activity outputs

Workflow context is automatically injected during execution.

---

# 16. Memory Retrieval

Before prompt generation:

```text
User Prompt

↓

Generate Embedding

↓

Vector Search

↓

Rank Results

↓

Filter

↓

Inject Memory
```

Memory retrieval integrates with the Memory System.

---

# 17. Tool Result Injection

Tool outputs are normalized before insertion.

Example:

```yaml
tool:
  id: postgres-query
  status: success
  summary: |
    Retrieved 15 customer records.
```

Large outputs are summarized automatically.

---

# 18. Security Policies

The Context Manager enforces:

- Secret masking
- Prompt injection detection
- PII redaction
- Tenant isolation
- Policy enforcement
- Output filtering

No restricted information is injected into prompts.

---

# 19. Prompt Injection Protection

Detection techniques:

- Rule-based filters
- Policy validation
- Instruction isolation
- Context boundary enforcement
- Tool permission checks

Malicious instructions are ignored or flagged.

---

# 20. Multi-Model Optimization

Different models receive different prompt layouts.

Supported optimizations:

- GPT models
- Claude models
- Gemini models
- Local LLMs
- Reasoning models

Prompt formatting is provider-aware.

---

# 21. Replay Support

Context replay reconstructs historical prompts.

Replay includes:

- Original prompt
- Memory state
- Workflow state
- Tool outputs
- Policies

Replay enables deterministic debugging.

---

# 22. Observability

Metrics:

- Context generation latency
- Token usage
- Compression ratio
- Memory retrieval count
- Cache hit rate
- Prompt size
- Retrieval latency

---

# 23. Rust Interfaces

```rust
pub trait ContextManager {
    fn build_context(
        &self,
        request: ContextRequest,
    ) -> Result<ExecutionContext>;

    fn optimize(
        &self,
        context: ExecutionContext,
    ) -> Result<ExecutionContext>;

    fn validate(
        &self,
        context: &ExecutionContext,
    ) -> Result<()>;
}
```

---

# 24. Module Organization

```text
engine-context/
├── builder/
├── optimizer/
├── templates/
├── retrieval/
├── compression/
├── security/
├── tokenizer/
├── versioning/
├── replay/
├── metrics/
└── mod.rs
```

---

# 25. Testing Strategy

## Unit Tests

- Prompt generation
- Token counting
- Compression
- Template rendering
- Policy enforcement

## Integration Tests

- Memory System integration
- Workflow Runtime integration
- Tool Framework integration
- Provider adapters

## Performance Tests

- Large context windows
- Million-message histories
- High-concurrency prompt generation
- Multi-model optimization

---

# 26. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Context generation | < 50 ms |
| Token optimization | < 20 ms |
| Compression | < 30 ms |
| Memory retrieval | < 30 ms |
| Availability | 99.99% |

---

# 27. Dependencies

- `docs/03-workflow-engine/agent-runtime.md`
- `docs/04-agent-framework/memory-system.md`
- `docs/04-agent-framework/planning-engine.md`
- `docs/04-agent-framework/tool-framework.md`

---

# 28. Related Documents

- `docs/04-agent-framework/agent-definition.md`
- `docs/04-agent-framework/provider-sdk.md`
- `docs/04-agent-framework/policy-engine.md`

---

# 29. Future Enhancements

- Adaptive prompt optimization
- AI-generated prompt templates
- Cross-agent shared context
- Multi-modal context assembly
- Dynamic token budgeting
- Context quality scoring
- Automatic prompt repair

---

# 30. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Context Manager Specification |