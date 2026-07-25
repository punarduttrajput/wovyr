<!--
File: docs/12-deployment/index.md
Document ID: DEP-INDEX-001
-->

# Deployment Index

**Document ID:** DEP-INDEX-001  
**File Path:** `docs/12-deployment/index.md`  
**Version:** 1.2.0  
**Status:** Active  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-07-18

---

# 1. Purpose

This document is the **central navigation** for deploying the Wovyr AI Platform. It is the practical, artifact-level guide (Docker, Compose, Kubernetes, Helm, Terraform) that operationalizes the conceptual [Deployment Architecture](../02-architecture/deployment-architecture.md).

---

# 2. Deployment Topologies

| Topology | Use | Guide |
|----------|-----|-------|
| Single binary | Local dev / evaluation | [docker.md](docker.md) |
| Bare-metal / systemd | Single-node appliance, no container runtime | [systemd.md](systemd.md) |
| Compose stack | Team / small self-host | [docker-compose.md](docker-compose.md) |
| Kubernetes | Production, scalable | [kubernetes.md](kubernetes.md) / [helm.md](helm.md) |
| Cloud infra | Managed datastores + K8s | [terraform.md](terraform.md) |

These map to the deployment models in
[C4 Container §7](../02-architecture/c4-container.md#7-deployment-models).

---

# 3. Components to Deploy

> **Current (v1.0, [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)
> Path A):** every service below runs inside **one `wovyr` binary** — there is
> no independent-service deployment today. The table's per-service split is
> the **aspirational v1.1+ topology**; see §10 of
> [docker-compose.md](docker-compose.md) for what's actually shipped.

```text
Stateless services        Stateful backends
─────────────────         ─────────────────
API Gateway               PostgreSQL (optional — marketplace registry is
Agent Runtime              wired; workflow store is library-only/unwired)
Workflow Engine           Qdrant (optional — CLI-only tiered memory backend;
LLM Gateway                gateway semantic cache is library-only/unwired)
Memory Engine             Redis (library-only gateway breaker sharing;
Tool Runtime (+ workers)   not attached by any shipping binary)
Plugin Engine             Object storage (S3-compatible) — not implemented
Dashboard (UI + BFF)      NATS JetStream — not used anywhere in this workspace
```

Each service exposes `/healthz`, `/readyz`, `/metrics`
([observability](../14-observability/index.md) — planned reference) once the
service split above is actually built; today the single `wovyr` binary
exposes `/healthz` and `/metrics` directly.

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [docker.md](docker.md) | Images, single-binary container, build/run |
| [systemd.md](systemd.md) | Bare-metal appliance install: systemd unit, `install.sh`, env-file config |
| [docker-compose.md](docker-compose.md) | Full local/self-host stack |
| [kubernetes.md](kubernetes.md) | Manifests, scaling, probes, networking |
| [helm.md](helm.md) | Helm chart, values, upgrades |
| [terraform.md](terraform.md) | Cloud infrastructure provisioning |
| [backup-and-restore.md](backup-and-restore.md) | `wovyr admin backup`/`restore`, KMS root-key escrow, RPO/RTO targets |
| [upgrade-and-migration.md](upgrade-and-migration.md) | Operator upgrade runbook: backup → binary swap → `wovyr admin migrate` → verify → rollback, per deployment shape |

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

- [`11-cli/commands.md`](../11-cli/commands.md) — `wovyr deploy`
- [`13-security`](../SUMMARY.md) *(planned)* · [`14-observability`](../SUMMARY.md) *(planned)*

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-14 | Added the bare-metal/systemd topology (RM-AIM-P3 DEP-301): `deployment/install.sh` + `deployment/systemd/*`, new [systemd.md](systemd.md) |
| 1.0.0 | 2026-06-27 | Initial Deployment Index |
