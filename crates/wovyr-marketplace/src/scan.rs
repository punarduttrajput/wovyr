//! Automated security scanning at publish
//! ([Marketplace §6 Review & Quality](../../docs/08-plugin-sdk/marketplace.md#6-review--quality)).
//!
//! [`scan`] is the static, deterministic pass in the spec's publish pipeline
//! (`Submit → automated checks → security scan → … → publish`): it examines the
//! *verified* package + manifest and produces a [`ScanReport`] of coded [`Finding`]s.
//! The report is stored on the [`PublishedVersion`](crate::listing::PublishedVersion)
//! (visible to consumers before install) and, when the operator configures a
//! [`RegistryPolicy::block_scan_severity`](crate::policy::RegistryPolicy) ceiling,
//! gates publish fail-closed. No clock, no network, no randomness — the same package
//! always yields the same report, findings in manifest declaration order.
//!
//! Known vulnerabilities come from two complementary sources: the operator's
//! manual `deny_components` list (emergency blocks) and an OSV/CVE feed
//! ([`crate::vuln::VulnFeed`], RM-AIM-P3 ECO-305) matched against every SBOM
//! component's `name@version` — both purely data-driven, keeping the scan
//! deterministic. Deferred (needs static analysis of artifact code, [§6]):
//! undeclared-usage detection (a module calling APIs its manifest doesn't
//! request).

use crate::vuln::VulnFeed;
use serde::{Deserialize, Serialize};
use wovyr_plugin::{Package, PluginManifest, is_broad, verify_digest};

/// Severity of a scan finding, ordered `Info < Warning < Critical` so a policy can
/// gate on a ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Advisory: worth surfacing, not inherently dangerous (e.g. no SBOM declared).
    Info,
    /// Elevated: broadens the blast radius (e.g. wildcard permission, native sandbox).
    Warning,
    /// Integrity or known-bad: tampered/missing content, denied component.
    Critical,
}

/// One coded security-scan finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable machine-readable code (e.g. `permission.broad`, `artifact.digest_mismatch`).
    pub code: String,
    /// How severe the finding is.
    pub severity: Severity,
    /// Human-readable detail naming the offending element.
    pub message: String,
}

/// The report of one scan run, stored with the published version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// All findings, in manifest declaration order. Empty ⇒ clean.
    pub findings: Vec<Finding>,
}

impl ScanReport {
    /// The most severe finding, or `None` when the scan is clean.
    pub fn max_severity(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }

    fn push(&mut self, code: &str, severity: Severity, message: impl Into<String>) {
        self.findings.push(Finding {
            code: code.into(),
            severity,
            message: message.into(),
        });
    }
}

/// Statically scan a verified `package`/`manifest` pair. `deny_components` is the
/// operator's SBOM deny-list (entries match a component `name` or `name@version` —
/// e.g. known-vulnerable releases); `vuln_feed` is the OSV/CVE feed (ECO-305 —
/// pass `&VulnFeed::default()` for a feedless scan).
///
/// Checks, by code:
/// - `artifact.missing` (**Critical**) — a manifest artifact has no blob in the package.
/// - `artifact.digest_mismatch` (**Critical**) — a bundled blob does not content-address
///   to its declared digest (install would also reject it; publish refuses to serve it).
/// - `component.denied` (**Critical**) — an SBOM component is on the operator deny-list.
/// - `component.vulnerable` (**Critical**) — an SBOM component matches an OSV/CVE
///   advisory in the operator's feed.
/// - `permission.broad` (**Warning**) — a requested permission carries a wildcard.
/// - `capability.unsandboxed` (**Warning**) — a capability asks for the `native`
///   (weakest-isolation) sandbox.
/// - `sbom.missing` / `provenance.missing` (**Info**) — no attestation to inspect.
/// - `component.unlicensed` (**Info**) — an SBOM component declares no license.
pub fn scan(
    package: &Package,
    manifest: &PluginManifest,
    deny_components: &[String],
    vuln_feed: &VulnFeed,
) -> ScanReport {
    let mut report = ScanReport::default();

    // Artifact integrity: every declared artifact must be present and content-address
    // to its pinned digest.
    for artifact in &manifest.artifacts {
        match package.artifact_bytes(&artifact.path) {
            None => report.push(
                "artifact.missing",
                Severity::Critical,
                format!("artifact `{}` is declared but not bundled", artifact.path),
            ),
            Some(bytes) => {
                if verify_digest(&artifact.digest, bytes).is_err() {
                    report.push(
                        "artifact.digest_mismatch",
                        Severity::Critical,
                        format!(
                            "artifact `{}` does not match its declared digest",
                            artifact.path
                        ),
                    );
                }
            }
        }
    }

    // Permission sanity: wildcards broaden the blast radius.
    for perm in &manifest.permissions {
        if is_broad(perm) {
            report.push(
                "permission.broad",
                Severity::Warning,
                format!("permission `{perm}` is a broad/wildcard grant"),
            );
        }
    }

    // Capability sandbox posture: asking for the weakest isolation is a signal.
    for cap in &manifest.capabilities {
        if cap.sandbox == "native" {
            report.push(
                "capability.unsandboxed",
                Severity::Warning,
                format!(
                    "capability `{}` requests the native (weakest) sandbox",
                    cap.id
                ),
            );
        }
    }

    // SBOM: deny-listed or unlicensed components; absence is advisory.
    match &manifest.sbom {
        None => report.push(
            "sbom.missing",
            Severity::Info,
            "package declares no SBOM; bundled components are unknown",
        ),
        Some(sbom) => {
            for c in &sbom.components {
                let pinned = format!("{}@{}", c.name, c.version);
                if deny_components.iter().any(|d| *d == c.name || *d == pinned) {
                    report.push(
                        "component.denied",
                        Severity::Critical,
                        format!("SBOM component `{pinned}` is on the operator deny-list"),
                    );
                }
                for adv in vuln_feed.advisories_for(&c.name, &c.version) {
                    let aliases = if adv.aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" (aka {})", adv.aliases.join(", "))
                    };
                    report.push(
                        "component.vulnerable",
                        Severity::Critical,
                        format!(
                            "SBOM component `{pinned}` matches advisory {}{aliases}: {}",
                            adv.id, adv.summary
                        ),
                    );
                }
                if c.license.is_empty() {
                    report.push(
                        "component.unlicensed",
                        Severity::Info,
                        format!("SBOM component `{pinned}` declares no license"),
                    );
                }
            }
        }
    }

    // Provenance: absence is advisory (an install-time `ProvenancePolicy` can require it).
    if manifest.provenance.is_none() {
        report.push(
            "provenance.missing",
            Severity::Info,
            "package declares no build provenance",
        );
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn manifest(yaml: &str) -> PluginManifest {
        PluginManifest::from_yaml(yaml).unwrap()
    }

    const ATTESTED_CLEAN: &str = r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: clean, version: 1.0.0, publisher: acme }
permissions:
  - secret:read:token
provenance: { builder: github-actions, source: github.com/acme/clean@v1, built_at: "2026-07-02T00:00:00Z" }
sbom:
  components:
    - { name: serde, version: "1.0.0", license: MIT }
"#;

    #[test]
    fn clean_attested_package_has_no_findings() {
        let m = manifest(ATTESTED_CLEAN);
        let pkg = Package::new(ATTESTED_CLEAN, Vec::new());
        let report = scan(&pkg, &m, &[], &VulnFeed::default());
        assert!(report.findings.is_empty(), "{report:?}");
        assert_eq!(report.max_severity(), None);
    }

    #[test]
    fn broad_permission_and_native_sandbox_are_warnings() {
        let yaml = r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: risky, version: 1.0.0, publisher: acme }
permissions:
  - net:egress:*
capabilities:
  - { kind: tool, id: risky.run, entry: run, sandbox: native }
provenance: { builder: ci, source: s, built_at: t }
sbom: { components: [] }
"#;
        let m = manifest(yaml);
        let report = scan(
            &Package::new(yaml, Vec::new()),
            &m,
            &[],
            &VulnFeed::default(),
        );
        let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["permission.broad", "capability.unsandboxed"]);
        assert_eq!(report.max_severity(), Some(Severity::Warning));
    }

    #[test]
    fn denied_component_matches_name_or_pinned_version() {
        let m = manifest(ATTESTED_CLEAN);
        let pkg = Package::new(ATTESTED_CLEAN, Vec::new());

        for deny in ["serde", "serde@1.0.0"] {
            let report = scan(&pkg, &m, &[deny.to_string()], &VulnFeed::default());
            assert_eq!(report.findings.len(), 1, "deny `{deny}`: {report:?}");
            assert_eq!(report.findings[0].code, "component.denied");
            assert_eq!(report.max_severity(), Some(Severity::Critical));
        }
        // A different pinned version does not match.
        let report = scan(&pkg, &m, &["serde@2.0.0".to_string()], &VulnFeed::default());
        assert!(report.findings.is_empty());
    }

    #[test]
    fn artifact_integrity_findings_are_critical() {
        let blob = b"wasm bytes".to_vec();
        let digest = format!("sha256:{:x}", Sha256::digest(&blob));
        let yaml = format!(
            r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: {{ name: art, version: 1.0.0, publisher: acme }}
artifacts:
  - {{ path: mod.wasm, digest: "{digest}" }}
  - {{ path: gone.wasm, digest: "{digest}" }}
provenance: {{ builder: ci, source: s, built_at: t }}
sbom: {{ components: [] }}
"#
        );
        let m = manifest(&yaml);

        // Bundled + matching digest for one, the other missing entirely.
        let pkg = Package::new(yaml.clone(), Vec::new()).with_artifact("mod.wasm", blob.clone());
        let report = scan(&pkg, &m, &[], &VulnFeed::default());
        let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["artifact.missing"]);

        // Tampered blob → digest mismatch.
        let pkg = Package::new(yaml, Vec::new())
            .with_artifact("mod.wasm", b"tampered".to_vec())
            .with_artifact("gone.wasm", blob);
        let report = scan(&pkg, &m, &[], &VulnFeed::default());
        let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["artifact.digest_mismatch"]);
        assert_eq!(report.max_severity(), Some(Severity::Critical));
    }

    #[test]
    fn missing_attestation_and_license_are_info() {
        let yaml = r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: bare, version: 1.0.0, publisher: acme }
"#;
        let m = manifest(yaml);
        let report = scan(
            &Package::new(yaml, Vec::new()),
            &m,
            &[],
            &VulnFeed::default(),
        );
        let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["sbom.missing", "provenance.missing"]);
        assert_eq!(report.max_severity(), Some(Severity::Info));

        let yaml = r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: nolic, version: 1.0.0, publisher: acme }
provenance: { builder: ci, source: s, built_at: t }
sbom:
  components:
    - { name: leftpad, version: "0.1.0" }
"#;
        let m = manifest(yaml);
        let report = scan(
            &Package::new(yaml, Vec::new()),
            &m,
            &[],
            &VulnFeed::default(),
        );
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "component.unlicensed");
    }

    /// ECO-305 acceptance: a known-vulnerable SBOM component is flagged via the
    /// OSV feed — by range match, with the advisory id + CVE alias in the
    /// message — and a fixed version is not.
    #[test]
    fn vulnerable_component_is_flagged_via_osv_feed() {
        let feed = VulnFeed::from_osv_json(
            r#"[{
                "id": "RUSTSEC-2026-0042",
                "aliases": ["CVE-2026-31337"],
                "summary": "serde_yaml_ng unsafe alias expansion",
                "affected": [{
                    "package": { "ecosystem": "crates.io", "name": "serde" },
                    "ranges": [{ "type": "SEMVER",
                                 "events": [{ "introduced": "0" }, { "fixed": "1.0.1" }] }]
                }]
            }]"#,
        )
        .unwrap();

        // ATTESTED_CLEAN's SBOM pins serde@1.0.0 — inside [0, 1.0.1).
        let m = manifest(ATTESTED_CLEAN);
        let pkg = Package::new(ATTESTED_CLEAN, Vec::new());
        let report = scan(&pkg, &m, &[], &feed);
        assert_eq!(report.findings.len(), 1, "{report:?}");
        assert_eq!(report.findings[0].code, "component.vulnerable");
        assert_eq!(report.max_severity(), Some(Severity::Critical));
        assert!(
            report.findings[0].message.contains("RUSTSEC-2026-0042")
                && report.findings[0].message.contains("CVE-2026-31337"),
            "message names the advisory and its CVE alias: {}",
            report.findings[0].message
        );

        // The same feed against a fixed version: clean.
        let fixed = ATTESTED_CLEAN.replace("1.0.0\", license: MIT", "1.0.1\", license: MIT");
        let m = manifest(&fixed);
        let report = scan(&Package::new(&fixed, Vec::new()), &m, &[], &feed);
        assert!(report.findings.is_empty(), "{report:?}");
    }

    #[test]
    fn severity_orders_info_warning_critical() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Critical);
    }
}
