<!--
File: docs/04-agent-framework/provider-sdk.md
Document ID: AGENT-006
-->

# Provider SDK Specification

**Document ID:** AGENT-006  
**File Path:** `docs/04-agent-framework/provider-sdk.md`  
**Version:** 1.1.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-13

---

# 1. Purpose

The Provider SDK abstracts AI model providers behind a common interface, allowing the Apex AI Platform to switch between LLM providers without changing business logic.

The SDK eliminates vendor lock-in by providing a unified API for:

- Chat Completion
- Text Generation
- Function Calling
- Structured Output
- Embeddings
- Image Generation
- Speech
- Moderation
- Fine-Tuning
- Batch Inference
- Streaming

---

# 2. Objectives

The Provider SDK shall provide:

- Unified provider interface
- Runtime provider selection
- Model capability discovery
- Automatic failover
- Cost optimization
- Rate limiting
- Retry handling
- Token accounting
- Streaming support
- Multi-provider routing

---

# 3. Design Principles

1. Provider-independent APIs.
2. Pluggable adapters.
3. Capability-based routing.
4. Vendor-neutral data models.
5. Automatic retries.
6. Observable execution.
7. Secure credential management.

---

# 4. High-Level Architecture

```text
                    Agent Runtime
                         │
                         ▼
                   Provider SDK
                         │
       ┌─────────────────┼──────────────────┐
       ▼                 ▼                  ▼
 Capability Engine   Provider Router   Token Manager
       │                 │                  │
       └─────────────────┼──────────────────┘
                         ▼
                  Provider Adapter
                         │
 ┌────────────┬──────────┼────────────┬────────────┐
 ▼            ▼          ▼            ▼            ▼
OpenAI    Anthropic    Gemini      Ollama     Azure OpenAI
```

---

# 5. Supported Providers

Initial provider implementations include:

| Provider | Status |
|-----------|--------|
| OpenAI | Supported |
| Azure OpenAI | Supported |
| Anthropic Claude | Supported |
| Google Gemini | Supported |
| Ollama | Supported |
| llama.cpp | Supported |
| HuggingFace | Supported |
| OpenRouter | Supported |
| AWS Bedrock | Planned |
| Mistral | Planned |
| Cohere | Planned |

**Implementation status (2026-07-13).** Two adapters exist in code:
`OpenAiProvider` speaks the OpenAI-compatible `/chat/completions` shape, which is
what makes the Azure OpenAI / Ollama / llama.cpp / HuggingFace (TGI) / OpenRouter /
Gemini-compat rows above work — one adapter, many endpoints, selected via
`APEX_OPENAI_BASE_URL`. `AnthropicProvider` (RM-AIM-P2 PRV-201) speaks Anthropic's
**native Messages API** — first-class `tool_use`/`tool_result` translation,
top-level `system` blocks, prompt caching (`cache_control`, on by default), and
real SSE streaming — selected via `ANTHROPIC_API_KEY` (`Gateway::from_env()` tries
OpenAI first when both keys are set) or the CLI's `--provider anthropic`. A local
in-process model is additionally available via the feature-gated
`MistralRsProvider`. All three real adapters honor the normalized `tool_choice`
(auto / none / required / named tool) and `response_format` (JSON mode / JSON
Schema) constraints on `ChatRequest` (RM-AIM-P2 PRV-202), failing closed on
combinations a backend can't express rather than silently degrading.
Bedrock/Vertex-style prefixed-model routing remains planned.

---

# 6. Provider Abstraction

Each provider implements a common interface.

```text
Agent Runtime

↓

Provider SDK

↓

Provider Adapter

↓

Provider API
```

Business logic never interacts directly with provider-specific SDKs.

---

# 7. Provider Capabilities

Capabilities include:

- Chat
- Completion
- Embeddings
- Function Calling
- Tool Calling
- Vision
- Audio Input
- Audio Output
- Image Generation
- JSON Output
- Streaming

Capability discovery occurs during initialization.

---

# 8. Model Registry

The SDK maintains a model registry.

Example:

```yaml
provider: openai

models:

  - gpt-5

  - gpt-5-mini

  - text-embedding

  - image-model
```

The registry supports dynamic updates.

---

# 9. Model Metadata

```yaml
modelId:
provider:
family:
contextWindow:
maxOutputTokens:
supportsStreaming:
supportsTools:
supportsVision:
supportsJson:
pricing:
status:
```

Metadata enables intelligent routing.

---

# 10. Provider Selection

Selection strategies:

- Explicit provider
- Lowest cost
- Lowest latency
- Highest availability
- Capability match
- Geographic region
- Tenant preference

---

# 11. Automatic Failover

Example:

```text
OpenAI

↓

Failure

↓

Anthropic

↓

Failure

↓

Gemini

↓

Success
```

Failover policies are configurable.

---

# 12. Request Lifecycle

```text
Request

↓

Capability Validation

↓

Provider Selection

↓

Authentication

↓

Request Serialization

↓

API Invocation

↓

Response Parsing

↓

Metrics

↓

Return
```

---

# 13. Streaming Support

Supported streaming:

- Token streaming
- Tool call streaming
- Audio streaming
- Image progress events

Streaming follows a unified event model: `ChatStreamEvent` (AIC-202) —
`Delta(text)` for assistant tokens, `ToolCallDelta { index, id, name, arguments }`
for incremental tool-call-argument fragments as the model composes a call
(`id`/`name` carry the values accumulated so far; the complete call still arrives
in the terminal response, which remains what the agent loop executes),
`ReasoningDelta(text)` for a provider-exposed thinking channel (Anthropic
`thinking_delta`, OpenAI-compatible `delta.reasoning_content` — display-only,
never part of the final message), and a terminal `Done(ChatResponse)`. Audio
streaming and image progress events are not yet implemented.

---

# 14. Function Calling

The SDK normalizes function/tool calling.

Example:

```yaml
function:

  name: search_documents

  parameters:

    query: string
```

Provider-specific formats are hidden.

---

# 15. Structured Output

Supported formats:

- JSON
- JSON Schema
- XML
- YAML
- Protocol Buffers (future)

Validation occurs after response generation.

---

# 16. Embedding Interface

Embedding providers expose:

```rust
embed(text)

embed_batch(texts)

similarity(a, b)
```

The interface is provider-independent.

---

# 17. Token Management

Tracks:

- Prompt tokens
- Completion tokens
- Cached tokens
- Total tokens
- Estimated cost

Supports budgeting and alerts.

---

# 18. Cost Optimization

Optimization strategies:

- Model downgrading
- Prompt compression
- Response caching
- Batch requests
- Multi-provider routing

---

# 19. Rate Limiting

Rate limits apply at:

- Provider
- Tenant
- Organization
- Agent
- User

Backoff strategies are configurable.

---

# 20. Security

Security features:

- Secret references
- API key rotation
- OAuth support
- mTLS
- Audit logging
- Request signing
- PII masking

---

# 21. Rust Interface

```rust
#[async_trait]
pub trait AIProvider {

    async fn chat(
        &self,
        request: ChatRequest,
    ) -> Result<ChatResponse>;

    async fn embed(
        &self,
        request: EmbeddingRequest,
    ) -> Result<EmbeddingResponse>;

    async fn generate_image(
        &self,
        request: ImageRequest,
    ) -> Result<ImageResponse>;
}
```

---

# 22. Module Organization

```text
engine-provider/
├── sdk/
├── router/
├── registry/
├── adapters/
│   ├── openai/
│   ├── anthropic/
│   ├── gemini/
│   ├── ollama/
│   └── azure/
├── embeddings/
├── streaming/
├── security/
├── metrics/
└── mod.rs
```

---

# 23. Testing Strategy

## Unit Tests

- Serialization
- Capability detection
- Routing
- Token accounting

## Integration Tests

- Provider APIs
- Streaming
- Failover
- Authentication

## Performance Tests

- High concurrency
- Large prompts
- Multi-provider routing
- Streaming throughput

---

# 24. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Provider routing | < 5 ms |
| Capability lookup | < 2 ms |
| Token accounting | < 1 ms |
| Failover decision | < 10 ms |
| Availability | 99.99% |

---

# 25. Dependencies

- `docs/04-agent-framework/context-manager.md`
- `docs/04-agent-framework/memory-system.md`
- `docs/04-agent-framework/tool-framework.md`

---

# 26. Related Documents

- `docs/04-agent-framework/agent-definition.md`
- `docs/04-agent-framework/policy-engine.md`
- `docs/05-llm-gateway/index.md`

---

# 27. Future Enhancements

- Intelligent provider benchmarking
- Automatic quality scoring
- Cost-aware response ranking
- Multi-model ensemble inference
- Local GPU scheduling
- Federated provider routing
- Edge inference support

---

# 28. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-26 | Initial Provider SDK Specification |
| 1.1.0 | 2026-07-13 | §13: concrete `ChatStreamEvent` model — tool-call-argument + reasoning deltas (RM-AIM-P2 AIC-202) |