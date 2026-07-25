<!--
File: docs/07-tool-runtime/git.md
Document ID: TRT-104
-->

# Built-in Tool: Git

**Document ID:** TRT-104  
**File Path:** `docs/07-tool-runtime/git.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `git.*` built-in tools let agents work with Git repositories in the workspace —
inspecting history, producing diffs, and (when granted) committing/pushing — for
code agents and automation.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `git.clone` | Clone a repo into the workspace |
| `git.status` | Working-tree status |
| `git.diff` | Produce a diff |
| `git.commit` | Commit staged changes |
| `git.branch` | Create/switch branches |
| `git.push` | Push to a remote (guarded) |

---

# 3. Schema (example: `git.diff`)

```json
// input
{ "cwd": "/workspace/repo", "staged": false }
// output
{ "diff": "diff --git a/... b/...", "files_changed": 2 }
```

---

# 4. Permissions

```text
fs:read:/workspace   fs:write:/workspace
net:egress:<git-host>            (clone/push)
secret:read:<git-token-ref>      (auth to remote)
git:write                        (commit/push)
```

`git.commit`/`git.push` require elevated grants; read-only inspection
(`status`/`diff`) is low-risk. The [Code Agent](../16-examples/code-agent.md)
deliberately withholds `commit` to force human review.

---

# 5. Sandbox & Safety

- Runs in the workspace sandbox; remote access only to granted hosts.
- Git credentials are secret references injected at run time, never logged
  ([secrets](security-isolation.md#7-secret-management)).
- Pushes are typically gated behind a [workflow approval](../09-api/workflows.md#8-human-tasks).

---

# 6. Determinism & Caching

Read operations (`status`/`diff`/`log`) may be cached briefly; mutating operations
are never cached.

---

# 7. Example

```bash
wovyr tools invoke git.diff --input '{"cwd":"/workspace/repo"}'
```

---

# 8. Related

- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`16-examples/code-agent.md`](../16-examples/code-agent.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Git tool spec |
