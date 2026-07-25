//! AES-256-GCM sealing primitives — the mechanical "wrap"/"unwrap" step
//! underneath every level of the key hierarchy in [`crate::Kms`]. This module
//! only knows how to seal/open bytes under one flat 32-byte key; the
//! hierarchy itself (root wraps tenant, tenant wraps DEK) lives in `kms.rs`.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use wovyr_common::{Error, Result};

/// A raw AES-256 key: root, tenant, or data-encryption key material. Plaintext
/// key bytes are never persisted — only the [`Sealed`] (wrapped) form is.
pub type KeyBytes = [u8; 32];

/// A nonce plus AEAD ciphertext (the ciphertext includes the authentication
/// tag). Hex-encoded on the wire so a `KmsStore` backend can serialize it as
/// plain JSON.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sealed {
    #[serde(with = "hex::serde")]
    pub nonce: Vec<u8>,
    #[serde(with = "hex::serde")]
    pub ciphertext: Vec<u8>,
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| Error::Runtime("failed to generate random bytes".into()))?;
    Ok(buf)
}

/// A fresh random AES-256 key — used to mint tenant keys and DEKs.
pub fn generate_key() -> Result<KeyBytes> {
    let bytes = random_bytes(32)?;
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Seal `plaintext` under `key` with a fresh random nonce (no associated data).
pub fn seal(key: &KeyBytes, plaintext: &[u8]) -> Result<Sealed> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| Error::Runtime("invalid AES-256 key".into()))?;
    let less_safe = LessSafeKey::new(unbound);
    let nonce_bytes = random_bytes(NONCE_LEN)?;
    let nonce = Nonce::try_assume_unique_for_key(&nonce_bytes)
        .map_err(|_| Error::Runtime("invalid nonce".into()))?;
    let mut in_out = plaintext.to_vec();
    less_safe
        .seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| Error::Runtime("AEAD seal failed".into()))?;
    Ok(Sealed {
        nonce: nonce_bytes,
        ciphertext: in_out,
    })
}

/// Open a [`Sealed`] value under `key`. Fails closed on any tamper,
/// corruption, or wrong-key attempt — AEAD deliberately does not distinguish
/// the reason, so callers can't use the error to learn anything about the key.
pub fn open(key: &KeyBytes, sealed: &Sealed) -> Result<Vec<u8>> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| Error::Runtime("invalid AES-256 key".into()))?;
    let less_safe = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(&sealed.nonce)
        .map_err(|_| Error::invalid("malformed nonce"))?;
    let mut in_out = sealed.ciphertext.clone();
    let plaintext_len = less_safe
        .open_in_place(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| Error::invalid("decryption failed (tampered ciphertext or wrong key)"))?
        .len();
    in_out.truncate(plaintext_len);
    Ok(in_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_then_open_round_trips() {
        let key = generate_key().unwrap();
        let sealed = seal(&key, b"hello kms").unwrap();
        assert_eq!(open(&key, &sealed).unwrap(), b"hello kms");
    }

    #[test]
    fn wrong_key_fails_closed() {
        let key = generate_key().unwrap();
        let other = generate_key().unwrap();
        let sealed = seal(&key, b"hello").unwrap();
        assert!(open(&other, &sealed).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_closed() {
        let key = generate_key().unwrap();
        let mut sealed = seal(&key, b"hello").unwrap();
        sealed.ciphertext[0] ^= 0xFF;
        assert!(open(&key, &sealed).is_err());
    }

    #[test]
    fn tampered_nonce_fails_closed() {
        let key = generate_key().unwrap();
        let mut sealed = seal(&key, b"hello").unwrap();
        sealed.nonce[0] ^= 0xFF;
        assert!(open(&key, &sealed).is_err());
    }

    #[test]
    fn each_seal_uses_a_fresh_nonce() {
        let key = generate_key().unwrap();
        let a = seal(&key, b"same plaintext").unwrap();
        let b = seal(&key, b"same plaintext").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn generated_keys_are_not_all_zero_and_differ() {
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b);
    }
}
