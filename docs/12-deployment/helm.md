<!--
File: docs/12-deployment/helm.md
Document ID: DEP-004
-->

# Helm

**Document ID:** DEP-004  
**File Path:** `docs/12-deployment/helm.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes the Apex AI Platform **Helm chart** — the packaged, configurable way to install and upgrade the platform on [Kubernetes](kubernetes.md).

---

# 2. Chart Layout

```text
apex/
├── Chart.yaml
├── values.yaml
├── templates/
│   ├── api-gateway/        # deployment, service, hpa
│   ├── agent-runtime/
│   ├── workflow-engine/
│   ├── llm-gateway/
│   ├── memory-engine/
│   ├── tool-runtime/       # control + worker pools
│   ├── plugin-engine/
│   ├── dashboard/
│   ├── ingress.yaml
│   ├── networkpolicies.yaml
│   └── migrations-job.yaml
└── charts/                 # optional subcharts: postgres, redis, qdrant, nats
```

Backends can be installed as subchart dependencies (dev) or pointed at managed
services (production, via [Terraform](terraform.md)).

---

# 3. Install

```bash
helm repo add apex https://charts.apex.example.com
helm install apex apex/apex -n apex --create-namespace -f my-values.yaml
```

---

# 4. Values (excerpt)

```yaml
global:
  image: { registry: ghcr.io/apex-ai, tag: "1.0.0" }
  domain: apex.example.com
  mtls: true

backends:
  postgres: { managed: true, url: "secret://apex/pg-url" }
  redis:    { managed: true, url: "secret://apex/redis-url" }
  qdrant:   { managed: true, url: "http://qdrant:6333" }
  nats:     { managed: true, url: "nats://nats:4222" }

services:
  apiGateway:   { replicas: 3, hpa: { min: 3, max: 20 } }
  memoryEngine: { replicas: 2, hpa: { min: 2, max: 10 } }
  toolRuntime:
    control: { replicas: 2 }
    workers:
      trusted:   { hpa: { min: 2, max: 20 } }
      untrusted: { runtimeClass: gvisor, nodePool: untrusted, hpa: { min: 1, max: 30 } }

dashboard: { enabled: true, replicas: 2 }
ingress:   { enabled: true, className: nginx, tls: true }
```

Values mirror the [Kubernetes](kubernetes.md) workload mapping and tool-worker
isolation.

---

# 5. Secrets

The chart consumes secret references, not literals:

```yaml
secrets:
  backend: external          # external | k8s
  refs:
    databaseUrl: secret://apex/pg-url
    providerKeys: secret://apex/llm-keys
```

With `external`, the chart wires the CSI secrets driver / vault; with `k8s`, it
expects pre-created Secrets.

---

# 6. Upgrades

```bash
helm upgrade apex apex/apex -n apex -f my-values.yaml
helm history apex -n apex
helm rollback apex <revision> -n apex
```

- Pre-upgrade hook runs DB [migrations](docker-compose.md#5-initialization).
- Rolling updates honor readiness probes and worker draining
  ([K8s §9](kubernetes.md#9-rollouts)).
- `helm rollback` reverts to a prior release revision.

---

# 7. Environments

Maintain per-environment values files (`values-dev.yaml`, `values-prod.yaml`) or
use a GitOps tool (Argo CD/Flux) to apply the chart declaratively.

---

# 8. Uninstall

```bash
helm uninstall apex -n apex      # leaves PVCs by default
```

Persistent data (PostgreSQL/Qdrant PVCs, object storage) is retained unless
explicitly removed.

---

# 9. Related Documents

- [`12-deployment/kubernetes.md`](kubernetes.md)
- [`12-deployment/terraform.md`](terraform.md)
- [`12-deployment/index.md`](index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Helm deployment guide |
