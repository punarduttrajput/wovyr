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
//! - [`runtime`] — the capability loaders (request JSON on stdin → response JSON on
//!   stdout): [`ContainerCapabilityRuntime`] (always compiled) runs a `tool`
//!   capability's staged entry in an OCI container (Docker/Podman/gVisor via
//!   apex-tools' `ContainerSandbox`), and [`WasiCapabilityRuntime`] (behind the
//!   `wasi` feature) runs `wasm32-wasi` modules in the in-process WASI sandbox.
//!   With no loader configured, the engine's default [`NotLoadedRuntime`] registers
//!   capabilities but errors on call.
//! - [`keyless`] — identity-based (Sigstore-shaped) signing ([ADR-0009]): short-lived
//!   [`IdentityCert`]s from a [`CertificateAuthority`], transparency-log-witnessed
//!   [`KeylessBundle`]s, and fully-offline verification against a pinned
//!   [`KeylessRoot`] + [`IdentityPolicy`]. The `rekor` feature adds [`rekor`], a
//!   [`TransparencyLog`] backed by a real Rekor server (`deployment/rekor/`).
//!
//! Deferred to later slices (see [overview §15](../../docs/08-plugin-sdk/overview.md#15-future-enhancements)):
//! a microVM capability loader (WASM and container loaders exist today),
//! upgrade/rollback, and marketplace distribution (incl. fetching missing dependencies
//! — dependency *resolution* against the installed catalog is implemented).

pub mod engine;
pub mod keyless;
pub mod manifest;
pub mod permissions;
#[cfg(feature = "rekor")]
pub mod rekor;
mod resolve;
pub mod runtime;
pub mod verify;

pub use engine::{
    CapabilityCall, CapabilityRuntime, InstalledPlugin, NotLoadedRuntime, Package, PluginEngine,
    PluginEvent, PluginState, PluginTool,
};
pub use keyless::{
    CertificateAuthority, IdentityCert, IdentityPolicy, IdentityRule, InMemoryCa,
    InMemoryTransparencyLog, KeylessBundle, KeylessRoot, LogEntryRef, SignerIdentity,
    TransparencyLog, sign_keyless, verify_keyless,
};
pub use manifest::{
    Artifact, CapabilityDescriptor, CapabilityKind, Compatibility, Dependency, Metadata,
    PLUGIN_API_VERSION, PluginManifest, Provenance, ProvenancePolicy, Sbom, SbomComponent,
};
pub use permissions::{grant_covers, is_broad, missing_grants};
pub use runtime::ContainerCapabilityRuntime;
#[cfg(feature = "wasi")]
pub use runtime::WasiCapabilityRuntime;
pub use verify::{TrustStore, verify_digest};
