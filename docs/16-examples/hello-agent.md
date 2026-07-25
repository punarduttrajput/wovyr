<!--
File: docs/16-examples/hello-agent.md
Document ID: EX-001
-->

# Example: Hello Agent

**Document ID:** EX-001  
**File Path:** `docs/16-examples/hello-agent.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Developer Relations Team  
**Last Updated:** 2026-06-27

---

# 1. Goal

Define and run the simplest possible agent — no tools, no memory — to learn the
core loop: **define → run → observe**.

---

# 2. Define

`agents/hello.yaml`:

```yaml
apiVersion: agent.wovyr.io/v1
kind: Agent
metadata:
  name: hello
spec:
  model_selector: { capability: chat, class: fast }
  instructions: |
    You are a friendly assistant. Greet the user and answer briefly.
```

The [model selector](../05-llm-gateway/routing.md#5-model-classes) lets the
[LLM Gateway](../05-llm-gateway/index.md) pick a fast model; no model is pinned.

---

# 3. Run (Local)

```bash
wovyr agents run --local -f agents/hello.yaml \
  --input '{"message":"Hi, who are you?"}' --stream
```

`--stream` shows the run as it happens
([run lifecycle](../09-api/agents.md#6-run-lifecycle--streaming)):

```text
start  · model: <fast model> (provider: ...)
delta  · "Hi! I'm a friendly assistant ..."
done   · tokens: 48, cost_usd: 0.0001
```

---

# 4. Run (Remote)

Publish, then run against a server:

```bash
wovyr agents create -f agents/hello.yaml
ID=$(wovyr agents list -o json | jq -r '.data[]|select(.name=="hello").id')
wovyr agents publish "$ID"
wovyr agents run "$ID" --input '{"message":"Hello!"}'
```

---

# 5. Observe

- **Usage**: every response includes tokens + cost
  ([token management](../05-llm-gateway/token-management.md)).
- **Trace**: the run produces a [trace](../14-observability/tracing.md) (gateway →
  provider) viewable in [Agent Studio](../10-dashboard/agent-studio.md#4-trace--step-inspector).

---

# 6. Next Steps

- Add retrieval → [RAG Agent](rag-agent.md)
- Add tools → [Code Agent](code-agent.md)
- Orchestrate multiple steps → [Customer Support](customer-support.md)

---

# 7. Related Documents

- [`09-api/agents.md`](../09-api/agents.md)
- [`11-cli/commands.md`](../11-cli/commands.md)
- [`16-examples/index.md`](index.md)

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Hello Agent example |
