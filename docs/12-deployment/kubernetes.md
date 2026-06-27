<!--
File: docs/12-deployment/kubernetes.md
Document ID: DEP-003
-->

# Kubernetes

**Document ID:** DEP-003  
**File Path:** `docs/12-deployment/kubernetes.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes deploying the Apex AI Platform on Kubernetes — the recommended production topology with independent scaling, health-gated rollouts, and isolated tool execution.

---

# 2. Workload Mapping

| Service | Workload | Scaling |
|---------|----------|---------|
| API Gateway | Deployment | HPA (CPU/RPS) |
| Agent Runtime | Deployment | HPA |
| Workflow Engine | Deployment | HPA |
| LLM Gateway | Deployment | HPA |
| Memory Engine | Deployment | HPA (read-heavy) |
| Tool Runtime (control) | Deployment | HPA |
| Tool Runtime (workers) | Deployment / DaemonSet | HPA + node pools |
| Plugin Engine | Deployment | HPA |
| Dashboard | Deployment | HPA |

Stateful backends (PostgreSQL, Redis, Qdrant, NATS) run as operators/StatefulSets
or managed services (see [Terraform](terraform.md)).

---

# 3. Probes

Every service exposes standard endpoints; map them to probes:

```yaml
livenessProbe:  { httpGet: { path: /healthz, port: 8080 }, periodSeconds: 10 }
readinessProbe: { httpGet: { path: /readyz,  port: 8080 }, periodSeconds: 5 }
```

Readiness gates traffic until dependencies (DB, NATS) are reachable.

---

# 4. Deployment Example

```yaml
apiVersion: apps/v1
kind: Deployment
metadata: { name: api-gateway }
spec:
  replicas: 3
  selector: { matchLabels: { app: api-gateway } }
  template:
    metadata: { labels: { app: api-gateway } }
    spec:
      securityContext: { runAsNonRoot: true, readOnlyRootFilesystem: true }
      containers:
        - name: api-gateway
          image: apex/api-gateway:1.0.0
          ports: [{ containerPort: 8080 }, { containerPort: 9090 }]
          envFrom: [{ secretRef: { name: apex-config } }]
          resources:
            requests: { cpu: "500m", memory: "256Mi" }
            limits:   { cpu: "2",    memory: "512Mi" }
```

---

# 5. Autoscaling

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata: { name: tool-runtime-worker }
spec:
  scaleTargetRef: { kind: Deployment, name: tool-runtime-worker }
  minReplicas: 2
  maxReplicas: 50
  metrics:
    - type: Pods
      pods: { metric: { name: tool_queue_seconds }, target: { type: AverageValue, averageValue: "0.05" } }
```

Tool worker pools autoscale on queue wait
([Worker Pool §6](../07-tool-runtime/worker-pool.md#6-autoscaling)); the cluster
autoscaler adds nodes for the **untrusted/microVM** pool separately.

---

# 6. Tool Worker Isolation

- Untrusted tool workers run on **dedicated node pools** (taints/tolerations) with
  gVisor/Kata runtime classes for strong isolation
  ([Sandbox backends](../07-tool-runtime/sandbox-runtime.md#2-isolation-backends)).
- `runtimeClassName: gvisor` (or `kata`) is set on untrusted worker pods.

```yaml
spec:
  runtimeClassName: gvisor
  nodeSelector: { apex.io/pool: untrusted }
  tolerations: [{ key: apex.io/untrusted, operator: Exists }]
```

---

# 7. Networking

- An Ingress (or Gateway API) routes external traffic to the API Gateway and
  Dashboard ([deployment architecture](../02-architecture/deployment-architecture.md)).
- **mTLS** between services via a service mesh or native TLS.
- NetworkPolicies enforce least-privilege east-west traffic; tool egress is
  controlled per [Tool Runtime network isolation](../07-tool-runtime/security-isolation.md#5-network-isolation).

---

# 8. Configuration & Secrets

- Config via ConfigMaps; secrets via Kubernetes Secrets backed by an external
  vault (e.g. CSI secrets driver).
- Provider keys and DB credentials are mounted as secret references, never in
  manifests.

---

# 9. Rollouts

- Rolling updates with `maxUnavailable: 0`, gated by readiness probes.
- Workers **drain** in-flight executions before termination
  (`terminationGracePeriodSeconds` aligned to max tool timeout).
- DB migrations run as a pre-deploy Job/initContainer.

---

# 10. Observability

- ServiceMonitors scrape `/metrics`; OpenTelemetry collector gathers traces.
- Dashboards/alerts per [Observability](../14-observability/index.md) (planned).

---

# 11. Related Documents

- [`12-deployment/helm.md`](helm.md)
- [`12-deployment/terraform.md`](terraform.md)
- [`12-deployment/index.md`](index.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Kubernetes deployment guide |
