<!--
File: docs/11-cli/index.md
Document ID: CLI-INDEX-001
-->

# CLI Index

**Document ID:** CLI-INDEX-001  
**File Path:** `docs/11-cli/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for the `apex` command-line interface — the primary terminal client for the Apex AI Platform.

The CLI serves developers, operators, and CI/CD. It is both a **client of the
[Platform API](../09-api/index.md)** (managing remote resources) and a **local
toolchain** (scaffolding projects, building plugins, running workflows locally,
diagnostics) — see the CLI Service in
[C4 Container §4.10](../02-architecture/c4-container.md).

---

# 2. What the CLI Does

| Mode | Examples |
|------|----------|
| Remote management | `apex agents run`, `apex workflows publish`, `apex plugins install` |
| Local development | `apex init`, `apex plugin new`, `apex workflow run --local` |
| Authoring/build | `apex plugin build`, `apex plugin sign` |
| Operations | `apex deploy`, `apex doctor`, `apex logs` |

It mirrors the [Platform API](../09-api/index.md) resource model so anything doable
in the [Dashboard](../10-dashboard/index.md) is scriptable from the terminal.

---

# 3. Composition

```text
apex (Rust binary)
   │
   ├── command parser + help
   ├── config + profiles (~/.apex/config)
   ├── auth (OAuth device flow / API key)
   ├── API client (REST/gRPC → API Gateway)
   └── local engine (embedded runtime for --local)
```

The CLI is a single self-contained Rust binary
([tech mapping](../02-architecture/c4-container.md): CLI = Rust).

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [installation.md](installation.md) | Install methods, platforms, updates, shell completion |
| [configuration.md](configuration.md) | Config file, profiles/contexts, auth, env vars |
| [commands.md](commands.md) | Full command reference |
| [examples.md](examples.md) | Task-oriented recipes and CI usage |

---

# 5. Design Principles

1. **API parity** — anything in the Dashboard is doable from the CLI.
2. **Scriptable** — stable output, `--output json`, predictable exit codes.
3. **Secure** — same authn/authz as every client; no privileged backdoor.
4. **Context-aware** — profiles bind a server, tenant, and project.
5. **Local-first dev** — scaffold, build, and run without a remote.
6. **Helpful** — rich `--help`, completion, and `apex doctor` diagnostics.

---

# 6. Dependencies

- [`09-api/index.md`](../09-api/index.md) — the API the CLI consumes
- [`09-api/authentication.md`](../09-api/authentication.md) — login + tokens
- [`08-plugin-sdk/plugin-api.md`](../08-plugin-sdk/plugin-api.md) — `apex plugin` build/publish

---

# 7. Related Documents

- [`10-dashboard/index.md`](../10-dashboard/index.md) — the other primary client
- [`12-deployment`](../SUMMARY.md) *(planned: `apex deploy` targets)*
- [`19-implementation-guide`](../SUMMARY.md) *(planned: dev environment)*

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial CLI Index |
