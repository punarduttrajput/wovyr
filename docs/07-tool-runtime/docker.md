<!--
File: docs/07-tool-runtime/docker.md
Document ID: TRT-105
-->

# Built-in Tool: Docker

**Document ID:** TRT-105  
**File Path:** `docs/07-tool-runtime/docker.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `docker.*` built-in tools let agents build and run containers for tasks that
need a containerized environment. This is a **high-privilege** tool, disabled by
default and confined to hardened worker pools.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `docker.build` | Build an image from a context |
| `docker.run` | Run a container, capture output |
| `docker.images` | List images |
| `docker.rm` | Remove containers/images |

---

# 3. Schema (example: `docker.run`)

```json
// input
{ "image": "node:20", "cmd": ["node","-v"], "timeout_ms": 60000 }
// output
{ "exit_code": 0, "stdout": "v20.x", "stderr": "" }
```

---

# 4. Permissions

```text
docker:run | docker:build
net:egress:<registry>     (image pull)
```

Granting Docker effectively grants container execution; it requires a high-trust
grant and tenant approval.

---

# 5. Sandbox & Safety

- **Not** run via the host Docker socket. Container workloads execute on the
  isolated **untrusted worker pool** with a strong runtime
  (gVisor/Kata) ([backends](sandbox-runtime.md#2-isolation-backends),
  [K8s isolation](../12-deployment/kubernetes.md#6-tool-worker-isolation)).
- Image provenance/signatures are verified before run
  ([supply chain](security-isolation.md#9-supply-chain--provenance)).
- Resource limits and egress allowlists apply as for any tool.
- Disabled by default; enable per project explicitly
  ([tool enablement](../09-api/tools.md#8-enablement)).

---

# 6. Determinism & Caching

Side-effecting; never cached. Pin image digests for reproducibility.

---

# 7. Example

```bash
wovyr tools enable docker.run --project ci
wovyr tools invoke docker.run --input '{"image":"alpine","cmd":["echo","hi"]}'
```

---

# 8. Related

- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`12-deployment/kubernetes.md`](../12-deployment/kubernetes.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Docker tool spec |
