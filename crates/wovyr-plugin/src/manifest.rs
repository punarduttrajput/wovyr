//! The plugin manifest (`plugin.yaml`) — the single source of truth the Plugin
//! Engine reads ([Plugin API §2](../../docs/08-plugin-sdk/plugin-api.md#2-the-manifest-pluginyaml)).
//!
//! The manifest declares identity, platform-API compatibility, requested
//! permissions, the capabilities the plugin contributes, and content-addressed
//! artifacts. It is parsed and validated at install time; failure is fail-closed
//! (a malformed manifest is rejected, never partially registered).

use serde::{Deserialize, Serialize};
use wovyr_common::{Error, Result};

/// The pinned plugin manifest API version.
pub const PLUGIN_API_VERSION: &str = "plugin.wovyr.io/v1";

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
    /// Software Bill of Materials — the components/dependencies the package bundles
    /// ([distribution §4](../../docs/08-plugin-sdk/distribution.md#4-provenance--sbom)).
    /// Part of the signed manifest, so it is tamper-evident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sbom: Option<Sbom>,
    /// Build provenance — who/what/when built the package, from which source. Signed
    /// (so it is attestable) and checked at install per [`ProvenancePolicy`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
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

/// A Software Bill of Materials: the third-party components a package bundles
/// ([distribution §4](../../docs/08-plugin-sdk/distribution.md#4-provenance--sbom)).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sbom {
    /// The bundled components and their versions.
    #[serde(default)]
    pub components: Vec<SbomComponent>,
}

/// One SBOM component (a dependency the package includes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SbomComponent {
    /// Component name.
    pub name: String,
    /// Component version.
    pub version: String,
    /// SPDX license id, if known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
}

/// Build provenance: who/what/when built the package, and from which source — the basis
/// for supply-chain policy like "only allow plugins built by trusted CI". `built_at` is a
/// caller-supplied string (recorded at build time), so this type holds no clock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The builder identity (e.g. `github-actions`, `local`).
    #[serde(default)]
    pub builder: String,
    /// The source the package was built from (repo URL + ref / commit).
    #[serde(default)]
    pub source: String,
    /// When it was built (RFC3339 string, recorded at build time).
    #[serde(default)]
    pub built_at: String,
}

/// Operator supply-chain policy enforced **at install** ([distribution §4]): a tenant can
/// require provenance/SBOM and restrict which builders are trusted. Default = permissive
/// (no requirement), so packages without attestation still install on an unconfigured node.
#[derive(Clone, Debug, Default)]
pub struct ProvenancePolicy {
    /// Require the package to carry build provenance.
    pub require_provenance: bool,
    /// Require the package to carry an SBOM.
    pub require_sbom: bool,
    /// If non-empty, the provenance `builder` must be one of these (e.g. trusted CI).
    pub allowed_builders: Vec<String>,
}

impl ProvenancePolicy {
    /// Check `manifest` against this policy, fail-closed ([`Error::invalid`]).
    pub fn check(&self, manifest: &PluginManifest) -> Result<()> {
        if self.require_sbom && manifest.sbom.is_none() {
            return Err(Error::invalid(format!(
                "plugin `{}` has no SBOM, which this node requires",
                manifest.qualified_id()
            )));
        }
        match &manifest.provenance {
            None if self.require_provenance || !self.allowed_builders.is_empty() => {
                Err(Error::invalid(format!(
                    "plugin `{}` has no build provenance, which this node requires",
                    manifest.qualified_id()
                )))
            }
            Some(prov) if !self.allowed_builders.is_empty() => {
                if self.allowed_builders.iter().any(|b| b == &prov.builder) {
                    Ok(())
                } else {
                    Err(Error::forbidden(format!(
                        "plugin `{}` was built by `{}`, not a trusted builder",
                        manifest.qualified_id(),
                        prov.builder
                    )))
                }
            }
            _ => Ok(()),
        }
    }
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

        // Dependency version requirements must be valid semver ranges (fail-closed at
        // parse, before resolution).
        for dep in &self.dependencies {
            if dep.name.trim().is_empty() {
                return Err(Error::invalid(
                    "dependency name must not be empty".to_string(),
                ));
            }
            parse_version_req(&dep.version)
                .map_err(|e| Error::invalid(format!("dependency `{}`: {e}", dep.name)))?;
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

/// Parse a semver requirement, accepting the spec's space-separated comparator syntax
/// (`>=0.1.0 <2.0.0`) — which the `semver` crate itself writes comma-separated. Used
/// for both `compatibility.platform_api` and dependency version ranges.
pub fn parse_version_req(range: &str) -> Result<semver::VersionReq> {
    let normalized = range.split_whitespace().collect::<Vec<_>>().join(",");
    semver::VersionReq::parse(&normalized)
        .map_err(|e| Error::invalid(format!("invalid version requirement `{range}`: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: plugin.wovyr.io/v1
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
        // No attestation declared in the base sample.
        assert!(m.provenance.is_none() && m.sbom.is_none());
    }

    const WITH_ATTESTATION: &str = r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: github, version: 1.4.0, publisher: acme }
provenance:
  builder: github-actions
  source: github.com/acme/github-plugin@v1.4.0
  built_at: "2026-06-30T10:00:00Z"
sbom:
  components:
    - { name: serde, version: "1.0.0", license: MIT }
    - { name: reqwest, version: "0.12.0" }
"#;

    #[test]
    fn parses_provenance_and_sbom() {
        let m = PluginManifest::from_yaml(WITH_ATTESTATION).unwrap();
        let prov = m.provenance.as_ref().unwrap();
        assert_eq!(prov.builder, "github-actions");
        assert_eq!(m.sbom.as_ref().unwrap().components.len(), 2);
    }

    #[test]
    fn provenance_policy_enforces_attestation_and_trusted_builders() {
        let bare = PluginManifest::from_yaml(SAMPLE).unwrap();
        let attested = PluginManifest::from_yaml(WITH_ATTESTATION).unwrap();

        // Default policy is permissive.
        assert!(ProvenancePolicy::default().check(&bare).is_ok());

        // Requiring provenance/SBOM rejects the un-attested package, accepts the attested.
        let strict = ProvenancePolicy {
            require_provenance: true,
            require_sbom: true,
            ..Default::default()
        };
        assert!(strict.check(&bare).is_err());
        assert!(strict.check(&attested).is_ok());

        // A trusted-builder allow-list rejects an untrusted (or absent) builder.
        let trusted = ProvenancePolicy {
            allowed_builders: vec!["github-actions".into()],
            ..Default::default()
        };
        assert!(trusted.check(&attested).is_ok());
        assert!(
            trusted.check(&bare).is_err(),
            "no provenance → not a trusted builder"
        );
        let other = PluginManifest::from_yaml(
            &WITH_ATTESTATION.replace("github-actions", "sketchy-laptop"),
        )
        .unwrap();
        assert!(matches!(
            trusted.check(&other).unwrap_err(),
            Error::Forbidden(_)
        ));
    }

    #[test]
    fn rejects_wrong_api_version() {
        let bad = SAMPLE.replace("plugin.wovyr.io/v1", "plugin.wovyr.io/v2");
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
