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

This is the install guide for running the Wovyr AI Platform as a bare-metal
**systemd-managed appliance** (RM-AIM-P3 DEP-301) — the packaging story the
README has marketed since [ADR-0010](../17-adr/ADR-0010-ga-deployment-topology.md)
(Path A: "GA ships as a single-node appliance") but that, until this ticket,
had no install artifact — only container/Kubernetes paths existed. If you're
deploying via Docker/Compose/Kubernetes instead, see
[docker.md](docker.md)/[docker-compose.md](docker-compose.md)/[kubernetes.md](kubernetes.md).

There is exactly one `wovyr` binary and one server entrypoint: `wovyr dev
--addr <host:port>` (there is no separate `wovyr serve` command — "dev" is a
naming quirk, not a mode distinction; see
`crates/wovyr-server/src/lib.rs`'s `serve()`, which this same command calls).

---

# 2. Artifacts

| File | Purpose |
|------|---------|
| [`deployment/install.sh`](../../deployment/install.sh) | Creates the system user, state directory, installs the binary + unit + env file |
| [`deployment/systemd/wovyr.service`](../../deployment/systemd/wovyr.service) | The systemd unit |
| [`deployment/systemd/wovyr.env.example`](../../deployment/systemd/wovyr.env.example) | Commented environment-file template (copied to `/etc/wovyr/wovyr.env` on first install) |

---

# 3. Install

```bash
# From a checkout of this repo, on the target host (or after copying a
# pre-built release binary over):
cargo build --release -p wovyr-cli   # skip if you already have the binary
sudo ./deployment/install.sh
```

`install.sh` (idempotent — safe to re-run after building a new binary):

1. Creates a dedicated system user + group `wovyr` (home `/var/lib/wovyr`, no
   login shell) if one doesn't already exist.
2. Creates `/var/lib/wovyr/.wovyr` (`0700`, owned by `wovyr:wovyr`) — the durable
   state root every store under `crates/wovyr-config/src/paths.rs` lives under
   (secrets vault, KMS keys, memory, workflows, tenancy catalog, audit log,
   plugin trust store, ...).
3. Installs (building first if needed) the binary to `/usr/local/bin/wovyr`.
4. Installs the unit to `/etc/systemd/system/wovyr.service`.
5. Installs `wovyr.env.example` to `/etc/wovyr/wovyr.env` (`0640`,
   `root:wovyr`) — **only if that file doesn't already exist**, so a re-run
   never clobbers your edits.
6. Runs `systemctl daemon-reload`.

It deliberately does **not** enable or start the service — review
`/etc/wovyr/wovyr.env` first (§4), then:

```bash
sudo systemctl enable --now wovyr
sudo systemctl status wovyr
journalctl -u wovyr -f
curl http://127.0.0.1:8080/healthz   # or your configured WOVYR_BIND_ADDR
```

Custom binary location or install prefix: `sudo ./deployment/install.sh
--binary /path/to/wovyr --prefix /opt/wovyr/bin` — the installed unit's
`ExecStart` is rewritten to match the given `--prefix` automatically.

---

# 4. Configuration (`/etc/wovyr/wovyr.env`)

The shipped default is **loopback-only, `disabled-loopback` auth** — safe out
of the box because nothing but the local host can reach it, but not suitable
for exposing beyond localhost as-is. `wovyr.env.example` documents every
variable inline; the load-bearing ones:

| Variable | Purpose |
|----------|---------|
| `WOVYR_BIND_ADDR` | **Required** — substituted straight into the unit's `ExecStart`. Default `127.0.0.1:8080`. |
| `WOVYR_AUTH_MODE` | `disabled-loopback` (default) / `apikey` / `jwt` — see [authentication.md](../13-security/authentication.md). |
| `WOVYR_TLS_CERT` / `WOVYR_TLS_KEY` | Required (or `WOVYR_TLS_TERMINATED_UPSTREAM=1`) to bind a non-loopback address at all (SEC-202) — the server refuses to start otherwise. |
| `WOVYR_KMS_ROOT_KEY` | Escrow this from a real secrets manager/HSM before sealing real data — see [backup-and-restore.md](backup-and-restore.md) §3's root-key-escrow note. Unset generates and persists a key under `/var/lib/wovyr/.wovyr/kms/root.key` on first use, evaluation-only. |
| `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` | LLM provider credentials. |

Changed the env file? `sudo systemctl restart wovyr` (the unit has no
config-reload signal — a restart is required).

---

# 5. Sandboxing

The shipped unit applies a moderate, real (not decorative) set of systemd
hardening directives: `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`
with `ReadWritePaths=/var/lib/wovyr` as the one writable exception,
`ProtectHome`, `ProtectKernelTunables`/`ProtectKernelModules`,
`ProtectControlGroups`, `RestrictSUIDSGID`. This is **not a one-size-fits-all
guarantee**: if you enable `wovyr-tools`' `shell`/`code_execute` builtins
(`WOVYR_ENABLE_SHELL_TOOL=1` — off by default) on a node with no
container/gVisor sandbox backend available, those tools spawn native child
processes that may need scratch space outside `/var/lib/wovyr`; relax
`ReadWritePaths` (or `ProtectSystem`) accordingly rather than disabling the
whole hardening block. `systemctl edit wovyr` applies a drop-in override
without touching the shipped unit file.

---

# 6. Backup, upgrade, uninstall

- **Backup/restore**: [backup-and-restore.md](backup-and-restore.md) — `wovyr
  admin backup`/`restore` operate on `/var/lib/wovyr/.wovyr` regardless of how
  the binary is invoked; run them as the `wovyr` user
  (`sudo -u wovyr wovyr admin backup ...`) so file ownership stays correct.
- **Upgrade**: build/install a new binary, re-run `install.sh` (only the
  binary and unit are replaced), `sudo systemctl restart wovyr`.
- **Uninstall**: `sudo systemctl disable --now wovyr && sudo rm
  /etc/systemd/system/wovyr.service /usr/local/bin/wovyr && sudo systemctl
  daemon-reload`. This deliberately leaves `/var/lib/wovyr` (durable state) and
  `/etc/wovyr` (config) in place — remove those explicitly if you want a full
  wipe: `sudo rm -rf /var/lib/wovyr /etc/wovyr` (irreversible — back up first if
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
| 1.0.0 | 2026-07-14 | Initial version (RM-AIM-P3 DEP-301): `deployment/install.sh` + `deployment/systemd/{wovyr.service,wovyr.env.example}`, CI smoke test |
