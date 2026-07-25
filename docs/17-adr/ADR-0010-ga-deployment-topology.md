<!--
File: docs/17-adr/ADR-0010-ga-deployment-topology.md
Document ID: ADR-0010
-->

# ADR-0010: GA Deployment Topology — Single-Node Appliance First, Distributed Platform Next

**Status:** Accepted (Path A ratified 2026-07-06)
**Date:** 2026-07-06
**Deciders:** Product Team, Engineering Leads (Architecture)
**Supersedes:** —

---

# Context

The 2026-07-06 solution-architecture review ([PRD-003](../01-product/prd-ga-hardening.md))
found that Wovyr's deployed behavior and its marketed behavior diverge. Its
one-line verdict:

> Wovyr is a well-designed single-node appliance wearing the marketing of a
> distributed multi-tenant platform.

The relevant facts, verified in code:

- The distributed-execution machinery — the workflow `PostgresStore`, `WorkQueue`,
  `Worker`, time-bounded leases, and sharded partitions (`crates/wovyr-workflow/`) —
  is **tested library code that no shipping binary wires**. The server hard-wires a
  `FileStore` at `~/.wovyr/workflows` (`crates/wovyr-server/src/lib.rs`, `default_workflows_engine`).
- Gateway fleet resilience is the same story: `with_redis_breakers`,
  `QdrantSemanticCache`, and `with_qdrant_semantic_cache` have zero references in
  `wovyr-server` or `apps/`.
- Every control-plane catalog (tenancy, secrets, KMS, plugins, webhooks, audit,
  the agent store) is file-only under `~/.wovyr`. Two server replicas cannot coexist:
  with per-pod volumes, replica 2 generates its own KMS root key and cannot decrypt
  replica 1's sealed data; with a shared volume, unlocked in-place file rewrites
  race and corrupt.
- The Helm chart is honest about this — it pins `replicas: 1`
  (`deployment/helm/wovyr/templates/wovyr-statefulset.yaml`) — but the roadmap and
  docs position sharding, HA, and horizontal scaling as platform capabilities.

Only the marketplace registry has a runtime-selected shared backend
(`PostgresRegistryStore` via `WOVYR_MARKETPLACE_POSTGRES_URL`), which proves the
trait-port pattern for backend promotion already works.

The review confirmed the dependency spine is **acyclic and one-directional** and
every store sits behind a swappable trait port. So the distributed capability is a
matter of **wiring existing abstractions onto the default path**, not a redesign —
but doing it credibly (shared control-plane state across replicas, lease-driven
crash recovery, migrations, real capacity engineering) is months of work.

This forces a decision that gates the scope of two PRD-003 workstreams (WS-5
distributed backend promotion, WS-6 server-side execution driver) and, more
fundamentally, decides **what product we are shipping at GA.** Per PRD-003 §5 it
must be ratified before those workstreams are estimated, and the ADR register
requires a recorded decision for a boundary choice of this size.

Two paths were identified:

- **Path A — Single-Node Appliance.** Position GA as a single-node (single-tenant,
  or single-node multi-tenant) deployment. Drop the distributed claims; keep the
  library code but mark it library-only. Harden the single-node story (auth,
  crash-safety, backup, honest docs).
- **Path B — Distributed Platform.** Deliver everything in Path A **plus** wire the
  Postgres/queue/lease/partition machinery, shared control-plane state, migrations,
  and multi-replica correctness onto the default path.

Constraints that shaped the choice:

- **Every Path-A fix is also a Path-B prerequisite.** Authentication, crash-safe
  writes, migrations, and the execution driver are required regardless of topology —
  none of it is throwaway if Path A ships first.
- **The published PyPI SDK is accruing users.** API-contract debt becomes permanent
  breaking-change debt the longer GA is delayed, which pressures against a
  months-long Path B before *any* defensible GA.
- **GA's own exit criteria** ([v1.0 §5](../18-roadmap/v1.0.md#5-exit-criteria))
  require published SLOs met in production and an external pen test passed — both
  achievable on a single node; neither requires horizontal scale.
- **Honesty is a release-quality gate.** Shipping GA while docs claim an unwired
  capability is itself a defect (PRD-003 R-5.5).

---

# Decision

**Adopt Path A for the GA milestone. Schedule Path B as the immediately-following
major milestone.**

Concretely:

1. **GA ships as a single-node appliance.** The supported topology is one `wovyr`
   binary (optionally backed by Postgres/Qdrant for the backends already wired —
   marketplace registry, and CLI-side tiered memory), deployed as the Helm chart's
   single-replica `StatefulSet`. `replicas: 1` becomes a **product statement**, not a
   temporary limitation.

2. **The distributed library code stays, marked library-only.** The workflow
   `PostgresStore`/`WorkQueue`/`Worker`/partitions, Redis breakers, and the Qdrant
   semantic cache remain in the tree and under test, but the roadmap, README, and
   `docs/12-deployment/*` are corrected to state plainly that they are not wired into
   the shipping binary and the deployment is single-node (PRD-003 R-5.5). No doc
   claims a capability the binary lacks.

3. **All topology-independent hardening is in GA scope regardless of this decision.**
   Authentication (WS-1), transport/resource hardening (WS-2), safe-by-default
   sandboxing (WS-3), crash-safe durable state (WS-4), the server-side execution
   driver and honest cancel (WS-6), backup/restore (WS-10), API stabilization (WS-7),
   observability/audit coverage (WS-8), and codebase health (WS-9) all ship at GA.

4. **Path B is a named follow-on milestone (v1.1 "Scale-Out"), not GA scope.** It
   promotes the control-plane catalogs to a shared backend, makes the KMS root key
   injection-only in multi-replica mode, wires the workflow queue/worker/lease path
   and the gateway fleet resilience onto the default path, and does the real
   capacity engineering (PRD-002's Scale work). Its entry is gated on GA shipping.

5. **The migration framework (PRD-003 R-5.3) is pulled forward into GA** even under
   Path A, because the marketplace Postgres backend already ships and inline
   `CREATE TABLE IF NOT EXISTS` DDL-on-startup is not a defensible GA posture for any
   Postgres-backed surface.

---

# Consequences

**Positive**

- **A defensible GA in weeks, not months.** Path A removes the dishonest distributed
  claims and delivers a hardened, authenticated, crash-safe single-node product that
  can pass an external pen test and meet single-node SLOs.
- **No throwaway work.** Every Path-A deliverable is a Path-B prerequisite; Path B is
  purely additive wiring on top of the same trait ports.
- **The API surface freezes sooner** (WS-7), capping the breaking-change exposure of
  the already-published SDK.
- **Honesty restored.** Docs, roadmap, and Helm chart all agree with the binary; the
  single-replica constraint is stated as intent, not apology.
- **Clear customer contract.** GA customers get a well-defined single-node appliance
  with known RPO/RTO (WS-10), not a platform whose HA story is aspirational.

**Negative / limitations**

- **No horizontal scale or HA at GA.** Customers needing multi-replica throughput or
  active-active resilience must wait for v1.1. This must be stated in GA marketing
  and sales qualification — mis-selling a single-node appliance as an HA platform
  reintroduces exactly the honesty gap this ADR closes.
- **A single node is a single point of failure.** Mitigated, not eliminated, by
  backup/restore (WS-10) and documented restore RTO; true failover is Path B.
- **The distributed library code carries maintenance cost while dormant** (it must
  still compile and pass its feature-gated tests once CI exercises the feature
  matrix — PRD-003 R-9.1). Accepted: deleting and re-adding it would be more
  expensive than keeping it warm.
- **Scale ceilings remain** (O(N) memory scan, checkpoint growth — PRD-003 PP-14).
  Acceptable for single-node GA corpora; addressed as capacity engineering in v1.1.

**Neutral**

- The trait-port architecture is unchanged; this decision is about *what is wired and
  claimed*, not about interfaces.

---

# Alternatives Considered

- **Path B as GA scope (distributed platform first).** Rejected for the GA milestone.
  It delays any defensible GA by months while the published SDK accretes users and
  API-contract debt hardens, and it pressures teams to fake-complete the hardest,
  least-bounded work (multi-replica correctness, real-scale capacity engineering)
  under a GA deadline. Nothing about Path B is lost by sequencing it second — it is
  purely additive. It becomes the v1.1 milestone.

- **Ship GA as-is and document the gaps as "known limitations."** Rejected. The gaps
  are not limitations, they are defects: unauthenticated access to crypto-shred
  (PP-02), an API that returns `202` for a no-op cancel (PP-15), workflows that never
  resume without a manual CLI invocation (PP-07). Documenting a defect does not make
  it GA-quality.

- **Delete the distributed library code entirely and commit to single-node forever.**
  Rejected. The code works and is tested; the trait ports make Path B tractable; and
  a single-node-only positioning forecloses the enterprise segment the product PRD
  ([PRD-001](../01-product/prd.md)) targets. Keeping it warm behind honest docs costs
  less than re-deriving it.

- **A "hybrid" GA: wire Postgres for control-plane state but stay single-replica.**
  Considered and folded into the decision partially — the migration framework (R-5.3)
  is pulled forward because a Postgres backend already ships. Full control-plane
  promotion is *not* pulled forward, because its value (multi-replica shared state)
  is unrealized without the rest of Path B, so it would be cost without benefit at
  GA.

---

# Related

- [PRD-003 §5](../01-product/prd-ga-hardening.md) — the Path A/B decision this ADR ratifies
- [PRD-002](../01-product/prd-future.md) — post-GA Scale work (Path B's capacity engineering)
- [`18-roadmap/v1.0.md`](../18-roadmap/v1.0.md) — GA milestone and exit criteria
- [ADR-0003](ADR-0003-postgresql.md) — PostgreSQL as system of record (the backend Path B promotes to)

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-06 | Proposed: GA ships as a single-node appliance (Path A); distributed platform (Path B) sequenced as the v1.1 follow-on; migration framework pulled forward into GA |
| 1.1.0 | 2026-07-06 | **Accepted — Path A ratified.** GA scope is the single-node appliance; Track B (distributed) is confirmed as the v1.1 "Scale-Out" milestone, gated on GA shipping. Phase-3 Track-B tickets ([RM-GA-P3](../18-roadmap/v1.0/phase3-scale-distribution-tickets.md)) are now firmly out of GA scope; Track A (migrations + docs honesty) is confirmed GA scope |
