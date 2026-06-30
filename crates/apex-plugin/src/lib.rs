//! Plugin Engine for the Apex AI Platform — the control plane for extensions
//! ([Plugin System Overview](../../docs/08-plugin-sdk/overview.md)).
//!
//! Plugins are how the platform gains tools, providers, memory backends, policies,
//! and workflow activities without forking or recompiling. This crate implements the
//! engine that installs, governs, and loads them:
//!
//! - [`manifest`] — the `plugin.yaml` contract ([Plugin API](../../docs/08-plugin-sdk/plugin-api.md)):
//!   identity, platform-API compatibility, requested permissions, contributed
//!   capabilities, and content-addressed artifacts; parsed + validated fail-closed.
//! - [`verify`] — package provenance: detached **ed25519** signature verification
//!   against a [`TrustStore`] of trusted publishers, and sha256 artifact digest
//!   checks for content-addressed staging.
//! - [`permissions`] — the least-privilege `domain:action:resource` grant model with
//!   `*`/prefix wildcards; a capability enables only when every permission it declares
//!   is granted.
//! - [`engine`] — the [`PluginEngine`]: the installed-plugin catalog and the
//!   install → enable → disable → uninstall lifecycle, routing tool capabilities into
//!   the [`ToolRegistry`](apex_tools::ToolRegistry) and emitting `plugin.*` events.
//! - [`runtime`] (behind the `wasi` feature) — [`WasiCapabilityRuntime`], the WASM
//!   capability loader: it runs a `tool` capability's staged `wasm32-wasi` module in
//!   the WASI sandbox (request JSON on stdin → response JSON on stdout). Without the
//!   feature, the engine's default [`NotLoadedRuntime`] registers capabilities but
//!   errors on call.
//!
//! Deferred to later slices (see [overview §15](../../docs/08-plugin-sdk/overview.md#15-future-enhancements)):
//! container/microVM capability loaders (only the WASM loader exists today),
//! upgrade/rollback, and marketplace distribution (incl. fetching missing dependencies
//! — dependency *resolution* against the installed catalog is implemented).

pub mod engine;
pub mod manifest;
pub mod permissions;
mod resolve;
#[cfg(feature = "wasi")]
pub mod runtime;
pub mod verify;

pub use engine::{
    CapabilityCall, CapabilityRuntime, InstalledPlugin, NotLoadedRuntime, Package, PluginEngine,
    PluginEvent, PluginState, PluginTool,
};
pub use manifest::{
    Artifact, CapabilityDescriptor, CapabilityKind, Compatibility, Dependency, Metadata,
    PLUGIN_API_VERSION, PluginManifest, Provenance, ProvenancePolicy, Sbom, SbomComponent,
};
pub use permissions::{grant_covers, is_broad, missing_grants};
#[cfg(feature = "wasi")]
pub use runtime::WasiCapabilityRuntime;
pub use verify::{TrustStore, verify_digest};
