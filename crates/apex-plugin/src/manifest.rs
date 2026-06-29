//! The plugin manifest (`plugin.yaml`) — the single source of truth the Plugin
//! Engine reads ([Plugin API §2](../../docs/08-plugin-sdk/plugin-api.md#2-the-manifest-pluginyaml)).
//!
//! The manifest declares identity, platform-API compatibility, requested
//! permissions, the capabilities the plugin contributes, and content-addressed
//! artifacts. It is parsed and validated at install time; failure is fail-closed
//! (a malformed manifest is rejected, never partially registered).

use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// The pinned plugin manifest API version.
pub const PLUGIN_API_VERSION: &str = "plugin.apex.io/v1";

/// A parsed plugin manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Manifest API version; must equal [`PLUGIN_API_VERSION`].
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    /// Kind discriminator; must be `Plugin`.
    pub kind: String,
    /// Identity and provenance.
    pub metadata: Metadata,
    /// Platform-API compatibility constraints.
    #[serde(default)]
    pub compatibility: Compatibility,
    /// Other plugins this one depends on (resolution deferred to a later slice).
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// Requested permission grants (`domain:action:resource`).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// The capabilities this plugin contributes.
    #[serde(default)]
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Content-addressed artifacts (wasm/binaries/images).
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

/// Plugin identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    /// Short plugin name, unique within a publisher namespace.
    pub name: String,
    /// Semantic version of the plugin.
    pub version: String,
    /// Publishing organization/author (the signing identity).
    pub publisher: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// SPDX license id.
    #[serde(default)]
    pub license: String,
    /// Optional project homepage.
    #[serde(default)]
    pub homepage: String,
}

/// Platform-API compatibility constraints.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Compatibility {
    /// Semver range of the platform API this plugin supports (e.g. `>=1.2.0 <2.0.0`).
    #[serde(default)]
    pub platform_api: String,
}

/// A dependency on another plugin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dependency {
    /// Dependency plugin name (`publisher/name` or bare name).
    pub name: String,
    /// Semver requirement.
    pub version: String,
}

/// The kind of capability a descriptor contributes, routed to its host subsystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    /// A tool, registered into the Tool Registry.
    Tool,
    /// An LLM provider, registered into the Gateway/model registry.
    Provider,
    /// A memory backend, registered into the Memory Engine.
    MemoryBackend,
    /// A policy, registered into the Policy Engine.
    Policy,
    /// A workflow activity, registered into the workflow activity registry.
    WorkflowActivity,
}

/// One capability the plugin contributes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    /// Capability kind (selects the host subsystem).
    pub kind: CapabilityKind,
    /// Globally-unique capability id (e.g. `github.create_issue`).
    pub id: String,
    /// Entry point within the package (relative path).
    #[serde(default)]
    pub entry: String,
    /// Sandbox preference for this capability (e.g. `wasm`, `container`).
    #[serde(default)]
    pub sandbox: String,
}

/// A content-addressed artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    /// Path within the package.
    pub path: String,
    /// Content digest, `sha256:<hex>`.
    pub digest: String,
}

impl PluginManifest {
    /// Parse a manifest from YAML and validate it.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let manifest: PluginManifest = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid plugin manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The qualified plugin id, `publisher/name` (the namespace grants bind to).
    pub fn qualified_id(&self) -> String {
        format!("{}/{}", self.metadata.publisher, self.metadata.name)
    }

    /// `publisher/name@version` — the fully-pinned reference used in grants/audit.
    pub fn reference(&self) -> String {
        format!("{}@{}", self.qualified_id(), self.metadata.version)
    }

    /// Validate structural invariants (fail-closed): correct apiVersion/kind,
    /// non-empty identity, a parseable version, and unique non-empty capability ids.
    pub fn validate(&self) -> Result<()> {
        if self.api_version != PLUGIN_API_VERSION {
            return Err(Error::invalid(format!(
                "unsupported plugin apiVersion `{}` (expected `{PLUGIN_API_VERSION}`)",
                self.api_version
            )));
        }
        if self.kind != "Plugin" {
            return Err(Error::invalid(format!(
                "unsupported manifest kind `{}` (expected `Plugin`)",
                self.kind
            )));
        }
        for (field, value) in [
            ("metadata.name", &self.metadata.name),
            ("metadata.version", &self.metadata.version),
            ("metadata.publisher", &self.metadata.publisher),
        ] {
            if value.trim().is_empty() {
                return Err(Error::invalid(format!("plugin {field} must not be empty")));
            }
        }
        semver::Version::parse(&self.metadata.version).map_err(|e| {
            Error::invalid(format!(
                "plugin version `{}` is not valid semver: {e}",
                self.metadata.version
            ))
        })?;

        let mut seen = std::collections::BTreeSet::new();
        for cap in &self.capabilities {
            if cap.id.trim().is_empty() {
                return Err(Error::invalid(
                    "capability id must not be empty".to_string(),
                ));
            }
            if !seen.insert(cap.id.as_str()) {
                return Err(Error::invalid(format!(
                    "duplicate capability id `{}`",
                    cap.id
                )));
            }
        }
        Ok(())
    }

    /// All `tool`-kind capabilities.
    pub fn tool_capabilities(&self) -> impl Iterator<Item = &CapabilityDescriptor> {
        self.capabilities
            .iter()
            .filter(|c| c.kind == CapabilityKind::Tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata:
  name: github
  version: 1.4.0
  publisher: acme
  description: GitHub tools
  license: Apache-2.0
compatibility:
  platform_api: ">=0.1.0 <2.0.0"
permissions:
  - net:egress:api.github.com
  - secret:read:github-token
capabilities:
  - kind: tool
    id: github.create_issue
    entry: capabilities/tools/create_issue
    sandbox: wasm
artifacts:
  - path: artifacts/github.wasm
    digest: sha256:abcd
"#;

    #[test]
    fn parses_and_validates_sample() {
        let m = PluginManifest::from_yaml(SAMPLE).unwrap();
        assert_eq!(m.qualified_id(), "acme/github");
        assert_eq!(m.reference(), "acme/github@1.4.0");
        assert_eq!(m.permissions.len(), 2);
        assert_eq!(m.tool_capabilities().count(), 1);
    }

    #[test]
    fn rejects_wrong_api_version() {
        let bad = SAMPLE.replace("plugin.apex.io/v1", "plugin.apex.io/v2");
        assert!(PluginManifest::from_yaml(&bad).is_err());
    }

    #[test]
    fn rejects_bad_version_and_duplicate_caps() {
        let bad_ver = SAMPLE.replace("1.4.0", "not-semver");
        assert!(PluginManifest::from_yaml(&bad_ver).is_err());

        let dup = format!("{SAMPLE}  - kind: tool\n    id: github.create_issue\n    entry: x\n");
        assert!(PluginManifest::from_yaml(&dup).is_err());
    }
}
