//! End-to-end **keyless** supply chain
//! ([ADR-0009](../../docs/17-adr/ADR-0009-keyless-signing.md)): no publisher key
//! exists anywhere —
//!
//! ```text
//! keyless sign → publish → search → download → install (+grant) → enable
//! ```
//!
//! Runs fully offline and deterministically: the CA and transparency log are the
//! in-process implementations, and both the registry and the plugin engine verify
//! against the same pinned `KeylessRoot` + `IdentityPolicy`.

use wovyr_marketplace::{InMemoryRegistryStore, Registry, SearchQuery};
use wovyr_plugin::keyless::{
    IdentityPolicy, IdentityRule, InMemoryCa, InMemoryTransparencyLog, KeylessRoot, SignerIdentity,
    generate_keypair, sign_keyless,
};
use wovyr_plugin::{Package, PluginEngine, PluginState, TrustStore};
use wovyr_tools::ToolRegistry;

const NOW: u64 = 1_700_000_000_000;

const MANIFEST: &str = r#"
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata:
  name: hello
  version: 1.0.0
  publisher: acme
  description: Keyless-signed example plugin
  license: Apache-2.0
permissions:
  - net:egress:api.example.com
capabilities:
  - { kind: tool, id: hello.greet, entry: greet, sandbox: wasm }
"#;

#[test]
fn keyless_publish_download_install_enable() {
    // ── Publisher side: certify an ephemeral key, sign, witness, discard. ──────
    let (ca_pkcs8, _) = generate_keypair().unwrap();
    let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap();
    let (log_pkcs8, _) = generate_keypair().unwrap();
    let log = InMemoryTransparencyLog::from_pkcs8(&log_pkcs8, NOW + 1000).unwrap();
    let identity = SignerIdentity {
        issuer: "https://ci.example.com".into(),
        subject: "release@acme.dev".into(),
    };
    let (ephemeral, _) = generate_keypair().unwrap();
    let bundle = sign_keyless(
        MANIFEST.as_bytes(),
        &identity,
        &ephemeral,
        &ca,
        Some(&log),
        NOW,
    )
    .unwrap();
    let wovyrpkg = Package::new(MANIFEST, Vec::new())
        .with_keyless(bundle)
        .to_wovyrpkg()
        .unwrap();

    // ── The trust every verifier pins (no per-publisher keys anywhere). ────────
    let root = KeylessRoot {
        ca_public_keys: vec![ca.public_key_hex()],
        log_public_keys: vec![log.public_key_hex()],
    };
    let policy = IdentityPolicy {
        allow: vec![IdentityRule {
            issuer: "https://ci.example.com".into(),
            subject: "release@acme.dev".into(),
            publisher: "acme".into(),
        }],
        require_transparency: true,
    };

    // ── Registry: publish → discover → download. ───────────────────────────────
    let registry = Registry::new(InMemoryRegistryStore::new(), TrustStore::new())
        .with_keyless(root.clone(), policy.clone());
    let out = registry
        .publish(&wovyrpkg, &["examples".into()], None)
        .unwrap();
    assert_eq!(out.reference, "acme/hello@1.0.0");

    let hits = registry
        .search(&SearchQuery {
            text: "hello".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits.len(), 1);

    let downloaded = registry.download("acme/hello", None).unwrap();
    let package = Package::from_wovyrpkg(&downloaded).unwrap();

    // ── Node: install (grant) → enable, against the same pinned root. ──────────
    let mut engine = PluginEngine::new(semver::Version::new(1, 0, 0), TrustStore::new())
        .with_keyless(root, policy);
    let installed = engine
        .install(&package, &["net:egress:api.example.com".to_string()])
        .unwrap();
    assert_eq!(installed.state, PluginState::Disabled);

    let mut tools = ToolRegistry::new();
    engine.enable("acme/hello", &mut tools).unwrap();
    assert!(tools.contains("hello.greet"));
    assert_eq!(
        engine.get("acme/hello").unwrap().state,
        PluginState::Enabled
    );
}
