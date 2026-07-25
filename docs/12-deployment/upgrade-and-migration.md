<!--
File: docs/12-deployment/upgrade-and-migration.md
Document ID: DEP-UPG-001
-->

# Upgrade & Migration Runbook

**Document ID:** DEP-UPG-001
**File Path:** `docs/12-deployment/upgrade-and-migration.md`
**Version:** 1.0.0
**Status:** Active — every command here exists and is CI-tested; the runbook as a
whole has been walked through on a dev host, not yet on a production fleet
**Owner:** Platform Operations Team
**Last Updated:** 2026-07-18

---

# 1. Purpose

The operator's end-to-end procedure for upgrading a running Wovyr deployment from
one released version to the next (RM-AIM-P3 DEP-302): back up, swap the binary,
run schema migrations, verify, and — if needed — roll back. Variants for the three
supported deployment shapes: **systemd** ([systemd.md](systemd.md)), **Compose**
([docker-compose.md](docker-compose.md)), and **Helm** ([helm.md](helm.md)).

---

# 2. What an upgrade touches

| Surface | Mechanism | Version coupling |
|---------|-----------|------------------|
| The `wovyr` binary (server + CLI are one binary) | Replace and restart | Release tags; see `CHANGELOG.md` |
| `~/.wovyr` durable state (auth, secrets, KMS, workflows, webhooks, tenancy, audit, …) | Carried forward in place; **no migration step** — on-disk formats are versioned and fail closed on skew | Wrapped payloads (workflow events, UI frames) reject *newer-than-understood* versions, so never run an **older** binary against state a newer binary wrote (see §7) |
| Postgres schemas (workflow, memory, marketplace — only if you opted into these backends) | `wovyr admin migrate`, the **only** thing that ever runs DDL | `connect` reads the schema version and fails closed on any mismatch — an un-migrated binary refuses to start serving that backend, and an old binary refuses a newer schema |
| SDKs (`@wovyr/sdk`, `wovyr-sdk`) | Client-side upgrade, out of band | Same `major.minor` as the platform = same API; `health()` warns once per client on skew |

Startup after any restart is self-healing by design: in-flight durable workflow
executions are resumed, the webhook outbox re-dispatches pending deliveries, and
interrupted **bare** async agent runs (not resumable by design) are reconciled to
`Failed` rather than left `Running` forever.

---

# 3. Pre-upgrade checklist

1. **Read the release notes** for every version between yours and the target
   (`CHANGELOG.md` on the GitHub release). Pre-GA, breaking wire/on-disk changes
   are permitted and called out there.
2. **Confirm your KMS root key is escrowed** (`WOVYR_KMS_ROOT_KEY`, or the
   generate-once `~/.wovyr/kms/root.key`). A backup without it does not restore
   secrets/encrypted memory — see
   [backup-and-restore.md §3](backup-and-restore.md).
3. **Note which Postgres-backed features you use** (`--target workflow`,
   `memory`, `marketplace`) — each has its own schema and its own migrate call.
4. **Plan the window.** The server drains gracefully on SIGTERM
   (`WOVYR_SHUTDOWN_GRACE_SECS`, default 30s), so expect up to that long between
   "stop" and "stopped". Single-node deployments have no rolling path — this is
   a brief hard outage; suspended workflows and queued webhooks resume on start.

---

# 4. The upgrade, end to end

The same five steps in every deployment shape; §5 gives the shape-specific
commands for steps 2–4.

**Step 1 — Backup.** With the old binary, while the server is still running
(backup quiesces writers via the shared `~/.wovyr` file locks — no stop needed):

```bash
wovyr admin backup /var/backups/wovyr/pre-$(date +%Y%m%d)   # or s3://bucket/prefix
```

If you use Postgres backends, also snapshot the database with your normal tooling
(`pg_dump`/provider snapshot) — `wovyr admin backup` covers `~/.wovyr` only.

**Step 2 — Stop the old server.** SIGTERM and wait for the drain (§5 per shape).

**Step 3 — Swap the binary/image** (§5 per shape).

**Step 4 — Migrate each Postgres schema you use** — *before* starting the new
server, once per schema, from any host that can reach the database:

```bash
wovyr admin migrate --target workflow    --database-url "$WOVYR_PG_URL"
wovyr admin migrate --target memory      --database-url "$WOVYR_MEMORY_POSTGRES_URL"
wovyr admin migrate --target marketplace --database-url "$WOVYR_MARKETPLACE_POSTGRES_URL"
```

Migrations are versioned (refinery), forward-only, and idempotent — re-running
against an already-migrated schema is a no-op. Skipping this step is safe-but-
loud: the new binary's `connect` fails closed with a clear version-mismatch error
rather than limping on a schema it doesn't match. There is no auto-migrate on
startup, deliberately.

**Step 5 — Start and verify (§6).**

---

# 5. Per-shape commands (steps 2–4)

## systemd (bare-metal appliance)

```bash
sudo systemctl stop wovyr                       # SIGTERM + drain
sudo ./deployment/install.sh --binary ./wovyr   # idempotent: replaces /usr/local/bin/wovyr,
                                               # never clobbers /etc/wovyr/wovyr.env
# step 4 migrations here (run as the wovyr user if the DB URL lives in wovyr.env:
#   sudo -u wovyr env $(grep -v '^#' /etc/wovyr/wovyr.env | xargs) wovyr admin migrate ...)
sudo systemctl start wovyr
```

`install.sh` is CI-tested to preserve an existing `/etc/wovyr/wovyr.env` — operator
config survives the upgrade. If you don't use install.sh, replace
`/usr/local/bin/wovyr` by hand and `systemctl start wovyr`.

## Compose

```bash
docker compose -f deployment/docker-compose.yml stop wovyr   # drains via SIGTERM
# bump the wovyr image tag in docker-compose.yml (or your override file)
docker compose -f deployment/docker-compose.yml pull wovyr
# step 4 migrations, e.g. through the new image against the compose network:
docker compose -f deployment/docker-compose.yml run --rm wovyr \
  admin migrate --target workflow --database-url "$WOVYR_PG_URL"
docker compose -f deployment/docker-compose.yml up -d wovyr
```

## Helm

```bash
# step 4 first (kubectl run a one-off pod with the new image, or port-forward
# Postgres and run migrate from a workstation), then:
helm upgrade wovyr deployment/helm/wovyr --reuse-values \
  --set wovyr.image.tag=<new-version>
kubectl rollout status statefulset/wovyr
```

The chart is a single-replica StatefulSet over a PVC — `helm upgrade` recreates
the one pod (brief outage, same as the other shapes), and `~/.wovyr` rides the
PVC through it.

---

# 6. Post-upgrade verification

```bash
# 1. The process is up and reports the new version:
curl -fsS http://127.0.0.1:8080/healthz          # {"status":"ok","version":"<new>"}
# 2. Metrics scrape works and the operability gauges recomputed from the stores:
curl -fsS http://127.0.0.1:8080/metrics | grep -E "wovyr_(workflow_executions_active|webhook_outbox_pending)"
# 3. Postgres-backed features actually connect (fails closed if a migrate was missed):
wovyr workflows list --server http://127.0.0.1:8080
# 4. A smoke run end to end:
wovyr agents run --server http://127.0.0.1:8080 -f examples/agents/hello.yaml --input '{"message":"Hi"}'
```

Then watch the logs for one dispatcher interval (`WOVYR_DISPATCH_INTERVAL_SECS`,
default 5s) to confirm startup recovery: resumed executions, re-dispatched
webhook deliveries, and any bare runs reconciled to `Failed` are all logged.
Finally, expect SDK clients on the old `major.minor` to log a one-time
version-skew warning from `health()` — upgrade the SDKs at your convenience
(same `major.minor` = same API).

---

# 7. Rollback

Two distinct cases:

**The new binary misbehaves but wrote nothing you need to keep** (caught in
verification): stop it, reinstall the previous binary, and — because on-disk
formats fail closed on *newer* versions — restore the pre-upgrade state rather
than pointing the old binary at state the new one may have touched:

```bash
wovyr admin restore /var/backups/wovyr/pre-<date> --yes   # verifies the sha256 manifest before writing
```

For Postgres backends, restore the matching database snapshot from step 1.
There is no `migrate --down`: schema rollback is *restore the snapshot*, by
design (forward-only migrations keep the version ledger unambiguous).

**The new version has been live and accepted real traffic**: rolling back means
losing the writes since the upgrade (restore = the backup's contents; RPO is
your backup cadence — see [backup-and-restore.md §5](backup-and-restore.md)).
Prefer rolling *forward* to a fixed patch release; treat restore-based rollback
as the disaster path.

---

# 8. Version-skew rules (summary)

- **Never** run an older binary against `~/.wovyr` state or a Postgres schema a
  newer binary has written/migrated — every versioned surface (workflow event
  envelopes, UI frame schema, refinery schema versions) rejects
  newer-than-understood data by design. Rollback = old binary **plus** restored
  old state, never old binary alone.
- The single binary serves CLI and server, and both share `~/.wovyr` — upgrade
  them as one unit on a host; don't run a new CLI against an old server's state
  directory on the same machine.
- SDKs are forward/backward tolerant within the same `major.minor`; across
  minors, `health()` warns and `CHANGELOG.md` (per SDK) lists what changed.

---

# 9. Related documents

- [backup-and-restore.md](backup-and-restore.md) — what `wovyr admin
  backup`/`restore` covers, S3 targets, RPO/RTO, KMS escrow
- [systemd.md](systemd.md) / [docker-compose.md](docker-compose.md) /
  [helm.md](helm.md) — the three deployment shapes
- [terraform.md](terraform.md) — infrastructure provisioning status (spec-only;
  decision recorded there)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-18 | RM-AIM-P3 DEP-302: initial end-to-end upgrade/migration runbook (backup → swap → migrate → verify → rollback, per-shape variants) |
