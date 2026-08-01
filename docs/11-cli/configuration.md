<!--
File: docs/11-cli/configuration.md
Document ID: CLI-002
-->

# CLI Configuration

**Document ID:** CLI-002  
**File Path:** `docs/11-cli/configuration.md`  
**Version:** 2.0.0  
**Status:** Shipped — describes what the `wovyr` binary actually reads today.
Everything here is verifiable against
[`apps/wovyr-cli/src/config.rs`](../../apps/wovyr-cli/src/config.rs) and the
generated [command reference](commands.md).  
**Owner:** AI Platform Team  
**Last Updated:** 2026-08-01

---

# 1. Purpose

How the `wovyr` CLI is configured: where it keeps state, how it authenticates
against a server, and every environment variable it reads.

> **Scope note.** Version 1.0.0 of this document described a context/profile
> system, an OAuth device-code login, OS-keychain token storage, mTLS, global
> `--tenant`/`--project`/`--output` flags, and eight `WOVYR_*` variables — none of
> which were ever implemented. It was rewritten to describe the shipped CLI. The
> unbuilt design is not a roadmap commitment; if any of it is revived it will be
> scoped through the normal PRD → ADR flow.

---

# 2. Configuration Precedence

There is no config file and no profile system. Settings resolve as:

```text
1. Command-line flags        (--server, --local, --tenant, ...)
2. Environment variables     (the tables in §5)
3. Stored credentials        (~/.wovyr/credentials.json, written by `wovyr login`)
4. Built-in defaults
```

Flags are **per-subcommand**, not global — `wovyr --help` lists only `-h`/`-V`.
`--server` and `--tenant`, where they exist, are declared on the individual
commands that accept them (see [commands.md](commands.md)).

---

# 3. State Directory

All local state lives under `~/.wovyr` (`%USERPROFILE%\.wovyr` on Windows),
resolved in one place by
[`wovyr-config`](../../crates/wovyr-config/src/paths.rs) so the CLI and the
`wovyr dev` server always agree on it.

| Path | Contents | Written by |
|---|---|---|
| `~/.wovyr/credentials.json` | Server URL + access token (owner-only permissions) | `login` / `logout` |
| `~/.wovyr/kms/` | KMS root key + tenant-key catalog | `kms`, any sealing operation |
| `~/.wovyr/secrets/` | The secret vault (encrypted at rest by default) | server, plugin secret injection |
| `~/.wovyr/memory/` | Memory engine file store | `memory` |
| `~/.wovyr/workflows/` | Executions, checkpoints, timers, schedules | `workflows` |
| `~/.wovyr/plugins/` | Trust store, installed catalog, staged artifacts | `plugin` |
| `~/.wovyr/marketplace/` | Local registry (`registry.json`) | `plugin publish`/`search`/`get` |
| `~/.wovyr/mcp/` | MCP connection store | resolved by an agent's `spec.mcp_servers` |
| `~/.wovyr/auth/` | The server's API-key store | `auth` |

`wovyr admin backup <dest>` snapshots this whole tree (and `admin restore` puts
it back) — see [backup-and-restore](../12-deployment/backup-and-restore.md).

---

# 4. Authentication

## 4.1 Storing credentials

```bash
wovyr login --server https://api.wovyr.example.com --token "$TOKEN"
wovyr whoami        # prints the server + a masked token
wovyr logout        # deletes ~/.wovyr/credentials.json
```

`--token` falls back to the `WOVYR_TOKEN` environment variable, which is the
supported way to authenticate in CI without an interactive step. The token is
sent as `Authorization: Bearer <token>` on remote requests, is never logged, and
the credentials file is created owner-only where the platform supports it.

There is **no** OAuth device flow, no OS-keychain integration, and no mTLS
support in the CLI. The target-state design for those lives in
[API authentication](../09-api/authentication.md), which is itself explicitly
labelled target-state.

## 4.2 Minting a server API key

When the server runs with `WOVYR_AUTH_MODE=apikey`, mint the credential a client
presents:

```bash
wovyr auth create-key alice --ttl-days 90
wovyr auth list-keys
wovyr auth revoke <key-id>
wovyr auth rotate <key-id> --grace-hours 24
```

This writes to the server's own `~/.wovyr/auth` store, so it must run on the
server's host. See
[SEC-101](../18-roadmap/v1.0/phase1-security-floor-tickets.md).

---

# 5. Environment Variables

Only the variables below are read by the CLI. Server-side variables
(`WOVYR_AUTH_MODE`, `WOVYR_TLS_*`, `WOVYR_RATE_LIMIT_*`, `WOVYR_PLATFORM_ADMINS`,
…) are documented with the server — they affect `wovyr dev` because it embeds
the server, not because the CLI itself reads them.

## 5.1 Authentication & logging

| Variable | Purpose |
|---|---|
| `WOVYR_TOKEN` | Access token, used by `login --token` when the flag is omitted |
| `WOVYR_LOG` | Log level (`error`…`trace`) |
| `WOVYR_LOG_FORMAT` | `json` for structured stderr logs |

## 5.2 Local runs

| Variable | Purpose |
|---|---|
| `WOVYR_LOCAL_PRIVILEGED` | `1` registers the privileged builtins (`shell`, `fs_write`, `code_execute`) for the whole session — the session-wide equivalent of `--allow-privileged-tools`, and the **only** way to enable them on the `workflows approve`/`signal`/`tick` resume paths, which take no flag (SBX-305) |
| `WOVYR_MISTRALRS_GGUF_REPO` / `_GGUF_FILE` / `_TOK_MODEL_ID` | Override the GGUF model `--provider mistralrs` loads (requires a `--features mistralrs` build) |
| `OPENAI_API_KEY` / `WOVYR_OPENAI_BASE_URL` | Select and point the OpenAI-compatible provider |
| `ANTHROPIC_API_KEY` / `WOVYR_ANTHROPIC_BASE_URL` | Select and point the native Anthropic Messages provider |

## 5.3 Keys, secrets & storage backends

| Variable | Purpose |
|---|---|
| `WOVYR_KMS_ROOT_KEY` | Hex root key. The production mode — forces escrow up front instead of relying on the generate-once `~/.wovyr/kms/root.key` |
| `WOVYR_KMS_ALLOW_EPHEMERAL` | `1` permits a throwaway in-memory key. **Test/dev only** — sealed data is unrecoverable after exit (SEC-405) |
| `WOVYR_SECRETS_PLAINTEXT` | `1` opts out of at-rest secret encryption, back to a plaintext `secrets.json` (SEC-101) |
| `WOVYR_MEMORY_POSTGRES_URL` / `WOVYR_MEMORY_QDRANT_URL` / `WOVYR_MEMORY_QDRANT_COLLECTION` | Select the tiered memory backend (requires a `--features tiered-memory` build; falls back to the file store when unset) |
| `WOVYR_MARKETPLACE_POSTGRES_URL` | Select the shared Postgres marketplace registry (requires a `--features postgres` build) |

## 5.4 S3 backup targets

Used when `admin backup`/`restore` is given an `s3://bucket/prefix` URI:

| Variable | Purpose |
|---|---|
| `WOVYR_S3_ENDPOINT` | S3-compatible endpoint URL |
| `WOVYR_S3_REGION` | Region for SigV4 signing |
| `WOVYR_S3_ACCESS_KEY_ID` / `WOVYR_S3_SECRET_ACCESS_KEY` | Credentials |

---

# 6. Output & Exit Codes

Output is human-readable text; several commands print JSON where the payload is
inherently JSON (a run result, an execution's event timeline). There is no
`--output`/`-o` format flag, no `--quiet`, and no colour control.

Exit codes are currently binary — `0` on success, `1` on any error, with the
message on stderr. Clap contributes its own `2` for a usage error. The graded
3–8 scheme in earlier revisions of this document was never implemented; do not
branch on it in scripts.

---

# 7. Local Mode

`--local` runs against the embedded runtime with no server. It shares the same
`~/.wovyr` state directory described in §3 — there is no separate
`~/.wovyr/local/`. With no provider key set, local runs use the deterministic
mock provider, which is what makes the examples reproducible offline.

Privileged builtins are **off** by default in local mode; opt in per-run with
`--allow-privileged-tools` or per-session with `WOVYR_LOCAL_PRIVILEGED=1`
(SBX-305).

---

# 8. Telemetry

The CLI sends **no** telemetry. There is no `wovyr config` command and no
`WOVYR_TELEMETRY` variable.

---

# 9. Related Documents

- [`11-cli/index.md`](index.md)
- [`11-cli/installation.md`](installation.md)
- [`11-cli/commands.md`](commands.md) — generated from the real clap tree
- [`11-cli/examples.md`](examples.md)
- [`09-api/authentication.md`](../09-api/authentication.md) — target-state auth design

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 2.0.0 | 2026-08-01 | Rewritten against the shipped CLI: removed the never-implemented context/profile system, OAuth device flow, keychain storage, mTLS, output-format flags, graded exit codes, `~/.wovyr/local/`, and telemetry; replaced the eight fictional `WOVYR_*` variables with the ones the binary actually reads; documented the real `~/.wovyr` layout |
| 1.0.0 | 2026-06-27 | Initial CLI Configuration (target-state design, largely unimplemented) |
