<!--
File: docs/07-tool-runtime/kubernetes.md
Document ID: TRT-106
-->

# Built-in Tool: Kubernetes

**Document ID:** TRT-106  
**File Path:** `docs/07-tool-runtime/kubernetes.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `k8s.*` built-in tools let operations agents inspect and (when explicitly
granted) manage Kubernetes resources — for diagnostics, deployments, and
remediation. High-privilege; read-only by default.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `k8s.get` | Get resources (pods, deployments, …) |
| `k8s.logs` | Fetch pod logs |
| `k8s.describe` | Describe a resource |
| `k8s.apply` | Apply a manifest (guarded) |
| `k8s.scale` | Scale a workload (guarded) |
| `k8s.rollout` | Restart/status a rollout (guarded) |

---

# 3. Schema (example: `k8s.get`)

```json
// input
{ "context": "secret://acme/kubeconfig-staging", "kind": "pods", "namespace": "wovyr" }
// output
{ "items": [ { "name": "api-gateway-...", "status": "Running" } ] }
```

`context` is a **secret reference** to a scoped kubeconfig.

---

# 4. Permissions

```text
secret:read:<kubeconfig-ref>     net:egress:<cluster-api>
k8s:read                          (get/logs/describe)
k8s:write                         (apply/scale/rollout)
```

Write operations require `k8s:write` plus a scoped kubeconfig with limited RBAC on
the target cluster — least privilege on **both** the platform and cluster side.

---

# 5. Sandbox & Safety

- Egress restricted to the granted cluster API endpoint.
- The kubeconfig's own RBAC bounds what the tool can do (defense in depth).
- Mutating ops are typically gated behind a
  [workflow approval](../09-api/workflows.md#8-human-tasks).
- Disabled by default; enable per project.

---

# 6. Determinism & Caching

Read ops may be cached briefly; mutating ops never cached.

---

# 7. Example

```bash
wovyr tools invoke k8s.get --input '{"context":"secret://acme/kubeconfig-staging","kind":"deployments","namespace":"wovyr"}'
```

---

# 8. Related

- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`13-security/secret-management.md`](../13-security/secret-management.md)
- [`12-deployment/kubernetes.md`](../12-deployment/kubernetes.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Kubernetes tool spec |
