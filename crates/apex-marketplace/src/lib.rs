//! Plugin Marketplace registry for the Apex AI Platform — the hosted ecosystem layer
//! over the signed plugin supply chain ([Marketplace](../../docs/08-plugin-sdk/marketplace.md)).
//!
//! Where [`apex_plugin`] is the **node-local** control plane (install/enable/uninstall a
//! package that is already on the box), this crate is the **hosted registry**: where
//! packages are *published*, *discovered*, *governed*, and *served for download*. It
//! depends only on [`apex_plugin`] (for the package/manifest/trust types it reuses) and
//! [`apex_common`].
//!
//! - [`listing`] — the [`Listing`]/[`PublishedVersion`]/[`Review`] model,
//!   [`PermissionRisk`] classification, [`ReviewStatus`] (the human-review lifecycle
//!   for the verified badge), and [`AbuseReport`]/[`AbuseReportStatus`] (the
//!   moderation lifecycle for a user-flagged listing)
//!   ([§3](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//! - [`policy`] — [`RegistryPolicy`], operator curation: publisher allow-list,
//!   permission-risk ceiling, blocklist, verified-only gating
//!   ([§7](../../docs/08-plugin-sdk/marketplace.md#7-governance--curation)).
//! - [`store`] — the [`RegistryStore`] durability port with [`InMemoryRegistryStore`]
//!   and [`FileRegistryStore`] backends, plus a capability-gated
//!   [`PostgresRegistryStore`](postgres::PostgresRegistryStore) (`postgres` cargo
//!   feature) for a shared, multi-node catalog.
//! - [`registry`] — the [`Registry`] control plane: publish (signature-verified +
//!   policy-gated + security-scanned), search/discover, download, rate,
//!   install-count, the human-review workflow
//!   ([§6](../../docs/08-plugin-sdk/marketplace.md#6-review--quality)):
//!   `request_review` → `approve_review`/`reject_review` gates the **verified**
//!   badge (not `publish` itself — community/unreviewed listings publish and
//!   install fine without it), alongside the pre-existing `set_verified` operator
//!   override — and the **abuse-report workflow**
//!   ([§8](../../docs/08-plugin-sdk/marketplace.md#8-ratings--feedback)):
//!   `report_abuse` files a report against a listing; a moderator
//!   `resolve_abuse_report` (optionally delisting — excluded from discovery/
//!   download exactly like a policy blocklist entry, but a dynamic moderation
//!   decision) or `dismiss_abuse_report`s it.
//! - [`scan`] — automated static security scanning at publish
//!   ([§6]): artifact integrity, permission sanity, sandbox posture, SBOM
//!   deny-list/licensing, and provenance presence, reported as coded [`Finding`]s
//!   and optionally gating publish via
//!   [`RegistryPolicy::block_scan_severity`](policy::RegistryPolicy).
//!
//! Deferred (later slices, [§9](../../docs/08-plugin-sdk/marketplace.md#9-monetization-planned)):
//! undeclared-usage detection / CVE feeds for the scanner, recommendations, and
//! monetization/billing.

pub mod listing;
pub mod policy;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod registry;
pub mod scan;
pub mod store;

pub use listing::{
    AbuseReport, AbuseReportStatus, Listing, ListingRecord, PermissionRisk, PublishedVersion,
    Review, ReviewStatus,
};
pub use policy::RegistryPolicy;
#[cfg(feature = "postgres")]
pub use postgres::PostgresRegistryStore;
pub use registry::{DEFAULT_CHANNEL, PublishOutcome, Registry, SearchQuery};
pub use scan::{Finding, ScanReport, Severity, scan};
pub use store::{FileRegistryStore, InMemoryRegistryStore, RegistryState, RegistryStore};
