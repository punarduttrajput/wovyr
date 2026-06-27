<!--
File: docs/08-plugin-sdk/versioning.md
Document ID: PLG-005
-->

# Plugin Versioning & Lifecycle

**Document ID:** PLG-005  
**File Path:** `docs/08-plugin-sdk/versioning.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how plugins are **versioned**, how the Plugin Engine checks **compatibility** and resolves **dependencies**, and the full **lifecycle** of an installed plugin (install → enable → upgrade → rollback → uninstall).

The goal: extensions evolve safely without breaking running tenants or the platform.

---

# 2. Semantic Versioning

Plugins use semver `MAJOR.MINOR.PATCH`:

| Bump | Meaning |
|------|---------|
| MAJOR | Breaking change to a capability's contract (input/output schema, behavior) |
| MINOR | Backward-compatible capability or feature addition |
| PATCH | Backward-compatible fix |

Capability input/output schemas are part of the contract: a breaking schema change
requires a MAJOR bump (consistent with the
[Workflow DSL versioning rules](../03-workflow-engine/workflow-dsl.md#23-workflow-versioning)).

---

# 3. Platform-API Compatibility

Each plugin declares the platform API range it supports:

```yaml
compatibility:
  platform_api: ">=1.2.0 <2.0.0"
```

The Plugin Engine refuses to install/enable a plugin whose range excludes the
running platform API version. This decouples plugin releases from platform
releases while preventing silent incompatibility. The Plugin API itself is
versioned (`plugin.apex.io/v1`, see [Plugin API §9](plugin-api.md#9-versioning-the-api)).

---

# 4. Dependency Resolution

Plugins may depend on other plugins:

```yaml
dependencies:
  - name: http-core
    version: "^1.0.0"
```

```text
Requested plugin
   │
   ▼
Build dependency graph (transitive)
   │
   ▼
Resolve versions (highest compatible per semver ranges)
   │
   ├── conflict? → report unsatisfiable constraints, abort
   ▼
Install missing deps (verified) → enable in dependency order
```

- Resolution prefers the **highest version satisfying all ranges**.
- Conflicting ranges produce a clear, actionable error rather than a silent pick.
- A future **lockfile** captures the resolved set for reproducible installs
  (see [Overview §15](overview.md#15-future-enhancements)).

---

# 5. Multiple Versions & Pinning

- A tenant pins a specific plugin version; different tenants may run different
  versions of the same plugin simultaneously.
- Running workflows/agents continue on the version they started with (consistent
  with [Workflow DSL §23](../03-workflow-engine/workflow-dsl.md#23-workflow-versioning)).
- An "active" version per tenant serves new invocations; pinned executions are
  unaffected by upgrades.

---

# 6. Channels

Publishers may release to channels so tenants choose their risk appetite:

| Channel | Use |
|---------|-----|
| `stable` | Production-ready (default) |
| `beta` | Pre-release testing |
| `edge` | Latest, may be unstable |

Tenants subscribe a plugin to a channel and control auto-upgrade behavior.

---

# 7. Lifecycle Operations

```text
INSTALL  → verify + resolve + register (disabled)
ENABLE   → capabilities go live; emit plugin.enabled
DISABLE  → capabilities removed from hosts; state retained
UPGRADE  → install new version; migrate; swap active version
ROLLBACK → re-activate prior version
UNINSTALL→ unregister + remove artifacts + revoke grants
```

| Operation | Restart needed | Notes |
|-----------|----------------|-------|
| Install | no | Stages, does not enable |
| Enable / Disable | no | Hot; routes capabilities in/out of hosts |
| Upgrade | no | Drains old version's in-flight work first |
| Rollback | no | Fast revert to last-known-good |
| Uninstall | no | Blocked if other plugins depend on it |

All operations are atomic and emit `plugin.*` events to the
[Event Bus](../02-architecture/event-driven-architecture.md).

---

# 8. Upgrade Safety

```text
1. Verify new package (signature + compat + deps)
2. Diff permissions vs. current → require grant if new perms (Permissions §9)
3. Run capability migrations (e.g. config/schema changes)
4. Stage new version alongside old
5. Drain in-flight invocations on the old version
6. Atomically switch the active version
7. Keep old version available for rollback window
```

If any step fails, the upgrade aborts and the old version remains active —
upgrades never leave a tenant in a half-migrated state.

---

# 9. Deprecation & Retirement

- Publishers mark versions `deprecated` (still runnable, warned) or `yanked`
  (blocked for new installs; existing installs warned).
- The marketplace surfaces deprecation; the Plugin Engine warns operators on
  affected tenants.
- Security-critical versions can be **force-disabled** platform-wide via a
  revocation signal (see [Distribution §8](distribution.md#8-revocation)).

---

# 10. Compatibility Testing

The SDK encourages contract tests so upgrades are safe:

- Schema compatibility checks (new version accepts old inputs where MINOR/PATCH).
- Golden-output tests for capability behavior.
- The marketplace can run automated compatibility checks before publishing.

---

# 11. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Compatibility check | < 50 ms |
| Dependency resolution (typical graph) | < 500 ms |
| Enable/disable | < 200 ms |
| Upgrade drain | bounded by in-flight timeout |

---

# 12. Dependencies

- [`08-plugin-sdk/plugin-api.md`](plugin-api.md)
- [`08-plugin-sdk/permissions.md`](permissions.md)
- [`08-plugin-sdk/distribution.md`](distribution.md)
- [`03-workflow-engine/workflow-dsl.md`](../03-workflow-engine/workflow-dsl.md#23-workflow-versioning)

---

# 13. Related Documents

- [`08-plugin-sdk/overview.md`](overview.md)
- [`08-plugin-sdk/marketplace.md`](marketplace.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin Versioning & Lifecycle specification |
