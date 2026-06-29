//! Package signature & provenance verification
//! ([overview §6](../../docs/08-plugin-sdk/overview.md#6-installation-lifecycle)).
//!
//! Every package is signature-verified before install (fail-closed): the manifest
//! bytes carry a detached **ed25519** signature, checked against the trusted public
//! key registered for the manifest's `publisher`. An unknown publisher or a bad
//! signature aborts the install with no capability registered.

use apex_common::{Error, Result};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Verify that `bytes` content-addresses to the `declared` digest (`sha256:<hex>`).
/// Used to **stage artifacts** fail-closed during install: a missing prefix, an
/// unsupported algorithm, or a digest mismatch aborts the install.
pub fn verify_digest(declared: &str, bytes: &[u8]) -> Result<()> {
    let hex = declared.strip_prefix("sha256:").ok_or_else(|| {
        Error::invalid(format!(
            "unsupported artifact digest `{declared}` (expected `sha256:<hex>`)"
        ))
    })?;
    let actual = hex::encode(Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(hex) {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "artifact digest mismatch: declared `{declared}`, computed `sha256:{actual}`"
        )))
    }
}

/// Minimal lowercase hex codec (avoids pulling a `hex` crate for a couple of uses).
pub(crate) mod hex {
    use apex_common::{Error, Result};

    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let mut s = String::with_capacity(bytes.as_ref().len() * 2);
        for b in bytes.as_ref() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    pub fn decode(s: &str) -> Result<Vec<u8>> {
        if s.len() % 2 != 0 {
            return Err(Error::invalid("hex string has odd length".to_string()));
        }
        (0..s.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&s[i..i + 2], 16)
                    .map_err(|e| Error::invalid(format!("invalid hex: {e}")))
            })
            .collect()
    }
}

/// A registry of trusted publisher signing keys (ed25519 public keys, 32 bytes).
/// Operators populate it with the publishers they trust; verification fails closed
/// for any publisher not present.
///
/// Serializable so a durable trust store (e.g. the CLI's
/// `~/.apex/plugins/trust.json`) persists across processes; keys are stored as raw
/// bytes.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    keys: BTreeMap<String, Vec<u8>>,
}

impl TrustStore {
    /// An empty trust store (trusts no publisher).
    pub fn new() -> Self {
        Self::default()
    }

    /// Trust `publisher`'s ed25519 public key for future verifications.
    pub fn trust(&mut self, publisher: impl Into<String>, public_key: Vec<u8>) -> &mut Self {
        self.keys.insert(publisher.into(), public_key);
        self
    }

    /// Whether any publisher is trusted.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The trusted publishers, sorted.
    pub fn publishers(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }

    /// Verify a detached ed25519 `signature` over `message` against the trusted key
    /// for `publisher`. Fail-closed: unknown publisher or invalid signature errors.
    pub fn verify(&self, publisher: &str, message: &[u8], signature: &[u8]) -> Result<()> {
        let key = self
            .keys
            .get(publisher)
            .ok_or_else(|| Error::invalid(format!("untrusted plugin publisher `{publisher}`")))?;
        UnparsedPublicKey::new(&ED25519, key)
            .verify(message, signature)
            .map_err(|_| {
                Error::invalid(format!(
                    "plugin signature verification failed for publisher `{publisher}`"
                ))
            })
    }
}

/// Test/tooling helpers for producing signing keys and signatures. Confined to test
/// builds so no ambient randomness enters the library's core logic; the real signing
/// path lives in the `apex plugin sign` tooling (deferred).
#[cfg(test)]
pub(crate) mod testing {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// Generate an ed25519 keypair, returning `(key_pair, public_key_bytes)`.
    pub(crate) fn generate_keypair() -> (Ed25519KeyPair, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8");
        let public = kp.public_key().as_ref().to_vec();
        (kp, public)
    }

    /// Produce a detached signature over `message`.
    pub(crate) fn sign(kp: &Ed25519KeyPair, message: &[u8]) -> Vec<u8> {
        kp.sign(message).as_ref().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_a_valid_signature() {
        let (kp, public) = testing::generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);

        let message = b"manifest bytes";
        let sig = testing::sign(&kp, message);
        assert!(trust.verify("acme", message, &sig).is_ok());
    }

    #[test]
    fn rejects_tampered_message() {
        let (kp, public) = testing::generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", public);

        let sig = testing::sign(&kp, b"original");
        assert!(trust.verify("acme", b"tampered", &sig).is_err());
    }

    #[test]
    fn rejects_untrusted_publisher() {
        let (kp, _public) = testing::generate_keypair();
        let trust = TrustStore::new(); // trusts nobody
        let sig = testing::sign(&kp, b"m");
        assert!(trust.verify("acme", b"m", &sig).is_err());
    }

    #[test]
    fn verifies_matching_artifact_digest() {
        // sha256("hello world") is a well-known constant.
        let digest = "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_digest(digest, b"hello world").is_ok());
        // Case-insensitive on the hex.
        assert!(
            verify_digest(
                &digest.to_uppercase().replace("SHA256", "sha256"),
                b"hello world"
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_digest_mismatch_and_bad_prefix() {
        let digest = "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_digest(digest, b"tampered").is_err());
        assert!(verify_digest("md5:abcd", b"hello world").is_err());
    }

    #[test]
    fn rejects_signature_from_wrong_key() {
        let (kp, _public) = testing::generate_keypair();
        let (_other, other_public) = testing::generate_keypair();
        let mut trust = TrustStore::new();
        trust.trust("acme", other_public); // trust a different key

        let sig = testing::sign(&kp, b"m");
        assert!(trust.verify("acme", b"m", &sig).is_err());
    }
}
