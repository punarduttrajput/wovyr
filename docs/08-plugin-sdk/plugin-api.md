<!--
File: docs/08-plugin-sdk/plugin-api.md
Document ID: PLG-002
-->

# Plugin API & SDK

**Document ID:** PLG-002  
**File Path:** `docs/08-plugin-sdk/plugin-api.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **authoring contract** of the Plugin SDK: the manifest format, the capability traits a plugin implements, capability registration, and the developer workflow.

It is the developer-facing counterpart to the [Plugin Engine](overview.md), which consumes what the SDK produces.

---

# 2. The Manifest (`plugin.yaml`)

Every plugin declares itself in a manifest — the single source of truth the Plugin
Engine reads.

```yaml
apiVersion: plugin.wovyr.io/v1
kind: Plugin

metadata:
  name: github
  version: 1.4.0
  publisher: acme
  description: GitHub tools and workflow activities
  license: Apache-2.0
  homepage: https://example.com/github-plugin

compatibility:
  platform_api: ">=1.2.0 <2.0.0"   # semver range of platform API

dependencies:
  - name: http-core
    version: "^1.0.0"

permissions:
  - net:egress:api.github.com
  - secret:read:github-token

capabilities:
  - kind: tool
    id: github.create_issue
    entry: capabilities/tools/create_issue
    sandbox: wasm
  - kind: workflow_activity
    id: github.wait_for_pr
    entry: capabilities/activities/wait_for_pr

artifacts:
  - path: artifacts/github.wasm
    digest: sha256:...
```

| Block | Purpose |
|-------|---------|
| `metadata` | Identity, version, publisher, license |
| `compatibility` | Platform API semver range (see [Versioning](versioning.md)) |
| `dependencies` | Other plugins this one needs |
| `permissions` | Requested capability grants (see [Permissions](permissions.md)) |
| `capabilities` | What the plugin contributes |
| `artifacts` | Content-addressed binaries/wasm/images |

---

# 3. Capability Descriptor

Each capability names its `kind`, a unique `id`, an `entry` point, and
kind-specific config:

```yaml
- kind: tool
  id: github.create_issue
  entry: capabilities/tools/create_issue
  sandbox: wasm
  input_schema: schemas/create_issue.input.json
  output_schema: schemas/create_issue.output.json
```

`id`s are globally unique within a publisher namespace (`publisher/capability`).
Tool capabilities additionally conform to the
[Tool Framework manifest](../04-agent-framework/tool-framework.md#10-tool-manifest).

---

# 4. Rust SDK Traits

The SDK exposes one trait per capability kind. A plugin implements the traits for
the kinds it provides.

```rust
/// Common to every plugin.
pub trait Plugin {
    fn manifest(&self) -> &PluginManifest;
    fn on_enable(&mut self, ctx: &PluginContext) -> Result<()> { Ok(()) }
    fn on_disable(&mut self, ctx: &PluginContext) -> Result<()> { Ok(()) }
}

/// A tool capability (executed by the Tool Runtime).
#[async_trait]
pub trait ToolCapability {
    fn descriptor(&self) -> &ToolDescriptor;
    async fn execute(&self, req: ToolRequest, ctx: ToolContext)
        -> Result<ToolResponse>;
}

/// A provider capability (hosted by the LLM Gateway).
#[async_trait]
pub trait ProviderCapability {
    fn models(&self) -> Vec<ModelMetadata>;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
}

/// A workflow activity capability.
#[async_trait]
pub trait ActivityCapability {
    fn descriptor(&self) -> &ActivityDescriptor;
    async fn run(&self, input: ActivityInput, ctx: ActivityContext)
        -> Result<ActivityOutput>;
}
```

`ToolCapability` mirrors the
[Tool SDK traits](../04-agent-framework/tool-framework.md#42-core-rust-traits);
`ProviderCapability` mirrors the
[`AIProvider` trait](../04-agent-framework/provider-sdk.md#21-rust-interface). The
Plugin SDK re-exports these so a plugin and a built-in capability share one
interface.

---

# 5. Plugin Context

At enable/load time the host passes a `PluginContext` granting only what the
manifest requested:

```rust
pub struct PluginContext {
    pub plugin_id: PluginId,
    pub tenant: Option<TenantId>,
    pub config: serde_json::Value,    // validated plugin config
    pub secrets: SecretAccessor,      // scoped to granted secret refs
    pub logger: Logger,
    pub metrics: MetricsHandle,
}
```

The context is the **only** way a plugin touches platform resources — there are no
ambient globals. Anything not granted is unreachable.

---

# 6. Capability Registration

```text
Plugin enabled
   │
   ▼
SDK enumerates capabilities from the manifest
   │
   ▼
Plugin Engine validates descriptors + schemas
   │
   ▼
Each capability registered with its host:
   tool             → Tool Registry
   provider         → Provider/Model Registry
   memory_backend   → Memory Engine backend registry
   policy           → Policy Engine
   workflow_activity→ Workflow activity registry
```

Registration is transactional: either all of a plugin's capabilities register or
none do.

---

# 7. Configuration & Schema

- A plugin declares a JSON-Schema for its configuration; the Engine validates
  operator/tenant-supplied config against it before enable.
- Input/output schemas for tool/activity capabilities are validated at execution
  time by their host (see
  [Tool Runtime Execution API](../07-tool-runtime/execution-api.md)).

---

# 8. Developer Workflow & CLI

```bash
wovyr plugin new github --kind tool,workflow_activity   # scaffold
wovyr plugin build                                      # compile capabilities
wovyr plugin test                                       # run capability tests
wovyr plugin sign --key <key>                           # sign package
wovyr plugin publish --registry <url>                   # publish
wovyr plugin install ./github-1.4.0.wovyrpkg             # local install
```

The CLI produces a reproducible, signed `.wovyrpkg` package (see
[Distribution](distribution.md)).

---

# 9. Versioning the API

- Plugin API is namespaced `plugin.wovyr.io/v1`.
- The SDK is semver-versioned; plugins declare a compatible `platform_api` range.
- Trait additions are backward compatible (default methods); breaking changes bump
  the API version (`v2`) and run side by side during deprecation. See
  [Versioning](versioning.md).

---

# 10. Testing Support

The SDK ships test harnesses that:

- Run a capability in a mock host with a fake `PluginContext`
- Validate the manifest and schemas
- Assert permission usage matches declarations (no undeclared access)
- Exercise tool capabilities against the
  [Tool Runtime execution contract](../07-tool-runtime/execution-api.md)

---

# 11. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#42-core-rust-traits)
- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md#21-rust-interface)
- [`08-plugin-sdk/permissions.md`](permissions.md)
- [`08-plugin-sdk/versioning.md`](versioning.md)

---

# 12. Related Documents

- [`08-plugin-sdk/overview.md`](overview.md)
- [`08-plugin-sdk/distribution.md`](distribution.md)
- [`08-plugin-sdk/sandbox.md`](sandbox.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin API & SDK specification |
