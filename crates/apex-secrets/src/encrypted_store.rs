//! An at-rest-**encrypting** [`SecretStore`]: seals a secret's value (and its
//! retained previous value) through [`apex_kms`] before it ever reaches
//! disk — closing the gap [`FileSecretStore`](crate::FileSecretStore) leaves
//! open (its `secrets.json` holds plaintext, by that module's own admission).
//! Uses the secret's own `namespace` as the KMS tenant, so encryption
//! follows the exact isolation boundary secrets already have — a tenant that
//! can never read another tenant's secret via the [`Vault`](crate::Vault)
//! also can't have its values recovered by a `Kms` operating under a
//! different tenant.
//!
//! Persisted to a distinct `secrets.enc.json` (not `secrets.json`) so an
//! encrypted and a plaintext store can never be pointed at the same
//! directory and silently misparse each other's file.

use crate::reference::SecretRef;
use crate::secret::{Secret, SecretMetadata};
use crate::store::SecretStore;
use apex_common::{Error, Result};
use apex_kms::{Kms, SealedData};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

type EncryptedRecordMap = BTreeMap<(String, String), EncryptedRecord>;

/// The on-disk shape: `value`/`previous` are sealed via [`Kms`], never
/// plaintext at rest.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct EncryptedRecord {
    namespace: String,
    name: String,
    sealed_value: SealedData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed_previous: Option<SealedData>,
    version: u32,
}

/// Filesystem store whose persisted `secrets.enc.json` never holds a
/// plaintext value.
///
/// Like [`FileSecretStore`](crate::FileSecretStore), every mutation re-reads
/// `secrets.enc.json` from disk under a cross-process advisory lock (RM-GA-P2
/// DUR-403) rather than caching the catalog in memory — the CLI and server share
/// this directory by design.
pub struct EncryptedFileSecretStore {
    dir: PathBuf,
    path: PathBuf,
    kms: Arc<dyn Kms>,
}

impl EncryptedFileSecretStore {
    /// Open (or create) the store under `dir` (`dir/secrets.enc.json` is read
    /// lazily, fresh, on every operation), sealing/unsealing every value through
    /// `kms`.
    pub fn new(dir: impl Into<PathBuf>, kms: Arc<dyn Kms>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::config(format!("create secrets dir: {e}")))?;
        let path = dir.join("secrets.enc.json");
        Ok(Self { dir, path, kms })
    }

    fn load(&self) -> Result<EncryptedRecordMap> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|e| Error::config(format!("read secrets.enc.json: {e}")))?;
        let list: Vec<EncryptedRecord> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("parse secrets.enc.json: {e}")))?;
        Ok(list
            .into_iter()
            .map(|r| ((r.namespace.clone(), r.name.clone()), r))
            .collect())
    }

    fn persist(&self, map: &EncryptedRecordMap) -> Result<()> {
        let list: Vec<&EncryptedRecord> = map.values().collect();
        let bytes = serde_json::to_vec_pretty(&list)
            .map_err(|e| Error::config(format!("encode secrets.enc.json: {e}")))?;
        apex_common::fs::atomic_write(&self.path, bytes)
            .map_err(|e| Error::config(format!("write secrets.enc.json: {e}")))
    }

    /// Cross-process lock guarding a read-modify-write cycle (DUR-403).
    fn lock(&self) -> Result<apex_common::fs::FileLock> {
        apex_common::fs::FileLock::acquire(&self.dir)
            .map_err(|e| Error::config(format!("lock secrets store: {e}")))
    }

    fn seal(&self, tenant: &str, plaintext: &str) -> Result<SealedData> {
        apex_kms::envelope::seal(self.kms.as_ref(), tenant, plaintext.as_bytes())
    }

    fn unseal(&self, tenant: &str, sealed: &SealedData) -> Result<String> {
        let bytes = apex_kms::envelope::open(self.kms.as_ref(), tenant, sealed)?;
        String::from_utf8(bytes)
            .map_err(|_| Error::invalid("decrypted secret value is not valid UTF-8"))
    }

    fn encrypt(&self, secret: &Secret) -> Result<EncryptedRecord> {
        let sealed_value = self.seal(&secret.namespace, secret.raw_value())?;
        let sealed_previous = secret
            .raw_previous()
            .map(|p| self.seal(&secret.namespace, p))
            .transpose()?;
        Ok(EncryptedRecord {
            namespace: secret.namespace.clone(),
            name: secret.name.clone(),
            sealed_value,
            sealed_previous,
            version: secret.version,
        })
    }

    fn decrypt(&self, record: &EncryptedRecord) -> Result<Secret> {
        let value = self.unseal(&record.namespace, &record.sealed_value)?;
        let previous = record
            .sealed_previous
            .as_ref()
            .map(|s| self.unseal(&record.namespace, s))
            .transpose()?;
        Ok(Secret::from_parts(
            record.namespace.clone(),
            record.name.clone(),
            value,
            previous,
            record.version,
        ))
    }
}

impl SecretStore for EncryptedFileSecretStore {
    fn put(&self, secret: Secret) -> Result<()> {
        let record = self.encrypt(&secret)?;
        let _flock = self.lock()?;
        let mut map = self.load()?;
        map.insert((record.namespace.clone(), record.name.clone()), record);
        self.persist(&map)
    }

    fn get(&self, namespace: &str, name: &str) -> Result<Option<Secret>> {
        let record = self
            .load()?
            .get(&(namespace.to_string(), name.to_string()))
            .cloned();
        record.map(|r| self.decrypt(&r)).transpose()
    }

    fn delete(&self, namespace: &str, name: &str) -> Result<bool> {
        let _flock = self.lock()?;
        let mut map = self.load()?;
        let existed = map
            .remove(&(namespace.to_string(), name.to_string()))
            .is_some();
        if existed {
            self.persist(&map)?;
        }
        Ok(existed)
    }

    fn list(&self, namespace: &str) -> Result<Vec<SecretMetadata>> {
        // Metadata is value-free, so listing never has to unseal anything.
        Ok(self
            .load()?
            .values()
            .filter(|r| r.namespace == namespace)
            .map(|r| SecretMetadata {
                namespace: r.namespace.clone(),
                name: r.name.clone(),
                version: r.version,
                reference: SecretRef {
                    namespace: r.namespace.clone(),
                    name: r.name.clone(),
                }
                .to_string(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_kms::{InMemoryKmsStore, LocalKms, generate_key};

    fn kms() -> Arc<dyn Kms> {
        Arc::new(LocalKms::new(
            generate_key().unwrap(),
            Arc::new(InMemoryKmsStore::new()),
        ))
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "apex_secrets_enc_test_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_value_and_metadata() {
        let dir = temp_dir("roundtrip");
        let store = EncryptedFileSecretStore::new(&dir, kms()).unwrap();

        store.put(Secret::new("acme", "token", "v1")).unwrap();
        let fetched = store.get("acme", "token").unwrap().unwrap();
        assert_eq!(fetched.value().expose(), "v1");

        let meta = store.list("acme").unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].version, 1);
        assert_eq!(meta[0].reference, "secret://acme/token");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_keeps_previous_value_recoverable() {
        let dir = temp_dir("rotation");
        let store = EncryptedFileSecretStore::new(&dir, kms()).unwrap();

        let mut secret = Secret::new("acme", "token", "v1");
        store.put(secret.clone()).unwrap();
        secret.rotate("v2");
        store.put(secret).unwrap();

        let fetched = store.get("acme", "token").unwrap().unwrap();
        assert_eq!(fetched.value().expose(), "v2");
        assert_eq!(fetched.previous_value().unwrap().expose(), "v1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_disk_bytes_never_contain_the_plaintext_value() {
        let dir = temp_dir("plaintext-check");
        let store = EncryptedFileSecretStore::new(&dir, kms()).unwrap();
        store
            .put(Secret::new("acme", "token", "s3cr3t-plaintext-marker"))
            .unwrap();

        let bytes = std::fs::read(dir.join("secrets.enc.json")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("s3cr3t-plaintext-marker"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persists_across_reopen_with_the_same_kms() {
        let dir = temp_dir("reopen");
        let kms = kms();
        {
            let store = EncryptedFileSecretStore::new(&dir, kms.clone()).unwrap();
            store.put(Secret::new("acme", "token", "v1")).unwrap();
        }
        let reopened = EncryptedFileSecretStore::new(&dir, kms).unwrap();
        assert_eq!(
            reopened
                .get("acme", "token")
                .unwrap()
                .unwrap()
                .value()
                .expose(),
            "v1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cannot_decrypt_with_a_different_kms_instance() {
        let dir = temp_dir("wrong-kms");
        {
            let store = EncryptedFileSecretStore::new(&dir, kms()).unwrap();
            store.put(Secret::new("acme", "token", "v1")).unwrap();
        }
        // A different KMS instance (independent root + tenant keys) cannot
        // recover the value — fails closed rather than returning garbage.
        let reopened = EncryptedFileSecretStore::new(&dir, kms()).unwrap();
        assert!(reopened.get("acme", "token").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_is_tenant_scoped_and_never_touches_sealed_values() {
        let dir = temp_dir("list-scope");
        let store = EncryptedFileSecretStore::new(&dir, kms()).unwrap();
        store.put(Secret::new("acme", "a", "1")).unwrap();
        store.put(Secret::new("acme", "b", "2")).unwrap();
        store.put(Secret::new("beta", "a", "3")).unwrap();

        assert_eq!(store.list("acme").unwrap().len(), 2);
        assert_eq!(store.list("beta").unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_and_persists() {
        let dir = temp_dir("delete");
        let store = EncryptedFileSecretStore::new(&dir, kms()).unwrap();
        store.put(Secret::new("acme", "token", "v1")).unwrap();

        assert!(store.delete("acme", "token").unwrap());
        assert!(!store.delete("acme", "token").unwrap());
        assert!(store.get("acme", "token").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
