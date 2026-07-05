//! The registry catalog store ([Marketplace §3](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//!
//! [`RegistryStore`] is the durability port for published listings. [`InMemoryRegistryStore`]
//! (tests/single-process) and [`FileRegistryStore`] (one `registry.json`) share their
//! CRUD logic via [`RegistryState`]. Operations are fail-closed: mutating an absent
//! listing is [`Error::NotFound`](apex_common::Error::NotFound).

use crate::listing::{
    AbuseReport, AbuseReportStatus, ListingRecord, PublishedVersion, Review, ReviewStatus,
};
use apex_common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The full registry catalog, serialized as one document by [`FileRegistryStore`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegistryState {
    /// Listings keyed by qualified id (`publisher/name`).
    pub listings: BTreeMap<String, ListingRecord>,
}

impl RegistryState {
    /// Insert or update a version under its listing, creating the listing if needed.
    /// Merges `categories`, sets the named `channel` to this version, and keeps
    /// `versions` sorted newest-first. Replaces an existing same-version entry.
    pub fn upsert_version(
        &mut self,
        publisher: &str,
        name: &str,
        version: PublishedVersion,
        categories: &[String],
        channel: &str,
    ) {
        let id = format!("{publisher}/{name}");
        let rec = self
            .listings
            .entry(id.clone())
            .or_insert_with(|| ListingRecord {
                id,
                publisher: publisher.to_string(),
                name: name.to_string(),
                categories: Vec::new(),
                versions: Vec::new(),
                channels: BTreeMap::new(),
                reviews: Vec::new(),
                installs: 0,
                verified: false,
                review: ReviewStatus::Unreviewed,
                abuse_reports: Vec::new(),
                delisted: false,
            });
        for c in categories {
            if !rec.categories.contains(c) {
                rec.categories.push(c.clone());
            }
        }
        rec.channels
            .insert(channel.to_string(), version.version.clone());
        rec.versions.retain(|v| v.version != version.version);
        rec.versions.push(version);
        rec.resort();
    }

    fn get_mut(&mut self, listing_id: &str) -> Result<&mut ListingRecord> {
        self.listings
            .get_mut(listing_id)
            .ok_or_else(|| Error::NotFound(format!("listing `{listing_id}` not found")))
    }

    /// Append a review to a listing.
    pub fn add_review(&mut self, listing_id: &str, review: Review) -> Result<()> {
        self.get_mut(listing_id)?.reviews.push(review);
        Ok(())
    }

    /// Set (or clear) a listing's verified badge directly — an operator override
    /// alongside the [`request_review`](Self::request_review)/
    /// [`approve_review`](Self::approve_review)/[`reject_review`](Self::reject_review)
    /// workflow (e.g. an immediate takedown, or back-compat with a pre-workflow
    /// verified listing). Does not touch [`ReviewStatus`].
    pub fn set_verified(&mut self, listing_id: &str, verified: bool) -> Result<()> {
        self.get_mut(listing_id)?.verified = verified;
        Ok(())
    }

    /// A publisher requests human review of their listing's current latest version
    /// ([Marketplace §6]) — the step gating the **verified** badge (not `publish`
    /// itself; community listings publish and install without ever entering this
    /// lifecycle). Fails if there is no published version yet, or if a review is
    /// already pending.
    pub fn request_review(&mut self, listing_id: &str) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        if rec.review.is_pending() {
            return Err(Error::conflict(format!(
                "listing `{listing_id}` already has a pending review"
            )));
        }
        let version = rec
            .latest()
            .map(|v| v.version.clone())
            .ok_or_else(|| Error::invalid("cannot request review: no published version"))?;
        rec.review = ReviewStatus::Pending { version };
        Ok(())
    }

    /// A reviewer approves the pending review, setting the verified badge. Fails if
    /// no review is pending.
    pub fn approve_review(&mut self, listing_id: &str, reviewer: &str) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        let ReviewStatus::Pending { version } = &rec.review else {
            return Err(Error::invalid(format!(
                "listing `{listing_id}` has no pending review"
            )));
        };
        rec.review = ReviewStatus::Approved {
            reviewer: reviewer.to_string(),
            version: version.clone(),
        };
        rec.verified = true;
        Ok(())
    }

    /// A reviewer rejects the pending review with actionable `reason`, clearing the
    /// verified badge; the publisher may address it and request review again. Fails
    /// if no review is pending.
    pub fn reject_review(&mut self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        let ReviewStatus::Pending { version } = &rec.review else {
            return Err(Error::invalid(format!(
                "listing `{listing_id}` has no pending review"
            )));
        };
        rec.review = ReviewStatus::Rejected {
            reviewer: reviewer.to_string(),
            version: version.clone(),
            reason: reason.to_string(),
        };
        rec.verified = false;
        Ok(())
    }

    /// Increment a listing's install count.
    pub fn record_install(&mut self, listing_id: &str) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        rec.installs = rec.installs.saturating_add(1);
        Ok(())
    }

    /// Submit an abuse report against a listing ([Marketplace §8]), returning its
    /// sequential id (0-based, per listing) for a later resolve/dismiss decision.
    pub fn report_abuse(&mut self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64> {
        let rec = self.get_mut(listing_id)?;
        let id = rec.abuse_reports.len() as u64;
        rec.abuse_reports.push(AbuseReport {
            id,
            reporter: reporter.to_string(),
            reason: reason.to_string(),
            status: AbuseReportStatus::Open,
        });
        Ok(id)
    }

    /// The index of `report_id` within `rec.abuse_reports`, if it is still open.
    fn open_report_index(rec: &ListingRecord, listing_id: &str, report_id: u64) -> Result<usize> {
        let idx = rec
            .abuse_reports
            .iter()
            .position(|r| r.id == report_id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "abuse report `{report_id}` not found on listing `{listing_id}`"
                ))
            })?;
        if !rec.abuse_reports[idx].status.is_open() {
            return Err(Error::conflict(format!(
                "abuse report `{report_id}` on listing `{listing_id}` is already resolved"
            )));
        }
        Ok(idx)
    }

    /// A moderator resolves an open abuse report as valid. `delist` set to `true`
    /// removes the listing from discovery/download; `false` records the finding
    /// without delisting. Fails if the listing or report is absent, or the report is
    /// already resolved/dismissed.
    pub fn resolve_abuse_report(
        &mut self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        let idx = Self::open_report_index(rec, listing_id, report_id)?;
        rec.abuse_reports[idx].status = AbuseReportStatus::Resolved {
            moderator: moderator.to_string(),
            delisted: delist,
        };
        if delist {
            rec.delisted = true;
        }
        Ok(())
    }

    /// A moderator dismisses an open abuse report as not actionable, with `reason`.
    /// Fails if the listing or report is absent, or the report is already
    /// resolved/dismissed.
    pub fn dismiss_abuse_report(
        &mut self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        let idx = Self::open_report_index(rec, listing_id, report_id)?;
        rec.abuse_reports[idx].status = AbuseReportStatus::Dismissed {
            moderator: moderator.to_string(),
            reason: reason.to_string(),
        };
        Ok(())
    }
}

/// Durable storage for marketplace listings.
pub trait RegistryStore: Send + Sync {
    /// Insert/update a published version (see [`RegistryState::upsert_version`]).
    fn upsert_version(
        &self,
        publisher: &str,
        name: &str,
        version: PublishedVersion,
        categories: &[String],
        channel: &str,
    ) -> Result<()>;

    /// Fetch one listing by qualified id.
    fn get(&self, listing_id: &str) -> Result<Option<ListingRecord>>;

    /// All listings, sorted by id.
    fn all(&self) -> Result<Vec<ListingRecord>>;

    /// Append a review to a listing (fail-closed if absent).
    fn add_review(&self, listing_id: &str, review: Review) -> Result<()>;

    /// Set a listing's verified badge (fail-closed if absent).
    fn set_verified(&self, listing_id: &str, verified: bool) -> Result<()>;

    /// A publisher requests human review of the listing's current latest version
    /// (fail-closed if absent, unpublished, or already pending).
    fn request_review(&self, listing_id: &str) -> Result<()>;

    /// A reviewer approves the pending review (fail-closed if none is pending).
    fn approve_review(&self, listing_id: &str, reviewer: &str) -> Result<()>;

    /// A reviewer rejects the pending review with feedback (fail-closed if none is
    /// pending).
    fn reject_review(&self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()>;

    /// Increment a listing's install count (fail-closed if absent).
    fn record_install(&self, listing_id: &str) -> Result<()>;

    /// Submit an abuse report against a listing (fail-closed if absent). Returns
    /// the report's sequential id (0-based, per listing).
    fn report_abuse(&self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64>;

    /// A moderator resolves an open abuse report (fail-closed if the listing/report
    /// is absent or the report is already resolved/dismissed).
    fn resolve_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()>;

    /// A moderator dismisses an open abuse report (fail-closed if the listing/report
    /// is absent or the report is already resolved/dismissed).
    fn dismiss_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()>;
}

/// Lets a `Registry` be built over a boxed, runtime-selected backend
/// (`Registry<Box<dyn RegistryStore>>`) — the seam a binary uses to pick
/// `FileRegistryStore` vs. a capability-gated `PostgresRegistryStore` at startup
/// (e.g. from an environment variable) without becoming generic over it.
impl RegistryStore for Box<dyn RegistryStore> {
    fn upsert_version(
        &self,
        publisher: &str,
        name: &str,
        version: PublishedVersion,
        categories: &[String],
        channel: &str,
    ) -> Result<()> {
        (**self).upsert_version(publisher, name, version, categories, channel)
    }

    fn get(&self, listing_id: &str) -> Result<Option<ListingRecord>> {
        (**self).get(listing_id)
    }

    fn all(&self) -> Result<Vec<ListingRecord>> {
        (**self).all()
    }

    fn add_review(&self, listing_id: &str, review: Review) -> Result<()> {
        (**self).add_review(listing_id, review)
    }

    fn set_verified(&self, listing_id: &str, verified: bool) -> Result<()> {
        (**self).set_verified(listing_id, verified)
    }

    fn request_review(&self, listing_id: &str) -> Result<()> {
        (**self).request_review(listing_id)
    }

    fn approve_review(&self, listing_id: &str, reviewer: &str) -> Result<()> {
        (**self).approve_review(listing_id, reviewer)
    }

    fn reject_review(&self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()> {
        (**self).reject_review(listing_id, reviewer, reason)
    }

    fn record_install(&self, listing_id: &str) -> Result<()> {
        (**self).record_install(listing_id)
    }

    fn report_abuse(&self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64> {
        (**self).report_abuse(listing_id, reporter, reason)
    }

    fn resolve_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()> {
        (**self).resolve_abuse_report(listing_id, report_id, moderator, delist)
    }

    fn dismiss_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()> {
        (**self).dismiss_abuse_report(listing_id, report_id, moderator, reason)
    }
}

/// In-memory registry store for tests and single-process use.
#[derive(Default)]
pub struct InMemoryRegistryStore {
    state: Mutex<RegistryState>,
}

impl InMemoryRegistryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl RegistryStore for InMemoryRegistryStore {
    fn upsert_version(
        &self,
        publisher: &str,
        name: &str,
        version: PublishedVersion,
        categories: &[String],
        channel: &str,
    ) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .upsert_version(publisher, name, version, categories, channel);
        Ok(())
    }

    fn get(&self, listing_id: &str) -> Result<Option<ListingRecord>> {
        Ok(self.state.lock().unwrap().listings.get(listing_id).cloned())
    }

    fn all(&self) -> Result<Vec<ListingRecord>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .listings
            .values()
            .cloned()
            .collect())
    }

    fn add_review(&self, listing_id: &str, review: Review) -> Result<()> {
        self.state.lock().unwrap().add_review(listing_id, review)
    }

    fn set_verified(&self, listing_id: &str, verified: bool) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .set_verified(listing_id, verified)
    }

    fn request_review(&self, listing_id: &str) -> Result<()> {
        self.state.lock().unwrap().request_review(listing_id)
    }

    fn approve_review(&self, listing_id: &str, reviewer: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .approve_review(listing_id, reviewer)
    }

    fn reject_review(&self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .reject_review(listing_id, reviewer, reason)
    }

    fn record_install(&self, listing_id: &str) -> Result<()> {
        self.state.lock().unwrap().record_install(listing_id)
    }

    fn report_abuse(&self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64> {
        self.state
            .lock()
            .unwrap()
            .report_abuse(listing_id, reporter, reason)
    }

    fn resolve_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .resolve_abuse_report(listing_id, report_id, moderator, delist)
    }

    fn dismiss_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .dismiss_abuse_report(listing_id, report_id, moderator, reason)
    }
}

/// File-backed registry store: the whole catalog persisted to one `registry.json`.
/// Reads/writes the file under a `Mutex` per operation (single-node, like the other
/// `~/.apex` stores).
pub struct FileRegistryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileRegistryStore {
    /// A store backed by `path` (created on first write).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<RegistryState> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::config(format!("corrupt registry store: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RegistryState::default()),
            Err(e) => Err(Error::from(e)),
        }
    }

    fn save(&self, state: &RegistryState) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(state)?;
        std::fs::write(&self.path, bytes)?;
        Ok(())
    }

    /// Run `f` against the loaded state and persist the result atomically under the lock.
    fn mutate<T>(&self, f: impl FnOnce(&mut RegistryState) -> Result<T>) -> Result<T> {
        let _guard = self.lock.lock().unwrap();
        let mut state = self.load()?;
        let out = f(&mut state)?;
        self.save(&state)?;
        Ok(out)
    }
}

impl RegistryStore for FileRegistryStore {
    fn upsert_version(
        &self,
        publisher: &str,
        name: &str,
        version: PublishedVersion,
        categories: &[String],
        channel: &str,
    ) -> Result<()> {
        self.mutate(|state| {
            state.upsert_version(publisher, name, version, categories, channel);
            Ok(())
        })
    }

    fn get(&self, listing_id: &str) -> Result<Option<ListingRecord>> {
        let _guard = self.lock.lock().unwrap();
        Ok(self.load()?.listings.get(listing_id).cloned())
    }

    fn all(&self) -> Result<Vec<ListingRecord>> {
        let _guard = self.lock.lock().unwrap();
        Ok(self.load()?.listings.values().cloned().collect())
    }

    fn add_review(&self, listing_id: &str, review: Review) -> Result<()> {
        self.mutate(|state| state.add_review(listing_id, review))
    }

    fn set_verified(&self, listing_id: &str, verified: bool) -> Result<()> {
        self.mutate(|state| state.set_verified(listing_id, verified))
    }

    fn request_review(&self, listing_id: &str) -> Result<()> {
        self.mutate(|state| state.request_review(listing_id))
    }

    fn approve_review(&self, listing_id: &str, reviewer: &str) -> Result<()> {
        self.mutate(|state| state.approve_review(listing_id, reviewer))
    }

    fn reject_review(&self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()> {
        self.mutate(|state| state.reject_review(listing_id, reviewer, reason))
    }

    fn record_install(&self, listing_id: &str) -> Result<()> {
        self.mutate(|state| state.record_install(listing_id))
    }

    fn report_abuse(&self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64> {
        self.mutate(|state| state.report_abuse(listing_id, reporter, reason))
    }

    fn resolve_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()> {
        self.mutate(|state| state.resolve_abuse_report(listing_id, report_id, moderator, delist))
    }

    fn dismiss_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()> {
        self.mutate(|state| state.dismiss_abuse_report(listing_id, report_id, moderator, reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::PermissionRisk;

    fn version(v: &str) -> PublishedVersion {
        PublishedVersion {
            version: v.into(),
            description: "d".into(),
            license: "MIT".into(),
            permissions: vec![],
            capabilities: vec![],
            risk: PermissionRisk::Low,
            scan: Default::default(),
            package_digest: "sha256:00".into(),
            package: "{}".into(),
        }
    }

    #[test]
    fn upsert_sorts_versions_newest_first_and_sets_channel() {
        let store = InMemoryRegistryStore::new();
        store
            .upsert_version("acme", "x", version("1.0.0"), &["dev".into()], "stable")
            .unwrap();
        store
            .upsert_version("acme", "x", version("1.2.0"), &[], "stable")
            .unwrap();
        store
            .upsert_version("acme", "x", version("1.1.0"), &[], "beta")
            .unwrap();

        let rec = store.get("acme/x").unwrap().unwrap();
        assert_eq!(
            rec.versions
                .iter()
                .map(|v| v.version.as_str())
                .collect::<Vec<_>>(),
            vec!["1.2.0", "1.1.0", "1.0.0"]
        );
        assert_eq!(rec.channels.get("stable").unwrap(), "1.2.0");
        assert_eq!(rec.channels.get("beta").unwrap(), "1.1.0");
        assert_eq!(rec.categories, vec!["dev".to_string()]);
        assert_eq!(rec.latest().unwrap().version, "1.2.0");
    }

    #[test]
    fn republish_same_version_replaces() {
        let store = InMemoryRegistryStore::new();
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();
        assert_eq!(store.get("acme/x").unwrap().unwrap().versions.len(), 1);
    }

    #[test]
    fn reviews_verified_installs_are_fail_closed_on_absent() {
        let store = InMemoryRegistryStore::new();
        assert!(store.record_install("nope/x").is_err());
        assert!(store.set_verified("nope/x", true).is_err());
        assert!(
            store
                .add_review(
                    "nope/x",
                    Review {
                        author: "a".into(),
                        rating: 5,
                        body: String::new()
                    }
                )
                .is_err()
        );
        assert!(store.request_review("nope/x").is_err());
        assert!(store.approve_review("nope/x", "alice").is_err());
        assert!(store.reject_review("nope/x", "alice", "no").is_err());
    }

    #[test]
    fn human_review_workflow_approves_and_sets_verified() {
        let store = InMemoryRegistryStore::new();
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();

        // No pending review yet: approve/reject are refused.
        assert!(store.approve_review("acme/x", "alice").is_err());
        assert!(store.reject_review("acme/x", "alice", "no").is_err());
        assert!(!store.get("acme/x").unwrap().unwrap().review.is_pending());

        store.request_review("acme/x").unwrap();
        assert!(store.get("acme/x").unwrap().unwrap().review.is_pending());
        // Double-requesting a pending review is refused.
        assert!(store.request_review("acme/x").is_err());

        store.approve_review("acme/x", "alice").unwrap();
        let rec = store.get("acme/x").unwrap().unwrap();
        assert!(rec.verified);
        assert_eq!(
            rec.review,
            ReviewStatus::Approved {
                reviewer: "alice".into(),
                version: "1.0.0".into(),
            }
        );
    }

    #[test]
    fn human_review_workflow_rejects_and_clears_verified() {
        let store = InMemoryRegistryStore::new();
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();
        store.request_review("acme/x").unwrap();
        store
            .reject_review("acme/x", "alice", "missing SBOM")
            .unwrap();

        let rec = store.get("acme/x").unwrap().unwrap();
        assert!(!rec.verified);
        assert_eq!(
            rec.review,
            ReviewStatus::Rejected {
                reviewer: "alice".into(),
                version: "1.0.0".into(),
                reason: "missing SBOM".into(),
            }
        );
        // A rejected listing may request review again.
        store.request_review("acme/x").unwrap();
        assert!(store.get("acme/x").unwrap().unwrap().review.is_pending());
    }

    #[test]
    fn request_review_requires_a_published_version() {
        // A listing with no published version (constructed directly against
        // `RegistryState`, since `upsert_version` always creates one) must refuse a
        // review request.
        let mut state = RegistryState::default();
        state.listings.insert(
            "acme/empty".into(),
            ListingRecord {
                id: "acme/empty".into(),
                publisher: "acme".into(),
                name: "empty".into(),
                categories: vec![],
                versions: vec![],
                channels: Default::default(),
                reviews: vec![],
                installs: 0,
                verified: false,
                review: ReviewStatus::Unreviewed,
                abuse_reports: vec![],
                delisted: false,
            },
        );
        assert!(state.request_review("acme/empty").is_err());
    }

    #[test]
    fn file_store_round_trips() {
        let dir = std::env::temp_dir().join(format!("apex-mkt-test-{}", std::process::id()));
        let path = dir.join("registry.json");
        let _ = std::fs::remove_file(&path);
        let store = FileRegistryStore::new(&path);
        store
            .upsert_version("acme", "x", version("1.0.0"), &["a".into()], "stable")
            .unwrap();
        store.record_install("acme/x").unwrap();

        // A fresh store over the same path sees the persisted state.
        let reopened = FileRegistryStore::new(&path);
        let rec = reopened.get("acme/x").unwrap().unwrap();
        assert_eq!(rec.installs, 1);
        assert_eq!(rec.latest().unwrap().version, "1.0.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_store_round_trips_abuse_reports() {
        let dir = std::env::temp_dir().join(format!("apex-mkt-abuse-test-{}", std::process::id()));
        let path = dir.join("registry.json");
        let _ = std::fs::remove_file(&path);
        let store = FileRegistryStore::new(&path);
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();
        let id = store.report_abuse("acme/x", "alice", "malware").unwrap();
        store
            .resolve_abuse_report("acme/x", id, "mod1", true)
            .unwrap();

        let reopened = FileRegistryStore::new(&path);
        let rec = reopened.get("acme/x").unwrap().unwrap();
        assert!(rec.delisted);
        assert_eq!(rec.abuse_reports.len(), 1);
        assert_eq!(
            rec.abuse_reports[0].status,
            AbuseReportStatus::Resolved {
                moderator: "mod1".into(),
                delisted: true,
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abuse_report_workflow_resolves_with_delisting() {
        let store = InMemoryRegistryStore::new();
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();

        let id = store
            .report_abuse("acme/x", "alice", "bundles malware")
            .unwrap();
        assert_eq!(id, 0);
        let rec = store.get("acme/x").unwrap().unwrap();
        assert_eq!(rec.abuse_reports.len(), 1);
        assert!(rec.abuse_reports[0].status.is_open());
        assert!(!rec.delisted);

        // A second report gets the next sequential id.
        let id2 = store.report_abuse("acme/x", "bob", "spam").unwrap();
        assert_eq!(id2, 1);

        store
            .resolve_abuse_report("acme/x", id, "mod1", true)
            .unwrap();
        let rec = store.get("acme/x").unwrap().unwrap();
        assert!(
            rec.delisted,
            "resolving with delist=true delists the listing"
        );
        assert_eq!(
            rec.abuse_reports[0].status,
            AbuseReportStatus::Resolved {
                moderator: "mod1".into(),
                delisted: true,
            }
        );
        // The second, unrelated report is untouched.
        assert!(rec.abuse_reports[1].status.is_open());

        // Re-resolving an already-resolved report is refused.
        assert!(
            store
                .resolve_abuse_report("acme/x", id, "mod1", false)
                .is_err()
        );
    }

    #[test]
    fn abuse_report_dismissed_does_not_delist() {
        let store = InMemoryRegistryStore::new();
        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();
        let id = store
            .report_abuse("acme/x", "alice", "false alarm")
            .unwrap();

        store
            .dismiss_abuse_report("acme/x", id, "mod1", "not a violation")
            .unwrap();
        let rec = store.get("acme/x").unwrap().unwrap();
        assert!(!rec.delisted);
        assert_eq!(
            rec.abuse_reports[0].status,
            AbuseReportStatus::Dismissed {
                moderator: "mod1".into(),
                reason: "not a violation".into(),
            }
        );

        // Re-dismissing an already-dismissed report is refused.
        assert!(
            store
                .dismiss_abuse_report("acme/x", id, "mod1", "again")
                .is_err()
        );
    }

    #[test]
    fn abuse_reports_are_fail_closed_on_absent_listing_or_report() {
        let store = InMemoryRegistryStore::new();
        assert!(store.report_abuse("nope/x", "alice", "reason").is_err());

        store
            .upsert_version("acme", "x", version("1.0.0"), &[], "stable")
            .unwrap();
        assert!(
            store
                .resolve_abuse_report("acme/x", 0, "mod1", true)
                .is_err(),
            "no report with id 0 exists yet"
        );
        assert!(
            store
                .dismiss_abuse_report("acme/x", 0, "mod1", "no")
                .is_err()
        );
    }

    #[test]
    fn boxed_dyn_store_delegates_to_the_underlying_backend() {
        // The seam `Registry<Box<dyn RegistryStore>>` relies on: a caller (e.g. a
        // server/CLI runtime store selector) boxes whichever backend it picked, and
        // every operation must behave exactly as calling the concrete type directly.
        let boxed: Box<dyn RegistryStore> = Box::new(InMemoryRegistryStore::new());
        boxed
            .upsert_version("acme", "x", version("1.0.0"), &["a".into()], "stable")
            .unwrap();
        assert_eq!(boxed.all().unwrap().len(), 1);
        boxed.record_install("acme/x").unwrap();
        assert_eq!(boxed.get("acme/x").unwrap().unwrap().installs, 1);
        assert!(boxed.record_install("nope/x").is_err());

        boxed.request_review("acme/x").unwrap();
        boxed.approve_review("acme/x", "alice").unwrap();
        assert!(boxed.get("acme/x").unwrap().unwrap().verified);
        assert!(boxed.reject_review("nope/x", "alice", "no").is_err());

        let report_id = boxed.report_abuse("acme/x", "carol", "spam").unwrap();
        boxed
            .resolve_abuse_report("acme/x", report_id, "mod1", true)
            .unwrap();
        assert!(boxed.get("acme/x").unwrap().unwrap().delisted);
        assert!(
            boxed
                .dismiss_abuse_report("nope/x", 0, "mod1", "no")
                .is_err()
        );
    }
}
