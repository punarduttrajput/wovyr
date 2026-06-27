<!--
File: docs/07-tool-runtime/filesystem.md
Document ID: TRT-101
-->

# Built-in Tool: Filesystem

**Document ID:** TRT-101  
**File Path:** `docs/07-tool-runtime/filesystem.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `fs.*` built-in tools let agents read and write files **within an allowed
workspace**. They run in the [Tool Runtime](index.md) sandbox under
[filesystem policy](sandbox-runtime.md#6-filesystem--process-model).

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `fs.read` | Read a file's contents |
| `fs.write` | Create/overwrite a file |
| `fs.append` | Append to a file |
| `fs.list` | List a directory |
| `fs.stat` | File metadata |
| `fs.delete` | Remove a file |

---

# 3. Schema (example: `fs.read`)

```json
// input
{ "path": "/workspace/src/config.rs" }
// output
{ "path": "...", "bytes": 1024, "content": "..." }
```

Inputs/outputs are validated against the tool's JSON schema
([execution API](execution-api.md#4-invocation-request)).

---

# 4. Permissions

```text
fs:read:/workspace      fs:write:/workspace
```

Access is **default-deny**; only explicitly granted paths are reachable
([tool isolation](security-isolation.md#6-filesystem-isolation)). Plugins/agents
must hold the matching [grant](../08-plugin-sdk/permissions.md).

---

# 5. Sandbox & Safety

- Runs with a fresh, isolated root; only granted paths mounted; system paths
  read-only.
- Output is bounded (`max_output_bytes`) to prevent huge reads.
- Path traversal outside allowed roots is rejected.
- Scratch is wiped on [teardown](sandbox-runtime.md#4-sandbox-lifecycle).

---

# 6. Determinism & Caching

`fs.read`/`fs.list`/`fs.stat` are read-only and may be cached briefly; writes are
side-effecting and never cached ([caching](worker-pool.md#11-caching)).

---

# 7. Example

```bash
apex tools invoke fs.read --input '{"path":"/workspace/README.md"}'
```

Commonly used by the [Code Agent](../16-examples/code-agent.md).

---

# 8. Related

- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`09-api/tools.md`](../09-api/tools.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Filesystem tool spec |
