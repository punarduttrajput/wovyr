//! [`seal`]/[`open`] — the application-layer envelope-encryption helper from
//! [Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption):
//! protect one payload (a memory record's content, a config field) end to end
//! over a [`Kms`], without the caller ever touching key material directly.

use crate::crypto::{self, Sealed};
use crate::kms::Kms;
use crate::model::WrappedDataKey;
use serde::{Deserialize, Serialize};
use wovyr_common::Result;

/// A payload sealed under a fresh, per-call DEK. Safe to persist as-is —
/// recovering the plaintext requires the KMS to unwrap `wrapped_key` first.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SealedData {
    pub wrapped_key: WrappedDataKey,
    pub sealed: Sealed,
}

/// Seal `plaintext` for `tenant`: mints a fresh DEK, encrypts with it, and
/// returns only the wrapped DEK alongside the ciphertext — the plaintext DEK
/// never leaves this call.
pub fn seal(kms: &dyn Kms, tenant: &str, plaintext: &[u8]) -> Result<SealedData> {
    let dek = kms.generate_data_key(tenant)?;
    let sealed = crypto::seal(&dek.plaintext, plaintext)?;
    Ok(SealedData {
        wrapped_key: dek.wrapped,
        sealed,
    })
}

/// Recover the plaintext from a [`SealedData`], for `tenant`.
pub fn open(kms: &dyn Kms, tenant: &str, sealed: &SealedData) -> Result<Vec<u8>> {
    let dek = kms.unwrap_data_key(tenant, &sealed.wrapped_key)?;
    crypto::open(&dek, &sealed.sealed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kms::LocalKms;
    use crate::store::InMemoryKmsStore;
    use std::sync::Arc;

    fn kms() -> LocalKms {
        LocalKms::new(
            crypto::generate_key().unwrap(),
            Arc::new(InMemoryKmsStore::new()),
        )
    }

    #[test]
    fn seal_then_open_round_trips() {
        let kms = kms();
        let sealed = seal(&kms, "acme", b"sensitive memory content").unwrap();
        assert_eq!(
            open(&kms, "acme", &sealed).unwrap(),
            b"sensitive memory content"
        );
    }

    #[test]
    fn opening_under_the_wrong_tenant_fails_closed() {
        let kms = kms();
        let sealed = seal(&kms, "acme", b"top secret").unwrap();
        assert!(open(&kms, "beta", &sealed).is_err());
    }

    #[test]
    fn each_call_mints_an_independent_dek_even_for_identical_plaintext() {
        let kms = kms();
        let a = seal(&kms, "acme", b"same content").unwrap();
        let b = seal(&kms, "acme", b"same content").unwrap();
        assert_ne!(
            a.wrapped_key.wrapped.ciphertext,
            b.wrapped_key.wrapped.ciphertext
        );
        assert_ne!(a.sealed.ciphertext, b.sealed.ciphertext);
    }
}
