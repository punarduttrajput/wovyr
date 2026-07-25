//! Durable storage for secrets — the vault's backing catalog.
//!
//! [`SecretStore`] is the port; [`InMemorySecretStore`] (tests/single-process) and
//! [`FileSecretStore`] (one plaintext `secrets.json`) are the backends here. A
//! production deployment would back this with a managed vault (cloud secrets
//! manager / HashiCorp Vault)
//! ([secret-management §3](../../docs/13-security/secret-management.md#3-secret-vault))
//! — or, short of that, swap in
//! [`EncryptedFileSecretStore`](crate::EncryptedFileSecretStore) (in
//! `encrypted_store.rs`), which seals values through `wovyr-kms` before they
//! reach disk. At-rest encryption remains the *store's* responsibility (this
//! trait is encryption-agnostic either way).

use crate::secret::{Secret, SecretMetadata};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use wovyr_common::{Error, Result};

type SecretMap = BTreeMap<(String, String), Secret>;

/// A durable catalog of secrets, keyed by `(namespace, name)`.
pub trait SecretStore: Send + Sync {
    /// Create or replace a secret.
    fn put(&self, secret: Secret) -> Result<()>;
    /// Fetch a secret (value included) by namespace + name.
    fn get(&self, namespace: &str, name: &str) -> Result<Option<Secret>>;
    /// Remove a secret; returns whether it existed.
    fn delete(&self, namespace: &str, name: &str) -> Result<bool>;
    /// List the value-free metadata of every secret in `namespace`, sorted by name.
    fn list(&self, namespace: &str) -> Result<Vec<SecretMetadata>>;
}

/// In-process store (tests / single process).
#[derive(Default)]
pub struct InMemorySecretStore {
    inner: Mutex<BTreeMap<(String, String), Secret>>,
}

impl InMemorySecretStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), Secret>> {
        self.inner.lock().expect("secret store mutex poisoned")
    }
}

impl SecretStore for InMemorySecretStore {
    fn put(&self, secret: Secret) -> Result<()> {
        self.lock()
            .insert((secret.namespace.clone(), secret.name.clone()), secret);
        Ok(())
    }

    fn get(&self, namespace: &str, name: &str) -> Result<Option<Secret>> {
        Ok(self
            .lock()
            .get(&(namespace.to_string(), name.to_string()))
            .cloned())
    }

    fn delete(&self, namespace: &str, name: &str) -> Result<bool> {
        Ok(self
            .lock()
            .remove(&(namespace.to_string(), name.to_string()))
            .is_some())
    }

    fn list(&self, namespace: &str) -> Result<Vec<SecretMetadata>> {
        Ok(self
            .lock()
            .values()
            .filter(|s| s.namespace == namespace)
            .map(Secret::metadata)
            .collect())
    }
}

/// Filesystem store: the whole catalog in one `secrets.json` under a directory.
///
/// Every mutation re-reads `secrets.json` from disk under a cross-process advisory
/// lock (RM-GA-P2 DUR-403) rather than caching the catalog in memory across calls —
/// the CLI and server share this directory by design, so a persistent in-memory
/// cache would let one process's write silently clobber a concurrent writer's
/// change (a lost update) even though `atomic_write` alone keeps each individual
/// write torn-write-safe.
pub struct FileSecretStore {
    dir: PathBuf,
    path: PathBuf,
}

impl FileSecretStore {
    /// Open (or create) the store under `dir` (`dir/secrets.json` is read lazily,
    /// fresh, on every operation — nothing is cached at construction time).
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::config(format!("create secrets dir: {e}")))?;
        let path = dir.join("secrets.json");
        Ok(Self { dir, path })
    }

    fn load(&self) -> Result<SecretMap> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|e| Error::config(format!("read secrets.json: {e}")))?;
        let list: Vec<Secret> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("parse secrets.json: {e}")))?;
        Ok(list
            .into_iter()
            .map(|s| ((s.namespace.clone(), s.name.clone()), s))
            .collect())
    }

    fn persist(&self, map: &SecretMap) -> Result<()> {
        let list: Vec<&Secret> = map.values().collect();
        let bytes = serde_json::to_vec_pretty(&list)
            .map_err(|e| Error::config(format!("encode secrets.json: {e}")))?;
        wovyr_common::fs::atomic_write(&self.path, bytes)
            .map_err(|e| Error::config(format!("write secrets.json: {e}")))
    }

    /// Cross-process lock guarding a read-modify-write cycle (DUR-403).
    fn lock(&self) -> Result<wovyr_common::fs::FileLock> {
        wovyr_common::fs::FileLock::acquire(&self.dir)
            .map_err(|e| Error::config(format!("lock secrets store: {e}")))
    }
}

impl SecretStore for FileSecretStore {
    fn put(&self, secret: Secret) -> Result<()> {
        let _flock = self.lock()?;
        let mut map = self.load()?;
        map.insert((secret.namespace.clone(), secret.name.clone()), secret);
        self.persist(&map)
    }

    fn get(&self, namespace: &str, name: &str) -> Result<Option<Secret>> {
        Ok(self
            .load()?
            .get(&(namespace.to_string(), name.to_string()))
            .cloned())
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
        Ok(self
            .load()?
            .values()
            .filter(|s| s.namespace == namespace)
            .map(Secret::metadata)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(store: &dyn SecretStore) {
        store.put(Secret::new("acme", "token", "v1")).unwrap();
        store.put(Secret::new("acme", "other", "x")).unwrap();
        store.put(Secret::new("beta", "token", "v1")).unwrap();

        assert_eq!(
            store
                .get("acme", "token")
                .unwrap()
                .unwrap()
                .value()
                .expose(),
            "v1"
        );
        // List is tenant-scoped.
        let acme = store.list("acme").unwrap();
        assert_eq!(acme.len(), 2);
        assert!(acme.iter().all(|m| m.namespace == "acme"));
        assert_eq!(store.list("beta").unwrap().len(), 1);

        assert!(store.delete("acme", "token").unwrap());
        assert!(!store.delete("acme", "token").unwrap());
        assert!(store.get("acme", "token").unwrap().is_none());
    }

    #[test]
    fn in_memory_round_trips() {
        roundtrip(&InMemorySecretStore::new());
    }

    #[test]
    fn file_store_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("wovyr_secrets_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = FileSecretStore::new(&dir).unwrap();
            roundtrip(&store);
            store.put(Secret::new("acme", "keep", "v1")).unwrap();
        }
        // Reopen: persisted state survives.
        let reopened = FileSecretStore::new(&dir).unwrap();
        assert_eq!(
            reopened
                .get("acme", "keep")
                .unwrap()
                .unwrap()
                .value()
                .expose(),
            "v1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
