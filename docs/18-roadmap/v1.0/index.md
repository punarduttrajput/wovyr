<!--
File: docs/18-roadmap/v1.0/index.md
Document ID: GA-INDEX-001
-->

# GA-Completion Work (Tier A) — Index

**Document ID:** GA-INDEX-001
**File Path:** `docs/18-roadmap/v1.0/index.md`
**Version:** 1.0.0
**Status:** Active
**Owner:** Product Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

This folder holds one delivery doc per **Tier A workstream** — the concrete,
committed work between the current baseline and a defensible GA, scoped in
[PRD-002 §5](../../01-product/prd-future.md#5-tier-a--completing-10-committed-intent-near-term)
and tracked in the [v1.0 §3](../v1.0.md#3-in-scope) In-Scope table.

Each doc states the **current state** (honestly — several items are partly done),
the **gap**, a **work breakdown**, **exit criteria** (tying back to the
[v1.0 §5](../v1.0.md#5-exit-criteria) GA exit criteria), and the **environment
caveats** that block validation in the current dev environment.

These are **committed GA-completion work**, not the exploratory
[Tier B research bets](../future/index.md). "In progress" here means real code has
already shipped for that workstream.

---

# 2. Register

| Doc | Workstream | PRD ref | Status |
|-----|-----------|---------|--------|
| [GA-001](A1-scale-performance.md) | Scale & Performance Validation | [§5.1](../../01-product/prd-future.md#51-scale--performance-validation) | Planned |
| [GA-002](A2-reliability-ha-dr.md) | Reliability — HA, DR & Deployment Artifacts | [§5.2](../../01-product/prd-future.md#52-reliability-ha-dr-and-deployment-artifacts) | In progress |
| [GA-003](A3-security-completion.md) | Security — Root-of-Trust, PII & External Validation | [§5.3](../../01-product/prd-future.md#53-security-root-of-trust-pii-coverage-and-external-validation) | In progress |
| [GA-004](A4-marketplace-economics.md) | Marketplace Economics & Safety | [§5.4](../../01-product/prd-future.md#54-ecosystem-marketplace-economics--safety) | Planned |
| [GA-005](A5-sdk-distribution.md) | SDK Distribution & Migration Guides | [§5.5](../../01-product/prd-future.md#55-dx-sdk-distribution--migration-guides) | In progress |

---

# 3. Environment Caveats (cross-cutting)

Several Tier A items cannot be *validated* in the current dev environment, even
where the code can be written — flagged per doc, summarized here so the pattern
is visible:

- **GA-001 (scale)** — needs real managed Postgres/Qdrant at scale.
- **GA-002 (HA/DR)** — needs a real orchestrator + `kubectl`/`helm`/`terraform`.
- **GA-003 (security)** — needs a managed KMS/HSM and external audit/pen-test
  vendors; the PII sub-item also needs a `User` resource that doesn't exist yet.
- **GA-004 (marketplace)** — monetization needs a real billing provider (a mock
  proves the trait boundary).
- **GA-005 (SDK)** — npm publish needs a live operator 2FA OTP; migration guides
  are contingent on a first real deprecation.

The honest consequence: these are authored and reasoned about here, but their
exit criteria are met only against real infrastructure/engagements.

---

# 4. Relationship to Tier B

Tier A **completes** GA; [Tier B](../future/index.md) **extends** the platform
beyond it. Prioritization ([PRD-002 §7](../../01-product/prd-future.md#7-prioritization-framework))
is explicit: **Tier A dominates until the [v1.0 §5](../v1.0.md#5-exit-criteria)
exit criteria are met.**

---

# 5. Related Documents

- [`18-roadmap/v1.0.md`](../v1.0.md) — the GA milestone (RM-004)
- [`01-product/prd-future.md`](../../01-product/prd-future.md) — the requirements PRD (PRD-002)
- [`18-roadmap/future/index.md`](../future/index.md) — the Tier B research-bet index

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial Tier A GA-completion index (GA-001…GA-005) with per-workstream status and cross-cutting environment caveats |
