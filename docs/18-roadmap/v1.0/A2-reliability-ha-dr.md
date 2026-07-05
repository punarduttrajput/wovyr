<!--
File: docs/18-roadmap/v1.0/A2-reliability-ha-dr.md
Document ID: GA-002
-->

# GA Completion: Reliability — HA, DR & Deployment Artifacts

**Document ID:** GA-002
**File Path:** `docs/18-roadmap/v1.0/A2-reliability-ha-dr.md`
**Version:** 1.0.0
**Status:** In progress — a first slice (single-node compose) has landed
**Owner:** Reliability / Deployment Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Turn the "Reliability: HA, DR, and Deployment Artifacts" GA gap
([PRD-002 §5.2](../../01-product/prd-future.md#52-reliability-ha-dr-and-deployment-artifacts),
[v1.0 §3 Reliability row](../v1.0.md#3-in-scope)) into a delivery plan.

Committed GA-completion work — a first, real slice already shipped; the HA/DR
remainder is scoped here.

---

# 2. Current State

- **A real single-node deployment shipped.**
  [`deployment/docker-compose.yml`](../../../deployment/docker-compose.yml) runs
  one `apex` binary (built with `tiered-memory,postgres`) + Postgres (backs the
  marketplace registry, genuinely wired) + Qdrant (backs tiered memory). This is
  the *real* build, distinct from the aspirational multi-service C4 topology in
  [docker-compose.md](../../12-deployment/docker-compose.md).
- **Chaos-checked.** A Postgres outage degrades marketplace routes to a clean
  `502`, recovering automatically once Postgres returns, with the rest of the
  server (incl. `/healthz`) unaffected — and this exercise found and fixed a real
  latent crash bug (marketplace routes panicking on a Postgres-backed runtime).
- **CI builds the image** (`container-scan` job) and Trivy-scans it; the
  `Dockerfile` takes a `FEATURES` build arg and has a real `/healthz` healthcheck.
- **K8s / Helm / Terraform are spec-only.**
  [kubernetes.md](../../12-deployment/kubernetes.md),
  [helm.md](../../12-deployment/helm.md), and
  [terraform.md](../../12-deployment/terraform.md) describe the intent; **no
  artifacts exist in the repo.**

---

# 3. Gap

Single-node is not HA. There is no multi-replica deployment, no
backup/restore, and no DR runbook.

---

# 4. Scope & Requirements

## 4.1 Functional / deliverables
- **Kubernetes manifests + Helm chart + Terraform modules** for a multi-replica,
  HA deployment (validated against a real cluster).
- **Backup/restore procedures for every durable store**: the workflow store
  (`~/.apex/workflows`), memory, tenancy, secrets, the KMS tenant-key catalog
  (`~/.apex/kms`), and the marketplace registry (file or `PostgresRegistryStore`).
- A **DR runbook** with explicit RPO/RTO targets and a documented restore drill.

## 4.2 Non-functional
- The artifacts deploy the *actual* built binary/features, not the aspirational
  topology — same honesty bar the compose slice set.
- Backup covers the KMS catalog specifically: losing it makes every sealed
  secret/memory/webhook-secret unrecoverable (crypto-shred by accident).

---

# 5. Exit Criteria

> A **node-loss drill** and a **full-restore drill** both pass without data loss
> beyond the stated RPO, on a real multi-replica cluster.

Feeds the v1.0 exit criterion of meeting published SLOs in production
([v1.0 §5](../v1.0.md#5-exit-criteria)).

---

# 6. Dependencies & Environment Caveats

- **Requires a real orchestrator** and `kubectl` / `helm` / `terraform` —
  **none present in the current dev environment**, so the K8s/Helm/Terraform
  artifacts cannot be authored *and validated* here (authoring blind, without a
  cluster to test against, would repeat the "spec-only" problem this work exists
  to fix).
- KMS backup interacts with GA-003's cloud-KMS/HSM root
  ([A3](A3-security-completion.md)): a managed root changes what must be backed up.

---

# 7. Risks

| Risk | Mitigation |
|------|-----------|
| Authoring K8s/Helm/TF blind (no cluster) | Gate on real-cluster validation; don't ship unvalidated manifests as "done" |
| KMS-catalog loss = silent data loss | Treat the KMS catalog as a first-class backup target with a restore drill |
| Compose topology mistaken for HA | Doc clearly separates the shipped single-node slice from the HA remainder |

---

# 8. Related Documents

- [`01-product/prd-future.md`](../../01-product/prd-future.md) §5.2 — requirements
- [`18-roadmap/v1.0.md`](../v1.0.md) — Reliability row + §5 exit criteria
- [`12-deployment/docker-compose.md`](../../12-deployment/docker-compose.md) ·
  [kubernetes.md](../../12-deployment/kubernetes.md) ·
  [helm.md](../../12-deployment/helm.md) ·
  [terraform.md](../../12-deployment/terraform.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for reliability (HA/DR/deployment artifacts); records the shipped single-node compose slice and scopes the HA remainder |
