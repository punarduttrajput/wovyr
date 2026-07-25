<!--
File: docs/01-product/prd-future.md
Document ID: PRD-002
-->

# PRD: Future Platform Directions (Beyond & Completing 1.0)

**Document ID:** PRD-002
**File Path:** `docs/01-product/prd-future.md`
**Version:** 1.2.0
**Status:** Draft — planning input, not a commitment
**Owner:** Product Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

The master [PRD-001](prd.md) defines the product through GA. This companion PRD
scopes what comes **after the currently-implemented surface** — both the
remaining work to *finish* v1.0 and the exploratory research bets *beyond* it.

It exists because [`18-roadmap/future.md`](../18-roadmap/future.md) (RM-005) is a
one-line-per-idea catalogue with no problem statements, requirements, or exit
criteria — not enough to plan or prioritize against. This document turns that
catalogue into requirements: for each direction, *why* it matters, *what* "done"
means, what it depends on, and how it graduates from idea to committed work.

**This is a planning input, not a promise.** Nothing here is committed until it
graduates through an [ADR](../17-adr/index.md) into a concrete release
(see §10). Dates are deliberately omitted; sequencing is by dependency and
priority tier, not calendar.

---

# 2. Current Baseline (as of 2026-07-05)

Grounding this PRD in what actually exists, so "future" is measured from a real
line, not the aspirational spec:

- **Shipped (v0.1–v0.3, tagged):** the agent runtime, workflow engine (durable,
  distributed, scheduled), memory engine (hybrid retrieval, MMR, ABAC,
  compression), LLM gateway (failover, breaker, cache, hedging), tool runtime
  (sandbox spectrum through microVM, egress lockdown), plugin engine +
  marketplace (signing, SBOM/provenance, human review), multi-tenancy (RBAC/ABAC,
  quotas), events/webhooks, audit, and secrets.
- **In progress (v1.0):** DX/SDK (OpenAPI + TypeScript/Python clients — Python
  on PyPI), Security (envelope-encryption KMS live across secrets/memory/webhook
  stores, key-management surface, pen-tested + compliance-mapped), and
  Reliability (a real single-binary `docker-compose` deployment, chaos-checked).
- **Not started:** everything in §5–§6 below.

The honest gap: the platform is **feature-complete for a single node** but
**unproven at enterprise scale and unfinished on operational/certification
readiness**. That framing drives the two-tier structure below.

---

# 3. Problem Statement

Three distinct problems motivate post-baseline work:

1. **GA is not yet defensible.** v1.0's own exit criteria (published SLOs met in
   production, external pen test passed, reference customers on critical
   workloads — [v1.0 §5](../18-roadmap/v1.0.md)) are unmet. Scale, HA/DR, and
   third-party security validation are missing, so "production-ready" is a claim
   we cannot yet stand behind.
2. **The ecosystem has no economic engine.** The marketplace can publish, govern,
   and install plugins, but there is no monetization, revenue share, or abuse
   handling — so there is no incentive loop to grow a third-party ecosystem.
3. **The platform is static, not self-improving.** Routing, ranking, and
   warm-pool sizing are hand-tuned constants. A platform that observes its own
   behavior could tune them — a category of value the current architecture
   enables but does not yet capture.

---

# 4. Guiding Principles

Carried forward from the roadmap ([index §3](../18-roadmap/index.md)) and
coding standards, and binding on everything in this PRD:

1. **Honesty over aspiration.** A feature is "done" only when it exists in code
   *and* is verified. Docs follow decisions; status blocks state what is real.
2. **Vertical slices.** Each graduated item runs end to end, not just a lower
   layer.
3. **Determinism in core logic.** No ambient clocks/randomness in schedulable
   paths — the constraint that makes the engine testable stays inviolable, even
   for self-optimizing features (learning happens at the boundary, not in the
   deterministic core).
4. **Security & tenancy are not retrofitted.** Any new surface is tenant-scoped
   and fail-closed from its first commit.
5. **Trait boundaries absorb change.** New backends (cloud KMS, GPU schedulers,
   protocol gateways) implement an existing port rather than reshaping the spine.

---

# 5. Tier A — Completing 1.0 (Committed Intent, Near-Term)

These are not research bets. They are the concrete, already-scoped gaps between
the current baseline and a defensible GA. They should graduate first. Each has a
dedicated delivery doc under [`18-roadmap/v1.0/`](../18-roadmap/v1.0/index.md)
(GA-001…GA-005) with current state, work breakdown, exit criteria, and
environment caveats.

## 5.1 Scale & Performance Validation

**Delivery doc:** [GA-001](../18-roadmap/v1.0/A1-scale-performance.md)

**Problem.** Perf tests are deliberately toy-scale (hundreds of records,
in-process mock provider). The NFR targets — billions of memories, thousands of
concurrent runs ([performance-tests](../15-testing/performance-tests.md)) — are
unvalidated against real Postgres/Qdrant capacity.

**Requirements.**
- Memory sharding/partitioning validated at target cardinality against a live
  tiered backend, with published p50/p95/p99 retrieval latency.
- Workflow throughput validated with the existing queue partitioning (G6) across
  a real multi-worker pool under sustained load.
- A reproducible load-generation harness and a published scaling envelope
  (extending [distributed-execution §3.3](../03-workflow-engine/distributed-execution.md#33-scaling-envelope-g6)).

**Success criteria.** Documented, reproduced NFR numbers — or an honest,
published statement of where the current architecture tops out and why.

**Dependencies.** Real cloud capacity (absent in the current dev environment).

**Graduation gate.** ADR recording the target topology and measurement method.

## 5.2 Reliability: HA, DR, and Deployment Artifacts

**Delivery doc:** [GA-002](../18-roadmap/v1.0/A2-reliability-ha-dr.md)

**Problem.** The compose deployment is single-node. There are no Kubernetes,
Helm, or Terraform artifacts, and no backup/restore or DR story
([v1.0 §3, Reliability row](../18-roadmap/v1.0.md)).

**Requirements.**
- Kubernetes manifests + Helm chart + Terraform modules for a multi-replica,
  HA deployment (validated against a real cluster — tooling currently absent).
- Backup/restore procedures for every durable store (workflow, memory, tenancy,
  secrets, KMS catalog, marketplace registry).
- A documented DR runbook with RPO/RTO targets and a restore drill.

**Success criteria.** A node loss and a full-restore drill both pass without data
loss beyond the stated RPO.

**Dependencies.** A real orchestrator and `kubectl`/`helm`/`terraform`.

## 5.3 Security: Root-of-Trust, PII Coverage, and External Validation

**Delivery doc:** [GA-003](../18-roadmap/v1.0/A3-security-completion.md)

**Problem.** The KMS root key is an in-process/single-host stand-in; no
cloud-KMS/HSM backing exists. Field encryption covers secrets, memory, and
webhook secrets, but not future PII resources. No external pen test or formal
SOC 2 / ISO 27001 / GDPR audit has occurred
([compliance-mapping](../13-security/compliance-mapping.md)).

**Requirements.**
- A cloud-KMS-/HSM-backed `Kms` implementation behind the existing trait boundary
  (only tenant-key wrap/unwrap changes).
- Field-level encryption for any PII-bearing resource added later (e.g. a `User`
  with an email — [users.md](../09-api/users.md)), reusing the
  `envelope::seal`/`open` pattern already proven in three consumers.
- Close the documented residual-risk findings (notably the anonymous
  default-tenant RBAC bypass) as a deliberate, scoped hardening pass.
- An external penetration test and a formal compliance-mapping audit.

**Success criteria.** External pen-test report with no unresolved
high/critical findings; a completed control-mapping review by a qualified
third party.

## 5.4 Ecosystem: Marketplace Economics & Safety

**Delivery doc:** [GA-004](../18-roadmap/v1.0/A4-marketplace-economics.md)

**Problem.** The marketplace has no monetization, revenue share, or abuse-report
workflow, and the dashboard has no browse UI ([v1.0 §3, Ecosystem row](../18-roadmap/v1.0.md)).

**Requirements.**
- A monetization model (paid listings / revenue share) with billing integration
  behind a trait boundary (provider-neutral).
- An abuse-report + takedown workflow paralleling the existing human-review
  workflow.
- A marketplace browse/search UI in the dashboard SPA.

**Success criteria.** A paid plugin can be listed, purchased, installed, and its
publisher paid; a reported plugin can be triaged and disabled.

## 5.5 DX: SDK Distribution & Migration Guides

**Delivery doc:** [GA-005](../18-roadmap/v1.0/A5-sdk-distribution.md)

**Problem.** The TypeScript SDK is unpublished (blocked on an interactive npm
2FA OTP); there are no further language clients or migration guides.

**Requirements.**
- Publish `@wovyr/sdk` to npm (operator-supplied OTP).
- Evaluate additional language clients (Go/Java) against the same OpenAPI
  contract.
- Author migration guides once the first `/v1`→`/v2` deprecation actually occurs
  (the [deprecation policy](../09-api/deprecation-policy.md) exists; nothing has
  been deprecated to write against yet).

**Success criteria.** `npm i @wovyr/sdk` installs a working client; the
deprecation-policy headers are enforced in code once there is something to
deprecate.

---

# 6. Tier B — Research Bets (Exploratory, Beyond 1.0)

These expand *what the platform can do*. They are genuinely uncertain, require
ADRs before any build, and are expected to be pruned. Each corresponds to a
direction in [future.md §2](../18-roadmap/future.md); this PRD adds the
requirements scaffolding, and each has a dedicated exploration doc under
[`18-roadmap/future/`](../18-roadmap/future/index.md) (FUT-001…FUT-006) with the
full design sketch, invariants, risks, and graduation gate.

## 6.1 Autonomous Multi-Agent Systems

**Exploration doc:** [FUT-001](../18-roadmap/future/B1-multi-agent-systems.md)

**Opportunity.** Move from single-agent runs to self-organizing agent groups —
negotiation, delegation, emergent task decomposition
([multi-agent-coordination](../04-agent-framework/multi-agent-coordination.md)).

**Key requirements (if pursued).** A coordination protocol with bounded,
auditable delegation; deterministic-enough replay for debugging; hard tenant
and cost isolation so a swarm cannot escape its budget or namespace.

**Primary risk.** Unbounded fan-out of cost and non-determinism. **Gate:** a
demonstrated, budget-capped multi-agent task that outperforms a single agent on
a real benchmark.

## 6.2 Self-Optimizing Platform

**Exploration doc:** [FUT-002](../18-roadmap/future/B2-self-optimizing-platform.md)

**Opportunity.** Replace hand-tuned constants with live-scored decisions:
cost/quality-aware [routing](../05-llm-gateway/routing.md), self-tuning
[ranking](../06-memory-engine/ranking.md) weights, and adaptive warm-pool
sizing (the `SandboxPool` autoscaler already exposes the knobs).

**Key requirements.** All learning happens at the boundary; the deterministic
core is untouched (principle §4.3). Every automated decision is observable and
overridable. A/B or shadow evaluation before any policy goes live.

**Primary risk.** A feedback loop that optimizes a proxy metric into a
regression. **Gate:** a shadow-mode deployment showing measured improvement with
no quality regression.

## 6.3 Advanced Memory

**Exploration doc:** [FUT-003](../18-roadmap/future/B3-advanced-memory.md)

**Opportunity.** Knowledge-graph reasoning at scale (tagged v1-deferred in
[wovyr-memory]), multi-modal memory (image/audio), time-travel queries,
cross-agent memory fusion, and confidence scoring
([memory futures](../06-memory-engine/overview.md#16-future-enhancements)).

**Key requirements.** Preserve the existing ABAC/tenant-isolation and
encryption guarantees across new modalities; keep hybrid retrieval's scoring
explainable (the `score_breakdown` contract).

**Primary risk.** Modality-specific stores fragmenting the isolation model.
**Gate:** an ADR proving the graph/multi-modal store upholds tenant isolation
and encryption end to end.

## 6.4 Execution Frontiers

**Exploration doc:** [FUT-004](../18-roadmap/future/B4-execution-frontiers.md)

**Opportunity.** Snapshot/restore sandboxes for near-instant cold starts
(extending warm pooling), GPU-aware scheduling, edge/regional inference pools,
and a WASM component model for portable polyglot plugins
([tool-runtime futures](../07-tool-runtime/overview.md#15-future-enhancements)).

**Key requirements.** New backends implement the existing `SandboxBackend`
spectrum and `TrustClass` floors — the isolation contract is non-negotiable.
GPU scheduling integrates with the `FairScheduler`, not around it.

**Primary risk.** A faster backend weakening the isolation floor. **Gate:**
adversarial escape tests (the v0.3 precedent) pass against the new backend
before it is selectable.

## 6.5 Ecosystem & Interoperability

**Exploration doc:** [FUT-005](../18-roadmap/future/B5-ecosystem-interop.md)

**Opportunity.** An MCP gateway and broader protocol interop, prompt/model
registries, and federated cross-organization plugin/memory sharing
([future §2.5](../18-roadmap/future.md)).

**Key requirements.** Interop rides the existing provider/tool abstractions.
Federation is fail-closed across org boundaries — cross-org sharing is explicit,
scoped, and revocable, never default.

**Primary risk.** Federation becoming a cross-tenant data-leak vector. **Gate:**
a threat model + ADR for the trust boundary before any cross-org path ships.

## 6.6 Trust & Evaluation

**Exploration doc:** [FUT-006](../18-roadmap/future/B6-trust-evaluation.md)
(upstream prerequisite for 6.1, 6.2, and 6.3)

**Opportunity.** A built-in AI evaluation service, continuous quality-regression
gates in CI, and maturing provenance/attestation/policy-as-code
([future §2.6](../18-roadmap/future.md)).

**Key requirements.** Evaluation is deterministic and reproducible (fixed seeds,
recorded fixtures). Policy-as-code integrates with the existing Policy Engine and
RBAC/ABAC model.

**Primary risk.** Flaky evals eroding trust in the gate itself. **Gate:** a
regression suite with quantified, stable variance before it can block a merge.

---

# 7. Prioritization Framework

Candidates are scored on four axes; Tier A dominates Tier B until GA is
defensible.

| Axis | Question |
|------|----------|
| **Necessity** | Does GA / a reference customer require it? (Tier A ≫ Tier B) |
| **Leverage** | Does it unlock other work or an ecosystem loop? |
| **Confidence** | Is the approach known, or genuinely research? |
| **Isolation cost** | Can it ship behind an existing trait without reshaping the spine? |

Default order: **finish Tier A → prune Tier B by evidence → graduate the
survivors one vertical slice at a time.**

---

# 8. Success Metrics

- **GA readiness:** all [v1.0 §5 exit criteria](../18-roadmap/v1.0.md) met
  (SLOs in production, external pen test passed, reference customers live).
- **Scale:** published, reproduced NFR numbers (or an honest ceiling).
- **Ecosystem:** first paid plugin transacted end to end; abuse workflow
  exercised.
- **Self-optimization (if pursued):** a live tuning policy showing measured gain
  with zero quality regression in shadow mode.
- **Process:** every graduated item traces to an ADR and lands with tests +
  updated docs (the standing bar).

---

# 9. Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Research bets crowd out GA completion | Tier A dominates prioritization until exit criteria are met |
| Scope sprawl in exploratory work | ADR-gated graduation; ideas expire if not graduated |
| Scale/HA work unverifiable in-house | Explicitly flag environment dependencies; don't claim what wasn't run |
| Self-optimization regresses quality | Shadow/A-B evaluation mandatory before any policy goes live |
| New execution/memory backends weaken isolation | Adversarial escape/isolation tests gate selectability |
| Federation leaks cross-tenant data | Threat model + fail-closed trust boundary before any cross-org path |

---

# 10. Graduation Process

Unchanged from [future.md §3](../18-roadmap/future.md#3-how-ideas-graduate):

```text
future (idea) → ADR (decision) → release roadmap (v1.x / v2) → docs + build
```

This PRD is the **idea → requirements** step. An item becomes committed only when
an ADR records the decision and it takes a slot in a concrete release. Items that
never graduate are pruned, not carried indefinitely.

---

# 11. Traceability

Each direction here maps to: a `future.md` §2 entry (origin), a subsystem spec
(the surface it extends), and — once graduated — an ADR and a release-roadmap
row. No implementation should begin against this PRD directly; it begins against
the ADR this PRD justifies.

---

# 12. Related Documents

- [`01-product/prd.md`](prd.md) — the master PRD (PRD-001)
- [`18-roadmap/v1.0.md`](../18-roadmap/v1.0.md) — GA milestone (Tier A source)
- [`18-roadmap/future.md`](../18-roadmap/future.md) — research-bet catalogue (Tier B source)
- [`17-adr/index.md`](../17-adr/index.md) — where graduation decisions are recorded
- [`01-product/non-functional-requirements.md`](non-functional-requirements.md) — NFR targets referenced in §5.1

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-05 | Linked each Tier A workstream (§5.1–§5.5) to its dedicated delivery doc under [`18-roadmap/v1.0/`](../18-roadmap/v1.0/index.md) (GA-001…GA-005) |
| 1.1.0 | 2026-07-05 | Linked each Tier B direction (§6.1–§6.6) to its dedicated exploration doc under [`18-roadmap/future/`](../18-roadmap/future/index.md) (FUT-001…FUT-006) |
| 1.0.0 | 2026-07-05 | Initial future/beyond-GA PRD: a two-tier structure (Tier A completing v1.0 — scale, HA/DR, security root-of-trust + external audit, marketplace economics, SDK distribution; Tier B research bets — the six `future.md` directions elevated to requirements with graduation gates), grounded in the 2026-07-05 baseline |
