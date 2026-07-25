<!--
File: docs/07-tool-runtime/shell.md
Document ID: TRT-102
-->

# Built-in Tool: Shell

**Document ID:** TRT-102  
**File Path:** `docs/07-tool-runtime/shell.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `shell.run` built-in tool executes a command **inside a sandbox** — for builds,
tests, and scripted tasks — with strict isolation and **no network by default**.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `shell.run` | Run a command and capture stdout/stderr/exit code |

---

# 3. Schema

```json
// input
{ "command": "cargo test", "cwd": "/workspace", "timeout_ms": 60000 }
// output
{ "exit_code": 0, "stdout": "...", "stderr": "", "duration_ms": 5400 }
```

Output is streamed ([streaming](execution-api.md#7-streaming)) and bounded.

---

# 4. Permissions

```text
fs:read:/workspace   fs:write:/workspace   (network: denied unless granted)
```

`shell.run` is high-risk: it is granted sparingly and is **floored to a stronger
sandbox** for untrusted contexts ([backend selection](sandbox-runtime.md#3-backend-selection)).

---

# 5. Sandbox & Safety

- Runs as a non-root user with dropped capabilities, `no-new-privileges`, seccomp
  filtering ([isolation](security-isolation.md#10-non-root--capability-dropping)).
- **Default-deny egress**; only granted hosts reachable
  ([network isolation](security-isolation.md#5-network-isolation)).
- CPU/memory/disk/time limits enforced; breach kills the sandbox.
- Ephemeral: nothing persists between runs.

---

# 6. Determinism & Caching

`shell.run` is side-effecting and **never cached**. For reproducibility, pin
commands and inputs; avoid network-dependent commands.

---

# 7. Example

```bash
wovyr tools invoke shell.run --input '{"command":"cargo build","cwd":"/workspace"}'
```

Used by the [Code Agent](../16-examples/code-agent.md) to build and test.

---

# 8. Related

- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Shell tool spec |
