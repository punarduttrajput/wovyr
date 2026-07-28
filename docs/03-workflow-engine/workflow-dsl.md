# Workflow DSL Specification

**Document ID:** WF-003
**Version:** 1.1.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-07-13

---

# 1. Purpose

This document defines the Wovyr Workflow Domain Specific Language (DSL).

The DSL is the canonical format for describing workflows independent of programming language or execution environment.

The DSL supports:

* Sequential execution
* Parallel execution
* Conditional branching
* Loops
* Human tasks
* AI activities
* Tool invocations
* Event handling
* Scheduling
* Compensation
* Retry policies
* Sub-workflows

---

# 2. Design Goals

The DSL must be:

* Human readable
* Machine writable
* Deterministic
* Versioned
* Extensible
* Schema validated
* IDE friendly

---

# 3. Supported Formats

Authoring formats:

* YAML
* JSON
* Visual Designer Export
* Rust SDK Builder
* AI-generated Workflow

All formats compile into the **Workflow Intermediate Representation (WIR)** before validation and execution.

---

# 4. High-Level Structure

```yaml
apiVersion: workflow.wovyr.io/v1
kind: Workflow

metadata:
  name: invoice-approval
  version: 1.0.0
  description: Invoice approval process

spec:
  input:
  variables:
  activities:
  transitions:
  retry:
  compensation:
```

---

# 5. Metadata

Required fields:

```yaml
metadata:
  name:
  version:
```

Optional fields:

```yaml
metadata:
  description:
  labels:
  annotations:
  owner:
  tags:
```

---

# 6. Inputs

Workflow inputs define external parameters.

Example:

```yaml
input:
  customerId: string
  invoiceAmount: decimal
  currency: string
```

Inputs are immutable throughout execution.

---

# 7. Variables

Variables store workflow state.

Example:

```yaml
variables:
  approved: false
  riskScore: 0
  summary: ""
```

Variables are mutable and persisted after each state transition.

---

# 8. Activity Definition

Every activity contains:

```yaml
id:
type:
name:
inputs:
outputs:
timeout:
retry:
```

The `id` must be unique within a workflow.

---

# 9. Activity Types

Supported activity types:

| Type       | Description              |
| ---------- | ------------------------ |
| function   | Execute Rust code        |
| http       | REST request             |
| grpc       | gRPC call                |
| ai         | LLM inference            |
| agent      | Run a stored agent       |
| tool       | Registered platform tool |
| script     | Sandboxed script         |
| human      | Human approval           |
| event      | Wait for external event  |
| timer      | Delay execution          |
| subprocess | Invoke another workflow  |

---

# 10. Sequential Flow

Example:

```yaml
activities:
  - id: validate
    type: function

  - id: summarize
    type: ai

  - id: store
    type: function

transitions:
  - from: validate
    to: summarize

  - from: summarize
    to: store
```

---

# 11. Conditional Branching

```yaml
branches:
  - when: amount > 10000
    goto: managerApproval

  - when: amount <= 10000
    goto: autoApprove
```

Conditions use the platform expression language.

---

# 12. Parallel Execution

```yaml
parallel:
  branches:

    - workflow: fraudCheck

    - workflow: inventoryCheck

    - workflow: pricingCheck
```

Execution continues after all branches complete unless configured otherwise.

---

# 13. Loops (`for_each` / `map`)

**Implemented (WFL-301/302):** fan-out over a collection, as an engine-native
activity *type*. `while`/`until` loops are **not implemented** — earlier revisions of
this section described a `loop: {while, until, foreach}` block with a `collection:`
key that never existed in code.

```yaml
- id: summarize_all
  type: for_each                 # `map` is an alias
  inputs:
    items: "${fetch.docs}"       # a ${...} reference, or a literal array
    max_concurrent: 4            # optional; default 8
    max_items: 500               # optional; default 1000 (fail-closed bound)
    max_total_cost_usd: 5.00     # optional; no aggregate spend cap by default
    max_total_tokens: 2000000    # optional; no aggregate token cap by default
    activity:                    # the per-item body template
      type: tool
      name: summarize
      inputs: { doc: "${item}" } # `item` / `item_index` injected per instance
```

Each item becomes its own durable `ActivityRecord` with the reserved instance id
`<parent_id>[<index>]` (declared activity ids therefore may not contain `[` or `]`).
Outputs join in **item order** regardless of completion timing, so the persisted
history stays deterministic. The resolved collection is pinned into the checkpoint on
first encounter and never recomputed on resume; a resume re-drives only the instances
that never reached `Completed`. Engine-native body types (`wait`, `workflow`,
`for_each`, `map`) cannot nest and are rejected at load.

## 13.1 Aggregate cost and token ceilings (RES-601)

`max_items` bounds item **count** only. Because a body may be a full `agent`
activity — its own model + tool loop — a fan-out that stays comfortably under
`max_items` can still expand into an unbounded number of billable model calls inside
a *single* execution. An internal red-team run drove a 200-item fan-out, each item
spawning a researcher agent, to 200 completed children under one
`wovyr workflows run --local` invocation.

`max_total_cost_usd` and `max_total_tokens` bound the fan-out's total. Both are
optional and independent — a local model bills $0/token but still consumes real
capacity, so the token ceiling matters even at zero cost.

Semantics:

- Usage accumulates **as each item lands**, before the next is launched. Crossing a
  ceiling stops launching further items and fails the `for_each` activity closed
  (which fails the workflow through the normal saga path).
- Items **already in flight** are allowed to finish and commit durably — the ceiling
  changes when new items stop *starting*, never the per-item durability guarantee.
- Omitting both is behavior-identical to before this feature existed.
- A ceiling of `0`, a negative cost, or a non-finite cost is a **load error**, not a
  request for "unlimited".
- Activities that report no usage (`tool`, `function`, `human`) contribute zero, so
  non-model fan-outs need no configuration.

The engine obtains per-item usage from a reserved `__usage` key
(`{cost_usd, total_tokens}`) that the platform executor adds to `ai` and `agent`
activity outputs. `wovyr-workflow` deliberately does not depend on the LLM gateway, so
the executor — the one layer holding the `Usage` — reports it rather than the engine
querying for it. The key is additive: `${activity.message}` references are unaffected.

## 13.2 A `for_each` ceiling is the *only* per-execution budget under `--local`

Worth stating plainly, because the server-side protection does not apply here:

| | `wovyr dev` / hosted server | `wovyr … run --local` |
|---|---|---|
| Per-project daily LLM spend / tokens | enforced (`X-Wovyr-Project`, SRV-202/203) | **not enforced** — no `AgentResolver::admit` hook exists on the CLI path |
| Concurrent agent runs | enforced (optionally fleet-shared) | **not enforced** |
| Per-execution ceiling | **none** — the project budget is a *daily rate*, not a per-run cap | **none** |
| `for_each` aggregate ceiling | enforced (§13.1) | enforced (§13.1) |

So a single submission can stay entirely inside one day's server-side budget and still
fan out without limit within that one execution, and a `--local` run has no
project-level quota at all. Set `max_total_cost_usd` / `max_total_tokens` on any
`for_each` whose body performs model work. A per-execution budget spanning the whole
workflow (not just one fan-out) is not implemented.

---

# 14. AI Activity

Example:

```yaml
type: ai

provider: openai

model: gpt-5

prompt: summarize_customer

temperature: 0.2

maxTokens: 2048
```

Optional fields:

* systemPrompt
* responseSchema
* costLimit
* fallbackModel
* timeout

**Implemented shape (RM-AIM-P2 RUN-201).** The activity schema that actually ships
carries everything under `inputs` (the `ActivityDef` struct has no top-level
`model`/`temperature` fields), and the shared executor
(`wovyr_runtime::PlatformActivityExecutor`) reads:

```yaml
- id: summarize
  type: ai
  inputs:
    prompt: "You are a terse summarizer."   # system instruction
    message: "${fetch.body}"                 # user turn (or `text`)
    model: claude-sonnet-5                   # optional pin; else the gateway default
    temperature: 0.2                         # optional
    max_tokens: 2048                         # optional
    response_format:                         # optional, PRV-202's wire shape:
      json_schema: { name: summary, schema: { type: object } }
```

A malformed `response_format` fails the activity permanently (a definition bug is
not worth retrying); a transient provider error or quota rejection is retryable,
while validation/bad-request errors fail permanently. The aspirational top-level
fields above (`provider`, `costLimit`, `fallbackModel`, `timeout`, named-prompt
references) are not yet implemented.

---

# 14a. Agent Activity

Runs a *stored* agent (registered via `POST /api/v1/agents`) through its full
model/tool loop — unlike `ai` (a single bare chat call), an `agent` activity can call
tools, loop, and return a multi-step result.

```yaml
type: agent

name: greeter        # the stored agent's id

inputs:
  message: "${input.text}"
```

The activity's output is `{ message: <agent's final text>, steps: <model call count> }`.
The agent is resolved from the *submitting tenant's* store — a workflow can never reach
another tenant's agent. If no agent with that id exists in the tenant, the activity fails
permanently (no retry).

---

# 15. Human Task

```yaml
type: human

assignee: finance-manager

approvalRequired: true

timeout: 48h
```

Human tasks suspend workflow execution until completion or timeout.

---

# 16. Tool Invocation

```yaml
type: tool

tool: email.send

inputs:
  template: approval
```

Tools are resolved through the Tool Registry.

---

# 17. Event Wait

```yaml
type: event

event: PaymentReceived

timeout: 7d
```

Execution resumes upon receiving the matching event.

---

# 18. Timer

```yaml
type: timer

delay: 24h
```

Supported formats:

* Seconds
* Minutes
* Hours
* Days
* Cron expressions

---

# 19. Retry Policy

Global example:

```yaml
retry:
  attempts: 5
  strategy: exponential
  delay: 30s
```

Activity-specific retry policies override global defaults.

---

# 20. Compensation

```yaml
compensation:

  reserveInventory:
    compensate: releaseInventory

  chargePayment:
    compensate: refundPayment
```

Compensation handlers are explicit and versioned.

---

# 21. Error Handling

Example:

```yaml
onError:

  strategy: retry

  fallback: notifySupport

  continue: false
```

Supported strategies:

* Retry
* Ignore
* Fail
* Compensate
* Redirect

---

# 22. Security

Activities may specify execution constraints.

```yaml
security:

  permissions:
    - invoice.read
    - invoice.write

  tenantIsolation: true
```

The runtime validates permissions before execution.

---

# 23. Workflow Versioning

Each workflow definition includes:

```yaml
metadata:
  version: 2.1.0
```

Rules:

* Major versions may introduce breaking changes.
* Minor versions add backward-compatible features.
* Patch versions fix defects without changing behavior.

Running executions continue using the version with which they were started.

---

# 24. Validation Rules

The compiler validates:

* Schema correctness
* Unique activity IDs
* Reachable nodes
* No orphaned transitions
* Valid expressions
* Retry policy consistency
* Compensation mappings
* Security policy references

Invalid workflows must not be accepted for execution.

---

# 25. Workflow Intermediate Representation (WIR)

All authoring formats compile into an internal graph model.

```text
YAML
   │
JSON
   │
Visual Designer
   │
Rust SDK
   │
AI Generator
   ▼
Workflow Parser
   ▼
Workflow Intermediate Representation (WIR)
   ▼
Validator
   ▼
Execution Planner
   ▼
Workflow Runtime
```

The WIR is the single source of truth for execution.

---

# 26. Extensibility

The DSL supports extension through:

* Custom activity types
* Custom expressions
* New schedulers
* Additional serialization formats
* Plugin-defined activities

Extensions must declare schemas and validation rules.

---

# 27. Related Documents

* Workflow Overview
* Execution Model
* DAG Engine
* Scheduler
* State Machine
* Persistence
* Retry Engine
* Compensation
* Rust SDK Design

---

# 28. Revision History

| Version | Date       | Description                        |
| ------- | ---------- | ---------------------------------- |
| 1.0.0   | 2026-06-26 | Initial Workflow DSL Specification |
| 1.1.0   | 2026-07-13 | §14: implemented `ai`-activity input shape — model pin, temperature/max_tokens, response_format, error classification (RM-AIM-P2 RUN-201) |
