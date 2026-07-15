<!--
File: docs/18-roadmap/v1.1/index.md
Document ID: RM-AIM-INDEX
-->

# v1.1 — AI Platform Maturity & Production Readiness — Index

**Document ID:** RM-AIM-INDEX
**File Path:** `docs/18-roadmap/v1.1/index.md`
**Version:** 1.2.0
**Status:** In progress — Phase 1 done (WFL-104 lease fencing deferred), Phase 2
done, Phase 3 well underway: ECO-301/302/304, all of WS-H (WFL-301..308), all of
WS-E (SBX-301..304), SRV-302..307, and DEP-301 are done; the UI (UI-301..306),
DX (DX-301..306), and observability (OBS-301/302) tracks plus ECO-303/305 and
DEP-302 remain
**Owner:** Product / AI Engineering
**Last Updated:** 2026-07-15

---

# 1. Purpose

This folder holds the implementation tickets for [PRD-004](../../01-product/prd-ai-platform-maturity.md)
(AI Platform Maturity), the milestone after the v1.0 GA hardening
([PRD-003](../../01-product/prd-ga-hardening.md), shipped). Where v1.0 made the
appliance *safe and honest*, v1.1 makes it a *capable, operable, extensible AI
product*.

The tickets derive from a 2026-07-09 five-front engineering audit (AI core,
workflow+server, dashboard UI, tools/plugins/sandbox, DX/CI/deployment/docs) — ~90
findings, mapped in [PRD-004 §12](../../01-product/prd-ai-platform-maturity.md#12-traceability-matrix--findings--requirements--tickets).

Ticket format matches the v1.0 phase docs
([RM-GA-P2](../v1.0/phase2-durability-execution-tickets.md) is the reference):
problem with file:line evidence, the change, acceptance criteria, files,
dependencies, size (S ≈ ≤2 days, M ≈ 3–5 days, L ≈ 1–2 weeks).

---

# 2. Phase Register

| Phase | Doc | Theme | Tickets |
|-------|-----|-------|---------|
| 1 | [RM-AIM-P1](phase1-production-truth-tickets.md) | Make production claims true (P0/P1) | AIC-101..103, PRV-101, SBX-101/102, SRV-101..104, WFL-101..104, SEC-101, DX-101..103, UI-101/102 |
| 2 | [RM-AIM-P2](phase2-credible-ai-product-tickets.md) | Credible AI product (P1/P2) | AIC-201/202, PRV-201..205, RAG-201..205, EVL-201..203, SRV-201..203, SAF-201/202, RUN-201/202, OBS-201 |
| 3 | [RM-AIM-P3](phase3-ecosystem-scale-tickets.md) | Ecosystem & scale (P2/P3) | ECO-301..305, SBX-301..304, WFL-301..308, SRV-302..307, SEC-301/302, RAG-301, UI-301..306, DX-301..306, DEP-301/302, OBS-301/302 |

---

# 3. Sequencing

- **Phase 1 first, and within it `PRV-101` (cost table) first of all** — it silently
  disables quota enforcement across the whole platform, so every cost/quota number
  is fiction until it lands.
- Phase 1 is dominated by "wire the good primitive onto the path" fixes (sandbox
  activation, execution driver hardening, release reconciliation) — low-risk,
  high-credibility.
- Phase 2 depends on Phase 1's cost/context work (eval and quotas need real cost;
  RAG reranking benefits from the provider work). Phase 3 is the broad, parallel
  ecosystem/DX/UI tail.

---

# 4. Scale-Out fold-in

PRD-003 earmarked a "v1.1 Scale-Out" (distributed Path B). Its single-node-
correctness subset (Postgres pooling/TLS/fencing, distributed rate limiting) is
committed here as Phase 1/2 P0/P1 tickets; the full multi-replica shared-catalog
promotion (PRD-003 R-5.1/R-5.2) remains **gated on Product demand** and is tracked,
not committed, pending that decision. See
[PRD-004 §7](../../01-product/prd-ai-platform-maturity.md#7-distributed-scale-out-folded-from-prd-003-path-b).

---

# 5. Related

- [PRD-004](../../01-product/prd-ai-platform-maturity.md) — the requirements PRD
- [v1.0 GA hardening](../v1.0/index.md) — the milestone this builds on
- [`18-roadmap/index.md`](../index.md) — the roadmap top-level

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-09 | Initial v1.1 milestone index: three phase ticket docs derived from PRD-004 / the 2026-07-09 engineering audit |
| 1.2.0 | 2026-07-15 | Status refresh to match the Phase 3 ticket doc: 22 of 39 P3 tickets done (ecosystem, workflow-scale, sandbox, server tracks + DEP-301); UI/DX/OBS tracks, ECO-303/305, DEP-302 remain |
| 1.1.0 | 2026-07-14 | Status refresh: Phase 1 complete (WFL-104 lease fencing deferred within its ticket), Phase 2 complete (SAF-202 was the last ticket); Phase 3 remains planned |
