<!--
File: docs/19-implementation-guide/build-system.md
Document ID: IMPL-002
-->

# Build System & SDK

**Document ID:** IMPL-002  
**File Path:** `docs/19-implementation-guide/build-system.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes how the Wovyr AI Platform is built — the Cargo workspace, the
task runner, build performance, artifact production, and the Rust SDK.

---

# 2. Workspace Layout

Per [ADR-0001](../17-adr/ADR-0001-project-structure.md), a single Cargo workspace:

```text
wovyr/
├── crates/        # libraries: common, provider-sdk, tool-sdk, plugin-sdk, ...
├── apps/          # service binaries: api-gateway, agent-runtime, ...
├── sdk/           # public Rust SDK (+ generated clients)
├── plugins/       # first-party plugins
├── dashboard/     # Angular SPA (built; calls wovyr-server directly — NestJS BFF deferred)
└── deployment/    # Docker, Helm, Terraform
```

Shared logic lives in `crates/`; each deployable is a thin binary over them. The
`dashboard/` Angular workspace (npm) builds with `ng build`; its dev server proxies
`/api` to `wovyr-server` (`dashboard/proxy.conf.json`). The NestJS BFF is deferred —
see [dashboard/overview.md](../10-dashboard/overview.md).

---

# 3. Task Runner

A `make` (or `cargo xtask`) front end wraps common tasks:

```bash
make build        # cargo build --workspace
make test         # cargo nextest run + integration
make lint         # cargo clippy -D warnings + fmt --check
make image SVC=api-gateway   # build a service container
make sdk          # build/package the SDK
```

---

# 4. Build Performance

- `cargo nextest` for fast, parallel tests.
- `sccache`/shared cache for incremental and CI builds.
- Affected-crate detection runs only impacted tests on PRs.
- Distroless [multi-stage images](../12-deployment/docker.md#3-build) keep artifacts
  small.

---

# 5. Artifacts

| Artifact | Built by |
|----------|----------|
| Service binaries | `cargo build --release` per app |
| Container images | per-service Dockerfiles (signed) |
| `wovyr` CLI | `apps/cli` |
| Rust SDK crate | `sdk/` |
| Plugin packages | `wovyr plugin build` ([plugin SDK](../08-plugin-sdk/plugin-api.md)) |

Images and releases are **signed** with provenance
([release process](release-process.md), [distribution](../08-plugin-sdk/distribution.md#3-signing)).

---

# 6. The Rust SDK

The SDK (`sdk/`) is the reference way to build on the platform:

- Typed clients for the [Platform API](../09-api/index.md) (REST/gRPC).
- The [tool](../04-agent-framework/tool-framework.md#42-core-rust-traits) and
  [provider](../04-agent-framework/provider-sdk.md#21-rust-interface) traits used by
  both built-ins and [plugins](../08-plugin-sdk/plugin-api.md#4-rust-sdk-traits).
- A builder for authoring [workflows](../03-workflow-engine/workflow-dsl.md) in Rust.

> Note: an earlier draft referenced an `05-sdk/` docs path; SDK *usage* is documented
> here and in the [Plugin SDK](../08-plugin-sdk/plugin-api.md). The deployable LLM
> service lives in [section 05](../05-llm-gateway/index.md).

---

# 7. Code Generation

- API clients and OpenAPI/proto definitions are generated and checked in or built in
  CI (single source of truth = the [API contract](../09-api/overview.md)).
- DSL/schema types are generated from canonical schemas.

---

# 8. CI Build Pipeline

```text
fmt+clippy → build → unit → integration → package(images/SDK) → sign
```

Matches the [testing CI pipeline](../15-testing/index.md#5-ci-pipeline-overview).

Two additional jobs run in parallel with the default-feature pipeline
(RM-GA-P2 CI-901):

- **Feature matrix** — `cargo hack clippy --each-feature --workspace` lints every
  feature-gated code path under the same `-D warnings` policy as default-feature
  code. The one exclusion is `mistralrs` (the full mistral.rs inference engine —
  a compile too heavy for every PR; it stays buildable locally).
- **Service-container integration** — Postgres, Qdrant, and Redis run as CI
  service containers, and the capability-gated integration tests
  (`wovyr-workflow`/`wovyr-marketplace` Postgres stores, `wovyr-memory` tiered
  backend, `wovyr-provider` Qdrant semantic cache + Redis breaker) run against
  them with their gating env vars set. The job greps for the tests' `skipping:`
  convention and fails on it, so a silently-skipped test can never read as
  green coverage.

---

# 9. Related

- [`19-implementation-guide/development-environment.md`](development-environment.md)
- [`19-implementation-guide/release-process.md`](release-process.md)
- [`17-adr/ADR-0001-project-structure.md`](../17-adr/ADR-0001-project-structure.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Build System & SDK guide |
| 1.1.0 | 2026-07-07 | §8: CI-901 feature-matrix (cargo-hack each-feature clippy) + service-container integration jobs |
