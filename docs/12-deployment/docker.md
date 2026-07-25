<!--
File: docs/12-deployment/docker.md
Document ID: DEP-001
-->

# Docker

**Document ID:** DEP-001  
**File Path:** `docs/12-deployment/docker.md`  
**Version:** 1.1.0  
**Status:** Draft — describes the **long-term, aspirational** per-service
image split (`wovyr/api-gateway`, `wovyr/agent-runtime`, …), a gRPC port, and
NATS/object-storage config. **Not built** — the platform today is one binary
(`wovyr`) built from one Dockerfile
([`deployment/docker/Dockerfile`](../../deployment/docker/Dockerfile), which
does build in CI and takes an optional `FEATURES` build arg), with no gRPC
surface and no NATS dependency anywhere in this workspace. For what actually
builds and runs, see [`docker-compose.md`](docker-compose.md) §10 and
[`deployment/docker-compose.yml`](../../deployment/docker-compose.yml).
**Owner:** Platform Operations Team  
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document describes the container images for the Wovyr AI Platform and how to build and run them with Docker — the foundation for all higher-level topologies.

---

# 2. Image Strategy

| Image | Contents |
|-------|----------|
| `wovyr/platform` | All Rust services in one binary (dev/all-in-one) |
| `wovyr/api-gateway` | API Gateway |
| `wovyr/agent-runtime` | Agent Runtime |
| `wovyr/workflow-engine` | Workflow Engine |
| `wovyr/llm-gateway` | LLM Gateway |
| `wovyr/memory-engine` | Memory Engine |
| `wovyr/tool-runtime` | Tool Runtime (control plane + worker) |
| `wovyr/plugin-engine` | Plugin Engine |
| `wovyr/dashboard` | Angular UI + NestJS BFF |

The single `wovyr/platform` image enables the
[single-binary dev mode](../02-architecture/c4-container.md#7-deployment-models);
per-service images enable independent scaling in production.

---

# 3. Build

Multi-stage builds produce small, static images:

```dockerfile
# Build stage
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --bin api-gateway

# Runtime stage (distroless, non-root)
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/api-gateway /usr/local/bin/
USER nonroot
EXPOSE 8080
ENTRYPOINT ["api-gateway"]
```

Images are **distroless, non-root**, and run a single static binary. Tags follow
semver + git SHA; images are signed (see
[plugin signing parallel](../08-plugin-sdk/distribution.md#3-signing)).

---

# 4. Run (All-in-One)

```bash
docker run --rm -p 8080:8080 \
  -e WOVYR_DATABASE_URL=postgres://... \
  -e WOVYR_REDIS_URL=redis://... \
  -e WOVYR_QDRANT_URL=http://... \
  -e WOVYR_NATS_URL=nats://... \
  wovyr/platform:latest
```

For local evaluation without external state, the all-in-one image can start with
embedded/ephemeral backends (`--profile dev`).

---

# 5. Configuration

All config is environment-driven (12-factor). Common variables:

| Variable | Purpose |
|----------|---------|
| `WOVYR_DATABASE_URL` | PostgreSQL |
| `WOVYR_REDIS_URL` | Redis |
| `WOVYR_QDRANT_URL` | Qdrant |
| `WOVYR_NATS_URL` | NATS JetStream |
| `WOVYR_OBJECT_STORE_*` | Object storage |
| `WOVYR_LOG` | Log level |
| `WOVYR_SECRET_BACKEND` | Secret vault reference |

Secrets are passed via secret references, never baked into images.

---

# 6. Health & Ports

| Endpoint | Purpose |
|----------|---------|
| `/healthz` | Liveness |
| `/readyz` | Readiness (deps reachable) |
| `/metrics` | Prometheus |

Default service port is `8080` (HTTP) and `9090` (gRPC); the dashboard serves on
`3000`.

---

# 7. Resource Sizing (starting points)

| Service | CPU | Memory |
|---------|-----|--------|
| API Gateway | 0.5–2 | 256–512Mi |
| Agent Runtime | 1–4 | 512Mi–2Gi |
| Tool Runtime worker | 1–4 | 1–4Gi (sandbox-dependent) |
| Memory Engine | 1–4 | 1–4Gi |

Tune from observed [metrics](../14-observability/index.md) (planned).

---

# 8. Security

- Non-root, read-only root filesystem, dropped capabilities.
- Tool Runtime workers require elevated sandbox privileges and run on
  dedicated/hardened nodes (see
  [Tool Runtime isolation](../07-tool-runtime/security-isolation.md)).
- Image provenance/SBOM published per release.

---

# 9. Related Documents

- [`12-deployment/docker-compose.md`](docker-compose.md)
- [`12-deployment/kubernetes.md`](kubernetes.md)
- [`12-deployment/index.md`](index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-07 | RM-GA-P3 DOC-A2: marked the per-service image split, gRPC port, and NATS/object-storage config as long-term aspirational and not built — the platform is one binary with no gRPC surface and no NATS dependency; pointed to `docker-compose.md`/`deployment/docker/Dockerfile` for what actually ships |
| 1.0.0 | 2026-06-27 | Initial Docker deployment guide |
