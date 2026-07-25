//! The key hierarchy's data model
//! ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)): a root
//! key wraps versioned tenant keys, and a tenant key wraps per-use data
//! encryption keys (DEKs). Only wrapped forms are ever persisted or handed
//! back to a caller — plaintext key material lives only for the duration of
//! one seal/unwrap call.

use crate::crypto::{KeyBytes, Sealed};
use serde::{Deserialize, Serialize};

/// One version of a tenant's key, wrapped by the root key. Versions are
/// retained (never deleted on rotation) so a DEK wrapped under an older
/// version can still be unwrapped —
/// [`rewrap_data_key`](crate::Kms::rewrap_data_key) is how a caller migrates a
/// DEK onto the current version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TenantKeyVersion {
    pub version: u32,
    pub wrapped: Sealed,
}

/// A tenant's full key history, keyed by tenant id in the
/// [`KmsStore`](crate::KmsStore).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TenantKeyRecord {
    pub tenant: String,
    pub versions: Vec<TenantKeyVersion>,
    /// Set by [`Kms::destroy_tenant_key`](crate::Kms::destroy_tenant_key) —
    /// crypto-shredding: once set, `versions` is cleared and no DEK ever
    /// wrapped under this tenant can be unwrapped again, permanently.
    #[serde(default)]
    pub destroyed: bool,
}

impl TenantKeyRecord {
    /// The most recently rotated-in version (the one new DEKs get wrapped
    /// under).
    pub fn current_version(&self) -> Option<&TenantKeyVersion> {
        self.versions.last()
    }

    /// A specific historical version, by number.
    pub fn version(&self, version: u32) -> Option<&TenantKeyVersion> {
        self.versions.iter().find(|v| v.version == version)
    }
}

/// A data-encryption key: the caller gets the plaintext once (to seal/open
/// its own payload) plus the wrapped form to persist — the plaintext is never
/// stored.
#[derive(Clone, Debug)]
pub struct DataKey {
    pub plaintext: KeyBytes,
    pub wrapped: WrappedDataKey,
}

/// A DEK wrapped by a specific tenant key version — durable, safe to store
/// alongside the data it protects.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WrappedDataKey {
    pub tenant_key_version: u32,
    pub wrapped: Sealed,
}
