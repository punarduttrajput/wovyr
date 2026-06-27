<!--
File: docs/11-cli/examples.md
Document ID: CLI-004
-->

# CLI Examples & Recipes

**Document ID:** CLI-004  
**File Path:** `docs/11-cli/examples.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document collects task-oriented recipes for the `apex` CLI — end-to-end flows for everyday development, operations, and CI/CD. Commands reference the full [Command Reference](commands.md).

---

# 2. Getting Started

```bash
apex login
apex context use acme-dev
apex doctor                       # confirm connectivity + auth
apex users me                     # see your scopes
```

---

# 3. Scaffold & Run a Project Locally

```bash
apex init order-bot && cd order-bot
# edit agents/order-assistant.yaml

# run locally with no server
apex agents run --local -f agents/order-assistant.yaml \
  --input '{"message":"Where is order 123?"}' --stream
```

Local runs use the embedded runtime — fast iteration before publishing.

---

# 4. Author, Test, and Publish an Agent

```bash
apex agents create -f agents/order-assistant.yaml
ID=$(apex agents list -o json | jq -r '.data[] | select(.name=="order-assistant").id')

apex agents run "$ID" --input '{"message":"hi"}' --stream     # smoke test
apex agents publish "$ID"
```

---

# 5. Validate and Run a Workflow

```bash
apex workflows validate -f workflows/invoice-approval.yaml   # compile-check DSL
apex workflows create -f workflows/invoice-approval.yaml
WF=$(apex workflows list -o json | jq -r '.data[0].id')
apex workflows publish "$WF"

EXE=$(apex workflows run "$WF" --input @input.json -o json | jq -r '.execution_id')
apex workflows executions get "$EXE" --watch                 # stream state
```

Resume a workflow waiting on an event:

```bash
apex workflows executions signal "$EXE" \
  --event PaymentReceived --payload '{"amount":12000}'
```

Approve a human task:

```bash
TASK=$(apex workflows executions get "$EXE" -o json | jq -r '.pending_task_id')
apex workflows tasks complete "$TASK" --decision approved
```

---

# 6. Seed and Query Memory

```bash
apex memory namespaces create -f memory/knowledge.yaml
apex memory put -f memory/refund-policy.yaml
apex memory query "how long do refunds take?" --scope project --limit 5
```

---

# 7. Build and Publish a Plugin

```bash
apex plugin new github --kind tool,workflow_activity
cd github
apex plugin build && apex plugin test
apex plugin sign --key ~/.keys/acme.key
apex plugin publish --registry https://registry.apex.example.com
```

Install it elsewhere and grant permissions:

```bash
apex plugins install acme/github@1.4.0
PID=$(apex plugins list -o json | jq -r '.data[] | select(.name=="acme/github").id')
apex plugins grants add "$PID" --project support-bot \
  --permission net:egress:api.github.com \
  --permission secret:read:github-token
apex plugins enable "$PID"
```

---

# 8. CI/CD Pipeline (non-interactive)

```bash
# environment provides APEX_API_KEY, APEX_SERVER, APEX_PROJECT
set -euo pipefail

apex workflows validate -f workflows/*.yaml        # fail build on DSL errors
apex agents create -f agents/order-assistant.yaml
apex workflows publish "$(apex workflows list -o json | jq -r '.data[0].id')"
```

Uses [API-key auth](configuration.md#52-api-key-ci--service-accounts) and stable
[exit codes](configuration.md#8-exit-codes) so a failed step fails the pipeline.

---

# 9. Cost & Health Checks

```bash
apex status                                        # service health
apex projects quota get support-bot                # quota utilization
apex agents runs logs "$RUN" --follow              # tail a run
```

---

# 10. Scripting Patterns

```bash
# JSON output + jq for composition
apex agents list -o json | jq -r '.data[].name'

# quiet mode prints just the id for piping
NEW=$(apex agents create -f agent.yaml --quiet)
apex agents publish "$NEW"

# idempotent runs in retried jobs
apex workflows run "$WF" --input @in.json --idempotency-key "nightly-$(date +%F)"
```

---

# 11. Related Documents

- [`11-cli/commands.md`](commands.md)
- [`11-cli/configuration.md`](configuration.md)
- [`16-examples`](../SUMMARY.md) *(planned: full example apps)*

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial CLI Examples & Recipes |
