//! Adversarial tests against [`LocalKms`] and [`envelope`] — the boundary that
//! backs [Encryption §5](../../docs/13-security/encryption.md#5-key-management)'s
//! root→tenant→DEK hierarchy, and the thing an attacker with a captured wrapped
//! key, a corrupted store, or a forged wire value would actually poke at. The
//! unit tests in `src/kms.rs`/`src/crypto.rs` prove round-trip correctness and a
//! handful of single tamper cases; these prove the boundary holds up under an
//! attacker who *chooses* their input rather than one who just corrupts a byte:
//! laundering a key across a tenant boundary via the "rotate" operation itself,
//! corrupting the tenant-key layer (not just the DEK layer) via direct store
//! access, forging a version number to redirect decryption onto the wrong key,
//! blind-forging a wrapped value with no legitimate ciphertext at all, and
//! replaying a credential captured before crypto-shredding.
//!
//! No `docker`/network/filesystem access needed — this drives the crypto and
//! `Kms` trait boundary directly, so it runs unconditionally in CI.

use apex_kms::{
    InMemoryKmsStore, Kms, KmsStore, LocalKms, Sealed, WrappedDataKey, envelope, generate_key,
};
use std::collections::HashSet;
use std::sync::Arc;

fn kms_with_store() -> (LocalKms, Arc<InMemoryKmsStore>) {
    let store = Arc::new(InMemoryKmsStore::new());
    let kms = LocalKms::new(generate_key().unwrap(), store.clone());
    (kms, store)
}

/// `rewrap_data_key` exists to migrate a DEK onto its *own* tenant's current key
/// version without touching the plaintext it protects — it must never become a
/// way to launder a DEK across the tenant boundary by calling it with a
/// different tenant argument than the one that minted the DEK.
#[test]
fn attacker_cannot_launder_a_dek_across_tenants_via_rewrap() {
    let (kms, _store) = kms_with_store();
    let dek = kms.generate_data_key("acme").unwrap();

    // `beta` has its own, independently generated tenant key (also version 1),
    // so this isn't a "no such version" failure — the version number lines up,
    // it's the key material underneath that differs.
    let laundered = kms.rewrap_data_key("beta", &dek.wrapped);
    assert!(
        laundered.is_err(),
        "rewrap must not succeed across a tenant boundary even when the target \
         tenant happens to have a same-numbered key version"
    );
}

/// The unit tests in `crypto.rs` tamper with a *DEK-level* ciphertext. An
/// attacker with write access to the `KmsStore` (a compromised backing DB, a
/// tampered `kms.json`) instead corrupts the *tenant-key* layer directly —
/// this must fail the same way, not silently produce garbage tenant-key
/// material that then "succeeds" at unwrapping into nonsense.
#[test]
fn bit_flipped_tenant_key_wrapper_fails_closed_not_just_dek_level_tampering() {
    let (kms, store) = kms_with_store();
    // Provision tenant key version 1 for `acme`.
    kms.generate_data_key("acme").unwrap();

    let mut record = store.get("acme").unwrap().expect("provisioned above");
    record.versions[0].wrapped.ciphertext[0] ^= 0xFF;
    store.put(record).unwrap();

    assert!(
        kms.generate_data_key("acme").is_err(),
        "a corrupted tenant-key wrapper must fail closed, not mint a DEK under \
         garbage key material"
    );
}

/// AES-GCM's security depends on never reusing a nonce under the same key.
/// The existing unit test checks one pair; an attacker gets to observe as many
/// seals as the system performs, so check uniqueness holds at volume, across
/// both layers `envelope::seal` touches (the DEK wrapper and the payload).
#[test]
fn nonce_is_never_reused_across_many_seals_for_the_same_tenant() {
    let (kms, _store) = kms_with_store();
    let mut dek_nonces = HashSet::new();
    let mut payload_nonces = HashSet::new();

    for _ in 0..256 {
        let sealed = envelope::seal(&kms, "acme", b"same plaintext every time").unwrap();
        assert!(
            dek_nonces.insert(sealed.wrapped_key.wrapped.nonce.clone()),
            "DEK-wrapper nonce reused"
        );
        assert!(
            payload_nonces.insert(sealed.sealed.nonce.clone()),
            "payload nonce reused"
        );
    }
}

/// A `WrappedDataKey` carries its `tenant_key_version` as a plain, caller-
/// supplied field. An attacker who captures a DEK wrapped under an old
/// version, then relabels it as the *current* version after rotation, must
/// not get a successful decrypt under the wrong tenant key — AEAD has to
/// reject the mismatch, not just return different (wrong) plaintext.
#[test]
fn forged_tenant_key_version_on_a_wrapped_dek_fails_closed_via_aead() {
    let (kms, _store) = kms_with_store();
    let dek_v1 = kms.generate_data_key("acme").unwrap();
    kms.rotate_tenant_key("acme").unwrap(); // -> version 2

    let forged = WrappedDataKey {
        tenant_key_version: 2, // real ciphertext was sealed under version 1
        wrapped: dek_v1.wrapped.wrapped.clone(),
    };
    assert!(
        kms.unwrap_data_key("acme", &forged).is_err(),
        "relabeling a wrapped DEK's version must not smuggle it past AEAD \
         under a key that never sealed it"
    );
}

/// Distinct from relabeling real ciphertext: an attacker with no legitimate
/// wrapped value at all (e.g. probing the endpoint blind) fabricates one from
/// scratch. Must fail the same way as every other tamper case.
#[test]
fn blind_forgery_with_no_legitimate_ciphertext_is_rejected() {
    let (kms, _store) = kms_with_store();
    kms.generate_data_key("acme").unwrap(); // provision version 1

    let forged = WrappedDataKey {
        tenant_key_version: 1,
        wrapped: Sealed {
            nonce: vec![0u8; 12],
            ciphertext: vec![0x41; 48],
        },
    };
    assert!(kms.unwrap_data_key("acme", &forged).is_err());
}

/// Crypto-shredding is the platform's data-deletion guarantee
/// ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)) — it
/// must hold even against a credential an attacker captured *before* the
/// shred, and it must hold through the higher-level `envelope` API that real
/// consumers (`apex-secrets`, `apex-memory`) actually call, not just the raw
/// `Kms` trait.
#[test]
fn a_dek_captured_before_crypto_shredding_is_useless_after_even_via_envelope() {
    let (kms, _store) = kms_with_store();
    let sealed = envelope::seal(&kms, "acme", b"sensitive payload").unwrap();

    kms.destroy_tenant_key("acme").unwrap();

    assert!(
        envelope::open(&kms, "acme", &sealed).is_err(),
        "a SealedData captured before crypto-shredding must not be recoverable \
         afterward, even via the envelope convenience API"
    );
}
