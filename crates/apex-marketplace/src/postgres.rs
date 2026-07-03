//! Postgres-backed [`RegistryStore`]: one row per listing, shared across nodes
//! ([Marketplace §3](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//!
//! `InMemoryRegistryStore`/`FileRegistryStore` are single-process — a fleet of
//! `apex-server` nodes (or CLI invocations) each holding their own `registry.json`
//! can't see each other's publishes. This backend gives them one shared, durable
//! catalog instead, closing the "multi-node hosted registry" gap noted in
//! [docs/18-roadmap/v0.3.md](../../../docs/18-roadmap/v0.3.md).
//!
//! Mirrors the document-store shape of [`FileRegistryStore`](crate::FileRegistryStore):
//! the whole [`ListingRecord`] travels as JSON text in one column, keyed by its
//! qualified id (`publisher/name`). Mutations reuse [`RegistryState`]'s pure
//! upsert/review/verify/install logic against a one-entry map per call, so the
//! semantics are identical to the in-memory/file backends — this file only adds
//! the load/save plumbing.
//!
//! Uses the **synchronous** `postgres` crate rather than `tokio-postgres`: this
//! whole crate (`Registry` and every `RegistryStore` method) is deliberately sync,
//! so a blocking client — which manages its own internal runtime — fits without
//! forcing async onto every caller (server routes, CLI, tests). Enabled by the
//! `postgres` cargo feature.

use crate::listing::{ListingRecord, PublishedVersion, Review};
use crate::store::{RegistryState, RegistryStore};
use apex_common::{Error, Result};
use std::sync::Mutex;

fn pg_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("postgres {context}: {e}"))
}

/// A PostgreSQL-backed [`RegistryStore`].
pub struct PostgresRegistryStore {
    client: Mutex<postgres::Client>,
}

impl PostgresRegistryStore {
    /// Connect (NoTls — for a trusted/local DB) and ensure the schema exists.
    pub fn connect(conn_str: &str) -> Result<Self> {
        let mut client = postgres::Client::connect(conn_str, postgres::NoTls)
            .map_err(|e| pg_err("connect", e))?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS marketplace_listings (
                     id      TEXT PRIMARY KEY,
                     listing TEXT NOT NULL
                 );",
            )
            .map_err(|e| pg_err("migrate", e))?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    fn load(client: &mut postgres::Client, id: &str) -> Result<Option<ListingRecord>> {
        let row = client
            .query_opt(
                "SELECT listing FROM marketplace_listings WHERE id = $1",
                &[&id],
            )
            .map_err(|e| pg_err("load listing", e))?;
        match row {
            Some(row) => Ok(Some(serde_json::from_str(row.get::<_, &str>("listing"))?)),
            None => Ok(None),
        }
    }

    fn save(client: &mut postgres::Client, id: &str, rec: &ListingRecord) -> Result<()> {
        let payload = serde_json::to_string(rec)?;
        client
            .execute(
                "INSERT INTO marketplace_listings (id, listing) VALUES ($1, $2)
                 ON CONFLICT (id) DO UPDATE SET listing = EXCLUDED.listing",
                &[&id, &payload],
            )
            .map_err(|e| pg_err("save listing", e))?;
        Ok(())
    }

    /// Load the single existing row for `id` (if any) into a one-entry
    /// [`RegistryState`], run `f` against it, and persist whatever entry now sits
    /// under `id` — creating it (`upsert_version`), updating it, or surfacing `f`'s
    /// `NotFound` unchanged for the fail-closed operations.
    fn mutate<T>(&self, id: &str, f: impl FnOnce(&mut RegistryState) -> Result<T>) -> Result<T> {
        let mut client = self.client.lock().unwrap();
        let mut state = RegistryState::default();
        if let Some(rec) = Self::load(&mut client, id)? {
            state.listings.insert(id.to_string(), rec);
        }
        let out = f(&mut state)?;
        if let Some(rec) = state.listings.get(id) {
            Self::save(&mut client, id, rec)?;
        }
        Ok(out)
    }
}

impl RegistryStore for PostgresRegistryStore {
    fn upsert_version(
        &self,
        publisher: &str,
        name: &str,
        version: PublishedVersion,
        categories: &[String],
        channel: &str,
    ) -> Result<()> {
        let id = format!("{publisher}/{name}");
        self.mutate(&id, |state| {
            state.upsert_version(publisher, name, version, categories, channel);
            Ok(())
        })
    }

    fn get(&self, listing_id: &str) -> Result<Option<ListingRecord>> {
        let mut client = self.client.lock().unwrap();
        Self::load(&mut client, listing_id)
    }

    fn all(&self) -> Result<Vec<ListingRecord>> {
        let mut client = self.client.lock().unwrap();
        let rows = client
            .query("SELECT listing FROM marketplace_listings ORDER BY id", &[])
            .map_err(|e| pg_err("list listings", e))?;
        rows.iter()
            .map(|row| serde_json::from_str(row.get::<_, &str>("listing")).map_err(Error::from))
            .collect()
    }

    fn add_review(&self, listing_id: &str, review: Review) -> Result<()> {
        self.mutate(listing_id, |state| state.add_review(listing_id, review))
    }

    fn set_verified(&self, listing_id: &str, verified: bool) -> Result<()> {
        self.mutate(listing_id, |state| state.set_verified(listing_id, verified))
    }

    fn request_review(&self, listing_id: &str) -> Result<()> {
        self.mutate(listing_id, |state| state.request_review(listing_id))
    }

    fn approve_review(&self, listing_id: &str, reviewer: &str) -> Result<()> {
        self.mutate(listing_id, |state| {
            state.approve_review(listing_id, reviewer)
        })
    }

    fn reject_review(&self, listing_id: &str, reviewer: &str, reason: &str) -> Result<()> {
        self.mutate(listing_id, |state| {
            state.reject_review(listing_id, reviewer, reason)
        })
    }

    fn record_install(&self, listing_id: &str) -> Result<()> {
        self.mutate(listing_id, |state| state.record_install(listing_id))
    }
}
