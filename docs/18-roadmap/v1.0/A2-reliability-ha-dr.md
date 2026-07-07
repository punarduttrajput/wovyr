<!--
File: docs/18-roadmap/v1.0/A2-reliability-ha-dr.md
Document ID: GA-002
-->

# GA Completion: Reliability — HA, DR & Deployment Artifacts

**Document ID:** GA-002
**File Path:** `docs/18-roadmap/v1.0/A2-reliability-ha-dr.md`
**Version:** 1.2.0
**Status:** In progress — a first slice (single-node compose) has landed, plus
a first Kubernetes artifact (a Helm chart for that same single-node topology,
§2), and backup/restore + DR targets are now real for the single-node
appliance (§2, RM-GA-P2 DR-1001/DR-1002/DR-1003). Neither the Kubernetes
chart nor the DR targets have been validated against a real multi-replica
cluster — still gated per §7's own risk note.
**Owner:** Reliability / Deployment Team
**Last Updated:** 2026-07-07

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
- **A real Helm chart now exists**: [`deployment/helm/apex/`](../../../deployment/helm/apex/README.md)
  deploys the *same* single-node topology as compose (one `apex`
  `StatefulSet` fixed at 1 replica + Postgres + Qdrant `StatefulSet`s) —
  **not** multi-replica/HA, and **not validated against a real cluster**
  (none exists in this dev environment). What it *is* validated against:
  portable `kubectl`/`helm`/`kubeconform` binaries downloaded specifically
  for this, running fully offline (`helm lint`, `helm template`, and
  `kubeconform` schema-checking all 9 rendered resources against the real
  Kubernetes OpenAPI definitions — no apiserver needed for any of these).
  This caught and fixed a real bug (duplicate `app.kubernetes.io/name`/
  `instance` label keys) before it could reach a real manifest — genuine
  value, but explicitly *not* the real-cluster validation §7's risk table
  calls for.
- **Helm/Terraform for a multi-service, multi-replica HA topology remain
  spec-only.** [kubernetes.md](../../12-deployment/kubernetes.md) and
  [helm.md](../../12-deployment/helm.md) describe a materially bigger
  aspirational architecture (independent api-gateway/agent-runtime/
  workflow-engine/… services, each with an HPA); the platform is still one
  binary. [terraform.md](../../12-deployment/terraform.md) has no artifacts
  at all yet.
- **Backup/restore and DR targets are now real for the single-node appliance
  (Phase-2 DR-1001/DR-1002/DR-1003)** — the single-node slice of this
  document's §4.1 "backup/restore procedures" and "DR runbook with RPO/RTO
  targets" deliverables, done ahead of the multi-replica remainder they were
  originally scoped alongside: `apex admin backup`/`restore` snapshots and
  restores the *entire* `~/.apex` state directory (agents, secrets, memory,
  workflows, tenancy, the KMS tenant-key catalog, the marketplace registry,
  …) in one pass, quiescing every DUR-403-locked store directory for a
  consistent point-in-time copy; the KMS root key has a documented, mandatory
  escrow step (`APEX_KMS_ROOT_KEY`) with its own proven restore test. RPO
  (≤15 min, backup-cadence-driven) and RTO (<5 min restore) targets for this
  topology are defined and validated by a real timed drill at two data
  scales (425 files/8.8 MiB → 1.9 s restore; 4,025 files/74.5 MiB → 17.0 s
  restore) — see
  [backup-and-restore.md](../../12-deployment/backup-and-restore.md). This
  closes the single-node portion of §5's exit criterion; the **multi-replica,
  real-cluster** "node-loss drill" §5 also requires remains open, gated on
  the same missing live cluster as the rest of this document's HA remainder.

---

# 3. Gap

Single-node is not HA. There is no multi-replica deployment, no
backup/restore, and no DR runbook.

---

# 4. Scope & Requirements

## 4.1 Functional / deliverables
- **Kubernetes manifests + Helm chart + Terraform modules** for a multi-replica,
  HA deployment (validated against a real cluster). A **single-replica Helm
  chart for the current single-binary topology** now exists (§2) as a first
  step, offline-validated only — the multi-replica/HA version and the
  real-cluster validation are both still open.
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

- **Still requires a real orchestrator to fully validate.** `kubectl`/`helm`
  are no longer unavailable here — portable binaries were downloaded
  specifically to author §2's chart, plus `kubeconform` for full OpenAPI
  schema validation — so manifests are no longer authored *completely* blind.
  But none of that substitutes for a **live cluster**: no apiserver exists in
  this environment, so scheduling behavior, PVC provisioning, actual pod
  startup ordering, and the HA/multi-replica remainder of this deliverable
  still cannot be validated here. `terraform` itself remains undownloaded/
  untried.
- KMS backup interacts with GA-003's cloud-KMS/HSM root
  ([A3](A3-security-completion.md)): a managed root changes what must be backed up.

---

# 7. Risks

| Risk | Mitigation |
|------|-----------|
| Authoring K8s/Helm/TF blind (no cluster) | Partially mitigated for Helm: `helm lint`/`helm template`/`kubeconform` (downloaded binaries) catch structural and schema errors offline — already caught one real bug. Still gate on real-cluster validation before calling this deliverable done; don't let offline validation be mistaken for it |
| KMS-catalog loss = silent data loss | Treat the KMS catalog as a first-class backup target with a restore drill |
| Compose topology mistaken for HA | Doc clearly separates the shipped single-node slice from the HA remainder |

---

# 8. Related Documents

- [`01-product/prd-future.md`](../../01-product/prd-future.md) §5.2 — requirements
- [`18-roadmap/v1.0.md`](../v1.0.md) — Reliability row + §5 exit criteria
- [`12-deployment/docker-compose.md`](../../12-deployment/docker-compose.md) ·
  [kubernetes.md](../../12-deployment/kubernetes.md) ·
  [helm.md](../../12-deployment/helm.md) ·
  [terraform.md](../../12-deployment/terraform.md) ·
  [backup-and-restore.md](../../12-deployment/backup-and-restore.md)
- [`deployment/helm/apex/README.md`](../../../deployment/helm/apex/README.md) — the chart itself

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-07 | Recorded the single-node slice of §4.1's backup/restore + DR-runbook deliverables as done (RM-GA-P2 DR-1001/DR-1002/DR-1003): `apex admin backup`/`restore`, mandatory KMS root-key escrow, and RPO/RTO targets validated by a real timed drill — see [backup-and-restore.md](../../12-deployment/backup-and-restore.md). Updated §2 to record it; §5's exit criterion remains unmet for the multi-replica/real-cluster case, which this doesn't address |
| 1.1.0 | 2026-07-05 | Recorded the first real Kubernetes artifact: `deployment/helm/apex/` (single-replica Helm chart for the existing single-binary topology), offline-validated with downloaded `helm`/`kubectl`/`kubeconform` (caught a real duplicate-label bug). Updated §2/§4.1/§6/§7 to state plainly this is not HA and not validated against a real cluster — the exit criterion in §5 is unchanged and unmet |
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for reliability (HA/DR/deployment artifacts); records the shipped single-node compose slice and scopes the HA remainder |
