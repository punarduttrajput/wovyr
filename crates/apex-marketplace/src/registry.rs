//! The marketplace registry — the publish/discover/serve control plane
//! ([Marketplace](../../docs/08-plugin-sdk/marketplace.md)).
//!
//! [`Registry`] sits over a [`RegistryStore`] and applies the two governance layers the
//! spec requires before a package ever reaches a consumer:
//!
//! 1. **Supply-chain trust (always on)** — every publish re-verifies the package's
//!    detached ed25519 signature against the [`TrustStore`] of trusted publishers
//!    (reusing [`Package::verify`](apex_plugin::Package::verify)), so an untrusted
//!    publisher or a tampered manifest is rejected fail-closed.
//! 2. **Operator curation** — a [`RegistryPolicy`] caps which publishers may publish,
//!    the maximum permission risk, a blocklist, and whether only verified listings are
//!    discoverable/downloadable.
//!
//! Discovery ([§4](../../docs/08-plugin-sdk/marketplace.md#4-discovery)) is full-text
//! over name/publisher/description/categories/capability kinds, with category and
//! capability-kind filters. Determinism: the registry holds no clock or randomness;
//! ordering is by relevance then id, and install/review counts are explicit.

use crate::listing::{
    AbuseReport, Listing, ListingRecord, PermissionRisk, PublishedVersion, Review,
};
use crate::policy::RegistryPolicy;
use crate::scan::{self, ScanReport};
use crate::store::RegistryStore;
use crate::vuln::VulnFeed;
use apex_common::{Error, Result};
use apex_plugin::keyless::{IdentityPolicy, KeylessRoot};
use apex_plugin::{CapabilityKind, Package, TrustStore};
use sha2::{Digest, Sha256};

/// The default publish channel when a publisher does not name one.
pub const DEFAULT_CHANNEL: &str = "stable";

/// The outcome of a successful publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOutcome {
    /// The qualified listing id (`publisher/name`).
    pub listing_id: String,
    /// The fully-pinned reference (`publisher/name@version`) — the `plugin.published`
    /// event payload ([§10](../../docs/08-plugin-sdk/marketplace.md#10-lifecycle-integration)).
    pub reference: String,
    /// The channel the version was published to.
    pub channel: String,
    /// The security-scan report ([§6]) — advisory findings the publisher should see
    /// even when the policy admitted the version.
    pub scan: ScanReport,
}

/// A discovery query ([§4](../../docs/08-plugin-sdk/marketplace.md#4-discovery)).
#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    /// Free-text terms (matched case-insensitively over name/publisher/description/
    /// categories/capability kinds). Empty ⇒ match all.
    pub text: String,
    /// Restrict to listings carrying this category.
    pub category: Option<String>,
    /// Restrict to listings whose latest version contributes this capability kind.
    pub capability: Option<CapabilityKind>,
}

/// The registry control plane over a [`RegistryStore`].
pub struct Registry<S: RegistryStore> {
    store: S,
    trust: TrustStore,
    keyless: Option<(KeylessRoot, IdentityPolicy)>,
    policy: RegistryPolicy,
    vuln_feed: VulnFeed,
}

impl<S: RegistryStore> Registry<S> {
    /// A registry over `store`, trusting `trust`, with the default (permissive) policy
    /// and no vulnerability feed.
    pub fn new(store: S, trust: TrustStore) -> Self {
        Self {
            store,
            trust,
            keyless: None,
            policy: RegistryPolicy::default(),
            vuln_feed: VulnFeed::default(),
        }
    }

    /// Accept **keyless-signed** packages at publish
    /// ([ADR-0009](../../docs/17-adr/ADR-0009-keyless-signing.md)): a package
    /// carrying a keyless bundle verifies against this pinned `root` + identity
    /// `policy` instead of the publisher-key trust store. A bundle that fails to
    /// verify is rejected outright (no downgrade to the publisher-key path).
    pub fn with_keyless(mut self, root: KeylessRoot, policy: IdentityPolicy) -> Self {
        self.keyless = Some((root, policy));
        self
    }

    /// Apply an operator curation `policy`.
    pub fn with_policy(mut self, policy: RegistryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Attach an OSV/CVE vulnerability `feed` (ECO-305): every publish scan
    /// matches the package's SBOM components against it, producing Critical
    /// `component.vulnerable` findings that the policy's scan-severity ceiling
    /// can gate on.
    pub fn with_vuln_feed(mut self, feed: VulnFeed) -> Self {
        self.vuln_feed = feed;
        self
    }

    /// The attached vulnerability feed (empty unless
    /// [`with_vuln_feed`](Self::with_vuln_feed) was applied).
    pub fn vuln_feed(&self) -> &VulnFeed {
        &self.vuln_feed
    }

    /// The active policy.
    pub fn policy(&self) -> &RegistryPolicy {
        &self.policy
    }

    /// Publish a `.apexpkg` package to `channel` (default [`DEFAULT_CHANNEL`] if `None`).
    ///
    /// Verifies supply-chain trust (the publisher-key signature against the trust
    /// store, or the keyless bundle when [`with_keyless`](Self::with_keyless) is
    /// configured), enforces the publish policy (allow-list, blocklist,
    /// permission-risk ceiling, scan gate), content-addresses the package, and
    /// indexes it as a new version of its listing. Fail-closed: on any check failure
    /// nothing is indexed. `categories` are publisher/operator-supplied browse tags.
    pub fn publish(
        &self,
        apexpkg: &[u8],
        categories: &[String],
        channel: Option<&str>,
    ) -> Result<PublishOutcome> {
        let package = Package::from_apexpkg(apexpkg)?;
        // 1. Supply-chain trust over the exact manifest bytes: the keyless mode
        // ([ADR-0009]) when the registry accepts it and the package carries a bundle
        // (rejected outright on failure — no downgrade), else the publisher-key
        // signature against the trust store.
        let manifest = match (package.keyless_bundle(), &self.keyless) {
            (Some(_), Some((root, policy))) => package.verify_keyless(root, policy)?,
            _ => package.verify(&self.trust)?,
        };

        let listing_id = manifest.qualified_id();
        let risk = PermissionRisk::classify(&manifest.permissions);
        // 2. Operator curation.
        self.policy
            .check_publish(&manifest.metadata.publisher, &listing_id, risk)?;

        // 3. Automated security scan ([§6]) — the report is stored with the version
        // (consumers see it before install); a configured severity ceiling gates
        // publish fail-closed.
        let scan = scan::scan(
            &package,
            &manifest,
            &self.policy.deny_components,
            &self.vuln_feed,
        );
        self.policy.check_scan(&scan)?;

        let channel = channel.unwrap_or(DEFAULT_CHANNEL).to_string();
        let package_digest = format!("sha256:{:x}", Sha256::digest(apexpkg));
        let capabilities: Vec<CapabilityKind> =
            manifest.capabilities.iter().map(|c| c.kind).collect();

        let version = PublishedVersion {
            version: manifest.metadata.version.clone(),
            description: manifest.metadata.description.clone(),
            license: manifest.metadata.license.clone(),
            permissions: manifest.permissions.clone(),
            capabilities,
            risk,
            scan: scan.clone(),
            package_digest,
            package: String::from_utf8(apexpkg.to_vec())
                .map_err(|_| Error::invalid("`.apexpkg` is not valid UTF-8 JSON"))?,
        };

        self.store.upsert_version(
            &manifest.metadata.publisher,
            &manifest.metadata.name,
            version,
            categories,
            &channel,
        )?;

        Ok(PublishOutcome {
            reference: manifest.reference(),
            listing_id,
            channel,
            scan,
        })
    }

    /// Search/browse listings. Blocklisted listings, and (when the policy requires
    /// verified) unverified listings, are excluded. Results are ranked by a simple
    /// relevance score (name/publisher matches weigh highest) then by id.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Listing>> {
        let needle = query.text.to_lowercase();
        let mut scored: Vec<(i32, Listing)> = Vec::new();
        for rec in self.store.all()? {
            if self.policy.is_blocked(&rec.id) || rec.delisted {
                continue;
            }
            if self.policy.require_verified && !rec.verified {
                continue;
            }
            if let Some(cat) = &query.category {
                if !rec.categories.iter().any(|c| c == cat) {
                    continue;
                }
            }
            if let Some(kind) = query.capability {
                let has = rec
                    .latest()
                    .map(|v| v.capabilities.contains(&kind))
                    .unwrap_or(false);
                if !has {
                    continue;
                }
            }
            let score = relevance(&rec, &needle);
            if needle.is_empty() || score > 0 {
                scored.push((score, rec.to_listing()));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        Ok(scored.into_iter().map(|(_, l)| l).collect())
    }

    /// Fetch a listing's projection by id (respecting blocklist / delisting / verified
    /// policy).
    pub fn get(&self, listing_id: &str) -> Result<Option<Listing>> {
        if self.policy.is_blocked(listing_id) {
            return Ok(None);
        }
        match self.store.get(listing_id)? {
            Some(rec) if rec.delisted => Ok(None),
            Some(rec) if self.policy.require_verified && !rec.verified => Ok(None),
            Some(rec) => Ok(Some(rec.to_listing())),
            None => Ok(None),
        }
    }

    /// Download the `.apexpkg` bytes for a version (the latest stable if `version` is
    /// `None`). Honors blocklist + delisting + verified policy fail-closed. The
    /// returned bytes are the exact published package — the caller installs them
    /// through the Plugin Engine.
    pub fn download(&self, listing_id: &str, version: Option<&str>) -> Result<Vec<u8>> {
        if self.policy.is_blocked(listing_id) {
            return Err(Error::Forbidden(format!(
                "listing `{listing_id}` is blocklisted"
            )));
        }
        let rec = self
            .store
            .get(listing_id)?
            .ok_or_else(|| Error::NotFound(format!("listing `{listing_id}` not found")))?;
        if rec.delisted {
            return Err(Error::Forbidden(format!(
                "listing `{listing_id}` has been delisted"
            )));
        }
        if self.policy.require_verified && !rec.verified {
            return Err(Error::Forbidden(format!(
                "listing `{listing_id}` is not verified"
            )));
        }
        let picked = match version {
            Some(v) => rec.version(v),
            None => rec
                .channels
                .get(DEFAULT_CHANNEL)
                .and_then(|v| rec.version(v))
                .or_else(|| rec.latest()),
        };
        let v = picked.ok_or_else(|| {
            Error::NotFound(format!(
                "listing `{listing_id}` has no version `{}`",
                version.unwrap_or(DEFAULT_CHANNEL)
            ))
        })?;
        Ok(v.package.clone().into_bytes())
    }

    /// Record a 1–5 star review against a listing
    /// ([§8](../../docs/08-plugin-sdk/marketplace.md#8-ratings--feedback)).
    pub fn review(&self, listing_id: &str, review: Review) -> Result<()> {
        if !(1..=5).contains(&review.rating) {
            return Err(Error::invalid("rating must be between 1 and 5"));
        }
        self.store.add_review(listing_id, review)
    }

    /// Set a listing's verified badge directly — an operator override alongside the
    /// [`request_review`](Self::request_review)/[`approve_review`](Self::approve_review)/
    /// [`reject_review`](Self::reject_review) workflow (e.g. an immediate takedown, or
    /// back-compat with a pre-workflow verified listing). Does not touch
    /// `ReviewStatus`.
    pub fn set_verified(&self, listing_id: &str, verified: bool) -> Result<()> {
        self.store.set_verified(listing_id, verified)
    }

    /// A publisher requests human review of their listing's current latest version —
    /// the step gating the **verified** badge
    /// ([§6](../../docs/08-plugin-sdk/marketplace.md#6-review--quality)). Does not
    /// gate `publish` itself: community (unreviewed) listings publish and install
    /// fine without ever entering this lifecycle. Fails if there's no published
    /// version, or a review is already pending.
    pub fn request_review(&self, listing_id: &str) -> Result<()> {
        self.store.request_review(listing_id)
    }

    /// A reviewer approves the listing's pending review, setting the verified badge
    /// ([§6]). Fails if no review is pending.
    pub fn approve_review(&self, listing_id: &str, reviewer: &str) -> Result<()> {
        self.store.approve_review(listing_id, reviewer)
    }

    /// A reviewer rejects the listing's pending review with actionable `reason`
    /// ([§6]), clearing the verified badge; the publisher may address it and request
    /// review again. Fails if no review is pending.
    pub fn reject_review(&self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()> {
        self.store.reject_review(listing_id, reviewer, reason)
    }

    /// Increment a listing's install count (called after a successful install).
    pub fn record_install(&self, listing_id: &str) -> Result<()> {
        self.store.record_install(listing_id)
    }

    /// Submit an abuse report against a listing
    /// ([§8](../../docs/08-plugin-sdk/marketplace.md#8-ratings--feedback)) — user-flagged
    /// malware, IP infringement, deceptive metadata, etc. Feeds moderation and can
    /// trigger delisting via [`resolve_abuse_report`](Self::resolve_abuse_report).
    /// Returns the report's sequential id (0-based, per listing).
    pub fn report_abuse(&self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64> {
        if reason.trim().is_empty() {
            return Err(Error::invalid("abuse report reason must not be empty"));
        }
        self.store.report_abuse(listing_id, reporter, reason)
    }

    /// The abuse reports filed against a listing, for moderator review.
    pub fn abuse_reports(&self, listing_id: &str) -> Result<Vec<AbuseReport>> {
        Ok(self
            .store
            .get(listing_id)?
            .ok_or_else(|| Error::NotFound(format!("listing `{listing_id}` not found")))?
            .abuse_reports)
    }

    /// A moderator resolves an open abuse report as valid ([§8]). `delist` set to
    /// `true` removes the listing from discovery/download (mirroring a policy
    /// blocklist entry, but as a dynamic moderation decision); `false` records the
    /// finding without delisting. Fails if no report matches or it was already
    /// decided.
    pub fn resolve_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()> {
        self.store
            .resolve_abuse_report(listing_id, report_id, moderator, delist)
    }

    /// A moderator dismisses an open abuse report as not actionable, with `reason`
    /// ([§8]). Fails if no report matches or it was already decided.
    pub fn dismiss_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()> {
        self.store
            .dismiss_abuse_report(listing_id, report_id, moderator, reason)
    }
}

/// A small relevance score: exact-ish name/publisher matches weigh most, then
/// description/category/capability matches. `0` ⇒ no match.
fn relevance(rec: &ListingRecord, needle: &str) -> i32 {
    if needle.is_empty() {
        return 0;
    }
    let mut score = 0;
    if rec.name.to_lowercase().contains(needle) {
        score += 10;
    }
    if rec.publisher.to_lowercase().contains(needle) {
        score += 5;
    }
    if rec.id.to_lowercase().contains(needle) {
        score += 3;
    }
    if let Some(v) = rec.latest() {
        if v.description.to_lowercase().contains(needle) {
            score += 2;
        }
    }
    if rec
        .categories
        .iter()
        .any(|c| c.to_lowercase().contains(needle))
    {
        score += 1;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Severity;
    use crate::store::InMemoryRegistryStore;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// Build a signed `.apexpkg` for `manifest_yaml`, returning (bytes, public_key).
    fn signed_pkg(manifest_yaml: &str) -> (Vec<u8>, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = kp.public_key().as_ref().to_vec();
        let sig = kp.sign(manifest_yaml.as_bytes()).as_ref().to_vec();
        let pkg = Package::new(manifest_yaml, sig);
        (pkg.to_apexpkg().unwrap(), public)
    }

    const GITHUB: &str = r#"
apiVersion: plugin.apex.io/v1
kind: Plugin
metadata:
  name: github
  version: 1.4.0
  publisher: acme
  description: GitHub issue and PR tools
  license: Apache-2.0
permissions:
  - net:egress:api.github.com
capabilities:
  - { kind: tool, id: github.create_issue }
"#;

    fn trust_for(publisher: &str, public: Vec<u8>) -> TrustStore {
        let mut t = TrustStore::new();
        t.trust(publisher, public);
        t
    }

    #[test]
    fn publish_then_discover_and_download() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));

        let out = reg
            .publish(&pkg, &["devtools".into(), "scm".into()], None)
            .unwrap();
        assert_eq!(out.reference, "acme/github@1.4.0");
        assert_eq!(out.channel, "stable");

        // Discovery: full-text + category + capability filters.
        let hits = reg
            .search(&SearchQuery {
                text: "github".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        let l = &hits[0];
        assert_eq!(l.id, "acme/github");
        assert_eq!(l.permissions, vec!["net:egress:api.github.com".to_string()]);
        assert_eq!(l.risk, PermissionRisk::Medium);
        assert_eq!(l.channels.get("stable").unwrap(), "1.4.0");

        assert_eq!(
            reg.search(&SearchQuery {
                category: Some("scm".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            reg.search(&SearchQuery {
                category: Some("data".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            0
        );
        assert_eq!(
            reg.search(&SearchQuery {
                capability: Some(CapabilityKind::Tool),
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            reg.search(&SearchQuery {
                capability: Some(CapabilityKind::Provider),
                ..Default::default()
            })
            .unwrap()
            .len(),
            0
        );

        // Download returns the exact published package bytes.
        let bytes = reg.download("acme/github", None).unwrap();
        assert_eq!(bytes, pkg);
        // And it re-parses + re-verifies as the same plugin.
        let parsed = Package::from_apexpkg(&bytes).unwrap();
        assert_eq!(parsed.manifest().unwrap().reference(), "acme/github@1.4.0");
    }

    #[test]
    fn rejects_untrusted_publisher_at_publish() {
        let (pkg, _public) = signed_pkg(GITHUB);
        // Registry trusts nobody.
        let reg = Registry::new(InMemoryRegistryStore::new(), TrustStore::new());
        assert!(reg.publish(&pkg, &[], None).is_err());
        assert!(reg.search(&SearchQuery::default()).unwrap().is_empty());
    }

    #[test]
    fn policy_blocks_high_risk_publish() {
        let broad = GITHUB.replace("net:egress:api.github.com", "net:egress:*");
        let (pkg, public) = signed_pkg(&broad);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public))
            .with_policy(RegistryPolicy {
                max_permission_risk: PermissionRisk::Medium,
                ..Default::default()
            });
        let err = reg.publish(&pkg, &[], None).unwrap_err();
        assert!(matches!(err, Error::Forbidden(_)));
    }

    #[test]
    fn require_verified_hides_until_verified() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public))
            .with_policy(RegistryPolicy {
                require_verified: true,
                ..Default::default()
            });
        reg.publish(&pkg, &[], None).unwrap();

        // Unverified → hidden from discovery and download.
        assert!(reg.search(&SearchQuery::default()).unwrap().is_empty());
        assert!(reg.get("acme/github").unwrap().is_none());
        assert!(reg.download("acme/github", None).is_err());

        // Operator verifies → now visible.
        reg.set_verified("acme/github", true).unwrap();
        assert_eq!(reg.search(&SearchQuery::default()).unwrap().len(), 1);
        assert!(reg.download("acme/github", None).is_ok());
    }

    #[test]
    fn human_review_workflow_gates_the_verified_badge() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public))
            .with_policy(RegistryPolicy {
                require_verified: true,
                ..Default::default()
            });
        reg.publish(&pkg, &[], None).unwrap();

        // Publishing never required review — a community listing exists, just hidden
        // behind `require_verified` until it earns the badge.
        assert!(reg.get("acme/github").unwrap().is_none());
        assert!(reg.approve_review("acme/github", "alice").is_err());

        reg.request_review("acme/github").unwrap();
        assert!(
            reg.get("acme/github").unwrap().is_none(),
            "a pending review does not itself confer the verified badge"
        );

        reg.approve_review("acme/github", "alice").unwrap();
        let listing = reg.get("acme/github").unwrap().unwrap();
        assert!(listing.verified);
        assert_eq!(
            listing.review,
            crate::listing::ReviewStatus::Approved {
                reviewer: "alice".into(),
                version: "1.4.0".into(),
            }
        );
    }

    #[test]
    fn rejected_review_can_be_resubmitted() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));
        reg.publish(&pkg, &[], None).unwrap();

        reg.request_review("acme/github").unwrap();
        reg.reject_review("acme/github", "alice", "missing SBOM")
            .unwrap();
        assert!(!reg.get("acme/github").unwrap().unwrap().verified);

        // The rejection is actionable feedback, not a dead end — request again.
        reg.request_review("acme/github").unwrap();
        reg.approve_review("acme/github", "bob").unwrap();
        assert!(reg.get("acme/github").unwrap().unwrap().verified);
    }

    #[test]
    fn reviews_average_and_validate_range() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));
        reg.publish(&pkg, &[], None).unwrap();

        reg.review(
            "acme/github",
            Review {
                author: "u1".into(),
                rating: 5,
                body: "great".into(),
            },
        )
        .unwrap();
        reg.review(
            "acme/github",
            Review {
                author: "u2".into(),
                rating: 4,
                body: String::new(),
            },
        )
        .unwrap();
        assert!(
            reg.review(
                "acme/github",
                Review {
                    author: "u3".into(),
                    rating: 6,
                    body: String::new()
                }
            )
            .is_err()
        );

        let l = reg.get("acme/github").unwrap().unwrap();
        assert_eq!(l.rating, Some(4.5));
        assert_eq!(l.reviews, 2);
    }

    #[test]
    fn multiple_versions_resolve_channel_and_specific_download() {
        let (v1, public) = signed_pkg(GITHUB);
        let v2_yaml = GITHUB.replace("1.4.0", "1.5.0");
        // Re-sign 1.5.0 with the SAME key so the publisher's trusted key verifies both.
        let rng = SystemRandom::new();
        let _ = rng;
        // Use a fresh registry trusting the v1 key; v2 must be signed by the same key.
        // Simplest: build both from one keypair.
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let pub2 = kp.public_key().as_ref().to_vec();
        let mk = |yaml: &str| {
            let sig = kp.sign(yaml.as_bytes()).as_ref().to_vec();
            Package::new(yaml, sig).to_apexpkg().unwrap()
        };
        let p1 = mk(GITHUB);
        let p2 = mk(&v2_yaml);
        let _ = (v1, public);

        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", pub2));
        reg.publish(&p1, &[], Some("stable")).unwrap();
        reg.publish(&p2, &[], Some("stable")).unwrap();

        let l = reg.get("acme/github").unwrap().unwrap();
        assert_eq!(l.versions, vec!["1.5.0".to_string(), "1.4.0".to_string()]);
        assert_eq!(l.channels.get("stable").unwrap(), "1.5.0");

        // Default download = stable (1.5.0); specific version still reachable.
        assert_eq!(reg.download("acme/github", None).unwrap(), p2);
        assert_eq!(reg.download("acme/github", Some("1.4.0")).unwrap(), p1);
        assert!(reg.download("acme/github", Some("9.9.9")).is_err());
    }

    #[test]
    fn publish_stores_the_scan_report_and_projects_its_summary() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));

        // GITHUB declares neither an SBOM nor provenance → two advisory findings,
        // returned to the publisher and stored with the version.
        let out = reg.publish(&pkg, &[], None).unwrap();
        let codes: Vec<&str> = out.scan.findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["sbom.missing", "provenance.missing"]);
        assert_eq!(out.scan.max_severity(), Some(Severity::Info));

        // The consumer-facing projection carries the summary.
        let l = reg.get("acme/github").unwrap().unwrap();
        assert_eq!(l.scan_severity, Some(Severity::Info));
        assert_eq!(l.scan_findings, 2);
    }

    #[test]
    fn keyless_publish_accepted_when_configured_rejected_otherwise() {
        use apex_plugin::keyless::{
            InMemoryCa, InMemoryTransparencyLog, SignerIdentity, generate_keypair, sign_keyless,
        };
        const NOW: u64 = 1_700_000_000_000;

        // A keyless-only package: empty publisher-key signature, bundle attached.
        let (ca_pkcs8, _) = generate_keypair().unwrap();
        let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap();
        let (log_pkcs8, _) = generate_keypair().unwrap();
        let log = InMemoryTransparencyLog::from_pkcs8(&log_pkcs8, NOW + 1000).unwrap();
        let (eph, _) = generate_keypair().unwrap();
        let identity = SignerIdentity {
            issuer: "https://ci.example.com".into(),
            subject: "release@acme.dev".into(),
        };
        let bundle =
            sign_keyless(GITHUB.as_bytes(), &identity, &eph, &ca, Some(&log), NOW).unwrap();
        let pkg = Package::new(GITHUB, Vec::new())
            .with_keyless(bundle)
            .to_apexpkg()
            .unwrap();
        let root = KeylessRoot {
            ca_public_keys: vec![ca.public_key_hex()],
            log_public_keys: vec![log.public_key_hex()],
        };
        let policy = IdentityPolicy {
            allow: vec![apex_plugin::keyless::IdentityRule {
                issuer: "https://ci.example.com".into(),
                subject: "release@acme.dev".into(),
                publisher: "acme".into(),
            }],
            require_transparency: true,
        };

        // Without keyless config the registry falls to the (empty) trust store.
        let plain = Registry::new(InMemoryRegistryStore::new(), TrustStore::new());
        assert!(plain.publish(&pkg, &[], None).is_err());

        // With the pinned root + policy, the keyless package publishes end to end.
        let reg = Registry::new(InMemoryRegistryStore::new(), TrustStore::new())
            .with_keyless(root, policy);
        let out = reg.publish(&pkg, &[], None).unwrap();
        assert_eq!(out.reference, "acme/github@1.4.0");
        assert!(reg.get("acme/github").unwrap().is_some());
        // The published bytes round-trip with the bundle intact.
        let downloaded = reg.download("acme/github", None).unwrap();
        assert!(
            Package::from_apexpkg(&downloaded)
                .unwrap()
                .keyless_bundle()
                .is_some()
        );
    }

    #[test]
    fn abuse_report_resolved_with_delisting_hides_the_listing() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));
        reg.publish(&pkg, &[], None).unwrap();

        let id = reg
            .report_abuse("acme/github", "alice", "bundles malware")
            .unwrap();
        assert_eq!(reg.abuse_reports("acme/github").unwrap().len(), 1);
        assert!(
            reg.get("acme/github").unwrap().is_some(),
            "not delisted yet"
        );

        reg.resolve_abuse_report("acme/github", id, "mod1", true)
            .unwrap();

        // Delisting behaves like a blocklist entry: hidden from search/get, download
        // refused.
        assert!(reg.get("acme/github").unwrap().is_none());
        assert!(reg.search(&SearchQuery::default()).unwrap().is_empty());
        assert!(reg.download("acme/github", None).is_err());
    }

    #[test]
    fn abuse_report_dismissed_does_not_hide_the_listing() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));
        reg.publish(&pkg, &[], None).unwrap();

        let id = reg.report_abuse("acme/github", "alice", "spam").unwrap();
        reg.dismiss_abuse_report("acme/github", id, "mod1", "not a violation")
            .unwrap();

        assert!(reg.get("acme/github").unwrap().is_some());
        assert!(reg.download("acme/github", None).is_ok());
    }

    #[test]
    fn abuse_report_requires_a_non_empty_reason() {
        let (pkg, public) = signed_pkg(GITHUB);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));
        reg.publish(&pkg, &[], None).unwrap();

        assert!(reg.report_abuse("acme/github", "alice", "  ").is_err());
        assert!(reg.abuse_reports("acme/github").unwrap().is_empty());
    }

    #[test]
    fn scan_severity_ceiling_blocks_publish_fail_closed() {
        // A wildcard permission is a Warning finding; a policy blocking at Warning
        // refuses the publish and indexes nothing.
        let broad = GITHUB.replace("net:egress:api.github.com", "net:egress:*");
        let (pkg, public) = signed_pkg(&broad);
        let reg = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public))
            .with_policy(RegistryPolicy {
                block_scan_severity: Some(Severity::Warning),
                ..Default::default()
            });

        let err = reg.publish(&pkg, &[], None).unwrap_err();
        assert!(err.to_string().contains("permission.broad"), "{err}");
        assert!(
            reg.get("acme/github").unwrap().is_none(),
            "a blocked publish must index nothing"
        );

        // The same package publishes fine under the default (advisory-only) policy.
        let (pkg, public) = signed_pkg(&broad);
        let advisory = Registry::new(InMemoryRegistryStore::new(), trust_for("acme", public));
        let out = advisory.publish(&pkg, &[], None).unwrap();
        assert!(
            out.scan
                .findings
                .iter()
                .any(|f| f.code == "permission.broad")
        );
    }
}
