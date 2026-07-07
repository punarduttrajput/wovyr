<!--
File: docs/12-deployment/backup-and-restore.md
Document ID: DEP-BKUP-001
-->

# Backup, Restore & DR Targets (Single-Node Appliance)

**Document ID:** DEP-BKUP-001
**File Path:** `docs/12-deployment/backup-and-restore.md`
**Version:** 1.0.0
**Status:** Active — `apex admin backup`/`restore` and KMS root-key escrow are
implemented and tested; the RPO/RTO targets below are validated by a real,
timed drill (§4), not aspirational numbers.
**Owner:** Reliability / Deployment Team
**Last Updated:** 2026-07-07

---

# 1. Purpose

Scope: **the single-node appliance** (one `apex` binary, local `~/.apex`
state) — not the multi-replica/HA topology
[A2-reliability-ha-dr.md](../18-roadmap/v1.0/A2-reliability-ha-dr.md) scopes
separately. This document defines what gets backed up, how to back it up and
restore it, and the RPO/RTO targets that backup/restore cadence must meet
(RM-GA-P2 DR-1001/DR-1002/DR-1003).

---

# 2. What Gets Backed Up

Every durable store the CLI/server share lives under `~/.apex`: agents,
secrets, memory, workflows (executions, definitions, the timer/schedule
stores), tenancy, the audit chain, webhooks, the KMS tenant-key catalog, the
marketplace registry, the plugin catalog/trust store, server-local state
(quota/idempotency), and the CLI's `credentials.json`. `apex admin backup`
copies **all of it** in one pass — there is no per-store opt-out, since a
partial snapshot would be a silent gap in exactly the store an operator
forgot mattered.

**Not covered by `apex admin backup`:**
- **The KMS root key**, when sourced via the recommended
  `APEX_KMS_ROOT_KEY` production mode — by design, it is never written to
  disk in that mode, so there is nothing under `~/.apex` for a directory
  backup to capture. It has its own, separate escrow requirement (§3).
- **A Postgres-backed marketplace registry** (`PostgresRegistryStore`, the
  `postgres` cargo feature) — that state lives in Postgres, not
  `~/.apex/marketplace`; back it up with `pg_dump` on your normal Postgres
  DR schedule.

---

# 3. `apex admin backup` / `restore`

```bash
apex admin backup <dest>          # snapshot ~/.apex into <dest>
apex admin restore <src> --yes    # restore ~/.apex from a backup made above
```

**Mechanics** (see `apps/apex-cli/src/admin.rs`, RM-GA-P2 DR-1001):
- `backup` acquires the cross-process advisory lock
  ([DUR-403](../18-roadmap/v1.0/phase2-durability-execution-tickets.md)) on
  every existing store directory under `~/.apex` for the duration of the
  copy, so a snapshot taken while the server is live and writing never
  observes a half-written file — it either blocks briefly for an in-flight
  write to finish, or reads the fully-committed prior state.
- Every copied file is recorded in a manifest with its relative path, size,
  and sha256 digest.
- `restore` verifies every entry's digest against the manifest **before**
  touching the live `~/.apex`, so a corrupt or truncated backup fails closed
  instead of partially clobbering real state; writes land via the same
  crash-safe `atomic_write` every store itself uses
  ([DUR-401](../18-roadmap/v1.0/phase2-durability-execution-tickets.md)).
- `restore` requires `--yes` — it overwrites the live `~/.apex`
  irreversibly for anything written since the backup was taken.

**Root-key escrow** is a **separate, one-time** action, not a recurring
backup: set `APEX_KMS_ROOT_KEY` from a key generated and stored in a secrets
manager/HSM/sealed document *before* the appliance ever touches real data.
See [encryption.md §5](../13-security/encryption.md#5-key-management) for
the full rationale and a proven restore test
(`crates/apex-kms/tests/root_key_escrow_restore.rs`).

---

# 4. RPO / RTO Targets

## 4.1 RTO — Recovery Time Objective

> **Target: full data restore completes in under 5 minutes**, once a target
> host has the `apex` binary installed and the backup is reachable.

This covers only the `apex admin restore` step itself — provisioning a
replacement host (OS install, network config, pulling the `apex` image/
binary) is environment-specific and outside this tool's control; add that
lead time on top when planning an actual recovery.

**Measured** (this repo, 2026-07-07, release build, local NVMe-backed temp
storage — a timed drill, not an estimate):

| Scenario | Files | Size | Backup | Restore |
|----------|------:|-----:|-------:|--------:|
| Typical appliance | 425 | 8.8 MiB | 1.4–1.5 s | 1.9 s |
| Heavily used appliance (10×) | 4,025 | 74.5 MiB | 8.5 s | 17.0 s |

Both drills verified a byte-for-byte identical restore (every file's content
matched the pre-loss original; see §4.3 for method). Restore is
consistently slower than backup at the same scale — each restored file goes
through `atomic_write`'s temp-file-write + `fsync` + rename + parent-directory
`fsync` for crash safety, which is more syscall-heavy than backup's plain
write into a fresh, non-live destination — a deliberate trade of restore
speed for restore *safety* (an interrupted restore can never leave a torn
file in the live `~/.apex`).

At the measured 10× scale, restore used **17 s of a 300 s (5 min) budget** —
roughly 17× headroom for further data growth, a slower disk than this test
environment, or a busier host. Re-run the drill (§4.3) periodically as your
appliance's `~/.apex` grows, and revise the RTO target if real usage
approaches this budget.

## 4.2 RPO — Recovery Point Objective

> **Target: no more than 15 minutes of data loss**, achieved by running
> `apex admin backup` on a 15-minute cadence (e.g. a `cron`/systemd timer).

There is no built-in scheduled-backup daemon — `apex admin backup` is an
operator-invoked (or externally scheduled) command, so **RPO is entirely a
function of how often it's run.** 15 minutes is the recommended default,
not a hard limit: the measured backup cost (§4.1) is under 2 seconds at a
typical appliance's scale and well under 10 seconds even at 10× that, so a
tighter cadence (5 minutes, or continuous if your storage supports cheap
snapshots) costs negligible overhead if your tolerance for data loss is
lower. A minimal cron entry:

```cron
*/15 * * * * apex admin backup /mnt/backups/apex-$(date +\%Y\%m\%dT\%H\%M\%S)
```

(Prune old snapshots on your own retention policy — `apex admin backup`
does not manage retention itself.)

**Root-key escrow's "RPO" is different in kind**: it is a one-time action
that must complete *before* the appliance seals any data, not a recurring
backup — see §3.

## 4.3 Drill Method

The measured numbers in §4.1 come from a real, repeatable drill, not a
one-off manual check:

1. Populate a scratch `~/.apex` with representative content across every
   store (agents, secrets, tenancy, audit chain, webhooks, workflow
   executions/definitions, server state, memory records, marketplace
   registry, plugin catalog, credentials) at the target scale.
2. Time `apex admin backup <dest>` against it.
3. Delete the scratch `~/.apex` entirely (the "lost host").
4. Time `apex admin restore <dest> --yes` into a **fresh** `~/.apex`.
5. Diff every restored file against the backup byte-for-byte (excluding the
   backup's own manifest and the `.lock` files `acquire_all_locks` creates,
   neither of which are store data) to confirm the restore is exact, not
   merely fast.

Re-run this drill whenever the appliance's real `~/.apex` grows
substantially, or after a change to `apps/apex-cli/src/admin.rs`, and update
§4.1's table with the new numbers.

---

# 5. Related Documents

- [`18-roadmap/v1.0/phase2-durability-execution-tickets.md`](../18-roadmap/v1.0/phase2-durability-execution-tickets.md) — DUR-401/402/403 (the durability primitives `backup`/`restore` build on), DR-1001/1002/1003
- [`18-roadmap/v1.0/A2-reliability-ha-dr.md`](../18-roadmap/v1.0/A2-reliability-ha-dr.md) — the broader HA/DR remainder (multi-replica, real-cluster validation) this single-node scope feeds into
- [`13-security/encryption.md`](../13-security/encryption.md) §5 — KMS root-key escrow rationale and restore test
- [`12-deployment/docker-compose.md`](docker-compose.md) §10 — running `apex admin backup`/`restore` against the compose stack

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-07 | Initial version: documents `apex admin backup`/`restore` (DR-1001) and root-key escrow (DR-1002), and defines RPO (≤15 min, backup-cadence-driven) / RTO (<5 min restore) targets for the single-node appliance, validated by a real timed drill at two scales (DR-1003) |
