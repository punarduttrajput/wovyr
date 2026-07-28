<!--
File: docs/07-tool-runtime/security-isolation.md
Document ID: TRT-005
-->

# Tool Runtime Security & Isolation

**Document ID:** TRT-005  
**File Path:** `docs/07-tool-runtime/security-isolation.md`  
**Version:** 1.1.0  
**Status:** Draft — the network-isolation section (§5) reflects the current
implementation; other sections (filesystem, secrets, tenant isolation) are still
directional  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-03

---

# 1. Purpose

This document defines the **security model** of the Tool Runtime: the threat model, how untrusted code is contained, how network and filesystem access are constrained, how secrets are injected, and how tenants are isolated.

Tools are the platform's most dangerous surface — they run arbitrary, often third-party code. The Runtime treats every tool as **untrusted by default** and enforces least privilege end to end.

---

# 2. Threat Model

| Threat | Mitigation |
|--------|-----------|
| Malicious tool code | Sandboxing; default-deny network/fs; ephemeral execution |
| Sandbox escape | Strong backends (gVisor/microVM) for untrusted; hardened nodes |
| Data exfiltration | Egress allowlists; no ambient credentials; output size caps |
| Cross-tenant access | Per-tenant pools, namespaces, and storage; no reuse across tenants |
| Secret theft | In-memory injection; never logged; zeroed on teardown |
| Resource exhaustion (DoS) | Hard CPU/mem/disk/time/PID limits; fair scheduling |
| Supply-chain attack | Image signing + provenance verification |
| Privilege escalation | Non-root execution; dropped capabilities; read-only system paths |

---

# 3. Trust Classification

Every tool carries a trust class that drives isolation strength:

| Class | Source | Minimum backend |
|-------|--------|-----------------|
| First-party | Built by the platform team | native / wasm |
| Verified | Third-party, reviewed + signed | container / gVisor |
| Untrusted | Unreviewed / user-supplied | gVisor / microVM |

Tenant policy sets a **floor**: it may require a stronger backend than the tool's
class implies, never weaker (see
[Sandbox Runtime §3](sandbox-runtime.md#3-backend-selection)).

---

# 4. Authorization

Before execution, the Permission Engine evaluates the caller's grants against the
tool's required permissions
([Tool Framework §26–28](../04-agent-framework/tool-framework.md#26-permission-engine))
through the [Policy Engine](../04-agent-framework/policy-engine.md):

```text
Caller grants  ∩  Tool required permissions  ⊆  Policy allow  ⇒ execute
                                              else            ⇒ forbidden
```

Authorization is **fail-closed**: any evaluation error denies execution.

---

# 5. Network Isolation

Default policy is **deny-all egress**. Tools declare required destinations
([Tool Framework §34](../04-agent-framework/tool-framework.md#34-network-policies)),
enforced by the Runtime:

- An egress proxy / network policy allows only declared hosts/ports.
- DNS is restricted to allowed domains (prevents DNS exfiltration).
- No inbound connectivity to sandboxes.
- Per-tenant egress is metered; anomalous volume raises alerts.
- Untrusted tools route egress through an inspecting proxy.

```yaml
network:
  default: deny
  outbound_allow:
    - api.example.com:443
  dns_allow:
    - api.example.com
  inbound: deny
```

**Implemented:** `wovyr-tools`' [`EgressProxy`](../../crates/wovyr-tools/src/egress.rs)
is a host-side HTTP `CONNECT` tunnel enforcing the host allow-list; the container
backend points a sandboxed workload at it via `HTTPS_PROXY`. That alone was only
**cooperative** — a workload that ignored the env var and dialed out directly had
full `--network bridge` connectivity underneath it (the "L3 egress bypass"
gap noted in earlier revisions of this platform). This is now closed: before the
real command ever runs, the *host* attaches to the container's network namespace
via `nsenter` and applies an `iptables` default-deny to its `OUTPUT` chain,
allowing only loopback and the egress proxy's address
([`egress_lockdown`](../../crates/wovyr-tools/src/egress_lockdown.rs)) — so ignoring
`HTTPS_PROXY` now reaches nothing. The container starts running an inert
placeholder and only receives the real command via `docker exec` once the
lockdown is confirmed in place, so there is no window where untrusted code runs
before the restriction applies. Linux/Docker-specific (needs `nsenter` + `iptables`
on the host); fails closed if either is unavailable. Not yet extended to Podman
(its `network inspect` output shape differs) or to DNS-level restriction (the
lockdown allows only the literal egress-proxy IP, so DNS is moot for this path —
the container needs no DNS lookup to reach it).

## 5.1 The native backend's isolation is scoped, not universal (SEC-404)

§5's egress-proxy + `iptables`/`nsenter` lockdown describes the **container/gVisor**
path (verified/untrusted trust classes, or an operator-set policy floor). The
**native** backend — first-party `shell`/`code_execute` runs, the default trust
class — historically enforced **only resource limits** (timeout, output cap,
`setrlimit`/Job Object): no filesystem confinement beyond the run's working
directory, and no network isolation at all. On the default cross-platform path,
"sandboxed tools" was accurate for resource limits and false for confinement — a
native run could read `~/.wovyr/kms/root.key` and exfiltrate it over the open network.

The native path now has a **confinement floor**, not parity with the container path:

- **Linux**, with working unprivileged user+network namespaces: a native run is
  confined to a deny-all egress namespace (`unshare --map-root-user --net`, no
  interfaces configured — no route out). Probed once per process
  (`NativeSandbox::network_isolation_available`).
- **Windows/macOS**, and hardened Linux kernels with unprivileged namespaces
  disabled: there is **no native egress mechanism**. A native run there is
  unsandboxed for network access. This is never silent: it proceeds only as an
  **explicitly-acknowledged** operator choice (the CLI/local trusted context, or
  `WOVYR_ALLOW_UNSANDBOXED_NATIVE=1` on a hosted deployment) — logged loudly on every
  such run — or the tool call is **refused** (`PermissionDenied`) if unacknowledged.
- Filesystem confinement on the native path remains a **documented gap** on every
  platform: the run's working directory scopes relative paths, but nothing prevents
  an absolute path outside it, or a symlink escape. Cross-platform parity to the
  Linux+Docker container path is an explicit non-goal for the native backend (an
  untrusted or filesystem-sensitive run should select the container/gVisor backend
  instead, via trust classification or a policy floor — see §3).

## 5.2 Privileged builtins need an explicit opt-in under `--local` (SBX-305)

§5.1's confinement floor governs *how* a native tool call runs. This section governs
*whether the privileged tools are registered at all* for a local CLI run.

`shell`, `fs_write`, and `code_execute` are not default builtins
(`ToolRegistry::with_builtins()`); they are added only by
`with_privileged_builtins()`. A hosted server requires the operator to set
`WOVYR_ENABLE_SHELL_TOOL=1` before `shell` appears at all (SEC-301). The CLI's
`--local` paths used to call `with_privileged_builtins()` **unconditionally**,
treating "the operator typed `--local`" as the acknowledgement. That is materially
weaker than §5.1's floor, and it could not distinguish the two cases it was applied
to identically:

- a **single-operator trusted workstation**, where full host access to your own
  machine is the documented, accepted design; and
- a **shared, CI, or multi-tenant host**, where the same command grants full host
  access to whatever a model decides to do, for anyone who can supply a manifest.

The 2026-07-27 internal red-team assessment confirmed the consequence concretely: a
real model, driven only by an agent manifest that listed the `shell` tool, read a
host file outside the run's working directory, with nothing but a WARN line in the
way.

Both privileged registration paths now require an explicit signal:

| Command | Opt-in |
|---|---|
| `wovyr agents run --local` | `--allow-privileged-tools` |
| `wovyr workflows run --local` | `--allow-privileged-tools` |
| `wovyr workflows approve` / `signal` / `tick` | `WOVYR_LOCAL_PRIVILEGED=1` (no flag — these resume an existing execution) |

`WOVYR_LOCAL_PRIVILEGED=1` also satisfies the two `run` commands, for a session that
needs privileged tools throughout. Absent any signal, a `--local` run gets exactly the
safe builtins (`echo`, `fs_read`, `http_get`) — the same set the hosted server
defaults to.

**Fail closed, not fail quiet.** A manifest or definition that *names* a privileged
tool is rejected before anything runs, with an error naming the flag. Without that, the
run would still fail (`resolve_tools` rejects a tool missing from the registry) but
with a bare "unknown tool" message that reads like a typo and gives no hint that an
opt-in exists. The workflow-side check inspects `for_each`/`map` bodies too, since a
fan-out's per-item activity template is where a privileged tool would otherwise hide.

**This flag is a scope decision, not a sandbox.** Enabling it does not weaken §5.1 —
an enabled `shell` call still goes through the same confinement floor and still logs
the same loud warning on an unsandboxed native run. It is meant for a host where you
trust everything the agent may decide to do. On a shared host, prefer trust
classification (§3) so the call selects a container/gVisor backend instead.

---

# 6. Filesystem Isolation

- Fresh, isolated root per execution; only declared paths mounted
  ([Tool Framework §35](../04-agent-framework/tool-framework.md#35-filesystem-policies)).
- Workspace is per-execution scratch, wiped on teardown.
- System paths mounted read-only; no host path is ever bind-mounted into untrusted
  sandboxes.
- Output is read back through a bounded channel (`max_output_bytes`), not by the
  host reading sandbox disk.

---

# 7. Secret Management

Tools never see raw long-lived credentials. The Secret Injector:

```text
1. Resolve secret references from the tool's grants (Secret Vault)
2. Mint short-lived, scoped credentials where possible
3. Inject into the sandbox via in-memory env / tmpfs (never persistent disk)
4. Zero secrets on teardown; never write them to logs or audit
```

Aligned with
[Tool Framework §29–30](../04-agent-framework/tool-framework.md#29-secret-management).
Secrets are scoped to the single execution and revoked after.

---

# 8. Tenant Isolation

- Workers are pooled **per trust class**, and sandboxes are **never reused across
  tenants** (see [Worker Pool §3](worker-pool.md#3-worker-classes)).
- Scratch, network namespaces, and caches are tenant-scoped.
- Result caches and execution records are tenant-partitioned.
- Cross-tenant isolation is a **hard requirement**, verified by tests
  (zero leakage), per [Tool Framework §70](../04-agent-framework/tool-framework.md#70-security-requirements).

---

# 9. Supply Chain & Provenance

For third-party tools and images:

- Artifacts are content-addressed and pulled by digest.
- Signatures are verified (e.g. Sigstore-style) before a tool may run.
- Provenance/SBOM is recorded; tools failing verification are quarantined.
- A tool's manifest, permissions, and image are pinned per version so a published
  version cannot silently change.

---

# 10. Non-Root & Capability Dropping

- Tools run as an unprivileged user inside the sandbox.
- Linux capabilities are dropped to the minimum; `no-new-privileges` is set.
- seccomp/syscall filtering restricts the syscall surface (especially under
  gVisor/microVM).

---

# 11. Audit

Every execution writes a tamper-evident audit record:

```json
{
  "execution_id": "exec_01H...",
  "tenant": "acme", "principal": "agent:order-assistant",
  "tool": "http.request", "version": "1.2.0",
  "input_hash": "sha256:...", "egress": ["api.example.com"],
  "sandbox": "gvisor", "status": "succeeded",
  "secrets_used": ["ref:example-api-key"],
  "timestamp": "2026-06-27T10:00:00Z"
}
```

Inputs are hashed (not stored raw) unless policy requires retention; secrets are
referenced, never valued.

---

# 12. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Authorization decision | < 5 ms p95 |
| Secret injection | < 5 ms p95 |
| Cross-tenant leakage | 0 (hard) |
| Egress policy enforcement | 100% of executions |

---

# 13. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#26-permission-engine)
- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`13-security`](../SUMMARY.md) *(planned: platform security)*

---

# 14. Related Documents

- [`07-tool-runtime/overview.md`](overview.md)
- [`07-tool-runtime/worker-pool.md`](worker-pool.md)
- [`07-tool-runtime/observability-ops.md`](observability-ops.md)
- [`07-tool-runtime/e2b-gap-analysis.md`](e2b-gap-analysis.md) — persistent
  sessions must preserve these isolation guarantees (deny-by-default egress,
  trust-class floors), not relax them for convenience.

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-25 | SEC-404: new §5.1 scopes the "sandboxed tools" claim precisely for the native backend — a Linux deny-all egress floor via unprivileged netns, an explicit operator-acknowledgement-or-refusal path on Windows/macOS (never a silent unsandboxed run), and filesystem confinement on the native path named as a documented gap. |
| 1.1.0 | 2026-07-03 | §5 Network Isolation: closed the "L3 egress bypass" gap — `wovyr-tools`' container backend now applies a host-side `iptables` default-deny (via `nsenter` into the container's network namespace, before the real command runs) restricting `OUTPUT` to loopback + the egress proxy's address, so a workload ignoring `HTTPS_PROXY` no longer reaches anything. Linux/Docker-specific; not yet extended to Podman. Not run against a live Docker/nsenter/iptables environment in the authoring session — flagged for verification on first real use |
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Security & Isolation specification |
