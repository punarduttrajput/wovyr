<!--
File: docs/16-examples/rag-agent.md
Document ID: EX-002
-->

# Example: RAG Agent

**Document ID:** EX-002  
**File Path:** `docs/16-examples/rag-agent.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Developer Relations Team  
**Last Updated:** 2026-06-27

---

# 1. Goal

Build a **retrieval-augmented** agent that answers from a knowledge base stored in
the [Memory Engine](../06-memory-engine/index.md) — grounding answers in your data.

---

# 2. Create a Knowledge Namespace

`memory/knowledge.yaml`:

```yaml
kind: MemoryNamespace
name: product-kb
project: docs-bot
default_scope: project
embedding_model: text-embedding-3-large
retention: { semantic: permanent }
```

```bash
wovyr memory namespaces create -f memory/knowledge.yaml
```

The [embedding model is fixed per namespace](../06-memory-engine/semantic-memory.md#3-embeddings).

---

# 3. Ingest Knowledge

```bash
wovyr memory put -f - <<'YAML'
scope: project
project: docs-bot
type: semantic
title: Refund policy
content: Refunds are processed within 14 days of purchase.
tags: [policy, refunds]
YAML
```

Long documents are [chunked and embedded](../06-memory-engine/semantic-memory.md#4-chunking)
automatically.

---

# 4. Define the Agent

`agents/docs-bot.yaml`:

```yaml
kind: Agent
metadata: { name: docs-bot }
spec:
  model_selector: { capability: chat, class: balanced }
  instructions: |
    Answer using ONLY the retrieved knowledge. Cite the source title.
    If the answer isn't in memory, say you don't know.
  memory:
    enabled: true
    scopes: [project]
    retrieval: { strategy: hybrid, token_budget: 1500 }
```

Enabling memory makes the runtime [retrieve](../06-memory-engine/retrieval.md) and
[compress](../06-memory-engine/compression.md) relevant context before the model
call.

---

# 5. Run

```bash
wovyr agents run --local -f agents/docs-bot.yaml \
  --input '{"message":"How long do refunds take?"}' --stream
```

Expected: a grounded answer citing "Refund policy", with the retrieved memory and
its [score breakdown](../06-memory-engine/ranking.md#9-output) visible in the trace.

---

# 6. How It Works

```text
question → embed → hybrid retrieve (vector+keyword) → rank → compress
        → prompt (instructions + retrieved memory + question) → model → answer
```

Retrieval and ranking happen in the Memory Engine; the agent runtime assembles the
prompt via the [Context Manager](../04-agent-framework/context-manager.md).

---

# 7. Verify Grounding

- Ask something **not** in the KB → the agent should say it doesn't know.
- Inspect the trace to confirm which memories were retrieved and used
  ([Memory Explorer](../10-dashboard/memory-explorer.md)).

---

# 8. Next Steps

- Add tools to act on answers → [Code Agent](code-agent.md)
- Wrap in an approval flow → [Customer Support](customer-support.md)

---

# 9. Related Documents

- [`06-memory-engine/retrieval.md`](../06-memory-engine/retrieval.md)
- [`06-memory-engine/semantic-memory.md`](../06-memory-engine/semantic-memory.md)
- [`16-examples/index.md`](index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial RAG Agent example |
