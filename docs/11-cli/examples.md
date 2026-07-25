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

This document collects task-oriented recipes for the `wovyr` CLI — end-to-end flows for everyday development, operations, and CI/CD. Commands reference the full [Command Reference](commands.md).

---

# 2. Getting Started

```bash
wovyr login
wovyr context use acme-dev
wovyr doctor                       # confirm connectivity + auth
wovyr users me                     # see your scopes
```

---

# 3. Scaffold & Run a Project Locally

```bash
wovyr init order-bot && cd order-bot
# edit agents/order-assistant.yaml

# run locally with no server
wovyr agents run --local -f agents/order-assistant.yaml \
  --input '{"message":"Where is order 123?"}' --stream
```

Local runs use the embedded runtime — fast iteration before publishing.

---

# 4. Author, Test, and Publish an Agent

```bash
wovyr agents create -f agents/order-assistant.yaml
ID=$(wovyr agents list -o json | jq -r '.data[] | select(.name=="order-assistant").id')

wovyr agents run "$ID" --input '{"message":"hi"}' --stream     # smoke test
wovyr agents publish "$ID"
```

---

# 5. Validate and Run a Workflow

```bash
wovyr workflows validate -f workflows/invoice-approval.yaml   # compile-check DSL
wovyr workflows create -f workflows/invoice-approval.yaml
WF=$(wovyr workflows list -o json | jq -r '.data[0].id')
wovyr workflows publish "$WF"

EXE=$(wovyr workflows run "$WF" --input @input.json -o json | jq -r '.execution_id')
wovyr workflows executions get "$EXE" --watch                 # stream state
```

Resume a workflow waiting on an event:

```bash
wovyr workflows executions signal "$EXE" \
  --event PaymentReceived --payload '{"amount":12000}'
```

Approve a human task:

```bash
TASK=$(wovyr workflows executions get "$EXE" -o json | jq -r '.pending_task_id')
wovyr workflows tasks complete "$TASK" --decision approved
```

---

# 6. Seed and Query Memory

```bash
wovyr memory namespaces create -f memory/knowledge.yaml
wovyr memory put -f memory/refund-policy.yaml
wovyr memory query "how long do refunds take?" --scope project --limit 5
```

---

# 7. Build and Publish a Plugin

```bash
wovyr plugin new github --kind tool,workflow_activity
cd github
wovyr plugin build && wovyr plugin test
wovyr plugin sign --key ~/.keys/acme.key
wovyr plugin publish --registry https://registry.wovyr.example.com
```

Install it elsewhere and grant permissions:

```bash
wovyr plugins install acme/github@1.4.0
PID=$(wovyr plugins list -o json | jq -r '.data[] | select(.name=="acme/github").id')
wovyr plugins grants add "$PID" --project support-bot \
  --permission net:egress:api.github.com \
  --permission secret:read:github-token
wovyr plugins enable "$PID"
```

---

# 8. CI/CD Pipeline (non-interactive)

```bash
# environment provides WOVYR_API_KEY, WOVYR_SERVER, WOVYR_PROJECT
set -euo pipefail

wovyr workflows validate -f workflows/*.yaml        # fail build on DSL errors
wovyr agents create -f agents/order-assistant.yaml
wovyr workflows publish "$(wovyr workflows list -o json | jq -r '.data[0].id')"
```

Uses [API-key auth](configuration.md#52-api-key-ci--service-accounts) and stable
[exit codes](configuration.md#8-exit-codes) so a failed step fails the pipeline.

---

# 9. Cost & Health Checks

```bash
wovyr status                                        # service health
wovyr projects quota get support-bot                # quota utilization
wovyr agents runs logs "$RUN" --follow              # tail a run
```

---

# 10. Scripting Patterns

```bash
# JSON output + jq for composition
wovyr agents list -o json | jq -r '.data[].name'

# quiet mode prints just the id for piping
NEW=$(wovyr agents create -f agent.yaml --quiet)
wovyr agents publish "$NEW"

# idempotent runs in retried jobs
wovyr workflows run "$WF" --input @in.json --idempotency-key "nightly-$(date +%F)"
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
