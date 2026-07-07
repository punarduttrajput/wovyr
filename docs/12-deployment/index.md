<!--
File: docs/12-deployment/index.md
Document ID: DEP-INDEX-001
-->

# Deployment Index

**Document ID:** DEP-INDEX-001  
**File Path:** `docs/12-deployment/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation** for deploying the Apex AI Platform. It is the practical, artifact-level guide (Docker, Compose, Kubernetes, Helm, Terraform) that operationalizes the conceptual [Deployment Architecture](../02-architecture/deployment-architecture.md).

---

# 2. Deployment Topologies

| Topology | Use | Guide |
|----------|-----|-------|
| Single binary | Local dev / evaluation | [docker.md](docker.md) |
| Compose stack | Team / small self-host | [docker-compose.md](docker-compose.md) |
| Kubernetes | Production, scalable | [kubernetes.md](kubernetes.md) / [helm.md](helm.md) |
| Cloud infra | Managed datastores + K8s | [terraform.md](terraform.md) |

These map to the deployment models in
[C4 Container §7](../02-architecture/c4-container.md#7-deployment-models).

---

# 3. Components to Deploy

```text
Stateless services        Stateful backends
─────────────────         ─────────────────
API Gateway               PostgreSQL
Agent Runtime             Redis
Workflow Engine           Qdrant
LLM Gateway               Object storage (S3-compatible)
Memory Engine             NATS JetStream
Tool Runtime (+ workers)
Plugin Engine
Dashboard (UI + BFF)
```

Each service exposes `/healthz`, `/readyz`, `/metrics`
([observability](../14-observability/index.md) — planned reference).

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [docker.md](docker.md) | Images, single-binary container, build/run |
| [docker-compose.md](docker-compose.md) | Full local/self-host stack |
| [kubernetes.md](kubernetes.md) | Manifests, scaling, probes, networking |
| [helm.md](helm.md) | Helm chart, values, upgrades |
| [terraform.md](terraform.md) | Cloud infrastructure provisioning |
| [backup-and-restore.md](backup-and-restore.md) | `apex admin backup`/`restore`, KMS root-key escrow, RPO/RTO targets |

---

# 5. Principles

1. **Same artifacts everywhere** — one image set across topologies.
2. **12-factor config** — environment/secret-driven, no baked secrets.
3. **Stateless services, external state** — scale services freely.
4. **Health-gated rollouts** — probes + rolling updates.
5. **Secure by default** — mTLS, secrets via vault, least privilege.

---

# 6. Dependencies

- [`02-architecture/deployment-architecture.md`](../02-architecture/deployment-architecture.md)
- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)

---

# 7. Related Documents

- [`11-cli/commands.md`](../11-cli/commands.md) — `apex deploy`
- [`13-security`](../SUMMARY.md) *(planned)* · [`14-observability`](../SUMMARY.md) *(planned)*

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Deployment Index |
