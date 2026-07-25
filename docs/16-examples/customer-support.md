<!--
File: docs/16-examples/customer-support.md
Document ID: EX-004
-->

# Example: Customer Support Workflow

**Document ID:** EX-004  
**File Path:** `docs/16-examples/customer-support.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Developer Relations Team  
**Last Updated:** 2026-06-27

---

# 1. Goal

Build a **multi-step workflow** that triages a support request, drafts a reply with
a RAG agent, and routes high-value refunds to a **human approval** before acting —
combining agents, tools, branching, and human-in-the-loop.

---

# 2. Flow

```text
intake → classify (ai) ──► refund? ──yes──► amount > $100? ──yes──► human approval ──► issue refund (tool)
                                  │                       └─no──► issue refund (tool)
                                  └─no──► draft reply (agent) ──► send email (tool)
```

This uses [conditional branching](../03-workflow-engine/workflow-dsl.md#11-conditional-branching),
a [human task](../03-workflow-engine/workflow-dsl.md#15-human-task), and
[tool](../03-workflow-engine/workflow-dsl.md#16-tool-invocation) +
[ai](../03-workflow-engine/workflow-dsl.md#14-ai-activity) activities.

---

# 3. Workflow Definition (excerpt)

`workflows/support.yaml`:

```yaml
apiVersion: workflow.wovyr.io/v1
kind: Workflow
metadata: { name: support-triage, version: 1.0.0 }
spec:
  input: { ticketId: string, message: string, customerId: string }
  activities:
    - id: classify
      type: ai
      inputs: { prompt: classify_ticket, text: "${input.message}" }
    - id: draftReply
      type: ai
      inputs: { agent: docs-bot, message: "${input.message}" }   # the RAG agent
    - id: approve
      type: human
      assignee: support-lead
      timeout: 24h
    - id: issueRefund
      type: tool
      inputs: { tool: billing.refund, customerId: "${input.customerId}" }
    - id: sendEmail
      type: tool
      inputs: { tool: email.send, template: reply }
  branches:
    - when: "classify.intent == 'refund' && input.amount > 100"
      goto: approve
    - when: "classify.intent == 'refund'"
      goto: issueRefund
    - when: "classify.intent != 'refund'"
      goto: draftReply
  compensation:
    issueRefund: { compensate: billing.reverseRefund }
```

The `draftReply` step reuses the [RAG agent](rag-agent.md); `issueRefund` declares
[compensation](../03-workflow-engine/compensation-engine.md) so a downstream failure
can roll back the refund (saga).

---

# 4. Deploy & Run

```bash
wovyr workflows validate -f workflows/support.yaml
wovyr workflows create -f workflows/support.yaml
WF=$(wovyr workflows list -o json | jq -r '.data[]|select(.name=="support-triage").id')
wovyr workflows publish "$WF"

EXE=$(wovyr workflows run "$WF" --input @ticket.json -o json | jq -r '.execution_id')
wovyr workflows executions get "$EXE" --watch
```

---

# 5. Human Approval

When a refund exceeds $100, the execution **suspends** on the human task. The
support lead approves in the dashboard or via CLI:

```bash
TASK=$(wovyr workflows executions get "$EXE" -o json | jq -r '.pending_task_id')
wovyr workflows tasks complete "$TASK" --decision approved
```

The execution resumes and issues the refund
([human tasks](../09-api/workflows.md#8-human-tasks)).

---

# 6. Durability & Resilience

- The execution is durable — it survives restarts via
  [checkpointing](../03-workflow-engine/checkpointing-specification.md) and can wait
  24h for approval without holding resources.
- If `sendEmail` fails after a refund, [compensation](../03-workflow-engine/compensation-engine.md)
  reverses the refund.

---

# 7. Observe

Watch the execution animate in the
[Workflow Builder](../10-dashboard/workflow-builder.md#7-run--observe); inspect
per-activity inputs/outputs, retries, and the approval decision.

---

# 8. Related Documents

- [`03-workflow-engine/workflow-dsl.md`](../03-workflow-engine/workflow-dsl.md)
- [`09-api/workflows.md`](../09-api/workflows.md)
- [`16-examples/rag-agent.md`](rag-agent.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Customer Support Workflow example |
