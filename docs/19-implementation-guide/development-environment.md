<!--
File: docs/19-implementation-guide/development-environment.md
Document ID: IMPL-001
-->

# Development Environment

**Document ID:** IMPL-001  
**File Path:** `docs/19-implementation-guide/development-environment.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes how to set up a local development environment for the Wovyr
AI Platform.

---

# 2. Prerequisites

| Tool | Purpose |
|------|---------|
| Rust (stable, Edition 2024) | Build all services/SDKs ([ADR-0002](../17-adr/ADR-0002-rust.md)) |
| Docker + Compose | Local backends ([compose](../12-deployment/docker-compose.md)) |
| Node.js + pnpm | Dashboard (Angular UI + NestJS BFF) |
| `cargo nextest` | Fast test runner |
| `wovyr` CLI | Local run + auth ([CLI](../11-cli/index.md)) |

A devcontainer is provided so the toolchain is reproducible.

---

# 3. Bootstrap

```bash
git clone <repo> && cd wovyr
make setup        # rustup components, hooks, node deps
```

`make setup` installs formatters/linters and Git hooks (format + lint on commit).

---

# 4. Run Locally

```bash
make dev          # all-in-one platform with local backends
# or with the CLI:
wovyr dev          # embedded runtime for quick iteration
```

`make dev` starts the [Compose](../12-deployment/docker-compose.md) backends and the
all-in-one [platform image](../12-deployment/docker.md), seeds a dev tenant, and
exposes the API + dashboard.

---

# 5. Configuration

Local config is environment-driven
([deployment config](../12-deployment/docker.md#5-configuration)); a `.env.example`
is provided. For provider keys during development, use a local secret backend or
dev-only env vars — never commit secrets
([secret management](../13-security/secret-management.md)).

---

# 6. Editor Setup

- rust-analyzer for Rust; ESLint/Prettier for the dashboard.
- Recommended settings are committed (`.vscode/`, `rustfmt.toml`, `clippy` config).

---

# 7. Common Tasks

```bash
make build        # build all crates
make test         # unit + integration
make lint         # clippy + fmt check
make run-svc SVC=llm-gateway   # run one service
wovyr doctor       # diagnose env + connectivity
```

---

# 8. Troubleshooting

- `wovyr doctor` checks toolchain, backends, and version compatibility
  ([CLI](../11-cli/installation.md#8-first-run)).
- Slow builds: ensure `sccache`/workspace caching is enabled
  ([build system](build-system.md)).

---

# 9. Related

- [`19-implementation-guide/build-system.md`](build-system.md)
- [`12-deployment/docker-compose.md`](../12-deployment/docker-compose.md)
- [`11-cli/index.md`](../11-cli/index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Development Environment guide |
