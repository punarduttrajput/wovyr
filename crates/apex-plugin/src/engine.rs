//! The Plugin Engine — the control plane for extensions
//! ([overview §6 Installation Lifecycle](../../docs/08-plugin-sdk/overview.md#6-installation-lifecycle),
//! [versioning §7 Lifecycle Operations](../../docs/08-plugin-sdk/versioning.md#7-lifecycle-operations)).
//!
//! [`PluginEngine`] owns the catalog of installed plugins and walks each through the
//! lifecycle: **install** (verify signature → validate manifest → check platform-API
//! compatibility → confirm permission grants → stage content-addressed artifacts →
//! register capabilities, *disabled*), **enable** (route capabilities to their host
//! and go live), **disable** (withdraw from the host, retain state), and
//! **uninstall** (withdraw + drop from the catalog). Every step is fail-closed: a
//! failure aborts with no partially-registered capability.
//!
//! Capability routing is host-specific ([overview §3](../../docs/08-plugin-sdk/overview.md#3-plugin-engine-in-the-platform)).
//! Only `tool` capabilities have a live host today — the
//! [`ToolRegistry`](apex_tools::ToolRegistry) — so the engine registers a
//! [`PluginTool`] per tool capability and routes its execution to a
//! [`CapabilityRuntime`] (the sandbox loader). The default [`NotLoadedRuntime`] makes
//! a tool *visible* with correct metadata + permissions but returns an error on call,
//! since the wasm/container loader is deferred to a later slice. Other capability
//! kinds (provider/memory/policy/workflow_activity) are catalogued but not yet routed.

use crate::manifest::{
    CapabilityDescriptor, CapabilityKind, PluginManifest, ProvenancePolicy, parse_version_req,
};
use crate::permissions::missing_grants;
use crate::resolve;
use crate::verify::{TrustStore, verify_digest};
use apex_common::{Error, Result};
use apex_tools::{
    Tool, ToolContext, ToolError, ToolMetadata, ToolRegistry, ToolRequest, ToolResponse,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An installable plugin package: the signed manifest bytes, a detached ed25519
/// signature over those exact bytes, and the artifact blobs to content-verify.
///
/// The signature covers the **raw manifest YAML** so any tampering with declared
/// permissions, capabilities, or digests breaks verification.
#[derive(Clone, Default)]
pub struct Package {
    manifest_yaml: String,
    signature: Vec<u8>,
    artifacts: BTreeMap<String, Vec<u8>>,
}

impl Package {
    /// A package from raw manifest YAML and its detached signature.
    pub fn new(manifest_yaml: impl Into<String>, signature: Vec<u8>) -> Self {
        Self {
            manifest_yaml: manifest_yaml.into(),
            signature,
            artifacts: BTreeMap::new(),
        }
    }

    /// Attach an artifact blob, keyed by its manifest `path`. The bytes are
    /// digest-checked against the manifest during install.
    pub fn with_artifact(mut self, path: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.artifacts.insert(path.into(), bytes);
        self
    }

    /// Serialize to the single-file **`.apexpkg`** distribution format
    /// ([distribution §2](../../docs/08-plugin-sdk/distribution.md#2-package-format-apexpkg)):
    /// a self-contained JSON envelope bundling the manifest YAML, the detached
    /// signature, and every artifact blob (hex-encoded). Content-address the resulting
    /// bytes (sha256) for a stable package identity.
    pub fn to_apexpkg(&self) -> Result<Vec<u8>> {
        let envelope = ApexPkg {
            manifest: self.manifest_yaml.clone(),
            signature: crate::verify::hex::encode(&self.signature),
            artifacts: self
                .artifacts
                .iter()
                .map(|(p, b)| (p.clone(), crate::verify::hex::encode(b)))
                .collect(),
        };
        serde_json::to_vec_pretty(&envelope).map_err(Error::from)
    }

    /// Parse and validate the package's manifest (e.g. to read its identity before
    /// install).
    pub fn manifest(&self) -> Result<PluginManifest> {
        PluginManifest::from_yaml(&self.manifest_yaml)
    }

    /// Verify the package's detached ed25519 signature against `trust` and return the
    /// validated manifest. The signature must cover the exact manifest bytes, so any
    /// tampering with declared permissions, capabilities, or digests is rejected
    /// fail-closed. Artifact digests are **not** checked here — that happens at
    /// install/stage time ([`PluginEngine::install`]); this is the standalone
    /// supply-chain check a registry runs at publish, before any install.
    pub fn verify(&self, trust: &TrustStore) -> Result<PluginManifest> {
        let manifest = PluginManifest::from_yaml(&self.manifest_yaml)?;
        trust.verify(
            &manifest.metadata.publisher,
            self.manifest_yaml.as_bytes(),
            &self.signature,
        )?;
        Ok(manifest)
    }

    /// Reconstruct a package from `.apexpkg` bytes (the inverse of
    /// [`to_apexpkg`](Self::to_apexpkg)). The signature and artifacts are re-verified
    /// at install, so a tampered envelope is rejected there.
    pub fn from_apexpkg(bytes: &[u8]) -> Result<Self> {
        let envelope: ApexPkg = serde_json::from_slice(bytes)
            .map_err(|e| Error::invalid(format!("invalid .apexpkg: {e}")))?;
        let mut package = Package::new(
            envelope.manifest,
            crate::verify::hex::decode(&envelope.signature)?,
        );
        for (path, hex) in envelope.artifacts {
            package = package.with_artifact(path, crate::verify::hex::decode(&hex)?);
        }
        Ok(package)
    }
}

/// The on-disk `.apexpkg` envelope: manifest YAML + hex signature + hex artifact blobs.
#[derive(Serialize, Deserialize)]
struct ApexPkg {
    manifest: String,
    signature: String,
    #[serde(default)]
    artifacts: BTreeMap<String, String>,
}

/// Whether an installed plugin's capabilities are live in their hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    /// Installed and registered, but capabilities are withdrawn (not live).
    Disabled,
    /// Capabilities are registered with their hosts and serving invocations.
    Enabled,
}

/// A catalogued plugin and its current grant/lifecycle state. Serializable so a
/// durable catalog (e.g. the CLI's `~/.apex/plugins/catalog.json`) survives across
/// processes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPlugin {
    /// The validated manifest.
    pub manifest: PluginManifest,
    /// Current lifecycle state.
    pub state: PluginState,
    /// The permissions the operator granted at install (a superset of the
    /// manifest's requested permissions).
    pub granted_permissions: Vec<String>,
    /// Where this plugin's artifacts were staged on disk, if the engine has a
    /// staging directory configured. `None` means artifacts were verified but not
    /// persisted (so a disk-backed runtime can't load them).
    pub artifact_dir: Option<PathBuf>,
    /// The version this one replaced, retained for [`rollback`](PluginEngine::rollback)
    /// (single-level rollback window). `None` for a fresh install or after a rollback.
    #[serde(default)]
    pub previous: Option<Box<InstalledPlugin>>,
}

/// A lifecycle event emitted by the engine, mirroring the platform `plugin.*` events
/// ([overview §12](../../docs/08-plugin-sdk/overview.md#12-observability)). Carries the
/// fully-pinned `publisher/name@version` reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginEvent {
    /// `plugin.installed` — staged and registered (disabled).
    Installed(String),
    /// `plugin.enabled` — capabilities went live.
    Enabled(String),
    /// `plugin.disabled` — capabilities withdrawn, state retained.
    Disabled(String),
    /// `plugin.upgraded` — active version swapped (carries the new reference).
    Upgraded(String),
    /// `plugin.rolled_back` — reverted to the prior version (carries its reference).
    RolledBack(String),
    /// `plugin.uninstalled` — withdrawn and dropped from the catalog.
    Uninstalled(String),
}

/// Identifies a capability invocation for the [`CapabilityRuntime`].
pub struct CapabilityCall<'a> {
    /// The fully-pinned plugin reference (`publisher/name@version`).
    pub plugin: &'a str,
    /// The capability id being invoked.
    pub capability_id: &'a str,
    /// The capability's entry point within the package — for a `wasm` capability,
    /// the relative path (under [`artifact_dir`](Self::artifact_dir)) of the module
    /// to execute.
    pub entry: &'a str,
    /// The capability's requested sandbox backend (e.g. `wasm`, `container`).
    pub sandbox: &'a str,
    /// The directory the plugin's artifacts were staged into, if any. A disk-backed
    /// runtime resolves the artifact to run as `artifact_dir.join(entry)`.
    pub artifact_dir: Option<&'a Path>,
    /// The plugin's declared permissions (incl. any `secret:read:<name>`), so the
    /// runtime can resolve + inject the secrets the capability is entitled to.
    pub declared_permissions: &'a [String],
    /// The tool execution context (correlation ids, workdir, grants).
    pub ctx: &'a ToolContext,
}

/// Loads and executes a plugin capability's artifact in isolation — the bridge to the
/// [Tool Runtime](../../docs/07-tool-runtime/index.md) sandbox loader. The engine
/// constructs the [`PluginTool`] wrapper; the runtime performs the actual sandboxed
/// invocation. Pluggable so a future wasm/container loader drops in without touching
/// the control plane.
#[async_trait]
pub trait CapabilityRuntime: Send + Sync {
    /// Invoke a capability with `request`, returning its tool response.
    async fn invoke(
        &self,
        call: &CapabilityCall<'_>,
        request: ToolRequest,
    ) -> std::result::Result<ToolResponse, ToolError>;
}

/// The default runtime: capabilities register and advertise correctly but cannot yet
/// execute, since the sandbox loader is deferred. Calls fail closed with a clear
/// message rather than silently no-op'ing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotLoadedRuntime;

#[async_trait]
impl CapabilityRuntime for NotLoadedRuntime {
    async fn invoke(
        &self,
        call: &CapabilityCall<'_>,
        _request: ToolRequest,
    ) -> std::result::Result<ToolResponse, ToolError> {
        Err(ToolError::Internal(format!(
            "capability `{}` of plugin `{}` cannot execute: the plugin sandbox loader is not yet available",
            call.capability_id, call.plugin
        )))
    }
}

/// A [`Tool`] backing a plugin's `tool` capability. Advertises the capability's
/// metadata and the plugin's declared permissions to the registry, and routes
/// execution to the engine's [`CapabilityRuntime`].
pub struct PluginTool {
    plugin_ref: String,
    metadata: ToolMetadata,
    entry: String,
    sandbox: String,
    artifact_dir: Option<PathBuf>,
    runtime: Arc<dyn CapabilityRuntime>,
}

#[async_trait]
impl Tool for PluginTool {
    fn metadata(&self) -> ToolMetadata {
        self.metadata.clone()
    }

    fn input_schema(&self) -> Value {
        // The capability declares its own schema; until the loader can read it from
        // the artifact, advertise a permissive object schema.
        json!({ "type": "object" })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        request: ToolRequest,
    ) -> std::result::Result<ToolResponse, ToolError> {
        let call = CapabilityCall {
            plugin: &self.plugin_ref,
            capability_id: &self.metadata.id,
            entry: &self.entry,
            sandbox: &self.sandbox,
            artifact_dir: self.artifact_dir.as_deref(),
            declared_permissions: &self.metadata.permissions,
            ctx,
        };
        self.runtime.invoke(&call, request).await
    }
}

/// The Plugin Engine: catalog + lifecycle control plane.
pub struct PluginEngine {
    trust: TrustStore,
    platform_api: semver::Version,
    runtime: Arc<dyn CapabilityRuntime>,
    staging_dir: Option<PathBuf>,
    provenance_policy: ProvenancePolicy,
    plugins: BTreeMap<String, InstalledPlugin>,
    events: Vec<PluginEvent>,
}

impl PluginEngine {
    /// A new engine running `platform_api`, trusting the publishers in `trust`.
    pub fn new(platform_api: semver::Version, trust: TrustStore) -> Self {
        Self {
            trust,
            platform_api,
            runtime: Arc::new(NotLoadedRuntime),
            staging_dir: None,
            provenance_policy: ProvenancePolicy::default(),
            plugins: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    /// Enforce a supply-chain `policy` (provenance/SBOM) at install time
    /// ([distribution §4](../../docs/08-plugin-sdk/distribution.md#4-provenance--sbom)).
    pub fn with_provenance_policy(mut self, policy: ProvenancePolicy) -> Self {
        self.provenance_policy = policy;
        self
    }

    /// Use `runtime` to execute plugin capabilities (replaces [`NotLoadedRuntime`]).
    pub fn with_runtime(mut self, runtime: Arc<dyn CapabilityRuntime>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Persist verified artifacts under `dir` at install (content-addressed staging,
    /// [overview §6 step 7](../../docs/08-plugin-sdk/overview.md#6-installation-lifecycle)),
    /// so a disk-backed [`CapabilityRuntime`] (e.g. the WASM loader) can load them.
    /// Without it, artifacts are digest-verified but not written to disk.
    pub fn with_staging_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.staging_dir = Some(dir.into());
        self
    }

    /// Install a package, leaving its capabilities **registered but disabled**
    /// ([overview §6](../../docs/08-plugin-sdk/overview.md#6-installation-lifecycle)).
    /// `grants` is the set of permissions the operator consents to; the manifest's
    /// requested permissions must be a subset (fail-closed). Returns the catalog entry.
    ///
    /// Aborts (with nothing registered) on: invalid manifest, untrusted publisher or
    /// bad signature, platform-API incompatibility, ungranted permissions, a declared
    /// artifact whose bytes are missing or whose digest mismatches, or a plugin id
    /// already installed.
    pub fn install(&mut self, package: &Package, grants: &[String]) -> Result<&InstalledPlugin> {
        // 3. Parse + validate the manifest.
        let manifest = PluginManifest::from_yaml(&package.manifest_yaml)?;
        let id = manifest.qualified_id();

        if self.plugins.contains_key(&id) {
            return Err(Error::invalid(format!(
                "plugin `{id}` is already installed (upgrade is a separate operation)"
            )));
        }

        // 2,4,5,6,7. Verify (signature/compat/deps/permissions) + stage artifacts.
        let artifact_dir = self.verify_and_stage(package, &manifest, grants)?;

        // 8. Register (disabled).
        let reference = manifest.reference();
        self.plugins.insert(
            id.clone(),
            InstalledPlugin {
                manifest,
                state: PluginState::Disabled,
                granted_permissions: grants.to_vec(),
                artifact_dir,
                previous: None,
            },
        );
        self.events.push(PluginEvent::Installed(reference));
        Ok(&self.plugins[&id])
    }

    /// Upgrade an installed plugin to the version in `package`
    /// ([versioning §7/§8](../../docs/08-plugin-sdk/versioning.md#8-upgrade-safety)).
    /// Verifies the new package (signature/compat/deps), requires `grants` (unioned
    /// with the grants already on file) to cover the new version's permissions, stages
    /// the new artifacts alongside the old, atomically swaps the active version —
    /// re-registering tool capabilities if the plugin was enabled — and retains the
    /// prior version for [`rollback`](Self::rollback). Fail-closed: on any failure the
    /// old version stays active. Refuses an upgrade that would break an installed
    /// dependent's version requirement.
    pub fn upgrade(
        &mut self,
        package: &Package,
        grants: &[String],
        registry: &mut ToolRegistry,
    ) -> Result<()> {
        let manifest = PluginManifest::from_yaml(&package.manifest_yaml)?;
        let id = manifest.qualified_id();

        let current = self.plugins.get(&id).ok_or_else(|| {
            Error::NotFound(format!("plugin `{id}` is not installed (use install)"))
        })?;
        let from_version = current.manifest.metadata.version.clone();
        if manifest.metadata.version == from_version {
            return Err(Error::invalid(format!(
                "plugin `{id}` is already at version {from_version}"
            )));
        }

        // Reverse-dependency safety: the new version must still satisfy every installed
        // dependent's requirement, so an upgrade never breaks a dependent.
        let new_version = semver::Version::parse(&manifest.metadata.version)
            .map_err(|e| Error::invalid(format!("new version is not valid semver: {e}")))?;
        let broken = resolve::dependents_broken_by(&id, &new_version, &self.plugins);
        if !broken.is_empty() {
            return Err(Error::invalid(format!(
                "cannot upgrade `{id}` to {new_version}: would break dependent(s): {}",
                broken.join(", ")
            )));
        }

        // Carry forward previously-granted permissions so only *new* permissions need a
        // fresh grant ([versioning §8 step 2]).
        let mut effective = current.granted_permissions.clone();
        for g in grants {
            if !effective.contains(g) {
                effective.push(g.clone());
            }
        }

        // Verify + stage the new version (fail-closed before any swap).
        let artifact_dir = self.verify_and_stage(package, &manifest, &effective)?;

        // Atomically swap: preserve liveness, retain the prior version (single-level
        // rollback window), and re-route tools if enabled.
        let mut prior = self.plugins.remove(&id).expect("checked present above");
        let state = prior.state;
        if state == PluginState::Enabled {
            for cap in prior.manifest.tool_capabilities() {
                registry.unregister(&cap.id);
            }
        }
        prior.previous = None; // bound the rollback chain to one level
        let reference = manifest.reference();
        let upgraded = InstalledPlugin {
            manifest,
            state,
            granted_permissions: effective,
            artifact_dir,
            previous: Some(Box::new(prior)),
        };
        if state == PluginState::Enabled {
            register_tools(&upgraded, &self.runtime, registry);
        }
        self.plugins.insert(id, upgraded);
        self.events.push(PluginEvent::Upgraded(reference));
        Ok(())
    }

    /// Roll a plugin back to its retained previous version
    /// ([versioning §7](../../docs/08-plugin-sdk/versioning.md#7-lifecycle-operations)).
    /// Re-activates the prior version (preserving the current liveness), re-routing tool
    /// capabilities if enabled. Errors if no previous version is retained.
    pub fn rollback(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        let current = self
            .plugins
            .get(qualified_id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{qualified_id}` is not installed")))?;
        if current.previous.is_none() {
            return Err(Error::invalid(format!(
                "plugin `{qualified_id}` has no previous version to roll back to"
            )));
        }

        let mut current = self.plugins.remove(qualified_id).expect("checked present");
        let state = current.state;
        if state == PluginState::Enabled {
            for cap in current.manifest.tool_capabilities() {
                registry.unregister(&cap.id);
            }
        }
        let mut restored = *current.previous.take().expect("checked Some above");
        restored.state = state; // preserve liveness across the revert
        if state == PluginState::Enabled {
            register_tools(&restored, &self.runtime, registry);
        }
        let reference = restored.manifest.reference();
        self.plugins.insert(qualified_id.to_string(), restored);
        self.events.push(PluginEvent::RolledBack(reference));
        Ok(())
    }

    /// Enable plugin `qualified_id`: route its capabilities to their hosts and go live.
    /// Its transitive **dependencies are enabled first** (in dependency order); tool
    /// capabilities are registered into `registry`, other kinds are catalogued (no live
    /// host yet). Idempotent for already-enabled plugins, and fail-closed on a missing
    /// dependency or a dependency cycle.
    pub fn enable(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        if !self.plugins.contains_key(qualified_id) {
            return Err(Error::NotFound(format!(
                "plugin `{qualified_id}` is not installed"
            )));
        }
        // Dependencies first, then the plugin itself.
        let order = resolve::enable_order(qualified_id, &self.plugins)?;
        let runtime = Arc::clone(&self.runtime);
        for id in order {
            let plugin = self
                .plugins
                .get_mut(&id)
                .expect("resolved id is in the catalog");
            if plugin.state == PluginState::Enabled {
                continue;
            }
            register_tools(plugin, &runtime, registry);
            plugin.state = PluginState::Enabled;
            let reference = plugin.manifest.reference();
            self.events.push(PluginEvent::Enabled(reference));
        }
        Ok(())
    }

    /// Register the tool capabilities of every **already-enabled** plugin into
    /// `registry`, without changing state or emitting events. Used to rehydrate a
    /// process (e.g. an agent run) from a restored catalog so enabled plugin tools are
    /// callable. Plugins in [`PluginState::Disabled`] are skipped.
    pub fn register_enabled(&self, registry: &mut ToolRegistry) {
        for plugin in self.plugins.values() {
            if plugin.state == PluginState::Enabled {
                register_tools(plugin, &self.runtime, registry);
            }
        }
    }

    /// Disable plugin `qualified_id`: withdraw its capabilities from their hosts,
    /// retaining the catalog entry + grants. Idempotent for an already-disabled plugin,
    /// and **fail-closed if an enabled plugin still depends on it** (disable the
    /// dependents first).
    pub fn disable(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        let state = self
            .plugins
            .get(qualified_id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{qualified_id}` is not installed")))?
            .state;
        if state == PluginState::Disabled {
            return Ok(());
        }
        let enabled_dependents: Vec<String> = resolve::dependents(qualified_id, &self.plugins)
            .into_iter()
            .filter(|d| self.plugins.get(d).map(|p| p.state) == Some(PluginState::Enabled))
            .collect();
        if !enabled_dependents.is_empty() {
            return Err(Error::invalid(format!(
                "cannot disable `{qualified_id}`: still required by enabled plugin(s): {}",
                enabled_dependents.join(", ")
            )));
        }

        let plugin = self
            .plugins
            .get_mut(qualified_id)
            .expect("checked present above");
        for cap in plugin.manifest.tool_capabilities() {
            registry.unregister(&cap.id);
        }
        plugin.state = PluginState::Disabled;
        let reference = plugin.manifest.reference();
        self.events.push(PluginEvent::Disabled(reference));
        Ok(())
    }

    /// Uninstall plugin `qualified_id`: withdraw its capabilities and drop it from the
    /// catalog. **Fail-closed if any installed plugin still depends on it** (uninstall
    /// the dependents first).
    pub fn uninstall(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        if !self.plugins.contains_key(qualified_id) {
            return Err(Error::NotFound(format!(
                "plugin `{qualified_id}` is not installed"
            )));
        }
        let dependents = resolve::dependents(qualified_id, &self.plugins);
        if !dependents.is_empty() {
            return Err(Error::invalid(format!(
                "cannot uninstall `{qualified_id}`: still required by installed plugin(s): {}",
                dependents.join(", ")
            )));
        }
        // No dependents remain, so disabling can't be blocked.
        self.disable(qualified_id, registry)?;
        let plugin = self
            .plugins
            .remove(qualified_id)
            .expect("checked present above");
        self.events
            .push(PluginEvent::Uninstalled(plugin.manifest.reference()));
        Ok(())
    }

    /// Look up an installed plugin by `publisher/name`.
    pub fn get(&self, qualified_id: &str) -> Option<&InstalledPlugin> {
        self.plugins.get(qualified_id)
    }

    /// All installed plugins' qualified ids, sorted.
    pub fn installed(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    /// A snapshot of the catalog, sorted by qualified id — for persisting a durable
    /// installed-plugin list (the inverse of [`with_catalog`](Self::with_catalog)).
    pub fn catalog(&self) -> Vec<InstalledPlugin> {
        self.plugins.values().cloned().collect()
    }

    /// Restore a previously-persisted catalog into the engine (replacing any current
    /// entries), keyed by each plugin's qualified id. Lets the CLI/server rebuild an
    /// engine from `~/.apex/plugins/catalog.json` without re-running install.
    pub fn with_catalog(mut self, plugins: impl IntoIterator<Item = InstalledPlugin>) -> Self {
        self.plugins = plugins
            .into_iter()
            .map(|p| (p.manifest.qualified_id(), p))
            .collect();
        self
    }

    /// The lifecycle events emitted so far, in order.
    pub fn events(&self) -> &[PluginEvent] {
        &self.events
    }

    /// Shared install/upgrade verification (overview §6 steps 2,4–7): verify the
    /// signature, platform-API compatibility, dependency satisfaction, and permission
    /// grants, then stage (digest-verified) artifacts to disk. Returns the staged
    /// artifact directory, if a staging dir is configured. Fail-closed and side-effect
    /// free except for writing verified artifact bytes.
    fn verify_and_stage(
        &self,
        package: &Package,
        manifest: &PluginManifest,
        grants: &[String],
    ) -> Result<Option<PathBuf>> {
        let id = manifest.qualified_id();

        // Verify signature over the raw manifest bytes (untrusted publisher / tamper).
        self.trust.verify(
            &manifest.metadata.publisher,
            package.manifest_yaml.as_bytes(),
            &package.signature,
        )?;

        // Platform-API compatibility.
        self.check_compatibility(manifest)?;

        // Dependencies must be installed + version-compatible.
        for dep in &manifest.dependencies {
            resolve::resolve_dep(dep, &self.plugins)?;
        }

        // Every requested permission must be granted.
        let missing = missing_grants(&manifest.permissions, grants);
        if !missing.is_empty() {
            return Err(Error::invalid(format!(
                "plugin `{id}` requests ungranted permission(s): {}",
                missing.join(", ")
            )));
        }

        // Supply-chain policy: required provenance/SBOM and trusted builders ([dist §4]).
        self.provenance_policy.check(manifest)?;

        // Stage artifacts (content-addressed): each declared artifact must be present
        // and match its digest, then is written under the version-specific staging dir.
        let artifact_dir = self.staging_dir.as_ref().map(|root| {
            root.join(&manifest.metadata.publisher)
                .join(&manifest.metadata.name)
                .join(&manifest.metadata.version)
        });
        for artifact in &manifest.artifacts {
            let bytes = package.artifacts.get(&artifact.path).ok_or_else(|| {
                Error::invalid(format!(
                    "plugin `{id}` declares artifact `{}` but the package omits its bytes",
                    artifact.path
                ))
            })?;
            verify_digest(&artifact.digest, bytes)?;
            if let Some(dir) = &artifact_dir {
                let dest = dir.join(&artifact.path);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest, bytes)?;
            }
        }
        Ok(artifact_dir)
    }

    /// Refuse a plugin whose declared `platform_api` range excludes the running
    /// platform version. An empty range imposes no constraint.
    fn check_compatibility(&self, manifest: &PluginManifest) -> Result<()> {
        let range = manifest.compatibility.platform_api.trim();
        if range.is_empty() {
            return Ok(());
        }
        let req = parse_version_req(range)?;
        if req.matches(&self.platform_api) {
            Ok(())
        } else {
            Err(Error::invalid(format!(
                "plugin `{}` requires platform API `{range}`, but this platform is `{}`",
                manifest.qualified_id(),
                self.platform_api
            )))
        }
    }
}

/// Register every tool capability of `plugin` as a [`PluginTool`] into `registry`,
/// routing execution to `runtime`. Shared by [`PluginEngine::enable`] and
/// [`PluginEngine::register_enabled`].
fn register_tools(
    plugin: &InstalledPlugin,
    runtime: &Arc<dyn CapabilityRuntime>,
    registry: &mut ToolRegistry,
) {
    let plugin_ref = plugin.manifest.reference();
    for cap in plugin.manifest.tool_capabilities() {
        let tool = PluginTool {
            plugin_ref: plugin_ref.clone(),
            metadata: tool_metadata(&plugin.manifest, cap),
            entry: cap.entry.clone(),
            sandbox: cap.sandbox.clone(),
            artifact_dir: plugin.artifact_dir.clone(),
            runtime: Arc::clone(runtime),
        };
        registry.register(Arc::new(tool));
    }
}

/// Build the [`ToolMetadata`] a tool capability advertises: the capability id, the
/// plugin's version, and the plugin's declared permissions (enforced at call time by
/// the registry against the agent's grants).
fn tool_metadata(manifest: &PluginManifest, cap: &CapabilityDescriptor) -> ToolMetadata {
    debug_assert_eq!(cap.kind, CapabilityKind::Tool);
    let description = if manifest.metadata.description.is_empty() {
        format!("plugin capability `{}`", cap.id)
    } else {
        manifest.metadata.description.clone()
    };
    ToolMetadata::new(
        cap.id.clone(),
        manifest.metadata.version.clone(),
        "plugin",
        description,
    )
    .with_permissions(manifest.permissions.clone())
}

/// Resolve the secrets a capability is entitled to into environment variables for
/// sandbox injection ([secret-management §5](../../docs/13-security/secret-management.md#5-injection-into-tools--plugins)).
/// For each declared `secret:read:<name>` permission, the secret is resolved from `vault`
/// within `tenant` (fail-closed: the vault rejects a missing grant, a cross-tenant
/// reference, or an absent secret) and exposed to the guest as `APEX_SECRET_<NAME>`.
///
/// An empty `tenant` (no tenant context — e.g. the operator `plugin run` path) injects
/// nothing; a wildcard grant (`secret:read:*`) is skipped, since no concrete secret name
/// can be enumerated from it.
///
/// Compiled only where used — the WASM runtime (`wasi` feature) and tests.
#[cfg(any(test, feature = "wasi"))]
pub(crate) fn resolve_secret_env(
    declared_permissions: &[String],
    tenant: &str,
    vault: &apex_secrets::Vault,
) -> std::result::Result<Vec<(String, String)>, ToolError> {
    if tenant.is_empty() {
        return Ok(Vec::new());
    }
    let access = apex_secrets::SecretAccess::new(tenant, declared_permissions.to_vec());
    let mut env = Vec::new();
    for perm in declared_permissions {
        let Some(name) = perm.strip_prefix("secret:read:") else {
            continue;
        };
        if name.contains('*') {
            continue;
        }
        let reference = apex_secrets::SecretRef::new(tenant, name)
            .map_err(|e| ToolError::Internal(format!("secret reference: {e}")))?;
        let value = vault
            .resolve(&reference, &access)
            .map_err(|e| ToolError::Internal(format!("resolve secret `{name}`: {e}")))?;
        env.push((secret_env_var(name), value.expose().to_string()));
    }
    Ok(env)
}

/// Map a secret name to the env var it is injected as: `APEX_SECRET_<UPPER_SNAKE>`
/// (non-alphanumeric characters become `_`).
#[cfg(any(test, feature = "wasi"))]
fn secret_env_var(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("APEX_SECRET_{upper}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::testing::{generate_keypair, sign};

    const MANIFEST: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata:
  name: github
  version: 1.4.0
  publisher: acme
  description: GitHub tools
compatibility:
  platform_api: ">=0.1.0 <2.0.0"
permissions:
  - net:egress:api.github.com
capabilities:
  - kind: tool
    id: github.create_issue
    entry: capabilities/tools/create_issue
    sandbox: wasm
artifacts:
  - path: artifacts/github.wasm
    digest: sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
"#;

    /// A signed package whose single artifact's bytes hash to the declared digest
    /// (`sha256("hello world")`), plus the trust store that verifies it.
    fn signed_package() -> (Package, PluginEngine) {
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        let sig = sign(&kp, MANIFEST.as_bytes());
        let package = Package::new(MANIFEST, sig)
            .with_artifact("artifacts/github.wasm", b"hello world".to_vec());
        let engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust);
        (package, engine)
    }

    fn grants() -> Vec<String> {
        vec!["net:egress:*".to_string()]
    }

    #[test]
    fn install_enable_disable_uninstall_round_trip() {
        let (package, mut engine) = signed_package();
        let mut registry = ToolRegistry::new();

        let installed = engine.install(&package, &grants()).unwrap();
        assert_eq!(installed.state, PluginState::Disabled);
        assert_eq!(engine.installed(), vec!["acme/github".to_string()]);
        // Disabled → capability not yet in the registry.
        assert!(!registry.contains("github.create_issue"));

        engine.enable("acme/github", &mut registry).unwrap();
        assert_eq!(
            engine.get("acme/github").unwrap().state,
            PluginState::Enabled
        );
        assert!(registry.contains("github.create_issue"));
        // Tool advertises the plugin's declared permissions.
        let meta = registry.get("github.create_issue").unwrap().metadata();
        assert_eq!(meta.version, "1.4.0");
        assert_eq!(
            meta.permissions,
            vec!["net:egress:api.github.com".to_string()]
        );

        engine.disable("acme/github", &mut registry).unwrap();
        assert!(!registry.contains("github.create_issue"));
        assert_eq!(
            engine.get("acme/github").unwrap().state,
            PluginState::Disabled
        );

        engine.uninstall("acme/github", &mut registry).unwrap();
        assert!(engine.get("acme/github").is_none());

        assert_eq!(
            engine.events(),
            &[
                PluginEvent::Installed("acme/github@1.4.0".into()),
                PluginEvent::Enabled("acme/github@1.4.0".into()),
                PluginEvent::Disabled("acme/github@1.4.0".into()),
                PluginEvent::Uninstalled("acme/github@1.4.0".into()),
            ]
        );
    }

    #[test]
    fn catalog_snapshot_restores_and_register_enabled_rehydrates() {
        // Install + enable, snapshot the catalog, then round-trip it through JSON and
        // rebuild a fresh engine — register_enabled must re-register the tool.
        let (package, mut engine) = signed_package();
        let mut registry = ToolRegistry::new();
        engine.install(&package, &grants()).unwrap();
        engine.enable("acme/github", &mut registry).unwrap();

        let json = serde_json::to_string(&engine.catalog()).unwrap();
        let restored: Vec<InstalledPlugin> = serde_json::from_str(&json).unwrap();

        let rebuilt = PluginEngine::new(semver::Version::new(1, 0, 0), TrustStore::new())
            .with_catalog(restored);
        assert_eq!(rebuilt.installed(), vec!["acme/github".to_string()]);
        assert_eq!(
            rebuilt.get("acme/github").unwrap().state,
            PluginState::Enabled
        );

        let mut fresh = ToolRegistry::new();
        rebuilt.register_enabled(&mut fresh);
        assert!(fresh.contains("github.create_issue"));
    }

    #[test]
    fn apexpkg_round_trips_and_installs() {
        // Pack a signed package to .apexpkg bytes, reconstruct it, and install — the
        // reconstructed package must verify (signature + artifact digest) intact.
        let (package, mut engine) = signed_package();
        let bytes = package.to_apexpkg().unwrap();
        let restored = Package::from_apexpkg(&bytes).unwrap();
        engine.install(&restored, &grants()).unwrap();
        assert_eq!(engine.installed(), vec!["acme/github".to_string()]);
    }

    #[test]
    fn package_verify_checks_signature_without_installing() {
        let (package, engine) = signed_package();
        // A package signed by a trusted publisher verifies and yields the manifest.
        let manifest = package.verify(&engine.trust).unwrap();
        assert_eq!(manifest.qualified_id(), "acme/github");

        // Tampering with the manifest after signing breaks verification.
        let mut tampered = package.clone();
        tampered.manifest_yaml = tampered
            .manifest_yaml
            .replace("net:egress:api.github.com", "net:egress:*");
        assert!(tampered.verify(&engine.trust).is_err());

        // An untrusted publisher is rejected.
        assert!(package.verify(&TrustStore::new()).is_err());
    }

    #[test]
    fn register_enabled_skips_disabled_plugins() {
        let (package, mut engine) = signed_package();
        engine.install(&package, &grants()).unwrap(); // installed → Disabled
        let mut registry = ToolRegistry::new();
        engine.register_enabled(&mut registry);
        assert!(!registry.contains("github.create_issue"));
    }

    #[test]
    fn rejects_untrusted_publisher() {
        let (kp, _public) = generate_keypair();
        let sig = sign(&kp, MANIFEST.as_bytes());
        let package = Package::new(MANIFEST, sig)
            .with_artifact("artifacts/github.wasm", b"hello world".to_vec());
        // Trust store trusts nobody.
        let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), TrustStore::new());
        assert!(engine.install(&package, &grants()).is_err());
        assert!(engine.installed().is_empty());
    }

    #[test]
    fn rejects_tampered_manifest() {
        let (mut package, mut engine) = signed_package();
        // Mutating the manifest after signing invalidates the signature.
        package.manifest_yaml = package
            .manifest_yaml
            .replace("net:egress:api.github.com", "net:egress:*");
        assert!(engine.install(&package, &grants()).is_err());
    }

    #[test]
    fn rejects_ungranted_permissions() {
        let (package, mut engine) = signed_package();
        // No grants → the requested net:egress permission is ungranted.
        let err = engine.install(&package, &[]).unwrap_err();
        assert!(format!("{err}").contains("ungranted permission"));
        assert!(engine.installed().is_empty());
    }

    #[test]
    fn rejects_incompatible_platform_api() {
        let (package, _trusted) = signed_package();
        // Re-sign for a v3 platform that the plugin's `<2.0.0` range excludes.
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        let sig = sign(&kp, MANIFEST.as_bytes());
        let pkg = Package::new(MANIFEST, sig)
            .with_artifact("artifacts/github.wasm", b"hello world".to_vec());
        let mut engine = PluginEngine::new(semver::Version::new(3, 0, 0), trust);
        let err = engine.install(&pkg, &grants()).unwrap_err();
        assert!(format!("{err}").contains("platform API"));
        let _ = package; // silence unused in this variant
    }

    #[test]
    fn rejects_missing_or_mismatched_artifact() {
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        let sig = sign(&kp, MANIFEST.as_bytes());

        // Declared artifact bytes omitted.
        let no_bytes = Package::new(MANIFEST, sig.clone());
        let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust.clone());
        assert!(engine.install(&no_bytes, &grants()).is_err());

        // Wrong bytes → digest mismatch.
        let bad = Package::new(MANIFEST, sig)
            .with_artifact("artifacts/github.wasm", b"tampered".to_vec());
        let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust);
        assert!(engine.install(&bad, &grants()).is_err());
    }

    #[test]
    fn rejects_duplicate_install() {
        let (package, mut engine) = signed_package();
        engine.install(&package, &grants()).unwrap();
        assert!(engine.install(&package, &grants()).is_err());
    }

    #[tokio::test]
    async fn enabled_tool_executes_via_runtime() {
        // A runtime that echoes back which capability was invoked.
        struct EchoRuntime;
        #[async_trait]
        impl CapabilityRuntime for EchoRuntime {
            async fn invoke(
                &self,
                call: &CapabilityCall<'_>,
                request: ToolRequest,
            ) -> std::result::Result<ToolResponse, ToolError> {
                Ok(ToolResponse::success(json!({
                    "capability": call.capability_id,
                    "sandbox": call.sandbox,
                    "echo": request.parameters,
                })))
            }
        }

        let (package, engine) = signed_package();
        let mut engine = engine.with_runtime(Arc::new(EchoRuntime));
        let mut registry = ToolRegistry::new();
        engine.install(&package, &grants()).unwrap();
        engine.enable("acme/github", &mut registry).unwrap();

        // Granting the tool's required permission lets it run through the registry.
        let ctx = ToolContext {
            granted_permissions: Some(vec!["net:egress:api.github.com".to_string()]),
            ..ToolContext::default()
        };
        let resp = registry
            .execute(
                "github.create_issue",
                &ctx,
                ToolRequest::new(json!({"x": 1})),
            )
            .await
            .unwrap();
        assert!(resp.success);
        assert_eq!(resp.payload["capability"], "github.create_issue");
        assert_eq!(resp.payload["sandbox"], "wasm");
    }

    #[tokio::test]
    async fn default_runtime_is_not_loaded() {
        let (package, mut engine) = signed_package();
        let mut registry = ToolRegistry::new();
        engine.install(&package, &grants()).unwrap();
        engine.enable("acme/github", &mut registry).unwrap();
        let ctx = ToolContext {
            granted_permissions: Some(vec!["net:egress:api.github.com".to_string()]),
            ..ToolContext::default()
        };
        let err = registry
            .execute("github.create_issue", &ctx, ToolRequest::new(json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Internal(m) if m.contains("sandbox loader")));
    }

    // --- Dependency resolution ---------------------------------------------------

    const BASE: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: http-core, version: 1.2.0, publisher: acme }
capabilities:
  - { kind: tool, id: http.get }
"#;

    const DEPENDENT: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: app, version: 0.1.0, publisher: acme }
dependencies:
  - { name: http-core, version: "^1.0.0" }
capabilities:
  - { kind: tool, id: app.run }
"#;

    /// An engine trusting `acme`, plus a signer that produces packages for it.
    fn dep_engine() -> (PluginEngine, ring::signature::Ed25519KeyPair) {
        let (kp, public) = generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);
        (PluginEngine::new(semver::Version::new(1, 0, 0), trust), kp)
    }

    fn pkg(kp: &ring::signature::Ed25519KeyPair, manifest: &str) -> Package {
        Package::new(manifest, sign(kp, manifest.as_bytes()))
    }

    #[test]
    fn install_requires_dependencies_first() {
        let (mut engine, kp) = dep_engine();
        // Installing the dependent before its dependency fails closed.
        let err = engine.install(&pkg(&kp, DEPENDENT), &[]).unwrap_err();
        assert!(format!("{err}").contains("not installed"), "got {err}");
        assert!(engine.installed().is_empty());

        // With the dependency installed first, the dependent installs.
        engine.install(&pkg(&kp, BASE), &[]).unwrap();
        engine.install(&pkg(&kp, DEPENDENT), &[]).unwrap();
        assert_eq!(engine.installed().len(), 2);
    }

    #[test]
    fn enable_brings_up_dependencies_first() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap();
        engine.install(&pkg(&kp, DEPENDENT), &[]).unwrap();

        let mut registry = ToolRegistry::new();
        engine.enable("acme/app", &mut registry).unwrap();

        // The dependency was auto-enabled too, before the dependent (event order).
        assert_eq!(
            engine.get("acme/http-core").unwrap().state,
            PluginState::Enabled
        );
        assert!(registry.contains("http.get") && registry.contains("app.run"));
        let enabled: Vec<&PluginEvent> = engine
            .events()
            .iter()
            .filter(|e| matches!(e, PluginEvent::Enabled(_)))
            .collect();
        assert_eq!(
            enabled,
            vec![
                &PluginEvent::Enabled("acme/http-core@1.2.0".into()),
                &PluginEvent::Enabled("acme/app@0.1.0".into()),
            ]
        );
    }

    #[test]
    fn disable_blocked_by_enabled_dependent() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap();
        engine.install(&pkg(&kp, DEPENDENT), &[]).unwrap();
        let mut registry = ToolRegistry::new();
        engine.enable("acme/app", &mut registry).unwrap();

        // Can't disable the dependency while the dependent is enabled.
        let err = engine.disable("acme/http-core", &mut registry).unwrap_err();
        assert!(format!("{err}").contains("required by enabled plugin(s): acme/app"));

        // Disabling the dependent first unblocks it.
        engine.disable("acme/app", &mut registry).unwrap();
        engine.disable("acme/http-core", &mut registry).unwrap();
        assert_eq!(
            engine.get("acme/http-core").unwrap().state,
            PluginState::Disabled
        );
    }

    #[test]
    fn uninstall_blocked_by_installed_dependent() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap();
        engine.install(&pkg(&kp, DEPENDENT), &[]).unwrap();
        let mut registry = ToolRegistry::new();

        let err = engine
            .uninstall("acme/http-core", &mut registry)
            .unwrap_err();
        assert!(format!("{err}").contains("required by installed plugin(s): acme/app"));

        // Removing the dependent first unblocks it.
        engine.uninstall("acme/app", &mut registry).unwrap();
        engine.uninstall("acme/http-core", &mut registry).unwrap();
        assert!(engine.installed().is_empty());
    }

    #[test]
    fn install_rejects_version_conflict() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap(); // http-core 1.2.0
        let needs_v2 = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: app, version: 0.1.0, publisher: acme }
dependencies:
  - { name: http-core, version: "^2.0.0" }
"#;
        let err = engine.install(&pkg(&kp, needs_v2), &[]).unwrap_err();
        assert!(format!("{err}").contains("do not satisfy"), "got {err}");
    }

    // --- Upgrade / rollback ------------------------------------------------------

    const HTTP_V2: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: http-core, version: 2.0.0, publisher: acme }
capabilities:
  - { kind: tool, id: http.get2 }
"#;

    #[test]
    fn upgrade_swaps_active_version_and_reroutes_tools() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap(); // http-core 1.2.0
        let mut registry = ToolRegistry::new();
        engine.enable("acme/http-core", &mut registry).unwrap();
        assert!(registry.contains("http.get"));

        engine
            .upgrade(&pkg(&kp, HTTP_V2), &[], &mut registry)
            .unwrap();

        // Active version swapped, liveness preserved, tools re-routed.
        let p = engine.get("acme/http-core").unwrap();
        assert_eq!(p.manifest.metadata.version, "2.0.0");
        assert_eq!(p.state, PluginState::Enabled);
        assert!(!registry.contains("http.get") && registry.contains("http.get2"));
        assert!(p.previous.is_some(), "prior version retained for rollback");
        assert!(matches!(
            engine.events().last(),
            Some(PluginEvent::Upgraded(r)) if r == "acme/http-core@2.0.0"
        ));
    }

    #[test]
    fn rollback_restores_prior_version() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap();
        let mut registry = ToolRegistry::new();
        engine.enable("acme/http-core", &mut registry).unwrap();
        engine
            .upgrade(&pkg(&kp, HTTP_V2), &[], &mut registry)
            .unwrap();

        engine.rollback("acme/http-core", &mut registry).unwrap();
        let p = engine.get("acme/http-core").unwrap();
        assert_eq!(p.manifest.metadata.version, "1.2.0");
        assert_eq!(p.state, PluginState::Enabled);
        assert!(registry.contains("http.get") && !registry.contains("http.get2"));
        assert!(p.previous.is_none(), "single-level rollback window");

        // Nothing left to roll back to.
        assert!(engine.rollback("acme/http-core", &mut registry).is_err());
    }

    #[test]
    fn upgrade_refuses_to_break_a_dependent() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap(); // http-core 1.2.0
        engine.install(&pkg(&kp, DEPENDENT), &[]).unwrap(); // app needs http-core ^1.0.0
        let mut registry = ToolRegistry::new();

        // Upgrading http-core to 2.0.0 would break app's `^1.0.0` requirement.
        let err = engine
            .upgrade(&pkg(&kp, HTTP_V2), &[], &mut registry)
            .unwrap_err();
        assert!(format!("{err}").contains("would break dependent(s): acme/app"));
        // Old version remains active.
        assert_eq!(
            engine
                .get("acme/http-core")
                .unwrap()
                .manifest
                .metadata
                .version,
            "1.2.0"
        );
    }

    #[test]
    fn upgrade_requires_grants_for_new_permissions() {
        let (mut engine, kp) = dep_engine();
        engine.install(&pkg(&kp, BASE), &[]).unwrap();
        let v2_perm = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: http-core, version: 2.0.0, publisher: acme }
permissions:
  - net:egress:api.example.com
capabilities:
  - { kind: tool, id: http.get2 }
"#;
        let mut registry = ToolRegistry::new();
        // No grant for the newly-requested permission → fail-closed.
        assert!(
            engine
                .upgrade(&pkg(&kp, v2_perm), &[], &mut registry)
                .is_err()
        );
        // Granting it lets the upgrade through.
        engine
            .upgrade(
                &pkg(&kp, v2_perm),
                &["net:egress:*".to_string()],
                &mut registry,
            )
            .unwrap();
        assert_eq!(
            engine
                .get("acme/http-core")
                .unwrap()
                .manifest
                .metadata
                .version,
            "2.0.0"
        );
    }

    #[test]
    fn upgrade_requires_an_installed_plugin() {
        let (mut engine, kp) = dep_engine();
        let mut registry = ToolRegistry::new();
        let err = engine
            .upgrade(&pkg(&kp, HTTP_V2), &[], &mut registry)
            .unwrap_err();
        assert!(format!("{err}").contains("not installed"));
    }

    // --- Secret resolution / injection -------------------------------------------

    fn vault_with(tenant: &str, name: &str, value: &str) -> apex_secrets::Vault {
        let store = std::sync::Arc::new(apex_secrets::InMemorySecretStore::new());
        let vault = apex_secrets::Vault::new(store);
        vault.create(tenant, name, value).unwrap();
        vault
    }

    #[test]
    fn resolves_declared_secret_into_env() {
        let vault = vault_with("acme", "vpn-admin-token", "t0p-secret");
        let perms = vec![
            "net:egress:api.vpn.com".to_string(),
            "secret:read:vpn-admin-token".to_string(),
        ];
        let env = resolve_secret_env(&perms, "acme", &vault).unwrap();
        assert_eq!(
            env,
            vec![(
                "APEX_SECRET_VPN_ADMIN_TOKEN".to_string(),
                "t0p-secret".to_string()
            )]
        );
    }

    #[test]
    fn empty_tenant_injects_nothing() {
        let vault = vault_with("acme", "token", "v");
        let perms = vec!["secret:read:token".to_string()];
        // No tenant context (e.g. operator `plugin run`) → no injection.
        assert!(resolve_secret_env(&perms, "", &vault).unwrap().is_empty());
    }

    #[test]
    fn wildcard_grant_is_skipped() {
        let vault = vault_with("acme", "token", "v");
        let perms = vec!["secret:read:*".to_string()];
        // A wildcard can't be enumerated into a concrete env var.
        assert!(
            resolve_secret_env(&perms, "acme", &vault)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_secret_fails_closed() {
        let vault = vault_with("acme", "present", "v");
        let perms = vec!["secret:read:absent".to_string()];
        assert!(resolve_secret_env(&perms, "acme", &vault).is_err());
    }

    #[test]
    fn cross_tenant_secret_is_forbidden() {
        // The secret lives in acme; a beta workload that declares the same name must not
        // resolve it (the vault rejects the cross-tenant reference fail-closed).
        let vault = vault_with("acme", "token", "v");
        let perms = vec!["secret:read:token".to_string()];
        assert!(resolve_secret_env(&perms, "beta", &vault).is_err());
    }

    // --- Supply-chain (provenance/SBOM) policy ----------------------------------

    #[test]
    fn install_enforces_provenance_policy() {
        let (engine, kp) = dep_engine();
        let mut engine = engine.with_provenance_policy(ProvenancePolicy {
            require_provenance: true,
            ..Default::default()
        });
        // BASE declares no provenance → install is rejected fail-closed.
        let err = engine.install(&pkg(&kp, BASE), &[]).unwrap_err();
        assert!(format!("{err}").contains("provenance"), "got {err}");
        assert!(engine.installed().is_empty());

        // A package whose (signed) manifest carries provenance installs.
        let attested = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata: { name: http-core, version: 1.2.0, publisher: acme }
provenance: { builder: github-actions, source: "github.com/acme/x@v1", built_at: "2026-06-30T00:00:00Z" }
capabilities:
  - { kind: tool, id: http.get }
"#;
        engine.install(&pkg(&kp, attested), &[]).unwrap();
        assert_eq!(engine.installed(), vec!["acme/http-core".to_string()]);
    }
}
