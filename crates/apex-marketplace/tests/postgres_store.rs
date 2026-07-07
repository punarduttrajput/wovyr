//! Live integration test for the Postgres-backed registry store.
//!
//! Capability-gated like the workflow/memory Postgres backends: reads
//! `APEX_MARKETPLACE_POSTGRES_URL` and skips (logging) when unset/unreachable, so
//! offline CI still passes. Verifies the keystone property — a fleet of independent
//! connections share one durable catalog — plus round-tripping the full listing
//! shape (versions, channels, categories, reviews, verified badge, install count)
//! through the `RegistryStore` trait the same way `FileRegistryStore` is tested.
//!
//! Only compiled with `--features postgres`. `connect` only ever *reads* the
//! schema version (RM-GA-P3 MIG-A1) — migrate first, or every test here skips
//! with a "not migrated" reason instead of running. To run locally:
//!
//! ```bash
//! cargo run -p apex-cli --features postgres -- admin migrate --target marketplace \
//!   --database-url postgres://apex:apex@127.0.0.1:5433/apex
//! APEX_MARKETPLACE_POSTGRES_URL=postgres://apex:apex@127.0.0.1:5433/apex \
//!   cargo test -p apex-marketplace --features postgres --test postgres_store -- --nocapture
//! ```
//!
//! Each test uses a per-run nonce listing id, so the shared table stays isolated
//! across runs.

#![cfg(feature = "postgres")]

use apex_marketplace::{PermissionRisk, PostgresRegistryStore, RegistryStore, Review};
use std::time::{SystemTime, UNIX_EPOCH};

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Connect a `PostgresRegistryStore`, or `None` (logging a skip) when unconfigured
/// or unreachable.
fn store() -> Option<PostgresRegistryStore> {
    let url = match std::env::var("APEX_MARKETPLACE_POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: APEX_MARKETPLACE_POSTGRES_URL not set");
            return None;
        }
    };
    match PostgresRegistryStore::connect(&url) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("skipping: postgres unreachable at {url}: {e}");
            None
        }
    }
}

fn version(v: &str) -> apex_marketplace::PublishedVersion {
    apex_marketplace::PublishedVersion {
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
fn upsert_review_verify_and_install_round_trip_through_postgres() {
    let Some(store) = store() else { return };
    let id = format!("acme/pgtest-{}", nonce());
    let (publisher, name) = id.split_once('/').unwrap();

    store
        .upsert_version(publisher, name, version("1.0.0"), &["dev".into()], "stable")
        .unwrap();
    store
        .upsert_version(publisher, name, version("1.1.0"), &[], "beta")
        .unwrap();

    let rec = store.get(&id).unwrap().expect("listing was persisted");
    assert_eq!(
        rec.versions
            .iter()
            .map(|v| v.version.as_str())
            .collect::<Vec<_>>(),
        vec!["1.1.0", "1.0.0"],
        "newest-first, matching the in-memory/file store semantics"
    );
    assert_eq!(rec.channels.get("stable").unwrap(), "1.0.0");
    assert_eq!(rec.channels.get("beta").unwrap(), "1.1.0");
    assert_eq!(rec.categories, vec!["dev".to_string()]);

    store
        .add_review(
            &id,
            Review {
                author: "alice".into(),
                rating: 5,
                body: "great plugin".into(),
            },
        )
        .unwrap();
    store.set_verified(&id, true).unwrap();
    store.record_install(&id).unwrap();
    store.record_install(&id).unwrap();

    let rec = store.get(&id).unwrap().unwrap();
    assert_eq!(rec.reviews.len(), 1);
    assert_eq!(rec.reviews[0].author, "alice");
    assert!(rec.verified);
    assert_eq!(rec.installs, 2);
}

#[test]
fn fail_closed_on_a_listing_that_does_not_exist() {
    let Some(store) = store() else { return };
    let id = format!("acme/missing-{}", nonce());
    assert!(store.record_install(&id).is_err());
    assert!(store.set_verified(&id, true).is_err());
    assert!(
        store
            .add_review(
                &id,
                Review {
                    author: "a".into(),
                    rating: 1,
                    body: String::new(),
                },
            )
            .is_err()
    );
    assert!(store.request_review(&id).is_err());
    assert!(store.approve_review(&id, "alice").is_err());
    assert!(store.reject_review(&id, "alice", "no").is_err());
    assert!(store.get(&id).unwrap().is_none());
}

#[test]
fn human_review_workflow_round_trips_through_postgres() {
    let Some(store) = store() else { return };
    let id = format!("acme/pgreview-{}", nonce());
    let (publisher, name) = id.split_once('/').unwrap();
    store
        .upsert_version(publisher, name, version("1.0.0"), &[], "stable")
        .unwrap();

    // Not pending yet: a decision is refused.
    assert!(store.approve_review(&id, "alice").is_err());

    store.request_review(&id).unwrap();
    assert!(store.get(&id).unwrap().unwrap().review.is_pending());
    // Double-requesting a pending review is refused.
    assert!(store.request_review(&id).is_err());

    store.approve_review(&id, "alice").unwrap();
    let rec = store.get(&id).unwrap().unwrap();
    assert!(rec.verified);
    assert_eq!(
        rec.review,
        apex_marketplace::ReviewStatus::Approved {
            reviewer: "alice".into(),
            version: "1.0.0".into(),
        }
    );

    // A rejection on a fresh request clears the badge and is resubmittable.
    store.request_review(&id).unwrap();
    store.reject_review(&id, "bob", "needs an SBOM").unwrap();
    let rec = store.get(&id).unwrap().unwrap();
    assert!(!rec.verified);
    assert!(!rec.review.is_pending());
}

#[test]
fn independent_connections_share_the_same_durable_catalog() {
    // The realistic multi-node scenario: two separate connections (as two
    // `apex-server` processes, or a server + a CLI invocation, would each open)
    // must see each other's writes through the shared table.
    let Some(store1) = store() else { return };
    let url = std::env::var("APEX_MARKETPLACE_POSTGRES_URL").unwrap();
    let store2 = match PostgresRegistryStore::connect(&url) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skipping: second postgres connection failed: {e}");
            return;
        }
    };

    let id = format!("acme/shared-{}", nonce());
    let (publisher, name) = id.split_once('/').unwrap();
    store1
        .upsert_version(publisher, name, version("1.0.0"), &[], "stable")
        .unwrap();

    let seen_by_2 = store2
        .get(&id)
        .unwrap()
        .expect("visible from a fresh connection");
    assert_eq!(seen_by_2.latest().unwrap().version, "1.0.0");

    store2.record_install(&id).unwrap();
    let seen_by_1 = store1.get(&id).unwrap().unwrap();
    assert_eq!(
        seen_by_1.installs, 1,
        "install recorded by store2 is visible via store1"
    );
}
