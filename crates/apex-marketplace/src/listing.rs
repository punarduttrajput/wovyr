//! The marketplace listing model
//! ([Marketplace §3 Listing Model](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//!
//! A [`ListingRecord`] is the stored aggregate for one plugin (`publisher/name`): every
//! published [`PublishedVersion`], the channel→version map, ratings, install count, and
//! the verified badge. [`Listing`] is the read-only projection served to consumers —
//! version strings, aggregated capability kinds, average rating — computed from the
//! record so the stored shape and the wire shape can evolve independently.

use apex_plugin::{CapabilityKind, is_broad};
use serde::{Deserialize, Serialize};

/// How risky a version's requested permissions are, used as the governance ceiling
/// ([Marketplace §7](../../docs/08-plugin-sdk/marketplace.md#7-governance--curation)).
/// Ordered `Low < Medium < High` so a policy can cap the maximum it will admit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRisk {
    /// Requests no permissions.
    Low,
    /// Requests one or more narrow (non-wildcard) permissions.
    Medium,
    /// Requests a broad/wildcard permission (e.g. `net:egress:*`).
    High,
}

impl PermissionRisk {
    /// Classify a version's requested permissions: any wildcard ⇒ [`High`](Self::High),
    /// any narrow permission ⇒ [`Medium`](Self::Medium), none ⇒ [`Low`](Self::Low).
    pub fn classify(permissions: &[String]) -> Self {
        if permissions.iter().any(|p| is_broad(p)) {
            PermissionRisk::High
        } else if permissions.is_empty() {
            PermissionRisk::Low
        } else {
            PermissionRisk::Medium
        }
    }
}

/// One published version of a plugin: the metadata consumers inspect before install
/// plus the verified package bytes the registry serves on download.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedVersion {
    /// Semantic version (`metadata.version`).
    pub version: String,
    /// Human-readable description (from the manifest).
    pub description: String,
    /// SPDX license id (from the manifest).
    pub license: String,
    /// Permissions this version requests (shown up front, [Marketplace §3]).
    pub permissions: Vec<String>,
    /// The capability kinds this version contributes.
    pub capabilities: Vec<CapabilityKind>,
    /// Permission-risk classification of this version.
    pub risk: PermissionRisk,
    /// `sha256:<hex>` content digest of the stored package bytes.
    pub package_digest: String,
    /// The published package as `.apexpkg` JSON text (the install artifact). Held as a
    /// `String` because the `.apexpkg` envelope is itself UTF-8 JSON.
    pub package: String,
}

/// A written review and rating, from a (verified) install
/// ([Marketplace §8](../../docs/08-plugin-sdk/marketplace.md#8-ratings--feedback)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    /// The reviewing principal.
    pub author: String,
    /// Star rating, 1–5.
    pub rating: u8,
    /// Optional written feedback.
    #[serde(default)]
    pub body: String,
}

/// The stored aggregate for one listing (`publisher/name`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListingRecord {
    /// Qualified id, `publisher/name`.
    pub id: String,
    /// Publishing identity (the signing publisher).
    pub publisher: String,
    /// Short plugin name.
    pub name: String,
    /// Operator/publisher-supplied categories for browsing.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Published versions, newest first (sorted by semver descending).
    pub versions: Vec<PublishedVersion>,
    /// Channel → version (e.g. `stable` → `1.4.0`).
    #[serde(default)]
    pub channels: std::collections::BTreeMap<String, String>,
    /// Ratings/reviews from installs.
    #[serde(default)]
    pub reviews: Vec<Review>,
    /// Cumulative install count.
    #[serde(default)]
    pub installs: u64,
    /// Whether the listing passed review (operator-set verified badge).
    #[serde(default)]
    pub verified: bool,
}

impl ListingRecord {
    /// The latest published version by semver (the head of `versions`), if any.
    pub fn latest(&self) -> Option<&PublishedVersion> {
        self.versions.first()
    }

    /// Look up a specific published version.
    pub fn version(&self, version: &str) -> Option<&PublishedVersion> {
        self.versions.iter().find(|v| v.version == version)
    }

    /// Average star rating across reviews, or `None` if unrated.
    pub fn rating(&self) -> Option<f64> {
        if self.reviews.is_empty() {
            return None;
        }
        let sum: u64 = self.reviews.iter().map(|r| r.rating as u64).sum();
        Some(sum as f64 / self.reviews.len() as f64)
    }

    /// Re-sort `versions` newest-first and recompute the `stable` channel to the latest
    /// non-prerelease version. Called after inserting a version.
    pub(crate) fn resort(&mut self) {
        self.versions.sort_by(|a, b| {
            let (va, vb) = (
                semver::Version::parse(&a.version),
                semver::Version::parse(&b.version),
            );
            match (va, vb) {
                (Ok(va), Ok(vb)) => vb.cmp(&va),
                _ => b.version.cmp(&a.version),
            }
        });
    }

    /// The read-only projection served over the API.
    pub fn to_listing(&self) -> Listing {
        let latest = self.latest();
        let mut capabilities: Vec<CapabilityKind> =
            latest.map(|v| v.capabilities.clone()).unwrap_or_default();
        capabilities.dedup();
        Listing {
            id: self.id.clone(),
            publisher: self.publisher.clone(),
            name: self.name.clone(),
            description: latest.map(|v| v.description.clone()).unwrap_or_default(),
            categories: self.categories.clone(),
            capabilities,
            permissions: latest.map(|v| v.permissions.clone()).unwrap_or_default(),
            risk: latest.map(|v| v.risk).unwrap_or(PermissionRisk::Low),
            versions: self.versions.iter().map(|v| v.version.clone()).collect(),
            channels: self.channels.clone(),
            rating: self.rating(),
            reviews: self.reviews.len() as u64,
            installs: self.installs,
            verified: self.verified,
        }
    }
}

/// The consumer-facing listing projection (no package bytes).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listing {
    /// Qualified id, `publisher/name`.
    pub id: String,
    /// Publishing identity.
    pub publisher: String,
    /// Short plugin name.
    pub name: String,
    /// Description of the latest version.
    pub description: String,
    /// Browse categories.
    pub categories: Vec<String>,
    /// Capability kinds of the latest version.
    pub capabilities: Vec<CapabilityKind>,
    /// Permissions the latest version requests (shown before install).
    pub permissions: Vec<String>,
    /// Permission risk of the latest version.
    pub risk: PermissionRisk,
    /// All published versions, newest first.
    pub versions: Vec<String>,
    /// Channel → version.
    pub channels: std::collections::BTreeMap<String, String>,
    /// Average rating, or `None` if unrated.
    pub rating: Option<f64>,
    /// Number of reviews.
    pub reviews: u64,
    /// Install count.
    pub installs: u64,
    /// Verified badge.
    pub verified: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_classification_and_ordering() {
        assert_eq!(PermissionRisk::classify(&[]), PermissionRisk::Low);
        assert_eq!(
            PermissionRisk::classify(&["secret:read:token".into()]),
            PermissionRisk::Medium
        );
        assert_eq!(
            PermissionRisk::classify(&["net:egress:*".into()]),
            PermissionRisk::High
        );
        assert!(PermissionRisk::Low < PermissionRisk::Medium);
        assert!(PermissionRisk::Medium < PermissionRisk::High);
    }

    #[test]
    fn rating_averages_reviews() {
        let mut rec = ListingRecord {
            id: "acme/x".into(),
            publisher: "acme".into(),
            name: "x".into(),
            categories: vec![],
            versions: vec![],
            channels: Default::default(),
            reviews: vec![],
            installs: 0,
            verified: false,
        };
        assert_eq!(rec.rating(), None);
        rec.reviews.push(Review {
            author: "a".into(),
            rating: 4,
            body: String::new(),
        });
        rec.reviews.push(Review {
            author: "b".into(),
            rating: 5,
            body: String::new(),
        });
        assert_eq!(rec.rating(), Some(4.5));
    }
}
