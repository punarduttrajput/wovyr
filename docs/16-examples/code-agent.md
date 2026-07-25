<!--
File: docs/16-examples/code-agent.md
Document ID: EX-003
-->

# Example: Code Agent

**Document ID:** EX-003  
**File Path:** `docs/16-examples/code-agent.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Developer Relations Team  
**Last Updated:** 2026-06-27

---

# 1. Goal

Build a **tool-using** agent that operates on a repository — reading files, running
commands, and proposing changes — with tools executed safely in the
[Tool Runtime](../07-tool-runtime/index.md) sandbox.

---

# 2. Tools Used

| Tool | Purpose | Permissions |
|------|---------|-------------|
| `fs.read` / `fs.write` | Read/edit files | `fs:read`/`fs:write:/workspace` |
| `shell.run` | Run build/test | sandboxed, no egress |
| `git.diff` / `git.commit` | Version control | `fs:*:/workspace` |

These run sandboxed with **default-deny egress**
([tool isolation](../07-tool-runtime/security-isolation.md)); the agent gets only
the declared grants.

---

# 3. Define the Agent

`agents/code-agent.yaml`:

```yaml
kind: Agent
metadata: { name: code-agent }
spec:
  model_selector: { capability: chat, class: frontier }
  instructions: |
    You are a coding assistant. Read relevant files before editing.
    Run tests after changes. Propose a git diff; do not commit without approval.
  tools: [fs.read, fs.write, shell.run, git.diff]
  policies: [no-network-egress]
  budget: { max_cost_usd: 1.00 }
```

A frontier-class model is selected for harder reasoning; a policy forbids egress.

---

# 4. Run

```bash
wovyr agents run -f agents/code-agent.yaml --stream \
  --input '{"message":"Add input validation to parse_config and run the tests."}'
```

The stream shows tool calls interleaved with reasoning:

```text
tool_call · fs.read("src/config.rs")
delta     · "I'll add validation for empty keys..."
tool_call · fs.write("src/config.rs", ...)
tool_call · shell.run("cargo test")  → exit 0
tool_call · git.diff                 → proposed patch
done      · tokens: 9.2k, cost_usd: 0.21, tool_calls: 4
```

---

# 5. Sandbox & Safety

- Each tool runs in an [ephemeral sandbox](../07-tool-runtime/sandbox-runtime.md#4-sandbox-lifecycle);
  `shell.run` has **no network** and a CPU/mem/time limit.
- `git.commit` is **not** granted — the agent proposes a diff for human review,
  enforcing change control.
- All tool executions are [audited](../07-tool-runtime/security-isolation.md#11-audit).

---

# 6. Add Human Approval (optional)

Wrap the commit step in a workflow with a
[human task](../09-api/workflows.md#8-human-tasks) so a reviewer approves the diff
before `git.commit` runs — see [Customer Support](customer-support.md) for the
pattern.

---

# 7. Observe

- Per-tool duration, resources, and cost in the
  [trace inspector](../10-dashboard/agent-studio.md#4-trace--step-inspector).
- Tool execution metrics in [monitoring](../10-dashboard/monitoring.md).

---

# 8. Related Documents

- [`07-tool-runtime/execution-api.md`](../07-tool-runtime/execution-api.md)
- [`09-api/tools.md`](../09-api/tools.md)
- [`16-examples/index.md`](index.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Code Agent example |
