//! Live keyless-signing round-trip against a real Rekor transparency log
//! ([ADR-0009](../../docs/17-adr/ADR-0009-keyless-signing.md)).
//!
//! Capability-gated: requires the `rekor` cargo feature **and** `APEX_REKOR_URL`
//! pointing at a running Rekor (see `deployment/rekor/`); skips cleanly otherwise.
//!
//! ```bash
//! docker compose -f deployment/rekor/docker-compose.yml up -d
//! APEX_REKOR_URL=http://localhost:3000 \
//!   cargo test -p apex-plugin --features rekor --test rekor_live
//! ```
#![cfg(feature = "rekor")]

use apex_plugin::keyless::{
    IdentityPolicy, IdentityRule, InMemoryCa, KeylessRoot, SignerIdentity, generate_keypair,
    sign_keyless, verify_keyless,
};
use apex_plugin::rekor::RekorLog;

fn rekor_url() -> Option<String> {
    std::env::var("APEX_REKOR_URL")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Wall clock for the *signing* boundary (verification stays clock-free — it anchors
/// on the log's integrated time).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as u64
}

#[test]
fn keyless_sign_via_rekor_then_offline_verify() {
    let Some(url) = rekor_url() else {
        eprintln!("skipping: APEX_REKOR_URL not set");
        return;
    };

    let manifest = format!(
        "apiVersion: plugin.apex.io/v1\nkind: Plugin\n\
         metadata: {{ name: live, version: 1.0.0, publisher: acme }}\n# nonce {}\n",
        now_ms()
    );
    let identity = SignerIdentity {
        issuer: "https://ci.example.com".into(),
        subject: "release@acme.dev".into(),
    };

    // Sign: dev CA certifies an ephemeral key; the event is witnessed by real Rekor.
    let (ca_pkcs8, _) = generate_keypair().unwrap();
    let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap();
    let (eph, _) = generate_keypair().unwrap();
    let log = RekorLog::new(&url);
    let bundle = sign_keyless(
        manifest.as_bytes(),
        &identity,
        &eph,
        &ca,
        Some(&log),
        now_ms(),
    )
    .expect("rekor append should succeed against the live stack");

    // Rekor witnessed the event.
    let entry = bundle.log_entry.as_ref().expect("bundle carries the entry");
    assert!(!entry.uuid.is_empty());
    assert!(!entry.log_id.is_empty());
    assert!(entry.integrated_time_ms > 0, "{entry:?}");
    assert!(
        !entry.signed_entry_timestamp.is_empty(),
        "rekor returns a SET"
    );

    // Verify fully offline: pinned CA, no pinned log key (Rekor's SET
    // canonicalization is the deferred follow-up — the integrated time still
    // anchors the certificate window).
    let root = KeylessRoot {
        ca_public_keys: vec![ca.public_key_hex()],
        log_public_keys: vec![],
    };
    let policy = IdentityPolicy {
        allow: vec![IdentityRule {
            issuer: "https://ci.example.com".into(),
            subject: "release@acme.dev".into(),
            publisher: "acme".into(),
        }],
        require_transparency: true,
    };
    let verified = verify_keyless(manifest.as_bytes(), &bundle, &root, &policy, "acme").unwrap();
    assert_eq!(verified, identity);

    // Tampering still fails against the live-produced bundle.
    assert!(verify_keyless(b"tampered", &bundle, &root, &policy, "acme").is_err());
}
