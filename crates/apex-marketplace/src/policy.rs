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
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};

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
}
