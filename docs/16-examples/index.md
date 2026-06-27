<!--
File: docs/16-examples/index.md
Document ID: EX-INDEX-001
-->

# Examples Index

**Document ID:** EX-INDEX-001  
**File Path:** `docs/16-examples/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** Developer Relations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This section provides **worked examples** that combine the platform's building blocks — agents, tools, memory, workflows, plugins — into runnable applications. Each example is end to end: definition, run, and what to observe.

---

# 2. Examples

| Example | Demonstrates |
|---------|--------------|
| [hello-agent.md](hello-agent.md) | A minimal agent: define, run, stream |
| [rag-agent.md](rag-agent.md) | Retrieval-augmented agent over Memory Engine |
| [code-agent.md](code-agent.md) | Tool-using agent (git, shell) in a sandbox |
| [customer-support.md](customer-support.md) | Multi-step workflow + human approval |
| [vpn-agent.md](vpn-agent.md) | Operational agent calling external APIs via a plugin |

---

# 3. Conventions

- Definitions are YAML, runnable via the [CLI](../11-cli/commands.md) or
  [API](../09-api/index.md).
- Examples use a fake/dev provider by default; swap to a real model by changing the
  [model selector](../05-llm-gateway/routing.md#5-model-classes).
- Run locally with `--local` ([CLI local mode](../11-cli/commands.md#10-local-development))
  or against a server.

---

# 4. Prerequisites

```bash
apex login            # or APEX_API_KEY for CI
apex context use <ctx>
apex doctor
```

See [CLI Installation](../11-cli/installation.md) and
[Configuration](../11-cli/configuration.md).

---

# 5. Related Documents

- [`11-cli/examples.md`](../11-cli/examples.md) — CLI recipes
- [`10-dashboard/agent-studio.md`](../10-dashboard/agent-studio.md) — build these visually
- [`04-agent-framework/index.md`](../04-agent-framework/index.md)

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Examples Index |
