# RAG agent walkthrough (`docs-bot`)

A retrieval-augmented agent that answers from a knowledge base in the
[Memory Engine](../../docs/06-memory-engine/index.md). It implements the
[RAG agent example](../../docs/16-examples/rag-agent.md): the runtime retrieves
relevant memories and grounds the prompt in them before calling the model.

Memory persists locally under `~/.wovyr/memory/product-kb.jsonl`. No API key is
required — offline, the deterministic mock provider runs the pipeline (keyword
matching drives retrieval since mock embeddings are non-semantic). Set
`OPENAI_API_KEY` (and optionally `WOVYR_OPENAI_BASE_URL`) for real grounded answers.

## 1. Seed the knowledge base

```bash
wovyr memory put --namespace product-kb --tag policy \
  --content "Refunds are processed within 14 days of purchase."
wovyr memory put --namespace product-kb --tag support \
  --content "Support hours are 9am to 5pm, Monday through Friday."
wovyr memory put --namespace product-kb --tag plans \
  --content "The Pro plan includes priority email support and 100GB of storage."
```

## 2. Ask a grounded question

```bash
wovyr agents run --local -f examples/agents/docs-bot.yaml \
  --input '{"message":"How long do refunds take?"}' --stream
```

The stream shows which memories grounded the answer:

```text
start  · model: ... (provider: ...)
memory · [<id>] (score: 0.812)
delta  · "Refunds are processed within 14 days of purchase. [<id>]"
done   · tokens: ..., cost_usd: ...
```

## 3. Confirm grounding (no hallucination)

Ask something absent from the knowledge base — a well-grounded agent declines:

```bash
wovyr agents run --local -f examples/agents/docs-bot.yaml \
  --input '{"message":"What is the office WiFi password?"}' --stream
```

With nothing retrieved, the runtime injects `Retrieved knowledge: (none found...)`
and the agent answers that it doesn't know.

## How it works

```text
question → retrieve (hybrid: vector + keyword via RRF) → rank
        → inject as grounding context → model → grounded answer
```

Retrieval/ranking happen in `wovyr-memory`; the agent runtime injects the result
(see `crates/wovyr-agent/src/runtime.rs`). The agent never depends on the memory
crate directly — it retrieves through the `ContextRetriever` trait, which the CLI
implements over the local `MemoryEngine`.
