<!--
File: docs/18-roadmap/future/B1-multi-agent-systems.md
Document ID: FUT-001
-->

# Future Exploration: Autonomous Multi-Agent Systems

**Document ID:** FUT-001
**File Path:** `docs/18-roadmap/future/B1-multi-agent-systems.md`
**Version:** 1.1.0
**Status:** Exploratory — research bet, not committed. A prototype slice for
direction (b) now exists in code (`examples/workflows/research-team.yaml` +
`apex-server`, §8) — it proves the fan-out/join shape and closes the aggregate-
budget invariant, but does not itself satisfy the graduation gate (no benchmark
against a single agent). Still pre-ADR.
**Owner:** Agent Framework Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Flesh out the "Autonomous Multi-Agent Systems" research bet
([future.md §2.1](../future.md#21-autonomous-multi-agent-systems),
[PRD-002 §6.1](../../01-product/prd-future.md#61-autonomous-multi-agent-systems))
into a problem statement, a design sketch, requirements, and the evidence a
graduation ADR would need — without committing to build it.

This is exploratory. Nothing here becomes real until it graduates through an
[ADR](../../17-adr/index.md) into a concrete release.

---

# 2. Problem & Opportunity

Today an agent run is a single loop: `system+user message → gateway.chat_stream
→ tool calls → repeat → final answer`, bounded by `RunOptions::max_steps`
([`apex-agent`](../../../crates/apex-agent/src/runtime.rs)). One agent, one
budget, one context.

Many real tasks are decomposable — research, multi-step planning, parallel
sub-tasks with a join. The opportunity is **self-organizing agent groups**:
negotiation, delegation, and emergent task decomposition
([multi-agent-coordination](../../04-agent-framework/multi-agent-coordination.md)),
where a coordinator agent spawns and supervises sub-agents rather than doing
everything in one linear loop.

The risk that makes this a *bet* rather than a feature: multi-agent systems are
where cost and non-determinism explode. A naive implementation fans out
unboundedly and becomes impossible to debug or budget.

---

# 3. Current Baseline (what this would build on)

- **The single-agent loop** — `run_agent` / `run_agent_with_memory` and the
  `RunEventSink` progress contract already exist and are deterministic.
- **Tenant-fair scheduling** — `apex-tools`' `FairScheduler` (smooth weighted
  round-robin over a sandbox pool) already prevents one tenant from starving
  capacity; agent groups would schedule through it, not around it.
- **Cost metering** — the `Gateway`'s `CostObserver` already emits per-call cost
  events; a group's aggregate spend is observable with the existing hook.
- **Quotas** — `apex-tenancy`'s `QuotaLimits` / the server's `QuotaTracker`
  already cap `concurrent_agent_runs` and daily LLM cost per project.

The missing piece is **composition + supervision**, not the primitives.

---

# 4. Direction (design sketch, non-committal)

Two shapes to evaluate before an ADR:

- **(a) Coordinator-as-agent.** A coordinator agent invokes sub-agents through a
  first-class `spawn_agent`-style tool. Sub-agents are ordinary runs with derived
  ids (mirroring the child-workflow id scheme from
  [ADR-0008](../../17-adr/ADR-0008-subworkflows.md)), so they inherit durability,
  visibility, and cost metering for free. Delegation is explicit and auditable.
- **(b) Workflow-orchestrated agents.** Express the group as a workflow DAG where
  activities are agent runs. Reuses the durable engine's fan-out/join,
  compensation, and determinism directly — less "emergent," more controllable.

These are not mutually exclusive: (b) is the controllable near-term shape; (a) is
the research frontier. An ADR should pick the first slice.

---

# 5. Requirements

## 5.1 Functional
- A coordinator can spawn, supervise, and collect results from N sub-agents.
- Delegation decisions are recorded and replayable for debugging.
- A group has a single, enforced aggregate budget (cost + concurrency), not N
  independent budgets.

## 5.2 Invariants to preserve
- **Bounded fan-out.** No path may spawn unboundedly; depth/breadth caps are
  enforced fail-closed (cf. the missing depth guard noted in ADR-0008).
- **Tenant & cost isolation.** A group cannot exceed its tenant's quota or reach
  another tenant's namespace — the existing `QuotaTracker` / tenant-scoping
  boundaries hold for every sub-agent.
- **Deterministic-enough replay.** Sub-agent ids are derived, not random, so a
  run can be reconstructed.

---

# 6. Key Risks & Open Questions

- **Cost/latency blow-up** from unbounded delegation — the primary risk.
- **Debuggability** of emergent behavior — can a failed group be understood?
- **Coordination protocol** — negotiation/handoff semantics are unspecified; how
  much structure vs. emergence?
- **Failure semantics** — does one sub-agent's failure fail the group, or is it a
  soft/partial result?

---

# 7. Graduation Gate

This becomes an ADR + roadmap slot only when it can show:

> A budget-capped multi-agent task that **measurably outperforms** a single agent
> on a real benchmark, with bounded fan-out, enforced aggregate cost, and a
> replayable delegation trace.

Absent that evidence, it stays exploratory.

---

# 8. Prototype Slice (2026-07-05)

Direction (b) — workflow-orchestrated agents — was picked over (a)
coordinator-as-agent because it reuses the durable engine's existing fan-out/join
instead of a new dynamic delegation mechanism. Investigation found the shape was
**already mostly buildable**: `apex-server`'s `ServerExecutor` already ran a single
`agent`-typed workflow activity end to end through the real `run_agent` loop, and
the engine already runs any batch of simultaneously-ready activities concurrently
regardless of type ([`apex-workflow/tests/engine.rs::parallel_branches_run_concurrently`](../../../crates/apex-workflow/tests/engine.rs))
— so a static DAG with N parallel `agent` activities needed no new engine code.

**What this slice built:**
- [`examples/workflows/research-team.yaml`](../../../examples/workflows/research-team.yaml) —
  a coordinator pattern: two `agent` activities research opposite angles of
  `${input.topic}` in parallel (no edge between them), converging into a
  `synthesize` `agent` activity that combines both via
  `${proResearch.message}`/`${conResearch.message}`.
- **A real, previously-undiscovered gap fixed along the way**: `ServerExecutor`
  had **no `${...}` template resolution at all** — the engine hands executors
  the raw definition inputs and leaves interpolation to them
  ([execution model §14](../../03-workflow-engine/execution-model.md)), and only
  the CLI's `PlatformExecutor` implemented it. This meant the pre-existing
  `agent-review.yaml` example's `${draft.message}` reference never actually
  worked when run through the live server — only via `workflows run --local`.
  Extracted the resolution logic to `apex_workflow::resolve_template` (a shared
  helper both executors now call) and wired it into `ServerExecutor`, closing the
  gap for every activity type, not just `agent`.
- **Closed the "single enforced aggregate budget" invariant**
  ([§5.2](#52-invariants-to-preserve)): workflow-driven `agent` activities
  previously bypassed the project quota system entirely (unlike the direct
  `agents:run` endpoints), so a fan-out to N sub-agents had N independent,
  unmetered budgets. `ServerExecutor`'s `agent` branch now runs every sub-agent
  through the same `tenancy::admit_run`/`record_run_cost` gate a direct run uses,
  keyed by an `__project` marker `submit_handler` stamps from `X-Apex-Project` —
  so a group's concurrent sub-agents draw from one shared
  `concurrent_agent_runs`/`llm_cost_per_day_usd` ceiling. A quota rejection is
  `ActivityError::Retryable` (the slot frees once a sibling's run ends), not a
  permanent failure.

**What it proves** (`crates/apex-server/src/workflow_runner.rs`'s test module):
1. `research_team_fans_out_and_joins_two_agents` — both sub-agents produce
   output and the `synthesize` activity's resolved input contains both (no
   unresolved `${` placeholder survives) — "collect results from N sub-agents"
   and the templating fix, in one test.
2. `agent_activity_respects_project_quota` — with `concurrent_agent_runs: 0` on
   the submitting project, an `agent` activity fails on quota grounds instead of
   running unmetered — proving the budget wiring is live, not just plumbed
   (a deterministic-reject test, mirroring `apex-server/src/tenancy.rs`'s own
   quota tests, rather than a timing-based concurrency race against near-instant
   mock LLM calls).

**What it explicitly does not prove** (open problems for the eventual ADR):
- **No benchmark against a single agent** — [§7](#7-graduation-gate)'s
  graduation gate needs [FUT-006](B6-trust-evaluation.md)'s eval harness pointed
  at this workflow shape; that wiring doesn't exist yet.
- **Direction (a)** (coordinator-as-agent / a dynamic `spawn_agent` tool) is
  untouched — a materially different, more open-ended mechanism.
- **No CLI-local support** — `workflows run --local`'s `PlatformExecutor` still
  has no stored-agent concept to resolve an `agent` activity's `name` against;
  this bet is provable entirely through the server, which already has the
  `AgentStore` primitive.
- **No dynamic fan-out cap** — breadth is bounded by what the workflow author
  statically declares in the DAG; nothing enforces a depth/breadth ceiling for a
  hypothetical future where workflows could spawn workflows dynamically.

---

# 9. Dependencies

- [FUT-006 Trust & Evaluation](B6-trust-evaluation.md) — needed to *measure* the
  "outperforms a single agent" claim in the gate.
- The child-workflow model ([ADR-0008](../../17-adr/ADR-0008-subworkflows.md)) if
  direction (b) needs dynamic (not just statically-authored) composition later.

---

# 10. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.1 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.1 — requirements context
- [`04-agent-framework/multi-agent-coordination.md`](../../04-agent-framework/multi-agent-coordination.md)
- [`17-adr/ADR-0008-subworkflows.md`](../../17-adr/ADR-0008-subworkflows.md) — the child-execution precedent
- `examples/workflows/research-team.yaml` — the prototype slice (§8)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-05 | Added §8 Prototype Slice: a coordinator-pattern example workflow (fan-out to two `agent` activities, joined), a fix for `ServerExecutor`'s previously-missing `${...}` template resolution, and aggregate project-quota enforcement across a group's sub-agents. Still pre-ADR — gathers evidence for §7's gate, doesn't satisfy it |
| 1.0.0 | 2026-07-05 | Initial exploration doc for the multi-agent research bet |
