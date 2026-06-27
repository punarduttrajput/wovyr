<!--
File: docs/10-dashboard/agent-studio.md
Document ID: DASH-003
-->

# Agent Studio

**Document ID:** DASH-003  
**File Path:** `docs/10-dashboard/agent-studio.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies **Agent Studio** — the workspace for designing, testing, observing, and publishing agents. It is the visual front end over the [Agent Definition](../04-agent-framework/agent-definition.md) model and the [Agents API](../09-api/agents.md).

---

# 2. Agent Designer

A form-driven editor for the [agent definition](../09-api/agents.md#4-agent-definition):

| Section | Configures |
|---------|-----------|
| Identity | Name, description |
| Model | [Model selector](../05-llm-gateway/routing.md#4-selection-strategies) (capability/class/strategy) or pinned model |
| Instructions | System prompt / behavior |
| Tools | Attach tools from the [Tools API](../09-api/tools.md) (only those the project enables) |
| Memory | [Memory scopes](../06-memory-engine/memory-api.md#10-scopes--sharing) + toggles |
| Policies | Attach [policies](../04-agent-framework/policy-engine.md) (e.g. PII guard) |
| Budget | Default cost/token ceilings |

Tool and policy pickers show only resources the user is authorized to use.

---

# 3. Test Console

An interactive console to run the agent before publishing:

```text
Input ─► :run (draft version) ─► live stream
                                   ├─ planner steps
                                   ├─ tool calls (inputs/outputs)
                                   ├─ model deltas
                                   └─ memory reads/writes
```

The console streams the run via
[Agents API §6](../09-api/agents.md#6-run-lifecycle--streaming), exposing each step
so authors can see *why* the agent did what it did.

---

# 4. Trace & Step Inspector

For any run, the inspector shows the full execution trace:

- Planner reasoning and chosen plan
- Each tool invocation with arguments, result, duration, and cost
- Each model call with the [routing decision](../05-llm-gateway/routing.md#11-observability),
  tokens, and cost
- Memory retrievals with [score breakdowns](../06-memory-engine/ranking.md#9-output)

This makes agent behavior debuggable rather than opaque.

---

# 5. Sessions & Multi-Turn

Authors can test multi-turn behavior using
[sessions](../09-api/agents.md#7-sessions): the studio preserves conversation
context and (optionally) sticky model routing across turns.

---

# 6. Evaluation (Planned Integration)

- Run an agent against a set of test cases / golden outputs.
- Compare versions side by side (quality, cost, latency).
- Track regressions before publishing.

This ties into the planned [Testing](../SUMMARY.md) section and AI evaluation
service.

---

# 7. Versioning & Publish

- Edits create a draft; **publish** produces an immutable
  [agent_version](../09-api/agents.md#8-versioning--publishing).
- A version diff highlights changes to instructions, tools, model, and policies.
- Running agents and sessions continue on their start version.

---

# 8. Cost & Usage Preview

Before publishing, the studio estimates per-run cost from the configured model and
typical token usage (using
[LLM Gateway pricing](../05-llm-gateway/token-management.md#4-cost-computation)),
and shows actuals from test runs.

---

# 9. Templates

Start from agent templates (support agent, RAG assistant, code agent — see planned
[Examples](../SUMMARY.md)) and customize.

---

# 10. Governance

- Attaching tools/policies/memory respects the user's scopes and project enablement.
- Publishing requires `agents:write`; running tests requires `agents:run`.
- All actions are audited via the API.

---

# 11. Dependencies

- [`04-agent-framework/agent-definition.md`](../04-agent-framework/agent-definition.md)
- [`09-api/agents.md`](../09-api/agents.md)
- [`05-llm-gateway/routing.md`](../05-llm-gateway/routing.md)
- [`09-api/tools.md`](../09-api/tools.md)

---

# 12. Related Documents

- [`10-dashboard/overview.md`](overview.md)
- [`10-dashboard/workflow-builder.md`](workflow-builder.md)
- [`10-dashboard/memory-explorer.md`](memory-explorer.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Agent Studio specification |
