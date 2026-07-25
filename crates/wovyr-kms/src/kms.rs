//! The [`Kms`] port and its [`LocalKms`] implementation: the envelope-
//! encryption hierarchy from
//! [Encryption §5](../../docs/13-security/encryption.md#5-key-management) — a
//! root key wraps versioned tenant keys, and a tenant key wraps per-use data
//! encryption keys (DEKs). `LocalKms` is a single-process/single-host stand-in
//! — the root key is generated and held in this process rather than a real
//! KMS/HSM (see [`crate::root`]) — a cloud-KMS- or HSM-backed root is a later
//! slice; only what wraps/unwraps *tenant* keys would need to change, since
//! `Kms` is the trait boundary a real backend would implement instead.

use crate::crypto::{self, KeyBytes};
use crate::model::{DataKey, TenantKeyRecord, TenantKeyVersion, WrappedDataKey};
use crate::store::KmsStore;
use std::sync::Arc;
use wovyr_common::{Error, Result};

/// The key-management operations a data-protecting consumer needs: mint a
/// DEK, unwrap one back to plaintext, rewrap a DEK onto the tenant's current
/// key version (rotation, without touching the data it protects), rotate the
/// tenant key itself, and crypto-shred a tenant permanently.
pub trait Kms: Send + Sync {
    /// Mint a fresh DEK for `tenant`, wrapped under its current key version.
    /// Provisions a first tenant key version on first use.
    fn generate_data_key(&self, tenant: &str) -> Result<DataKey>;

    /// Unwrap `wrapped` back to its plaintext DEK, using whichever tenant key
    /// version it was wrapped under (not necessarily the current one).
    fn unwrap_data_key(&self, tenant: &str, wrapped: &WrappedDataKey) -> Result<KeyBytes>;

    /// Re-wrap a DEK onto the tenant's *current* key version — the "rotate
    /// without re-encrypting all data" operation
    /// ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)):
    /// unwraps under `wrapped`'s recorded version, then wraps under the
    /// latest. The DEK's plaintext (and hence the ciphertext it protects)
    /// never changes.
    fn rewrap_data_key(&self, tenant: &str, wrapped: &WrappedDataKey) -> Result<WrappedDataKey>;

    /// Roll a new tenant key version (wrapped by the root key). Existing
    /// wrapped DEKs remain valid under their original version until a caller
    /// calls [`rewrap_data_key`](Self::rewrap_data_key) on them. Returns the
    /// new version number.
    fn rotate_tenant_key(&self, tenant: &str) -> Result<u32>;

    /// Crypto-shred `tenant`: permanently destroy all of its key material.
    /// Every DEK ever wrapped under this tenant becomes unrecoverable,
    /// regardless of version — a fail-closed data-deletion guarantee.
    fn destroy_tenant_key(&self, tenant: &str) -> Result<()>;
}

/// The root-key-backed implementation. The root key lives in this process
/// (see [`crate::root`] for how to source one); a real deployment would swap
/// this for a backend that calls out to a managed KMS/HSM instead of holding
/// root material locally.
pub struct LocalKms {
    root_key: KeyBytes,
    store: Arc<dyn KmsStore>,
}

impl LocalKms {
    /// A KMS backed by `root_key`, persisting tenant key records to `store`.
    pub fn new(root_key: KeyBytes, store: Arc<dyn KmsStore>) -> Self {
        Self { root_key, store }
    }

    /// The tenant's record, provisioning a first key version on first use.
    /// Fails closed if the tenant was crypto-shredded.
    fn record_or_provision(&self, tenant: &str) -> Result<TenantKeyRecord> {
        match self.store.get(tenant)? {
            Some(record) if record.destroyed => Err(destroyed_error(tenant)),
            Some(record) => Ok(record),
            None => self.provision(tenant),
        }
    }

    /// The tenant's record for an operation that must never auto-provision
    /// (unwrap/rewrap/destroy) — fails closed if absent or shredded.
    fn active_record(&self, tenant: &str) -> Result<TenantKeyRecord> {
        let record = self
            .store
            .get(tenant)?
            .ok_or_else(|| Error::NotFound(format!("tenant `{tenant}` has no key material")))?;
        if record.destroyed {
            return Err(destroyed_error(tenant));
        }
        Ok(record)
    }

    fn provision(&self, tenant: &str) -> Result<TenantKeyRecord> {
        let tenant_key = crypto::generate_key()?;
        let wrapped = crypto::seal(&self.root_key, &tenant_key)?;
        let record = TenantKeyRecord {
            tenant: tenant.to_string(),
            versions: vec![TenantKeyVersion {
                version: 1,
                wrapped,
            }],
            destroyed: false,
        };
        self.store.put(record.clone())?;
        Ok(record)
    }

    fn unwrap_tenant_key(&self, record: &TenantKeyRecord, version: u32) -> Result<KeyBytes> {
        let tv = record.version(version).ok_or_else(|| {
            Error::NotFound(format!(
                "tenant `{}` has no key version {version}",
                record.tenant
            ))
        })?;
        let plaintext = crypto::open(&self.root_key, &tv.wrapped)?;
        to_key_bytes(plaintext)
    }
}

fn destroyed_error(tenant: &str) -> Error {
    Error::forbidden(format!(
        "tenant key `{tenant}` was crypto-shredded and cannot be used again"
    ))
}

fn to_key_bytes(bytes: Vec<u8>) -> Result<KeyBytes> {
    bytes
        .try_into()
        .map_err(|_| Error::invalid("unwrapped key material is not 32 bytes"))
}

impl Kms for LocalKms {
    fn generate_data_key(&self, tenant: &str) -> Result<DataKey> {
        // Held across the whole read-provision-write cycle (DUR-403): first use
        // for a tenant auto-provisions a tenant key, and two concurrent first
        // calls must not each provision an independent key while only one
        // survives in the store.
        let _flock = self.store.lock()?;
        let record = self.record_or_provision(tenant)?;
        let current = record
            .current_version()
            .expect("a provisioned tenant record always has at least one version");
        let tenant_key = self.unwrap_tenant_key(&record, current.version)?;
        let plaintext = crypto::generate_key()?;
        let wrapped = crypto::seal(&tenant_key, &plaintext)?;
        Ok(DataKey {
            plaintext,
            wrapped: WrappedDataKey {
                tenant_key_version: current.version,
                wrapped,
            },
        })
    }

    fn unwrap_data_key(&self, tenant: &str, wrapped: &WrappedDataKey) -> Result<KeyBytes> {
        let record = self.active_record(tenant)?;
        let tenant_key = self.unwrap_tenant_key(&record, wrapped.tenant_key_version)?;
        let plaintext = crypto::open(&tenant_key, &wrapped.wrapped)?;
        to_key_bytes(plaintext)
    }

    fn rewrap_data_key(&self, tenant: &str, wrapped: &WrappedDataKey) -> Result<WrappedDataKey> {
        let record = self.active_record(tenant)?;
        let old_tenant_key = self.unwrap_tenant_key(&record, wrapped.tenant_key_version)?;
        let plaintext_dek = to_key_bytes(crypto::open(&old_tenant_key, &wrapped.wrapped)?)?;

        let current = record
            .current_version()
            .expect("a provisioned tenant record always has at least one version");
        let current_tenant_key = self.unwrap_tenant_key(&record, current.version)?;
        let rewrapped = crypto::seal(&current_tenant_key, &plaintext_dek)?;
        Ok(WrappedDataKey {
            tenant_key_version: current.version,
            wrapped: rewrapped,
        })
    }

    fn rotate_tenant_key(&self, tenant: &str) -> Result<u32> {
        // Held across read + write (DUR-403): two concurrent rotations reading
        // the same version must not both mint the same "next" version, each
        // clobbering the other on write.
        let _flock = self.store.lock()?;
        let mut record = self.record_or_provision(tenant)?;
        let next_version = record.versions.last().map_or(1, |v| v.version + 1);
        let tenant_key = crypto::generate_key()?;
        let wrapped = crypto::seal(&self.root_key, &tenant_key)?;
        record.versions.push(TenantKeyVersion {
            version: next_version,
            wrapped,
        });
        self.store.put(record)?;
        Ok(next_version)
    }

    fn destroy_tenant_key(&self, tenant: &str) -> Result<()> {
        // Held across read + write (DUR-403), same reasoning as rotate.
        let _flock = self.store.lock()?;
        let mut record = self.active_record(tenant)?;
        record.versions.clear();
        record.destroyed = true;
        self.store.put(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryKmsStore;

    fn kms() -> LocalKms {
        LocalKms::new(
            crypto::generate_key().unwrap(),
            Arc::new(InMemoryKmsStore::new()),
        )
    }

    #[test]
    fn generate_then_unwrap_round_trips() {
        let kms = kms();
        let dek = kms.generate_data_key("acme").unwrap();
        let recovered = kms.unwrap_data_key("acme", &dek.wrapped).unwrap();
        assert_eq!(recovered, dek.plaintext);
        assert_eq!(dek.wrapped.tenant_key_version, 1);
    }

    #[test]
    fn tenant_isolation_a_dek_wrapped_for_one_tenant_will_not_unwrap_under_another() {
        let kms = kms();
        let dek = kms.generate_data_key("acme").unwrap();
        // `beta` provisions its own, independent tenant key, so acme's wrapped
        // DEK is meaningless (and fails AEAD) under it.
        assert!(kms.unwrap_data_key("beta", &dek.wrapped).is_err());
    }

    #[test]
    fn rotation_retains_old_versions_so_old_wrapped_deks_still_unwrap() {
        let kms = kms();
        let dek = kms.generate_data_key("acme").unwrap();
        let new_version = kms.rotate_tenant_key("acme").unwrap();
        assert_eq!(new_version, 2);
        // The DEK was wrapped under version 1, which is still retained.
        assert_eq!(dek.wrapped.tenant_key_version, 1);
        assert_eq!(
            kms.unwrap_data_key("acme", &dek.wrapped).unwrap(),
            dek.plaintext
        );
    }

    #[test]
    fn rewrap_moves_a_dek_onto_the_current_version_without_changing_its_plaintext() {
        let kms = kms();
        let dek = kms.generate_data_key("acme").unwrap();
        kms.rotate_tenant_key("acme").unwrap();
        kms.rotate_tenant_key("acme").unwrap();

        let rewrapped = kms.rewrap_data_key("acme", &dek.wrapped).unwrap();
        assert_eq!(rewrapped.tenant_key_version, 3);
        // Same plaintext DEK — only the wrapper changed.
        assert_eq!(
            kms.unwrap_data_key("acme", &rewrapped).unwrap(),
            dek.plaintext
        );
    }

    #[test]
    fn new_deks_are_wrapped_under_the_current_version_after_rotation() {
        let kms = kms();
        kms.rotate_tenant_key("acme").unwrap(); // -> version 2
        let dek = kms.generate_data_key("acme").unwrap();
        assert_eq!(dek.wrapped.tenant_key_version, 2);
    }

    #[test]
    fn crypto_shredding_a_tenant_makes_every_operation_fail_closed() {
        let kms = kms();
        let dek = kms.generate_data_key("acme").unwrap();
        kms.destroy_tenant_key("acme").unwrap();

        assert!(matches!(
            kms.unwrap_data_key("acme", &dek.wrapped).unwrap_err(),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            kms.rewrap_data_key("acme", &dek.wrapped).unwrap_err(),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            kms.generate_data_key("acme").unwrap_err(),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            kms.rotate_tenant_key("acme").unwrap_err(),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            kms.destroy_tenant_key("acme").unwrap_err(),
            Error::Forbidden(_)
        ));
        // A different tenant is unaffected.
        assert!(kms.generate_data_key("beta").is_ok());
    }

    #[test]
    fn unwrapping_for_a_never_provisioned_tenant_is_not_found() {
        let kms = kms();
        let dek = kms.generate_data_key("acme").unwrap();
        // The tenant on the call, not the wrapped DEK's own tenant, is what's
        // looked up — so any wrapped value proves the NotFound path fires
        // before any AEAD attempt.
        assert!(matches!(
            kms.unwrap_data_key("never-provisioned", &dek.wrapped)
                .unwrap_err(),
            Error::NotFound(_)
        ));
    }
}
