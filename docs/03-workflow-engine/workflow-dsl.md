# Workflow DSL Specification

**Document ID:** WF-003
**Version:** 1.0.0
**Status:** Draft
**Owner:** Workflow Engine Team
**Last Updated:** 2026-06-26

---

# 1. Purpose

This document defines the Apex Workflow Domain Specific Language (DSL).

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
apiVersion: workflow.apex.io/v1
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

# 13. Loops

Supported loop types:

```yaml
loop:
  while:
  until:
  foreach:
```

Example:

```yaml
foreach:
  collection: orders
  activity: processOrder
```

Loop execution is deterministic.

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
