//! End-to-end supply-chain test for a real third-party plugin.
//!
//! This is the cross-crate proof that closes the v0.3 exit criterion *"a third-party
//! plugin is built, published, installed, granted, and used"*
//! ([roadmap](../../../docs/18-roadmap/v0.3.md#5-exit-criteria)). The per-crate tests
//! cover the two halves in isolation — `apex-marketplace` proves publish→discover→
//! download, `apex-plugin` proves install→enable→execute — but nothing connects them
//! across the seam. This test drives the **whole chain against the committed example
//! plugin** ([`examples/plugins/echo`](../../../examples/plugins/echo)):
//!
//! ```text
//! sign → publish → search → download → install (+grant, +digest verify) → enable → use
//! ```
//!
//! Using the real, committed `plugin.yaml` + `echo.wasm` (rather than an in-test WAT
//! fixture) also guards the example itself: the install step re-verifies the manifest's
//! pinned `sha256` against the committed module, so a drift between the two fails here.
//!
//! The publish→discover→download→install half runs in normal CI. The final **execute**
//! step needs the WASM loader (Wasmtime), so it is gated behind this crate's `wasi`
//! feature: `cargo test -p apex-marketplace --features wasi`.

use apex_marketplace::{InMemoryRegistryStore, PermissionRisk, Registry, SearchQuery};
use apex_plugin::{CapabilityKind, Package, PluginEngine, TrustStore};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::path::PathBuf;

/// The single permission the echo plugin's manifest requests; the operator must grant
/// it at install time (install is fail-closed on an ungranted permission).
const ECHO_GRANT: &str = "net:egress:api.example.com";

/// Path to the committed example plugin directory.
fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/echo")
}

/// A throwaway scratch directory for staged artifacts, unique per test name + process.
fn staging_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("apex_mkt_e2e_{tag}_{}", std::process::id()))
}

/// Sign the example's manifest with a fresh ed25519 key (standing in for the
/// publisher's signing identity) and build a `.apexpkg` carrying the real wasm module.
/// Returns the package bytes plus a `TrustStore` that trusts the signer as `acme`.
fn signed_example_pkg() -> (Vec<u8>, TrustStore) {
    let dir = example_dir();
    let manifest_yaml = std::fs::read_to_string(dir.join("plugin.yaml"))
        .expect("read example plugin.yaml — is examples/plugins/echo committed?");
    let wasm = std::fs::read(dir.join("echo.wasm")).expect("read example echo.wasm");

    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let public = kp.public_key().as_ref().to_vec();
    let sig = kp.sign(manifest_yaml.as_bytes()).as_ref().to_vec();

    let apexpkg = Package::new(manifest_yaml, sig)
        .with_artifact("echo.wasm", wasm)
        .to_apexpkg()
        .expect("encode .apexpkg");

    let mut trust = TrustStore::new();
    trust.trust("acme", public);
    (apexpkg, trust)
}

/// Publish the example, discover it, download it, and install it through the Plugin
/// Engine — the full supply chain up to (but not including) execution, which needs no
/// WASM backend and so runs in normal CI. The install step re-verifies the manifest's
/// pinned artifact digest against the committed `echo.wasm`.
#[test]
fn example_plugin_publishes_discovers_downloads_and_installs() {
    let (apexpkg, trust) = signed_example_pkg();

    // --- Publish to the registry (re-verifies the signature against the trust store). ---
    let registry = Registry::new(InMemoryRegistryStore::new(), trust.clone());
    let out = registry
        .publish(&apexpkg, &["devtools".into()], None)
        .expect("publish signed example");
    assert_eq!(out.reference, "acme/echo@1.0.0");
    assert_eq!(out.listing_id, "acme/echo");
    assert_eq!(out.channel, "stable");

    // --- Discover it by text, category, and capability kind. ---
    let hits = registry
        .search(&SearchQuery {
            text: "echo".into(),
            ..Default::default()
        })
        .expect("search");
    assert_eq!(hits.len(), 1, "echo listing should be discoverable");
    let listing = &hits[0];
    assert_eq!(listing.id, "acme/echo");
    assert_eq!(listing.permissions, vec![ECHO_GRANT.to_string()]);
    // One egress permission to a single host → Medium risk (not the broad-wildcard High).
    assert_eq!(listing.risk, PermissionRisk::Medium);

    assert_eq!(
        registry
            .search(&SearchQuery {
                category: Some("devtools".into()),
                capability: Some(CapabilityKind::Tool),
                ..Default::default()
            })
            .unwrap()
            .len(),
        1,
        "echo should match its category + tool-capability filters"
    );

    // --- Download the exact published bytes (default channel = stable). ---
    let downloaded = registry.download("acme/echo", None).expect("download");
    assert_eq!(
        downloaded, apexpkg,
        "download returns the published bytes verbatim"
    );

    // --- Install through the Plugin Engine: reconstruct, grant, stage, digest-verify. ---
    let staging = staging_dir("install");
    let package = Package::from_apexpkg(&downloaded).expect("reparse downloaded .apexpkg");
    let mut engine =
        PluginEngine::new(semver::Version::new(1, 0, 0), trust).with_staging_dir(&staging);

    let installed = engine
        .install(&package, &[ECHO_GRANT.to_string()])
        .expect("install downloaded example (grant covers requested permission)");
    assert_eq!(installed.manifest.reference(), "acme/echo@1.0.0");
    assert!(
        installed
            .granted_permissions
            .contains(&ECHO_GRANT.to_string()),
        "the granted permission should be recorded on the installed plugin"
    );
    assert!(
        engine.installed().contains(&"acme/echo".to_string()),
        "engine catalog should list the installed plugin"
    );

    // The committed wasm was staged at <staging>/<publisher>/<name>/<version>/echo.wasm,
    // and its sha256 matched the manifest's pinned digest (install is fail-closed on a
    // mismatch), so reaching here proves the example's manifest and module agree.
    let staged = staging.join("acme/echo/1.0.0/echo.wasm");
    assert!(
        staged.is_file(),
        "echo.wasm should be staged at {}",
        staged.display()
    );

    // Installing the same trusted package *without* granting its requested permission
    // must fail closed — a regression guard isolating the grant check from the
    // (already-passed) signature + digest checks. A fresh signed pair keeps the publisher
    // trusted so only the missing grant can be the cause.
    let (apexpkg2, trust2) = signed_example_pkg();
    let package2 = Package::from_apexpkg(&apexpkg2).unwrap();
    let mut bare = PluginEngine::new(semver::Version::new(1, 0, 0), trust2)
        .with_staging_dir(staging_dir("ungranted"));
    assert!(
        bare.install(&package2, &[]).is_err(),
        "install without the requested permission granted must fail closed"
    );

    let _ = std::fs::remove_dir_all(&staging);
}

/// The final, gated step: enable the installed capability and **use it**, proving the
/// committed `echo.wasm` executes its request→response ABI through the real WASM loader.
#[cfg(feature = "wasi")]
#[tokio::test]
async fn example_plugin_capability_executes_end_to_end() {
    use apex_plugin::WasiCapabilityRuntime;
    use apex_tools::{ToolContext, ToolRegistry, ToolRequest};
    use serde_json::json;

    let (apexpkg, trust) = signed_example_pkg();

    // Publish + download through the registry, exactly as a consumer would.
    let registry = Registry::new(InMemoryRegistryStore::new(), trust.clone());
    registry
        .publish(&apexpkg, &["devtools".into()], None)
        .expect("publish");
    let downloaded = registry.download("acme/echo", None).expect("download");

    // Install with the WASM loader configured, then enable into a tool registry.
    let staging = staging_dir("execute");
    let package = Package::from_apexpkg(&downloaded).expect("reparse");
    let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), trust)
        .with_runtime(std::sync::Arc::new(
            WasiCapabilityRuntime::new().expect("init wasi runtime"),
        ))
        .with_staging_dir(&staging);

    engine
        .install(&package, &[ECHO_GRANT.to_string()])
        .expect("install");
    let mut tools = ToolRegistry::new();
    engine
        .enable("acme/echo", &mut tools)
        .expect("enable echo capability into the tool registry");

    // Use it: the echo capability returns whatever JSON it was sent — proving the full
    // request-on-stdin / response-on-stdout ABI through the staged, digest-verified
    // module a consumer downloaded from the marketplace.
    let payload = json!({"message": "hello from a plugin", "n": 7});
    let resp = tools
        .execute(
            "echo.run",
            &ToolContext::default(),
            ToolRequest::new(payload.clone()),
        )
        .await
        .expect("invoke echo.run end to end");
    assert!(resp.success);
    assert_eq!(resp.payload, payload);

    let _ = std::fs::remove_dir_all(&staging);
}
