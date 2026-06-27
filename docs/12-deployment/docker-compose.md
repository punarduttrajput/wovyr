<!--
File: docs/12-deployment/docker-compose.md
Document ID: DEP-002
-->

# Docker Compose

**Document ID:** DEP-002  
**File Path:** `docs/12-deployment/docker-compose.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes running the full Apex AI Platform with Docker Compose — the recommended path for team self-hosting and integration testing, bundling the services with their stateful backends.

---

# 2. Topology

```text
dashboard ─► api-gateway ─► agent-runtime · workflow-engine · llm-gateway
                            memory-engine · tool-runtime · plugin-engine
                                  │
        postgres · redis · qdrant · nats · minio (object storage)
```

This is the modular-monolith/team model from
[C4 Container §7](../02-architecture/c4-container.md#7-deployment-models).

---

# 3. Compose File (excerpt)

```yaml
services:
  api-gateway:
    image: apex/api-gateway:latest
    ports: ["8080:8080"]
    environment:
      APEX_DATABASE_URL: postgres://apex:apex@postgres:5432/apex
      APEX_REDIS_URL: redis://redis:6379
      APEX_QDRANT_URL: http://qdrant:6333
      APEX_NATS_URL: nats://nats:4222
    depends_on: [postgres, redis, qdrant, nats]

  memory-engine:
    image: apex/memory-engine:latest
    environment:
      APEX_QDRANT_URL: http://qdrant:6333
      APEX_DATABASE_URL: postgres://apex:apex@postgres:5432/apex

  postgres:
    image: postgres:16
    environment: { POSTGRES_USER: apex, POSTGRES_PASSWORD: apex, POSTGRES_DB: apex }
    volumes: ["pgdata:/var/lib/postgresql/data"]

  redis:    { image: redis:7 }
  qdrant:   { image: qdrant/qdrant:latest, volumes: ["qdrant:/qdrant/storage"] }
  nats:     { image: nats:latest, command: "-js" }   # JetStream
  minio:    { image: minio/minio, command: "server /data", volumes: ["minio:/data"] }

volumes: { pgdata: {}, qdrant: {}, minio: {} }
```

The full file declares every service; this excerpt shows the shape.

---

# 4. Bring-Up

```bash
apex deploy --target compose          # CLI convenience wrapper
# or directly:
docker compose up -d
docker compose ps
```

Compose starts backends first (via `depends_on` + healthchecks), then services.

---

# 5. Initialization

```bash
docker compose exec api-gateway apex-migrate up    # DB schema migrations
docker compose exec api-gateway apex-seed admin    # bootstrap first admin
```

Migrations are idempotent and run automatically on service start unless
`APEX_AUTO_MIGRATE=false`.

---

# 6. Configuration & Secrets

- Use a `.env` file or Compose `env_file` for configuration.
- For secrets, reference an external secret backend
  (`APEX_SECRET_BACKEND`) rather than committing values.
- Provider API keys (OpenAI/Anthropic/…) are injected as secret references
  consumed by the [LLM Gateway](../05-llm-gateway/overview.md#11-security).

---

# 7. Persistence & Backups

| Volume | Backs |
|--------|-------|
| `pgdata` | System of record (critical) |
| `qdrant` | Vector index (rebuildable from Postgres) |
| `minio` | Artifacts, archives |

Back up PostgreSQL regularly; Qdrant/Redis are rebuildable
([Memory storage §9](../06-memory-engine/storage-architecture.md#9-reindex--recovery)).

---

# 8. Profiles

| Profile | Includes |
|---------|----------|
| `core` | Services + backends |
| `observability` | + Prometheus + Grafana |
| `dev` | Hot-reload, seed data |

```bash
docker compose --profile observability up -d
```

---

# 9. Limitations

Compose suits single-host deployments. For HA, autoscaling, and isolated tool
workers, use [Kubernetes](kubernetes.md).

---

# 10. Related Documents

- [`12-deployment/docker.md`](docker.md)
- [`12-deployment/kubernetes.md`](kubernetes.md)
- [`12-deployment/index.md`](index.md)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Docker Compose deployment guide |
