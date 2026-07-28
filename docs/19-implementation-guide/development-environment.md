<!--
File: docs/19-implementation-guide/development-environment.md
Document ID: IMPL-001
-->

# Development Environment

**Document ID:** IMPL-001  
**File Path:** `docs/19-implementation-guide/development-environment.md`  
**Version:** 2.0.0  
**Status:** Current  
**Owner:** Engineering Team  
**Last Updated:** 2026-07-28

---

# 1. Purpose

How to set up a local development environment for Wovyr.

This document was rewritten on 2026-07-28 to describe what the repository actually
contains. The previous revision listed a NestJS BFF, `pnpm`, `cargo nextest`, a
devcontainer, and a `make run-svc SVC=…` multi-service layout — **none of which exist**;
Wovyr is a single Rust workspace producing one binary, plus an Angular dashboard.

---

# 2. Prerequisites

| Tool | Required? | Purpose |
|------|-----------|---------|
| Rust 1.85+ (Edition 2024) | **yes** | Builds everything ([ADR-0002](../17-adr/ADR-0002-rust.md)). MSRV 1.85; developed against 1.93. |
| Node.js 20+ (`npm`) | dashboard/SDK work only | Angular dashboard, TypeScript SDK, website. `npm`, not `pnpm`. |
| `wasm32-wasip1` target | plugin work only | `wovyr plugin new|build` and the scaffold round-trip test (which skips without it). |
| Docker | optional | Container/gVisor sandbox backends and their integration tests; the Compose stack ([compose](../12-deployment/docker-compose.md)). Tests skip cleanly when absent. |
| Postgres / Qdrant / Redis | optional | Only for the capability-gated integration tests of those backends. |

There is **no devcontainer** and no committed `.vscode/` settings.

Nothing in the core loop needs Docker or a model API key: the deterministic mock
provider makes `build`/`test`/`dev`/`run-hello` work fully offline.

---

# 3. Bootstrap

```bash
git clone https://github.com/punarduttrajput/wovyr && cd wovyr
make setup
```

`make setup` adds the `rustfmt`/`clippy` components and the `wasm32-wasip1` target, then
builds the workspace. It is idempotent. There are **no Git hooks** — run `make lint`
yourself before pushing (CI gates on exactly what that target runs).

## 3.1 The offline cargo config

If a build fails with `attempting to make an HTTP request, but --offline was
specified`, your `~/.cargo/config.toml` sets `[net] offline = true`. Override it for
the first build, which must populate the dependency cache:

```bash
cargo build --workspace --config net.offline=false
```

Once `~/.cargo` is warm, plain `cargo build` works offline.

---

# 4. Run Locally

```bash
make dev          # the all-in-one server on 127.0.0.1:8080
make run-hello    # one agent, end to end, with streaming output
```

`make dev` runs `wovyr dev` — the **embedded** single-node server (HTTP API + workflow
engine + memory + durable state under `~/.wovyr`). It does **not** start Compose
backends, seed a tenant, or serve the dashboard; those are separate:

```bash
make compose-up      # wovyr + Postgres + Qdrant containers
make dashboard-dev   # Angular dashboard (proxies to a server on :8080)
```

`WOVYR_ALLOW_ANONYMOUS=1` (which `make dev` sets) skips credential setup for local
work. It is refused on any non-loopback bind, so it cannot be exposed to a network —
see [auth](../13-security/index.md) for a real deployment.

---

# 5. Configuration

Configuration is environment-driven; there is **no `.env.example`** in the repo. The
variables each subsystem reads are documented where that subsystem is
([deployment](../12-deployment/docker.md#5-configuration) for the server,
`wovyr_config::env` for the shared `WOVYR_*` layer).

For provider keys during development, export `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` in
your shell — never commit them
([secret management](../13-security/secret-management.md)). With neither set, the
gateway resolves the mock provider and logs that it did.

---

# 6. Editor Setup

- **rust-analyzer** for Rust. No editor settings are committed — configure yours as you
  like; `cargo fmt`/`cargo clippy` are the only style authority (see
  [coding standards](coding-standards.md)).
- The dashboard uses Angular's own toolchain; there is no committed ESLint/Prettier
  config at the repo root.

---

# 7. Common Tasks

```bash
make build        # cargo build --workspace
make test         # cargo test --workspace
make lint         # clippy -D warnings + fmt --check  (what CI gates on)
make fmt          # cargo fmt --all
make clean        # cargo clean
```

Narrower loops, run directly:

```bash
cargo test -p wovyr-provider                    # one crate
cargo test -p wovyr-agent --test tool_loop      # one integration-test file
cargo test -p wovyr-tools --features wasi       # a feature-gated backend
```

Feature-gated code is **not** compiled by a plain `cargo build`. CI runs
`cargo hack clippy --each-feature`, so a new feature-gated module will be linted under
`-D warnings` on your PR even if it compiled fine locally. The exception is
`mistralrs` (excluded — too heavy a compile).

There is **no `wovyr doctor` command.**

---

# 8. Troubleshooting

| Symptom | Cause |
|---|---|
| `attempting to make an HTTP request, but --offline was specified` | The offline cargo config — see §3.1. |
| `skipping: …` in test output | A capability-gated test with its backend absent (Docker/Postgres/Qdrant/Redis/`wasm32-wasip1`). Expected locally; CI fails on these lines so they can't silently skip there. |
| Sandbox integration tests not running | Deliberate — they need the `sandbox-integration-tests` feature. A plain `cargo test --workspace` compiles none of them. |
| Dashboard build fails on a fresh checkout | It depends on `@wovyr/ui-react` via a `file:` path; `make dashboard-*` builds that first via npm `pre*` hooks. Use those targets rather than bare `ng`. |
| A test passes alone but fails in `cargo test --workspace` | Shared state. Server tests must build state via `AppState::for_test()`, not `from_env()` (which resolves real paths under `~/.wovyr`); a nested `cargo` invocation must clear `CARGO_MAKEFLAGS`. |

There is no `sccache` configuration committed; builds are plain cargo.

---

# 9. Related

- [`19-implementation-guide/build-system.md`](build-system.md)
- [`12-deployment/docker-compose.md`](../12-deployment/docker-compose.md)
- [`11-cli/index.md`](../11-cli/index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 2.0.0 | 2026-07-28 | Rewritten against the real repository: removed the NestJS BFF, `pnpm`, `cargo nextest`, devcontainer, Git hooks, `.env.example`, `make run-svc`, `wovyr doctor`, and `sccache` references (none exist). Added the offline-cargo override, the optional-vs-required prerequisite split, feature-gated test guidance, and a troubleshooting table. |
| 1.0.0 | 2026-06-27 | Initial Development Environment guide |
