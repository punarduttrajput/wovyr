<!--
File: docs/18-roadmap/future/B1-multi-agent-systems.md
Document ID: FUT-001
-->

# Future Exploration: Autonomous Multi-Agent Systems

**Document ID:** FUT-001
**File Path:** `docs/18-roadmap/future/B1-multi-agent-systems.md`
**Version:** 1.0.0
**Status:** Exploratory — research bet, not committed
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

# 8. Dependencies

- [FUT-006 Trust & Evaluation](B6-trust-evaluation.md) — needed to *measure* the
  "outperforms a single agent" claim in the gate.
- The child-workflow model ([ADR-0008](../../17-adr/ADR-0008-subworkflows.md)) if
  direction (b) is chosen.

---

# 9. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.1 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.1 — requirements context
- [`04-agent-framework/multi-agent-coordination.md`](../../04-agent-framework/multi-agent-coordination.md)
- [`17-adr/ADR-0008-subworkflows.md`](../../17-adr/ADR-0008-subworkflows.md) — the child-execution precedent

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial exploration doc for the multi-agent research bet |
