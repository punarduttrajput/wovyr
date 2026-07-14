<!--
File: docs/12-deployment/systemd.md
Document ID: DEP-SYSTEMD-001
-->

# Bare-Metal / systemd Install

**Document ID:** DEP-SYSTEMD-001
**File Path:** `docs/12-deployment/systemd.md`
**Version:** 1.0.0
**Status:** Active
**Owner:** Platform Operations Team
**Last Updated:** 2026-07-14

---

# 1. Purpose

This is the install guide for running the Apex AI Platform as a bare-metal
**systemd-managed appliance** (RM-AIM-P3 DEP-301) — the packaging story the
README has marketed since [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)
(Path A: "GA ships as a single-node appliance") but that, until this ticket,
had no install artifact — only container/Kubernetes paths existed. If you're
deploying via Docker/Compose/Kubernetes instead, see
[docker.md](docker.md)/[docker-compose.md](docker-compose.md)/[kubernetes.md](kubernetes.md).

There is exactly one `apex` binary and one server entrypoint: `apex dev
--addr <host:port>` (there is no separate `apex serve` command — "dev" is a
naming quirk, not a mode distinction; see
`crates/apex-server/src/lib.rs`'s `serve()`, which this same command calls).

---

# 2. Artifacts

| File | Purpose |
|------|---------|
| [`deployment/install.sh`](../../deployment/install.sh) | Creates the system user, state directory, installs the binary + unit + env file |
| [`deployment/systemd/apex.service`](../../deployment/systemd/apex.service) | The systemd unit |
| [`deployment/systemd/apex.env.example`](../../deployment/systemd/apex.env.example) | Commented environment-file template (copied to `/etc/apex/apex.env` on first install) |

---

# 3. Install

```bash
# From a checkout of this repo, on the target host (or after copying a
# pre-built release binary over):
cargo build --release -p apex-cli   # skip if you already have the binary
sudo ./deployment/install.sh
```

`install.sh` (idempotent — safe to re-run after building a new binary):

1. Creates a dedicated system user + group `apex` (home `/var/lib/apex`, no
   login shell) if one doesn't already exist.
2. Creates `/var/lib/apex/.apex` (`0700`, owned by `apex:apex`) — the durable
   state root every store under `crates/apex-config/src/paths.rs` lives under
   (secrets vault, KMS keys, memory, workflows, tenancy catalog, audit log,
   plugin trust store, ...).
3. Installs (building first if needed) the binary to `/usr/local/bin/apex`.
4. Installs the unit to `/etc/systemd/system/apex.service`.
5. Installs `apex.env.example` to `/etc/apex/apex.env` (`0640`,
   `root:apex`) — **only if that file doesn't already exist**, so a re-run
   never clobbers your edits.
6. Runs `systemctl daemon-reload`.

It deliberately does **not** enable or start the service — review
`/etc/apex/apex.env` first (§4), then:

```bash
sudo systemctl enable --now apex
sudo systemctl status apex
journalctl -u apex -f
curl http://127.0.0.1:8080/healthz   # or your configured APEX_BIND_ADDR
```

Custom binary location or install prefix: `sudo ./deployment/install.sh
--binary /path/to/apex --prefix /opt/apex/bin` — the installed unit's
`ExecStart` is rewritten to match the given `--prefix` automatically.

---

# 4. Configuration (`/etc/apex/apex.env`)

The shipped default is **loopback-only, `disabled-loopback` auth** — safe out
of the box because nothing but the local host can reach it, but not suitable
for exposing beyond localhost as-is. `apex.env.example` documents every
variable inline; the load-bearing ones:

| Variable | Purpose |
|----------|---------|
| `APEX_BIND_ADDR` | **Required** — substituted straight into the unit's `ExecStart`. Default `127.0.0.1:8080`. |
| `APEX_AUTH_MODE` | `disabled-loopback` (default) / `apikey` / `jwt` — see [authentication.md](../13-security/authentication.md). |
| `APEX_TLS_CERT` / `APEX_TLS_KEY` | Required (or `APEX_TLS_TERMINATED_UPSTREAM=1`) to bind a non-loopback address at all (SEC-202) — the server refuses to start otherwise. |
| `APEX_KMS_ROOT_KEY` | Escrow this from a real secrets manager/HSM before sealing real data — see [backup-and-restore.md](backup-and-restore.md) §3's root-key-escrow note. Unset generates and persists a key under `/var/lib/apex/.apex/kms/root.key` on first use, evaluation-only. |
| `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` | LLM provider credentials. |

Changed the env file? `sudo systemctl restart apex` (the unit has no
config-reload signal — a restart is required).

---

# 5. Sandboxing

The shipped unit applies a moderate, real (not decorative) set of systemd
hardening directives: `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`
with `ReadWritePaths=/var/lib/apex` as the one writable exception,
`ProtectHome`, `ProtectKernelTunables`/`ProtectKernelModules`,
`ProtectControlGroups`, `RestrictSUIDSGID`. This is **not a one-size-fits-all
guarantee**: if you enable `apex-tools`' `shell`/`code_execute` builtins
(`APEX_ENABLE_SHELL_TOOL=1` — off by default) on a node with no
container/gVisor sandbox backend available, those tools spawn native child
processes that may need scratch space outside `/var/lib/apex`; relax
`ReadWritePaths` (or `ProtectSystem`) accordingly rather than disabling the
whole hardening block. `systemctl edit apex` applies a drop-in override
without touching the shipped unit file.

---

# 6. Backup, upgrade, uninstall

- **Backup/restore**: [backup-and-restore.md](backup-and-restore.md) — `apex
  admin backup`/`restore` operate on `/var/lib/apex/.apex` regardless of how
  the binary is invoked; run them as the `apex` user
  (`sudo -u apex apex admin backup ...`) so file ownership stays correct.
- **Upgrade**: build/install a new binary, re-run `install.sh` (only the
  binary and unit are replaced), `sudo systemctl restart apex`.
- **Uninstall**: `sudo systemctl disable --now apex && sudo rm
  /etc/systemd/system/apex.service /usr/local/bin/apex && sudo systemctl
  daemon-reload`. This deliberately leaves `/var/lib/apex` (durable state) and
  `/etc/apex` (config) in place — remove those explicitly if you want a full
  wipe: `sudo rm -rf /var/lib/apex /etc/apex` (irreversible — back up first if
  in doubt).

---

# 7. CI verification

`.github/workflows/ci.yml`'s `systemd-install` job runs this exact install
path on a real `ubuntu-latest` runner (a full VM with a working systemd, not a
container) on every PR: builds the binary, runs `install.sh`, enables +
starts the service, and curls `/healthz` — so a change that breaks the unit,
the env file contract, or the install script itself fails CI rather than
being discovered on a real host.

---

# 8. Related Documents

- [index.md](index.md) — deployment topology overview
- [docker.md](docker.md) — the containerized equivalent
- [backup-and-restore.md](backup-and-restore.md) — RPO/RTO and root-key escrow
- [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md) — why single-node appliance is the shipped topology

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-14 | Initial version (RM-AIM-P3 DEP-301): `deployment/install.sh` + `deployment/systemd/{apex.service,apex.env.example}`, CI smoke test |
