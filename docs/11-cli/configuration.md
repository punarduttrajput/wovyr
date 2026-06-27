<!--
File: docs/11-cli/configuration.md
Document ID: CLI-002
-->

# CLI Configuration

**Document ID:** CLI-002  
**File Path:** `docs/11-cli/configuration.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes how the `apex` CLI is configured: the config file, **profiles/contexts** (server + tenant + project), authentication, environment variables, and output settings.

---

# 2. Configuration Precedence

Settings resolve in order (highest wins):

```text
1. Command-line flags        (--project, --output, ...)
2. Environment variables     (APEX_*)
3. Active profile            (~/.apex/config)
4. Built-in defaults
```

This lets CI override interactively-saved config without editing files.

---

# 3. Config File

Default location `~/.apex/config` (override with `APEX_CONFIG`):

```yaml
current_context: acme-prod

contexts:
  acme-prod:
    server: https://api.apex.example.com
    tenant: acme
    project: support-bot
    auth: oauth          # uses stored OAuth tokens
  acme-dev:
    server: https://dev.apex.example.com
    tenant: acme
    project: sandbox
    auth: apikey         # uses APEX_API_KEY / keychain

output:
  format: table          # table | json | yaml
  color: auto
```

A **context** binds a server, tenant, and project plus an auth method — the unit
the CLI switches between.

---

# 4. Contexts (Profiles)

```bash
apex context list
apex context use acme-dev
apex context show
apex context set --project sandbox        # mutate the current context
```

Most commands accept `--server`, `--tenant`, `--project` to override the active
context per invocation.

---

# 5. Authentication

## 5.1 Interactive login (OAuth Device Flow)

```bash
apex login                      # opens browser / shows device code
apex login --server https://api.apex.example.com
apex logout
```

Uses the OAuth2 Device Code flow ([API auth §3](../09-api/authentication.md#3-oauth2--oidc)).
Tokens are stored in the OS keychain (or an encrypted file fallback), never in the
plaintext config; refresh is automatic.

## 5.2 API key (CI / service accounts)

```bash
export APEX_API_KEY="apx_live_..."
apex agents list                # uses the key from the environment
```

API keys ([API auth §5](../09-api/authentication.md#5-api-keys)) are ideal for
non-interactive environments. The CLI never prints stored secrets.

## 5.3 mTLS

For zero-trust networks, a context may reference a client certificate for
[mTLS](../09-api/authentication.md#2-credential-types).

---

# 6. Environment Variables

| Variable | Purpose |
|----------|---------|
| `APEX_CONFIG` | Config file path |
| `APEX_CONTEXT` | Override active context |
| `APEX_SERVER` | Override server URL |
| `APEX_TENANT` / `APEX_PROJECT` | Override scope |
| `APEX_API_KEY` | API key auth |
| `APEX_OUTPUT` | Default output format |
| `APEX_NO_COLOR` | Disable colored output |
| `APEX_LOG` | Log level (`error`..`trace`) |

`APEX_*` variables make the CLI fully configurable for CI without a config file.

---

# 7. Output & Scripting

```bash
apex agents list --output json | jq '.data[].name'
apex workflows get wf_01H... -o yaml
```

- `--output table` (human) / `json` / `yaml`.
- `--quiet` suppresses non-essential output; IDs print to stdout for piping.
- Errors print the [standard error envelope](../09-api/overview.md#8-error-model)
  (in `json` mode) and a friendly message otherwise.

---

# 8. Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic error |
| 2 | Usage / invalid arguments |
| 3 | Authentication required/failed |
| 4 | Authorization denied |
| 5 | Not found |
| 6 | Conflict |
| 7 | Rate limited |
| 8 | Server error |

Stable exit codes make the CLI safe to use in scripts and pipelines.

---

# 9. Local Mode Settings

`--local` runs against the embedded runtime (no server). Local settings (data
directory, default provider keys for local dev) live under `~/.apex/local/`. See
[Commands §10](commands.md#10-local-development) and
[Examples](examples.md).

---

# 10. Telemetry

The CLI may send anonymized usage telemetry; disable with
`apex config set telemetry off` or `APEX_TELEMETRY=off`.

---

# 11. Dependencies

- [`09-api/authentication.md`](../09-api/authentication.md)
- [`11-cli/commands.md`](commands.md)

---

# 12. Related Documents

- [`11-cli/index.md`](index.md)
- [`11-cli/installation.md`](installation.md)
- [`11-cli/examples.md`](examples.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial CLI Configuration |
