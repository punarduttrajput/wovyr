//! Plugin Marketplace registry for the Apex AI Platform — the hosted ecosystem layer
//! over the signed plugin supply chain ([Marketplace](../../docs/08-plugin-sdk/marketplace.md)).
//!
//! Where [`apex_plugin`] is the **node-local** control plane (install/enable/uninstall a
//! package that is already on the box), this crate is the **hosted registry**: where
//! packages are *published*, *discovered*, *governed*, and *served for download*. It
//! depends only on [`apex_plugin`] (for the package/manifest/trust types it reuses) and
//! [`apex_common`].
//!
//! - [`listing`] — the [`Listing`]/[`PublishedVersion`]/[`Review`] model and
//!   [`PermissionRisk`] classification ([§3](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//! - [`policy`] — [`RegistryPolicy`], operator curation: publisher allow-list,
//!   permission-risk ceiling, blocklist, verified-only gating
//!   ([§7](../../docs/08-plugin-sdk/marketplace.md#7-governance--curation)).
//! - [`store`] — the [`RegistryStore`] durability port with [`InMemoryRegistryStore`]
//!   and [`FileRegistryStore`] backends.
//! - [`registry`] — the [`Registry`] control plane: publish (signature-verified +
//!   policy-gated), search/discover, download, rate, verify, install-count.
//!
//! Deferred (later slices, [§6/§9](../../docs/08-plugin-sdk/marketplace.md#6-review--quality)):
//! automated security scanning + the full review workflow (only the operator `verify`
//! toggle exists), recommendations, and monetization/billing.

pub mod listing;
pub mod policy;
pub mod registry;
pub mod store;

pub use listing::{Listing, ListingRecord, PermissionRisk, PublishedVersion, Review};
pub use policy::RegistryPolicy;
pub use registry::{DEFAULT_CHANNEL, PublishOutcome, Registry, SearchQuery};
pub use store::{FileRegistryStore, InMemoryRegistryStore, RegistryState, RegistryStore};
