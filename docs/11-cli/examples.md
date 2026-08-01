<!--
File: docs/11-cli/examples.md
Document ID: CLI-004
-->

# CLI Examples & Recipes

**Document ID:** CLI-004  
**File Path:** `docs/11-cli/examples.md`  
**Version:** 2.0.0  
**Status:** Shipped — every command below exists in the
[generated command reference](commands.md), which is itself regenerated from the
real clap tree and diffed in CI (DX-304).  
**Owner:** AI Platform Team  
**Last Updated:** 2026-08-01

---

# 1. Purpose

Task-oriented recipes for the `wovyr` CLI.

> **Scope note.** Version 1.0.0 of this document was written against a planned
> command surface and most of its recipes did not run: `wovyr init`, `doctor`,
> `status`, `context use`, `users me`, `agents create|list|publish`,
> `workflows create|publish|executions|tasks`, `plugins` (plural), `projects`,
> `-o json`, `--quiet`, and `--idempotency-key` are not implemented. It was
> rewritten against the shipped CLI. Where a recipe needs the HTTP API rather
> than a CLI command, that is called out explicitly instead of invented.

All local commands share the `~/.wovyr` state directory
([configuration §3](configuration.md#3-state-directory)). With no provider key
set, runs use the deterministic mock provider — so everything here works
offline.

---

# 2. Getting Started

```bash
wovyr --version
wovyr login --server https://api.wovyr.example.com --token "$TOKEN"
wovyr whoami                       # server + masked token
```

For a purely local session, skip `login` entirely and use `--local`.

---

# 3. Run an Agent Locally

```bash
git clone https://github.com/punarduttrajput/wovyr && cd wovyr

wovyr agents run --local -f examples/agents/hello.yaml \
  --input '{"message":"Hi"}' --stream
```

`--stream` renders the run as a live event stream (deltas, tool calls, tool
results). Useful variations:

```bash
# Raise the tool-loop budget for a task that needs many steps.
wovyr agents run --local -f examples/agents/web-reader.yaml \
  --input '{"message":"summarize example.com"}' --max-steps 20

# Force a specific provider (default `auto` picks OpenAI, then Anthropic, then mock).
wovyr agents run --local -f examples/agents/hello.yaml --provider anthropic

# A RAG agent: grounds the prompt from ~/.wovyr/memory (see §5).
wovyr agents run --local -f examples/agents/docs-bot.yaml \
  --input '{"message":"how long do refunds take?"}'
```

## 3.1 Privileged builtins are opt-in

`shell`, `fs_write`, and `code_execute` execute arbitrary commands, write
arbitrary files, and run arbitrary code as your user, driven by whatever the
model decides. A manifest naming one **fails closed** without an explicit
opt-in (SBX-305):

```bash
wovyr agents run --local -f examples/agents/shell-runner.yaml \
  --input '{"message":"list the current directory"}' \
  --allow-privileged-tools
```

Use `WOVYR_LOCAL_PRIVILEGED=1` to enable them for a whole session — it is also
the only way to enable them on the `workflows approve`/`signal`/`tick` resume
paths, which take no flag.

## 3.2 Run against a server

```bash
wovyr dev                                          # 127.0.0.1:8080, another terminal
wovyr agents run -f examples/agents/hello.yaml --input '{"message":"Hi"}'
```

Without `--local`, the run goes to `--server` or the URL stored by `login`.

---

# 4. Validate and Run a Workflow

```bash
wovyr workflows validate -f examples/workflows/support.yaml   # compile-check the DAG

wovyr workflows run --local -f examples/workflows/support.yaml \
  --input '{"ticket":"refund please"}' --id wf-demo
```

Inspect it — both are side-effect-free reads:

```bash
wovyr workflows status --id wf-demo
wovyr workflows show   --id wf-demo          # status + full event timeline
wovyr workflows list --status running --limit 20
```

Resume a `human` activity that suspended:

```bash
wovyr workflows approve -f examples/workflows/support.yaml \
  --id wf-demo --task manager_review --decision approved
```

Resume a `wait` activity:

```bash
# an event
wovyr workflows signal -f examples/workflows/support.yaml \
  --id wf-demo --event PaymentReceived --payload '{"amount":12000}'

# or a timer, by id
wovyr workflows signal -f examples/workflows/support.yaml \
  --id wf-demo --timer cooloff
```

Saga rollback — a failing step compensates completed ones in reverse order:

```bash
wovyr workflows run --local -f examples/workflows/saga-order.yaml --input '{}'
```

Multi-agent fan-out, resolving `agent` activities from a directory:

```bash
wovyr workflows run --local -f examples/workflows/research-team.yaml \
  --agents-dir examples/agents --input '{"topic":"vector databases"}'
```

## 4.1 Durable timers and schedules

`tick` fires anything due — a `wait: {timer: {after: "30d"}}` deadline or a
registered schedule. (The `wovyr dev` server polls these automatically; `tick`
is the CLI equivalent for local runs.)

```bash
wovyr workflows schedule create -f examples/workflows/greet-and-fetch.yaml \
  --id nightly --cron '0 2 * * *' --input '{"name":"ops"}'
wovyr workflows schedule list
wovyr workflows tick -f examples/workflows/greet-and-fetch.yaml
```

---

# 5. Seed and Query Memory

```bash
wovyr memory put --namespace support \
  --content "Refunds are processed within 30 days of the request." \
  --importance 0.8 --tag policy --tag refunds

# Offline, prefer `keyword` — mock embeddings make hybrid/vector noisy.
wovyr memory query "how long do refunds take?" \
  --namespace support --strategy keyword --limit 5
```

ABAC — a record's required scopes must all be granted by the reader:

```bash
wovyr memory put --namespace support --content "Escalation pager: ..." \
  --require-scope oncall
wovyr memory query "pager" --namespace support --grant oncall
```

Seal a record at rest through the platform KMS, and trade relevance for
diversity via MMR:

```bash
wovyr memory put --namespace support --content "Customer 123 card ends 4242" --sensitive
wovyr memory query "refunds" --namespace support --diversity 0.5
```

Consolidate stale, low-importance records into one summary:

```bash
wovyr memory compact --namespace support --max-importance 0.4 --keep-recent 10
```

---

# 6. Build, Sign, and Install a Plugin

```bash
wovyr plugin new github --publisher acme
wovyr plugin build github                    # → github/dist, digests computed
```

Signed install, the one-shot path — `publish --key` fills in digests, signs, and
prints the `trust` line to paste:

```bash
wovyr plugin publish github/dist --key acme.key --channel stable
```

Or the explicit steps:

```bash
wovyr plugin keygen acme
wovyr plugin sign --key acme.key --manifest github/dist/plugin.yaml
wovyr plugin trust acme --key acme.pub
wovyr plugin install github/dist --grant 'net:egress:api.github.com'
wovyr plugin enable acme/github
wovyr plugin list
```

Invoke a capability directly, without an agent (the operator test path):

```bash
wovyr plugin run acme/github --input '{"repo":"punarduttrajput/wovyr"}'
```

Keyless signing (ADR-0009 — a short-lived cert over an ephemeral key):

```bash
wovyr plugin keyless-init --allow 'https://ci.example.com|release@acme.dev|acme'
wovyr plugin keyless-sign --manifest github/dist/plugin.yaml \
  --issuer https://ci.example.com --subject release@acme.dev
```

Distribute as a single file, then discover and install elsewhere:

```bash
wovyr plugin pack github/dist                # → acme-github-0.1.0.wovyrpkg
wovyr plugin search github --category devtools
wovyr plugin get acme/github --version 0.1.0 --grant 'net:egress:api.github.com'
```

Moderation:

```bash
wovyr plugin report acme/github "ships an undeclared network call"
wovyr plugin reports acme/github
wovyr plugin resolve-abuse acme/github 1 --delist
```

---

# 7. Keys, Secrets, and API Keys

```bash
# Roll a tenant key. Already-sealed data stays readable under its old version.
wovyr kms rotate --tenant acme

# Crypto-shred a tenant — IRREVERSIBLE, refuses without --yes.
wovyr kms destroy --tenant acme --yes
```

Mint the credential a client presents when the server runs
`WOVYR_AUTH_MODE=apikey` (run this on the server's host):

```bash
wovyr auth create-key ci-bot --ttl-days 90
wovyr auth list-keys
wovyr auth rotate <key-id> --grace-hours 24
wovyr auth revoke <key-id>
```

---

# 8. Backup, Restore, and Migrations

```bash
wovyr admin backup /backups/wovyr-$(date +%F)
wovyr admin backup s3://my-bucket/wovyr/           # needs WOVYR_S3_* (config §5.4)

wovyr admin restore /backups/wovyr-2026-08-01 --yes   # overwrites live ~/.wovyr
```

Backup verifies every file's sha256 before writing anything, so a corrupt
archive fails closed. Bring a Postgres-backed schema up before first use:

```bash
wovyr admin migrate --target workflow    --database-url "$PG_URL"
wovyr admin migrate --target memory      --database-url "$PG_URL"
wovyr admin migrate --target marketplace --database-url "$PG_URL"
```

See [backup-and-restore](../12-deployment/backup-and-restore.md) for RPO/RTO
targets and what the snapshot deliberately excludes.

---

# 9. CI/CD Pipeline (non-interactive)

```bash
set -euo pipefail
export WOVYR_TOKEN="$CI_WOVYR_TOKEN"     # picked up by `login --token`
export WOVYR_LOG=info

# Fail the build on a bad definition — no server needed.
for f in workflows/*.yaml; do wovyr workflows validate -f "$f"; done

# Smoke-test the agent against the deterministic mock provider.
wovyr agents run --local -f agents/order-assistant.yaml --input '{"message":"ping"}'
```

Two constraints worth designing around:

- **Exit codes are binary** — `0` success, `1` error (clap adds `2` for a usage
  error). There is no graded 3–8 scheme; branch on success/failure only.
- **There is no `-o json`.** For machine-readable output, call the HTTP API
  directly (`/api/v1/...`) or use the
  [TypeScript](../../sdks/typescript)/[Python](../../sdks/python) SDK. Agent and
  workflow *run results* are already JSON on stdout.

Registering an agent or submitting a workflow to a **server** is an API
operation, not a CLI one — `POST /api/v1/agents`, `POST /api/v1/workflows`. Both
accept an `Idempotency-Key` header, which is how a retried CI job avoids
double-submitting.

---

# 10. Related Documents

- [`11-cli/commands.md`](commands.md) — generated reference, the authority on flags
- [`11-cli/configuration.md`](configuration.md)
- [`16-examples/index.md`](../16-examples/index.md) — worked end-to-end examples
- [`09-api/index.md`](../09-api/index.md) — the HTTP surface for what the CLI doesn't cover

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 2.0.0 | 2026-08-01 | Rewritten against the shipped CLI — every recipe now uses commands and flags that exist. Removed `init`/`doctor`/`status`/`context`/`users`/`agents create\|list\|publish`/`workflows create\|publish\|executions\|tasks`/`plugins` (plural)/`projects`, `-o json`, `--quiet`, and `--idempotency-key`; pointed the machine-readable and server-registration cases at the HTTP API instead |
| 1.0.0 | 2026-06-27 | Initial CLI Examples & Recipes (written against a planned command surface) |
