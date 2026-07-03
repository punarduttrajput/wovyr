//! The registry catalog store ([Marketplace §3](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//!
//! [`RegistryStore`] is the durability port for published listings. [`InMemoryRegistryStore`]
//! (tests/single-process) and [`FileRegistryStore`] (one `registry.json`) share their
//! CRUD logic via [`RegistryState`]. Operations are fail-closed: mutating an absent
//! listing is [`Error::NotFound`](apex_common::Error::NotFound).

use crate::listing::{ListingRecord, PublishedVersion, Review};
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

    /// Set (or clear) a listing's verified badge.
    pub fn set_verified(&mut self, listing_id: &str, verified: bool) -> Result<()> {
        self.get_mut(listing_id)?.verified = verified;
        Ok(())
    }

    /// Increment a listing's install count.
    pub fn record_install(&mut self, listing_id: &str) -> Result<()> {
        let rec = self.get_mut(listing_id)?;
        rec.installs = rec.installs.saturating_add(1);
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

    /// Increment a listing's install count (fail-closed if absent).
    fn record_install(&self, listing_id: &str) -> Result<()>;
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

    fn record_install(&self, listing_id: &str) -> Result<()> {
        self.state.lock().unwrap().record_install(listing_id)
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

    fn record_install(&self, listing_id: &str) -> Result<()> {
        self.mutate(|state| state.record_install(listing_id))
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
}
