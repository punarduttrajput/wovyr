//! Marketplace governance & curation policy
//! ([Marketplace §7](../../docs/08-plugin-sdk/marketplace.md#7-governance--curation)).
//!
//! [`RegistryPolicy`] is how an operator curates what their deployment exposes: which
//! publishers may publish, the maximum permission risk admitted, a blocklist, and
//! whether only verified listings are discoverable/installable. All checks are
//! fail-closed — a violation aborts publish (or hides a listing from consumers) rather
//! than degrading silently. The default policy is permissive (any trusted publisher,
//! any risk, verified not required) so the registry works out of the box; the
//! signature-trust check in [`crate::Registry`] is the always-on supply-chain floor.

use crate::listing::PermissionRisk;
use crate::scan::{ScanReport, Severity};
use serde::{Deserialize, Serialize};
use wovyr_common::{Error, Result};

/// Operator curation rules applied to publish and consumption.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryPolicy {
    /// If set, only these publishers may publish (allow-list). `None` ⇒ any trusted
    /// publisher.
    #[serde(default)]
    pub allow_publishers: Option<Vec<String>>,
    /// The maximum [`PermissionRisk`] a published version may carry.
    #[serde(default = "default_max_risk")]
    pub max_permission_risk: PermissionRisk,
    /// Listing ids (`publisher/name`) that may not be published or served.
    #[serde(default)]
    pub blocklist: Vec<String>,
    /// When true, only verified listings are returned from discovery and downloadable.
    #[serde(default)]
    pub require_verified: bool,
    /// SBOM components (`name` or `name@version`, e.g. known-vulnerable releases) the
    /// scanner flags as [`Severity::Critical`] `component.denied` findings.
    #[serde(default)]
    pub deny_components: Vec<String>,
    /// Block publish when the security scan reports a finding **at or above** this
    /// severity. `None` ⇒ scanning is advisory only (the report is still stored with
    /// the version, but never blocks).
    #[serde(default)]
    pub block_scan_severity: Option<Severity>,
}

fn default_max_risk() -> PermissionRisk {
    PermissionRisk::High
}

impl Default for RegistryPolicy {
    fn default() -> Self {
        Self {
            allow_publishers: None,
            max_permission_risk: PermissionRisk::High,
            blocklist: Vec::new(),
            require_verified: false,
            deny_components: Vec::new(),
            block_scan_severity: None,
        }
    }
}

impl RegistryPolicy {
    /// Whether `publisher` is permitted to publish under this policy.
    pub fn allows_publisher(&self, publisher: &str) -> bool {
        match &self.allow_publishers {
            Some(list) => list.iter().any(|p| p == publisher),
            None => true,
        }
    }

    /// Whether `listing_id` is blocklisted.
    pub fn is_blocked(&self, listing_id: &str) -> bool {
        self.blocklist.iter().any(|b| b == listing_id)
    }

    /// Enforce the publish-time rules for a candidate version, fail-closed.
    pub fn check_publish(
        &self,
        publisher: &str,
        listing_id: &str,
        risk: PermissionRisk,
    ) -> Result<()> {
        if self.is_blocked(listing_id) {
            return Err(Error::Forbidden(format!(
                "listing `{listing_id}` is blocklisted by marketplace policy"
            )));
        }
        if !self.allows_publisher(publisher) {
            return Err(Error::Forbidden(format!(
                "publisher `{publisher}` is not in the marketplace allow-list"
            )));
        }
        if risk > self.max_permission_risk {
            return Err(Error::Forbidden(format!(
                "version permission risk `{risk:?}` exceeds policy maximum `{:?}`",
                self.max_permission_risk
            )));
        }
        Ok(())
    }

    /// Enforce the scan-gating rule: when a [`block_scan_severity`](Self) ceiling is
    /// configured and `report` carries a finding at or above it, publish is refused
    /// fail-closed, naming the blocking findings. `None` ⇒ always passes (advisory).
    pub fn check_scan(&self, report: &ScanReport) -> Result<()> {
        let Some(ceiling) = self.block_scan_severity else {
            return Ok(());
        };
        let blocking: Vec<String> = report
            .findings
            .iter()
            .filter(|f| f.severity >= ceiling)
            .map(|f| format!("{} ({:?}): {}", f.code, f.severity, f.message))
            .collect();
        if blocking.is_empty() {
            Ok(())
        } else {
            Err(Error::Forbidden(format!(
                "security scan blocks publish (policy blocks at `{ceiling:?}` and above): {}",
                blocking.join("; ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_admits_any_trusted_publisher() {
        let p = RegistryPolicy::default();
        assert!(p.allows_publisher("anyone"));
        assert!(
            p.check_publish("acme", "acme/x", PermissionRisk::High)
                .is_ok()
        );
    }

    #[test]
    fn allow_list_and_blocklist_and_risk_ceiling() {
        let p = RegistryPolicy {
            allow_publishers: Some(vec!["acme".into()]),
            max_permission_risk: PermissionRisk::Medium,
            blocklist: vec!["acme/bad".into()],
            require_verified: true,
            ..Default::default()
        };
        // Not allow-listed.
        assert!(
            p.check_publish("evil", "evil/x", PermissionRisk::Low)
                .is_err()
        );
        // Blocklisted.
        assert!(
            p.check_publish("acme", "acme/bad", PermissionRisk::Low)
                .is_err()
        );
        // Over the risk ceiling.
        assert!(
            p.check_publish("acme", "acme/x", PermissionRisk::High)
                .is_err()
        );
        // Within all bounds.
        assert!(
            p.check_publish("acme", "acme/x", PermissionRisk::Medium)
                .is_ok()
        );
    }

    #[test]
    fn scan_gate_blocks_at_or_above_the_ceiling() {
        use crate::scan::Finding;
        let report = ScanReport {
            findings: vec![
                Finding {
                    code: "sbom.missing".into(),
                    severity: Severity::Info,
                    message: "no SBOM".into(),
                },
                Finding {
                    code: "permission.broad".into(),
                    severity: Severity::Warning,
                    message: "wildcard".into(),
                },
            ],
        };

        // No ceiling ⇒ advisory only, never blocks.
        assert!(RegistryPolicy::default().check_scan(&report).is_ok());

        // Critical ceiling ⇒ a Warning finding does not block.
        let critical_gate = RegistryPolicy {
            block_scan_severity: Some(Severity::Critical),
            ..Default::default()
        };
        assert!(critical_gate.check_scan(&report).is_ok());

        // Warning ceiling ⇒ the Warning finding blocks, and the error names it.
        let warning_gate = RegistryPolicy {
            block_scan_severity: Some(Severity::Warning),
            ..Default::default()
        };
        let err = warning_gate.check_scan(&report).unwrap_err();
        assert!(err.to_string().contains("permission.broad"), "{err}");
        assert!(
            !err.to_string().contains("sbom.missing"),
            "sub-ceiling findings are not blamed: {err}"
        );
    }
}
