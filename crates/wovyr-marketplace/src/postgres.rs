//! Postgres-backed [`RegistryStore`]: one row per listing, shared across nodes
//! ([Marketplace §3](../../docs/08-plugin-sdk/marketplace.md#3-listing-model)).
//!
//! `InMemoryRegistryStore`/`FileRegistryStore` are single-process — a fleet of
//! `wovyr-server` nodes (or CLI invocations) each holding their own `registry.json`
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
//!
//! **Schema migrations (RM-GA-P3 MIG-A1):** `connect` used to run inline
//! `CREATE TABLE IF NOT EXISTS` DDL on every call. Schema changes now live in
//! versioned `migrations/*.sql` files (applied via [`refinery`], tracked in an
//! `wovyr_marketplace_schema_history` table distinct from the other
//! Postgres-backed crates' own history tables so all three can share one
//! physical database without colliding). [`PostgresRegistryStore::run_migrations`]
//! is the only thing that ever runs DDL — invoked explicitly via `wovyr admin
//! migrate`, never by `connect`. `connect` only *reads* the schema version and
//! fails closed (`Error::Config`) if it doesn't match this binary's expected
//! version exactly.

use crate::listing::{ListingRecord, PublishedVersion, Review};
use crate::store::{RegistryState, RegistryStore};
use refinery::Migrate;
use std::sync::Mutex;
use wovyr_common::{Error, Result};

refinery::embed_migrations!("migrations");

/// Distinct per-crate so `wovyr-workflow`/`wovyr-memory`/`wovyr-marketplace` can
/// all migrate the same physical Postgres database without their version
/// tracking colliding.
const MIGRATION_TABLE: &str = "wovyr_marketplace_schema_history";

fn pg_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("postgres {context}: {e}"))
}

/// This binary's expected schema version — the highest version among its own
/// embedded migrations. Pure/local: no database round-trip needed to know it.
fn expected_schema_version() -> u32 {
    migrations::runner()
        .get_migrations()
        .iter()
        .map(|m| m.version())
        .max()
        .unwrap_or(0)
}

/// Read (never write) the schema version actually applied to `client`, and
/// fail closed if it doesn't match [`expected_schema_version`] exactly.
fn assert_schema_version(client: &mut postgres::Client) -> Result<()> {
    let expected = expected_schema_version();
    let applied = Migrate::get_last_applied_migration(client, MIGRATION_TABLE)
        .map_err(|e| {
            Error::config(format!(
                "marketplace Postgres schema is not migrated (expected version {expected}): \
                 {e}; run `wovyr admin migrate --target marketplace --database-url <url>` first"
            ))
        })?
        .map(|m| m.version())
        .unwrap_or(0);
    if applied < expected {
        return Err(Error::config(format!(
            "marketplace Postgres schema is at version {applied}, but this binary needs \
             version {expected}; run `wovyr admin migrate --target marketplace --database-url <url>`"
        )));
    }
    if applied > expected {
        return Err(Error::config(format!(
            "marketplace Postgres schema is at version {applied}, newer than this binary's \
             version {expected}; upgrade the wovyr binary before connecting to this database"
        )));
    }
    Ok(())
}

/// A PostgreSQL-backed [`RegistryStore`].
pub struct PostgresRegistryStore {
    client: Mutex<postgres::Client>,
}

impl PostgresRegistryStore {
    /// Connect (NoTls — for a trusted/local DB) and verify the schema is at the
    /// version this binary expects — never runs DDL. See [`Self::run_migrations`].
    pub fn connect(conn_str: &str) -> Result<Self> {
        let mut client = postgres::Client::connect(conn_str, postgres::NoTls)
            .map_err(|e| pg_err("connect", e))?;
        assert_schema_version(&mut client)?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    /// Apply every pending migration, creating the tracking table on first run.
    /// The only place this crate ever issues DDL — called explicitly via
    /// `wovyr admin migrate`, not from `connect`, so the serving/CLI query path
    /// needs no schema-modification privilege.
    pub fn run_migrations(conn_str: &str) -> Result<()> {
        let mut client = postgres::Client::connect(conn_str, postgres::NoTls)
            .map_err(|e| pg_err("connect", e))?;
        migrations::runner()
            .set_migration_table_name(MIGRATION_TABLE)
            .run(&mut client)
            .map_err(|e| pg_err("migrate", e))?;
        Ok(())
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

    fn report_abuse(&self, listing_id: &str, reporter: &str, reason: &str) -> Result<u64> {
        self.mutate(listing_id, |state| {
            state.report_abuse(listing_id, reporter, reason)
        })
    }

    fn resolve_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        delist: bool,
    ) -> Result<()> {
        self.mutate(listing_id, |state| {
            state.resolve_abuse_report(listing_id, report_id, moderator, delist)
        })
    }

    fn dismiss_abuse_report(
        &self,
        listing_id: &str,
        report_id: u64,
        moderator: &str,
        reason: &str,
    ) -> Result<()> {
        self.mutate(listing_id, |state| {
            state.dismiss_abuse_report(listing_id, report_id, moderator, reason)
        })
    }
}
