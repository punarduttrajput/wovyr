<!--
File: docs/12-deployment/docker-compose.md
Document ID: DEP-002
-->

# Docker Compose

**Document ID:** DEP-002  
**File Path:** `docs/12-deployment/docker-compose.md`  
**Version:** 1.1.0  
**Status:** Draft — §3's topology is the aspirational C4 multi-service split
(future milestone); a real, working compose file exists today at
[`deployment/docker-compose.yml`](../../deployment/docker-compose.yml) for
what's actually built: the single `apex` binary + Postgres + Qdrant. See §12.  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-07-05

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

# 10. Implemented Today

§3's topology (separate `api-gateway`/`memory-engine`/… images) is a future
milestone — the actual v0.1 deployable artifact is one binary (`apex`, via
[`deployment/docker/Dockerfile`](../../deployment/docker/Dockerfile)), not a
microservice split. [`deployment/docker-compose.yml`](../../deployment/docker-compose.yml)
reflects that reality:

```bash
make compose-up      # or: docker compose -f deployment/docker-compose.yml up -d --build
curl http://localhost:8080/healthz
make compose-down
```

- **`apex`** — the embedded single-node server (`apex dev`), built with the
  `tiered-memory,postgres` cargo features.
- **`postgres`** — backs the **marketplace registry** (`PostgresRegistryStore`,
  selected when `APEX_MARKETPLACE_POSTGRES_URL` is set): this *is* wired into
  the running server. Verified live, including a chaos check — stopping
  Postgres mid-flight makes marketplace routes fail closed with a clean `502
  provider_error` (not a crash; `apex`'s own `/healthz` stays unaffected, since
  it doesn't depend on Postgres), and restarting Postgres recovers on the very
  next request with no `apex` restart needed (each call opens a fresh
  connection rather than holding a pool).
- **`qdrant`** — backs the **tiered memory** store (Postgres + Qdrant), but
  that integration is CLI-only today (`apex memory put/query`, built with
  `tiered-memory`) — `apex dev`'s embedded server does not route memory
  through it, always using the local file store instead. Exercise it against
  the same compose network with `docker compose -f
  deployment/docker-compose.yml run --rm apex memory put --namespace demo
  --content "hello"`.

This pass also found and fixed a real bug: the sync `postgres` crate's
`Client` drives its own internal Tokio runtime for every call (including
`connect`), which panics ("Cannot start a runtime from within a runtime")
when invoked directly from an Axum handler — a handler already runs on one
of the server's own runtime threads. Every marketplace route now runs its
registry operation via `tokio::task::spawn_blocking` (see
`crates/apex-server/src/marketplace.rs`'s `with_registry` helper), which
moves the whole synchronous call onto a plain OS thread outside the async
runtime, where the nested `block_on` is fine.

---

# 11. Related Documents

- [`12-deployment/docker.md`](docker.md)
- [`12-deployment/kubernetes.md`](kubernetes.md)
- [`12-deployment/index.md`](index.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-05 | Added a real, working `deployment/docker-compose.yml` (§10): the actual single-binary `apex` + Postgres (marketplace registry, genuinely wired into the server) + Qdrant (tiered memory, CLI-only today). Parameterized `deployment/docker/Dockerfile` with a `FEATURES` build arg and added `curl` for a real `/healthz` healthcheck. Found and fixed a real bug while verifying this live: every marketplace route panicked when Postgres-backed, because the sync `postgres` crate's blocking calls can't run directly on an Axum handler's own async-runtime thread — fixed via `tokio::task::spawn_blocking`. Chaos-checked: a Postgres outage degrades marketplace routes to a clean `502`, recovering automatically once Postgres returns, with `apex`'s own health and every non-marketplace route unaffected throughout |
| 1.0.0 | 2026-06-27 | Initial Docker Compose deployment guide |
