//! Durable storage for tenant key records — the KMS's backing catalog. Only
//! ever holds *wrapped* key material (sealed by the root key); an entry here
//! is inert without the root key that wrapped it, unlike a secret store
//! (which holds plaintext) — see
//! [`apex_secrets::FileSecretStore`](../../apex-secrets/src/store.rs) for the
//! analogous plaintext case.

use crate::model::TenantKeyRecord;
use apex_common::{Error, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A durable catalog of tenant key records, keyed by tenant id.
pub trait KmsStore: Send + Sync {
    /// Fetch a tenant's key record, if one has ever been provisioned.
    fn get(&self, tenant: &str) -> Result<Option<TenantKeyRecord>>;
    /// Create or replace a tenant's key record.
    fn put(&self, record: TenantKeyRecord) -> Result<()>;

    /// Acquire a cross-process lock spanning a `get` + mutate + `put` cycle
    /// (RM-GA-P2 DUR-403). `LocalKms`'s `generate_data_key` (first
    /// provisioning), `rotate_tenant_key`, and `destroy_tenant_key` all read
    /// the current record, compute a new one, and write it back — without a
    /// lock spanning that whole sequence, two concurrent callers (this
    /// process and another sharing the same `~/.apex` directory) could each
    /// act on the same stale read and one's update would silently clobber
    /// the other's (e.g. two racing rotations both minting "version 2",
    /// clobbering rather than stacking). `None` for backends with no
    /// cross-process concern (the default, and what `InMemoryKmsStore` uses).
    fn lock(&self) -> Result<Option<apex_common::fs::FileLock>> {
        Ok(None)
    }
}

/// In-process store (tests / single process).
#[derive(Default)]
pub struct InMemoryKmsStore {
    inner: Mutex<BTreeMap<String, TenantKeyRecord>>,
}

impl InMemoryKmsStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, TenantKeyRecord>> {
        self.inner.lock().expect("kms store mutex poisoned")
    }
}

impl KmsStore for InMemoryKmsStore {
    fn get(&self, tenant: &str) -> Result<Option<TenantKeyRecord>> {
        Ok(self.lock().get(tenant).cloned())
    }

    fn put(&self, record: TenantKeyRecord) -> Result<()> {
        self.lock().insert(record.tenant.clone(), record);
        Ok(())
    }
}

/// Filesystem store: the whole catalog in one `kms.json` under a directory.
///
/// Every operation re-reads `kms.json` from disk rather than caching the catalog
/// in memory — the CLI and server share this directory by design, so a persistent
/// cache would let one process's write silently clobber a concurrent writer's
/// change. `lock()` (RM-GA-P2 DUR-403) exposes the directory's cross-process lock
/// so `LocalKms`'s multi-step operations (read the current record, mutate,
/// write it back) can hold it across the whole sequence, not just the `put`.
pub struct FileKmsStore {
    dir: PathBuf,
    path: PathBuf,
}

impl FileKmsStore {
    /// Open (or create) the store under `dir` (`dir/kms.json` is read lazily,
    /// fresh, on every operation — nothing is cached at construction time).
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| Error::config(format!("create kms dir: {e}")))?;
        let path = dir.join("kms.json");
        Ok(Self { dir, path })
    }

    fn load(&self) -> Result<BTreeMap<String, TenantKeyRecord>> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let bytes =
            std::fs::read(&self.path).map_err(|e| Error::config(format!("read kms.json: {e}")))?;
        let list: Vec<TenantKeyRecord> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("parse kms.json: {e}")))?;
        Ok(list.into_iter().map(|r| (r.tenant.clone(), r)).collect())
    }

    fn persist(&self, map: &BTreeMap<String, TenantKeyRecord>) -> Result<()> {
        let list: Vec<&TenantKeyRecord> = map.values().collect();
        let bytes = serde_json::to_vec_pretty(&list)
            .map_err(|e| Error::config(format!("encode kms.json: {e}")))?;
        apex_common::fs::atomic_write(&self.path, bytes)
            .map_err(|e| Error::config(format!("write kms.json: {e}")))?;
        restrict_permissions(&self.path)
    }
}

/// Restrict `kms.json` to owner-only access, mirroring
/// [`crate::root::from_file`]'s handling of `root.key` — entries here are
/// inert ciphertext without the root key, but this closes the defense-in-depth
/// gap where the wrapped tenant-key catalog got whatever the process's
/// default file permissions/ACL were.
fn restrict_permissions(path: &std::path::Path) -> Result<()> {
    apex_common::fs::restrict_to_owner(path)
        .map_err(|e| Error::config(format!("restrict kms.json permissions: {e}")))
}

impl KmsStore for FileKmsStore {
    fn get(&self, tenant: &str) -> Result<Option<TenantKeyRecord>> {
        Ok(self.load()?.get(tenant).cloned())
    }

    fn put(&self, record: TenantKeyRecord) -> Result<()> {
        let mut map = self.load()?;
        map.insert(record.tenant.clone(), record);
        self.persist(&map)
    }

    fn lock(&self) -> Result<Option<apex_common::fs::FileLock>> {
        Ok(Some(
            apex_common::fs::FileLock::acquire(&self.dir)
                .map_err(|e| Error::config(format!("lock kms store: {e}")))?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;

    fn sample(tenant: &str) -> TenantKeyRecord {
        TenantKeyRecord {
            tenant: tenant.to_string(),
            versions: vec![crate::model::TenantKeyVersion {
                version: 1,
                wrapped: crypto::seal(&crypto::generate_key().unwrap(), b"tenant-key").unwrap(),
            }],
            destroyed: false,
        }
    }

    fn roundtrip(store: &dyn KmsStore) {
        store.put(sample("acme")).unwrap();
        store.put(sample("beta")).unwrap();

        assert_eq!(store.get("acme").unwrap().unwrap().tenant, "acme");
        assert!(store.get("ghost").unwrap().is_none());
    }

    #[test]
    fn in_memory_round_trips() {
        roundtrip(&InMemoryKmsStore::new());
    }

    #[test]
    fn file_store_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("apex_kms_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = FileKmsStore::new(&dir).unwrap();
            roundtrip(&store);
        }
        let reopened = FileKmsStore::new(&dir).unwrap();
        assert_eq!(reopened.get("acme").unwrap().unwrap().tenant, "acme");
        assert_eq!(reopened.get("beta").unwrap().unwrap().tenant, "beta");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_store_restricts_kms_json_to_owner_only() {
        let dir = std::env::temp_dir().join(format!("apex_kms_perm_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileKmsStore::new(&dir).unwrap();
        store.put(sample("acme")).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("kms.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        #[cfg(windows)]
        {
            let user = std::env::var("USERNAME").unwrap();
            let output = std::process::Command::new("icacls")
                .arg(dir.join("kms.json"))
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.contains(&user), "icacls output: {text}");
            assert!(!text.contains("(I)"), "icacls output: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
