<!--
File: docs/12-deployment/terraform.md
Document ID: DEP-005
-->

# Terraform

**Document ID:** DEP-005  
**File Path:** `docs/12-deployment/terraform.md`  
**Version:** 1.2.0  
**Status:** Draft — **spec-only, zero artifacts, deliberately (see the decision
note below).** No `.tf` files exist in
this repository yet, and this document describes the **long-term,
aspirational** multi-service cloud topology (a Kubernetes cluster + managed
Postgres/Redis/Qdrant/NATS + object storage), which mirrors the equally
aspirational [kubernetes.md](kubernetes.md)/[helm.md](helm.md) topology, not
what [`deployment/helm/wovyr/`](../../deployment/helm/wovyr/README.md) or
[`deployment/docker-compose.yml`](../../deployment/docker-compose.yml)
actually deploy today (single binary + optional Postgres + optional Qdrant,
no Redis, no NATS). Provision the real v1.0 topology's infrastructure by
hand or with generic Postgres/Kubernetes Terraform modules until this
document has real artifacts behind it.
**Owner:** Platform Operations Team  
**Last Updated:** 2026-07-18

> **Decision (RM-AIM-P3 DEP-302, 2026-07-18): first-party Terraform artifacts
> are scoped out for the current single-node topology.** What ships today — one
> binary on a PVC/host directory plus optional stock Postgres/Qdrant — is
> exactly what generic, battle-tested modules (a managed-Postgres module, a
> cluster module, or plain `helm_release`) already provision; a first-party
> module would wrap those with no Wovyr-specific logic and immediately go stale
> against the aspirational topology below. Revisit when the multi-service
> split (§2) actually exists to encode. Until then: provision infrastructure
> with your cloud's standard modules and deploy via
> [`deployment/helm/wovyr/`](../../deployment/helm/wovyr/README.md),
> [compose](docker-compose.md), or [systemd](systemd.md); the operator
> upgrade path is covered by
> [upgrade-and-migration.md](upgrade-and-migration.md).

---

# 1. Purpose

This document describes provisioning the **cloud infrastructure** for the Wovyr AI Platform with Terraform — the Kubernetes cluster, managed datastores, networking, and secrets that the [Helm](helm.md) release runs on.

---

# 2. Scope

Terraform provisions the *infrastructure*; Helm deploys the *application*. The
boundary:

```text
Terraform                         Helm
─────────                         ────
K8s cluster + node pools          Wovyr services
Managed PostgreSQL                (consumes DB URL)
Managed Redis                     (consumes Redis URL)
Qdrant (managed/self-hosted)      Memory Engine config
NATS / managed messaging          Event bus config
Object storage bucket             Artifacts/archives
DNS, TLS certs, ingress LB        Ingress
Secrets manager + IAM             Secret references
```

---

# 3. Module Structure

```text
infra/
├── main.tf
├── variables.tf
├── outputs.tf
└── modules/
    ├── network/        # VPC, subnets, security groups
    ├── kubernetes/     # cluster + node pools (incl. untrusted pool)
    ├── postgres/       # managed PostgreSQL (primary + replica)
    ├── redis/          # managed Redis
    ├── qdrant/         # Qdrant cluster
    ├── messaging/      # NATS / managed equivalent
    ├── objectstore/    # S3-compatible bucket
    └── secrets/        # secrets manager + IAM bindings
```

The provider is cloud-agnostic in shape; concrete modules target a specific cloud
(AWS/GCP/Azure).

---

# 4. Node Pools

The cluster module provisions separate pools matching the
[tool-worker isolation](kubernetes.md#6-tool-worker-isolation) model:

| Pool | Purpose | Notes |
|------|---------|-------|
| `system` | Control-plane services | General compute |
| `services` | Stateless platform services | Autoscaled |
| `untrusted` | Tool Runtime untrusted workers | Tainted; gVisor/Kata; isolated |
| `gpu` (optional) | ML/heavy tools | GPU nodes |

---

# 5. Example (excerpt)

```hcl
module "kubernetes" {
  source       = "./modules/kubernetes"
  cluster_name = "wovyr-prod"
  node_pools = {
    services  = { min = 3, max = 30, machine = "standard-4" }
    untrusted = { min = 1, max = 30, machine = "standard-4", taint = "wovyr.io/untrusted", runtime = "gvisor" }
  }
}

module "postgres" {
  source     = "./modules/postgres"
  ha         = true
  storage_gb = 200
}

output "db_url"    { value = module.postgres.connection_url, sensitive = true }
output "bucket"    { value = module.objectstore.name }
```

Sensitive outputs (DB URL, keys) are written to the secrets manager and surfaced to
Helm as [secret references](helm.md#5-secrets), never as plaintext values.

---

# 6. State & Workflow

```bash
terraform init      # remote backend (versioned, locked state)
terraform plan -var-file=prod.tfvars
terraform apply  -var-file=prod.tfvars
```

- Use a **remote, locked backend** for state.
- Maintain per-environment var files (`dev.tfvars`, `prod.tfvars`).
- Drive via CI with plan review before apply.

---

# 7. Secrets & IAM

- A secrets manager (cloud-native or Vault) stores DB credentials, provider keys,
  and signing keys.
- Workloads receive scoped IAM identities (e.g. IRSA/workload identity) to read
  only their secrets and object-store prefixes — least privilege.

---

# 8. Backups & DR

- Managed PostgreSQL: automated backups + PITR; cross-region replica for DR.
- Object storage: versioning + lifecycle rules.
- Qdrant/Redis are rebuildable
  ([Memory storage §9](../06-memory-engine/storage-architecture.md#9-reindex--recovery)),
  reducing DR scope to the system of record.

---

# 9. Hand-off to Helm

After `apply`, Terraform outputs feed the
[Helm values](helm.md#4-values-excerpt) (cluster credentials, backend URLs as
secret refs, bucket name), completing infra → application deployment.

---

# 10. Related Documents

- [`12-deployment/helm.md`](helm.md)
- [`12-deployment/kubernetes.md`](kubernetes.md)
- [`02-architecture/deployment-architecture.md`](../02-architecture/deployment-architecture.md)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-18 | RM-AIM-P3 DEP-302: recorded the explicit decision to scope out first-party Terraform artifacts for the single-node topology (generic modules + Helm/compose/systemd suffice; revisit at the multi-service split) |
| 1.1.0 | 2026-07-07 | RM-GA-P3 DOC-A2: marked as spec-only/zero-artifacts and the described topology as long-term aspirational, distinct from what `deployment/helm/wovyr/`/`deployment/docker-compose.yml` actually deploy today |
| 1.0.0 | 2026-06-27 | Initial Terraform deployment guide |
