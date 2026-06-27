<!--
File: docs/08-plugin-sdk/overview.md
Document ID: PLG-001
-->

# Plugin System Overview

**Document ID:** PLG-001  
**File Path:** `docs/08-plugin-sdk/overview.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **plugin system** of the Apex AI Platform — the Plugin SDK developers use to build extensions and the **Plugin Engine** service that installs, governs, and loads them.

The plugin system is what makes the platform extensible without forking or recompiling it: tools, providers, memory backends, policies, and workflow activities all arrive as plugins.

---

# 2. Scope

The plugin system is responsible for:

- An SDK + manifest format for authoring capabilities
- Packaging, signing, and publishing plugins
- Installing plugins: signature verification, dependency resolution, compatibility checks
- Registering capabilities with their host subsystems
- Lifecycle: enable, disable, upgrade, rollback, uninstall
- Permission grants and consent
- Isolated loading/execution of plugin code
- Marketplace distribution and discovery

It is **not** responsible for:

- Executing tool logic — that is the [Tool Runtime](../07-tool-runtime/index.md)
- Defining the tool model — see [Tool Framework](../04-agent-framework/tool-framework.md)
- Provider inference — see [LLM Gateway](../05-llm-gateway/index.md)

---

# 3. Plugin Engine in the Platform

```text
 Marketplace / Registry
        │  publish / pull
        ▼
   Plugin Engine ──► verify + resolve + register
        │
        ├──► Tool Runtime      (tool capabilities)
        ├──► LLM Gateway       (provider capabilities)
        ├──► Memory Engine     (memory_backend capabilities)
        ├──► Policy Engine      (policy capabilities)
        └──► Workflow Engine    (workflow_activity capabilities)
        │
        └── plugin.* events ──► Event Bus
```

The Plugin Engine is the **control plane for extensions**: it owns the catalog of
installed plugins and routes each capability to the subsystem that hosts it. See
[C4 Container §4.7](../02-architecture/c4-container.md).

---

# 4. Anatomy of a Plugin

```text
my-plugin/
├── plugin.yaml          # manifest: identity, capabilities, permissions, compat
├── capabilities/        # one or more capability implementations
│   ├── tools/
│   ├── providers/
│   └── activities/
├── artifacts/           # compiled wasm / binaries / images
├── LICENSE
├── README.md
└── SIGNATURE            # detached signature + provenance
```

The manifest is the contract; the [Plugin API](plugin-api.md) defines it in full.
Tool capabilities follow the
[Tool Framework package structure](../04-agent-framework/tool-framework.md#49-plugin-package-structure).

---

# 5. Core Responsibilities

## 5.1 Authoring (SDK)

The [Plugin SDK](plugin-api.md) provides traits, a manifest schema, codegen, and a
CLI (`apex plugin new|build|sign|publish`) so a developer can scaffold, implement,
and package a capability.

## 5.2 Installation

The Plugin Engine verifies the package signature, resolves dependencies, checks
platform-API compatibility, and stages the plugin (see [Versioning](versioning.md)).

## 5.3 Registration

Each capability is registered with its host (a `tool` into the
[Tool Registry](../04-agent-framework/tool-framework.md#12-tool-registry), a
`provider` into the [Provider SDK registry](../04-agent-framework/provider-sdk.md#8-model-registry),
etc.).

## 5.4 Governance

Plugins request permissions; operators/tenants grant them. The
[Policy Engine](../04-agent-framework/policy-engine.md) enforces grants at runtime.
See [Permissions](permissions.md).

## 5.5 Isolation

Untrusted plugin code runs sandboxed — tool plugins via the
[Tool Runtime](../07-tool-runtime/sandbox-runtime.md), others via the loading model
in [Sandbox](sandbox.md).

---

# 6. Installation Lifecycle

```text
1. Pull package (Marketplace / registry / file)
2. Verify signature + provenance
3. Parse manifest; validate schema
4. Check platform-API compatibility (semver range)
5. Resolve plugin dependencies
6. Present requested permissions for grant/consent
7. Stage artifacts (content-addressed)
8. Register capabilities (disabled)
9. Enable → capabilities become live
10. Emit plugin.installed / plugin.enabled
```

Failure at any step aborts cleanly with no partially-registered capabilities.

---

# 7. Trust Model

| Trust class | Source | Default isolation |
|-------------|--------|-------------------|
| First-party | Platform team | In-process / native |
| Verified | Reviewed + signed third-party | Sandboxed (WASM/container) |
| Community | Marketplace, unreviewed | Sandboxed (gVisor/microVM), restricted permissions |

Trust class composes with tenant policy floors, exactly as in
[Tool Runtime §3 Trust Classification](../07-tool-runtime/security-isolation.md#3-trust-classification).

---

# 8. Deployment Modes

| Mode | Description |
|------|-------------|
| Embedded | Plugin Engine in the all-in-one dev binary; local plugin dir |
| Standalone | Dedicated Plugin Engine service (enterprise default) |
| Air-gapped | Private registry mirror; no public marketplace access |

---

# 9. Module Organization

```text
service-plugin-engine/
├── api/             # install / enable / disable / upgrade / list
├── registry/        # installed-plugin catalog + capability index
├── verifier/        # signature + provenance verification
├── resolver/        # dependency + compatibility resolution
├── loader/          # capability loading + routing to hosts
├── permissions/     # grant + consent management
├── marketplace/     # registry/marketplace client
├── telemetry/       # plugin.* events, metrics, audit
└── main.rs

sdk-plugin/          # the authoring SDK (separate crate)
├── manifest/
├── traits/
├── codegen/
└── cli/
```

This corresponds to the `engine-plugin` crate in the
[DDD module map](../02-architecture/domain-driven-design.md).

---

# 10. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Install (verify + resolve + register) | < 3 s typical |
| Enable/disable | < 200 ms (no restart) |
| Capability load (cold) | host-dependent (see Tool Runtime) |
| Compatibility check | < 50 ms |
| Availability (Plugin Engine) | 99.99% |

---

# 11. Security

- All packages are signature- and provenance-verified before install.
- Plugins run with least privilege; unrequested access is impossible.
- Untrusted plugins are sandboxed and may be quarantined instantly.
- Install/enable/disable are audited and emit `plugin.*` events.

See [Permissions](permissions.md), [Sandbox](sandbox.md), and the planned
`13-security/` section.

---

# 12. Observability

The Plugin Engine emits `plugin.installed`, `plugin.enabled`, `plugin.disabled`,
`plugin.upgraded`, and `plugin.published` events to the
[Event Bus](../02-architecture/event-driven-architecture.md), plus metrics (install
success rate, active plugins, capability load latency) and audit records.

---

# 13. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#48-plugin-architecture)
- [`07-tool-runtime/index.md`](../07-tool-runtime/index.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`02-architecture/event-driven-architecture.md`](../02-architecture/event-driven-architecture.md)

---

# 14. Related Documents

- [`08-plugin-sdk/plugin-api.md`](plugin-api.md)
- [`08-plugin-sdk/permissions.md`](permissions.md)
- [`08-plugin-sdk/sandbox.md`](sandbox.md)
- [`08-plugin-sdk/versioning.md`](versioning.md)
- [`08-plugin-sdk/distribution.md`](distribution.md)
- [`08-plugin-sdk/marketplace.md`](marketplace.md)

---

# 15. Future Enhancements

- Cross-language plugins via the WASM component model
- Capability hot-reload with zero in-flight disruption
- Plugin dependency lockfiles and reproducible installs
- AI-assisted plugin scaffolding and review
- Revenue-share monetization in the marketplace

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin System Overview |
