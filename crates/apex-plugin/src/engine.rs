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

use crate::manifest::{CapabilityDescriptor, CapabilityKind, PluginManifest};
use crate::permissions::missing_grants;
use crate::verify::{TrustStore, verify_digest};
use apex_common::{Error, Result};
use apex_tools::{
    Tool, ToolContext, ToolError, ToolMetadata, ToolRegistry, ToolRequest, ToolResponse,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
}

/// Whether an installed plugin's capabilities are live in their hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginState {
    /// Installed and registered, but capabilities are withdrawn (not live).
    Disabled,
    /// Capabilities are registered with their hosts and serving invocations.
    Enabled,
}

/// A catalogued plugin and its current grant/lifecycle state.
#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    /// The validated manifest.
    pub manifest: PluginManifest,
    /// Current lifecycle state.
    pub state: PluginState,
    /// The permissions the operator granted at install (a superset of the
    /// manifest's requested permissions).
    pub granted_permissions: Vec<String>,
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
    /// `plugin.uninstalled` — withdrawn and dropped from the catalog.
    Uninstalled(String),
}

/// Identifies a capability invocation for the [`CapabilityRuntime`].
pub struct CapabilityCall<'a> {
    /// The fully-pinned plugin reference (`publisher/name@version`).
    pub plugin: &'a str,
    /// The capability id being invoked.
    pub capability_id: &'a str,
    /// The capability's entry point within the package.
    pub entry: &'a str,
    /// The capability's requested sandbox backend (e.g. `wasm`, `container`).
    pub sandbox: &'a str,
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
            plugins: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    /// Use `runtime` to execute plugin capabilities (replaces [`NotLoadedRuntime`]).
    pub fn with_runtime(mut self, runtime: Arc<dyn CapabilityRuntime>) -> Self {
        self.runtime = runtime;
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

        // 2. Verify signature over the raw manifest bytes (fail-closed on untrusted
        //    publisher or tampering).
        self.trust.verify(
            &manifest.metadata.publisher,
            package.manifest_yaml.as_bytes(),
            &package.signature,
        )?;

        // 4. Platform-API compatibility.
        self.check_compatibility(&manifest)?;

        // 6. Permission grants/consent: every requested permission must be granted.
        let missing = missing_grants(&manifest.permissions, grants);
        if !missing.is_empty() {
            return Err(Error::invalid(format!(
                "plugin `{id}` requests ungranted permission(s): {}",
                missing.join(", ")
            )));
        }

        // 7. Stage artifacts (content-addressed): each declared artifact must be
        //    present in the package and match its digest.
        for artifact in &manifest.artifacts {
            let bytes = package.artifacts.get(&artifact.path).ok_or_else(|| {
                Error::invalid(format!(
                    "plugin `{id}` declares artifact `{}` but the package omits its bytes",
                    artifact.path
                ))
            })?;
            verify_digest(&artifact.digest, bytes)?;
        }

        // 8. Register (disabled).
        let reference = manifest.reference();
        self.plugins.insert(
            id.clone(),
            InstalledPlugin {
                manifest,
                state: PluginState::Disabled,
                granted_permissions: grants.to_vec(),
            },
        );
        self.events.push(PluginEvent::Installed(reference));
        Ok(&self.plugins[&id])
    }

    /// Enable plugin `qualified_id`: route its capabilities to their hosts and go live.
    /// Tool capabilities are registered into `registry`; other kinds are catalogued
    /// (no live host yet). Idempotent for an already-enabled plugin.
    pub fn enable(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        let plugin = self
            .plugins
            .get_mut(qualified_id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{qualified_id}` is not installed")))?;
        if plugin.state == PluginState::Enabled {
            return Ok(());
        }

        let plugin_ref = plugin.manifest.reference();
        for cap in plugin.manifest.tool_capabilities() {
            let tool = PluginTool {
                plugin_ref: plugin_ref.clone(),
                metadata: tool_metadata(&plugin.manifest, cap),
                entry: cap.entry.clone(),
                sandbox: cap.sandbox.clone(),
                runtime: Arc::clone(&self.runtime),
            };
            registry.register(Arc::new(tool));
        }
        plugin.state = PluginState::Enabled;
        self.events.push(PluginEvent::Enabled(plugin_ref));
        Ok(())
    }

    /// Disable plugin `qualified_id`: withdraw its capabilities from their hosts,
    /// retaining the catalog entry + grants. Idempotent for an already-disabled plugin.
    pub fn disable(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        let plugin = self
            .plugins
            .get_mut(qualified_id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{qualified_id}` is not installed")))?;
        if plugin.state == PluginState::Disabled {
            return Ok(());
        }
        for cap in plugin.manifest.tool_capabilities() {
            registry.unregister(&cap.id);
        }
        plugin.state = PluginState::Disabled;
        self.events
            .push(PluginEvent::Disabled(plugin.manifest.reference()));
        Ok(())
    }

    /// Uninstall plugin `qualified_id`: withdraw its capabilities and drop it from the
    /// catalog.
    pub fn uninstall(&mut self, qualified_id: &str, registry: &mut ToolRegistry) -> Result<()> {
        self.disable(qualified_id, registry)?;
        let plugin = self
            .plugins
            .remove(qualified_id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{qualified_id}` is not installed")))?;
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

    /// The lifecycle events emitted so far, in order.
    pub fn events(&self) -> &[PluginEvent] {
        &self.events
    }

    /// Refuse a plugin whose declared `platform_api` range excludes the running
    /// platform version. An empty range imposes no constraint.
    fn check_compatibility(&self, manifest: &PluginManifest) -> Result<()> {
        let range = manifest.compatibility.platform_api.trim();
        if range.is_empty() {
            return Ok(());
        }
        // The spec writes ranges space-separated (`>=0.1.0 <2.0.0`); `semver` wants
        // comparators comma-separated. Normalize so the documented syntax parses.
        let normalized = range.split_whitespace().collect::<Vec<_>>().join(",");
        let req = semver::VersionReq::parse(&normalized)
            .map_err(|e| Error::invalid(format!("invalid platform_api range `{range}`: {e}")))?;
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
}
