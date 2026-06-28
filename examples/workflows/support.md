# Customer-support workflow walkthrough (`support-triage`)

A durable, multi-step workflow that triages a support ticket, routes high-value
refunds to a **human approval**, issues refunds (a `tool`), and drafts replies with
an `ai` step — implementing the
[customer-support example](../../docs/16-examples/customer-support.md). It exercises
conditional branching, a human-in-the-loop **waiting state**, and saga compensation.

```text
triage ─► refund & amount>100  ─► approve (human) ─► [approved] ─► issueRefund (tool)
       ├► refund & amount<=100 ─────────────────────────────────► issueRefund (tool)
       └► not a refund ─► draftReply (ai) ─► sendEmail (tool)
```

Executions persist under `~/.apex/workflows`, so the human task suspends durably:
`run` returns while suspended, and a later `approve` (even a separate process or
after a restart) resumes from the on-disk checkpoint. No API key needed — the `ai`
step uses the deterministic mock provider offline; set `OPENAI_API_KEY` for a real
model.

## Validate

```bash
apex workflows validate -f examples/workflows/support.yaml
```

## A) High-value refund → human approval

```bash
apex workflows run --local -f examples/workflows/support.yaml --id big \
  --input '{"intent":"refund","amount":250,"customerId":"cust-1","message":"want a refund"}'
```

The run **suspends** on `approve` and prints the resume command. Approve it:

```bash
apex workflows approve -f examples/workflows/support.yaml --id big --task approve --decision approved
```

The execution resumes and `issueRefund` completes. Approving with
`--decision rejected` instead skips the refund (the `approve.decision == 'approved'`
guard is false).

## B) Small refund → issued directly

```bash
apex workflows run --local -f examples/workflows/support.yaml --id small \
  --input '{"intent":"refund","amount":40,"customerId":"cust-2","message":"small refund"}'
```

`approve` is **Skipped**; `issueRefund` runs immediately.

## C) Non-refund → AI reply

```bash
apex workflows run --local -f examples/workflows/support.yaml --id q1 \
  --input '{"intent":"question","amount":0,"customerId":"cust-3","message":"how do I reset my password?"}'
```

`draftReply` (ai) → `sendEmail` (tool); the refund branch is skipped.

## Durability & compensation

- The execution survives restarts via the file-backed checkpoint store, so a human
  task can wait indefinitely without holding the process
  ([checkpointing](../../docs/03-workflow-engine/checkpointing-specification.md)).
- `issueRefund` declares `compensate: reverseRefund`. If a later activity fails, the
  engine rolls back completed work in reverse order via the saga
  ([compensation](../../docs/03-workflow-engine/compensation-engine.md)); this is
  covered by `crates/apex-workflow/tests/engine.rs`.
