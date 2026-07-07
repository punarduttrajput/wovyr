//! An at-rest-**encrypting** [`WebhookStore`]: seals a subscription's `secret`
//! (the HMAC signing key deliveries are verified against) through
//! [`apex_kms`] before it ever reaches disk — closing the gap
//! [`FileWebhookStore`](crate::FileWebhookStore) leaves open (its
//! `webhooks.json` holds `secret` in plaintext, by that module's own
//! admission). `url`/`events`/`active` stay plaintext — they're not secrets,
//! and the id is derived from them, so sealing them would just cost every
//! lookup a KMS round trip for no confidentiality benefit.
//!
//! Uses the subscription's own `tenant` as the KMS tenant, matching
//! [Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption)'s
//! pattern in [`apex_secrets::EncryptedFileSecretStore`](../../apex-secrets/src/encrypted_store.rs).
//!
//! Persisted to a distinct `webhooks.enc.json` (not `webhooks.json`) so an
//! encrypted and a plaintext store can never be pointed at the same
//! directory and silently misparse each other's file.

use crate::store::WebhookStore;
use crate::subscription::WebhookSubscription;
use apex_common::{Error, Result};
use apex_kms::{Kms, SealedData};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

type EncryptedSubscriptionMap = BTreeMap<String, EncryptedSubscription>;

/// The on-disk shape: `secret` is sealed via [`Kms`], never plaintext at rest.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct EncryptedSubscription {
    id: String,
    tenant: String,
    url: String,
    events: Vec<String>,
    sealed_secret: SealedData,
    #[serde(default = "default_true")]
    active: bool,
}

fn default_true() -> bool {
    true
}

/// Filesystem store whose persisted `webhooks.enc.json` never holds a
/// plaintext `secret`.
///
/// Like [`FileWebhookStore`](crate::FileWebhookStore), every mutation re-reads
/// `webhooks.enc.json` from disk under a cross-process advisory lock (RM-GA-P2
/// DUR-403) rather than caching the catalog in memory — the CLI and server share
/// this directory by design.
pub struct EncryptedFileWebhookStore {
    dir: PathBuf,
    path: PathBuf,
    kms: Arc<dyn Kms>,
}

impl EncryptedFileWebhookStore {
    /// Open (or create) the store under `dir` (`dir/webhooks.enc.json` is read
    /// lazily, fresh, on every operation), sealing/unsealing every `secret`
    /// through `kms`.
    pub fn new(dir: impl Into<PathBuf>, kms: Arc<dyn Kms>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::config(format!("create webhooks dir: {e}")))?;
        let path = dir.join("webhooks.enc.json");
        Ok(Self { dir, path, kms })
    }

    fn load(&self) -> Result<EncryptedSubscriptionMap> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|e| Error::config(format!("read webhooks.enc.json: {e}")))?;
        let list: Vec<EncryptedSubscription> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("parse webhooks.enc.json: {e}")))?;
        Ok(list.into_iter().map(|r| (r.id.clone(), r)).collect())
    }

    fn persist(&self, map: &EncryptedSubscriptionMap) -> Result<()> {
        let list: Vec<&EncryptedSubscription> = map.values().collect();
        let bytes = serde_json::to_vec_pretty(&list)
            .map_err(|e| Error::config(format!("encode webhooks.enc.json: {e}")))?;
        apex_common::fs::atomic_write(&self.path, bytes)
            .map_err(|e| Error::config(format!("write webhooks.enc.json: {e}")))
    }

    /// Cross-process lock guarding a read-modify-write cycle (DUR-403).
    fn lock(&self) -> Result<apex_common::fs::FileLock> {
        apex_common::fs::FileLock::acquire(&self.dir)
            .map_err(|e| Error::config(format!("lock webhook store: {e}")))
    }

    fn encrypt(&self, sub: &WebhookSubscription) -> Result<EncryptedSubscription> {
        let sealed_secret =
            apex_kms::envelope::seal(self.kms.as_ref(), &sub.tenant, sub.secret.as_bytes())?;
        Ok(EncryptedSubscription {
            id: sub.id.clone(),
            tenant: sub.tenant.clone(),
            url: sub.url.clone(),
            events: sub.events.clone(),
            sealed_secret,
            active: sub.active,
        })
    }

    fn decrypt(&self, record: &EncryptedSubscription) -> Result<WebhookSubscription> {
        let bytes =
            apex_kms::envelope::open(self.kms.as_ref(), &record.tenant, &record.sealed_secret)?;
        let secret = String::from_utf8(bytes)
            .map_err(|_| Error::invalid("decrypted webhook secret is not valid UTF-8"))?;
        Ok(WebhookSubscription {
            id: record.id.clone(),
            tenant: record.tenant.clone(),
            url: record.url.clone(),
            events: record.events.clone(),
            secret,
            active: record.active,
        })
    }
}

impl WebhookStore for EncryptedFileWebhookStore {
    fn register(&self, sub: WebhookSubscription) -> Result<WebhookSubscription> {
        let record = self.encrypt(&sub)?;
        let _flock = self.lock()?;
        let mut map = self.load()?;
        map.insert(record.id.clone(), record);
        self.persist(&map)?;
        Ok(sub)
    }

    fn get(&self, id: &str) -> Result<Option<WebhookSubscription>> {
        let record = self.load()?.get(id).cloned();
        record.map(|r| self.decrypt(&r)).transpose()
    }

    fn list(&self, tenant: &str) -> Result<Vec<WebhookSubscription>> {
        self.load()?
            .values()
            .filter(|r| r.tenant == tenant)
            .map(|r| self.decrypt(r))
            .collect()
    }

    fn delete(&self, id: &str) -> Result<()> {
        let _flock = self.lock()?;
        let mut map = self.load()?;
        if map.remove(id).is_none() {
            return Err(Error::NotFound(format!("webhook `{id}` not found")));
        }
        self.persist(&map)
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
            "apex_webhooks_enc_test_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_the_full_subscription() {
        let dir = temp_dir("roundtrip");
        let store = EncryptedFileWebhookStore::new(&dir, kms()).unwrap();
        let sub = WebhookSubscription::new(
            "acme",
            "https://hooks.example.com/x",
            vec!["plugin.*".into()],
            "shh",
        );

        let saved = store.register(sub.clone()).unwrap();
        assert_eq!(saved.secret, "shh");

        let fetched = store.get(&sub.id).unwrap().unwrap();
        assert_eq!(fetched, sub);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_disk_bytes_never_contain_the_plaintext_secret() {
        let dir = temp_dir("plaintext-check");
        let store = EncryptedFileWebhookStore::new(&dir, kms()).unwrap();
        store
            .register(WebhookSubscription::new(
                "acme",
                "https://x",
                vec!["*".into()],
                "s3cr3t-signing-key-marker",
            ))
            .unwrap();

        let bytes = std::fs::read(dir.join("webhooks.enc.json")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("s3cr3t-signing-key-marker"));
        // The URL is plaintext by design — no confidentiality need, and it's
        // useful for on-disk debugging without a KMS round trip.
        assert!(text.contains("https://x"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persists_across_reopen_with_the_same_kms() {
        let dir = temp_dir("reopen");
        let kms = kms();
        let id;
        {
            let store = EncryptedFileWebhookStore::new(&dir, kms.clone()).unwrap();
            id = store
                .register(WebhookSubscription::new(
                    "acme",
                    "https://x",
                    vec!["*".into()],
                    "shh",
                ))
                .unwrap()
                .id;
        }
        let reopened = EncryptedFileWebhookStore::new(&dir, kms).unwrap();
        assert_eq!(reopened.get(&id).unwrap().unwrap().secret, "shh");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cannot_decrypt_with_a_different_kms_instance() {
        let dir = temp_dir("wrong-kms");
        let id;
        {
            let store = EncryptedFileWebhookStore::new(&dir, kms()).unwrap();
            id = store
                .register(WebhookSubscription::new(
                    "acme",
                    "https://x",
                    vec!["*".into()],
                    "shh",
                ))
                .unwrap()
                .id;
        }
        // A different KMS instance (independent root + tenant keys) cannot
        // recover the secret — fails closed rather than returning garbage.
        let reopened = EncryptedFileWebhookStore::new(&dir, kms()).unwrap();
        assert!(reopened.get(&id).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_is_tenant_scoped_and_delete_removes() {
        let dir = temp_dir("list-delete");
        let store = EncryptedFileWebhookStore::new(&dir, kms()).unwrap();
        let a = store
            .register(WebhookSubscription::new(
                "acme",
                "https://a",
                vec!["*".into()],
                "s1",
            ))
            .unwrap();
        store
            .register(WebhookSubscription::new(
                "beta",
                "https://b",
                vec!["*".into()],
                "s2",
            ))
            .unwrap();

        assert_eq!(store.list("acme").unwrap().len(), 1);
        assert_eq!(store.list("beta").unwrap().len(), 1);

        store.delete(&a.id).unwrap();
        assert!(store.get(&a.id).unwrap().is_none());
        assert!(store.delete(&a.id).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_still_works_through_the_default_trait_method() {
        // `WebhookStore::matching` has a default impl over `list` — confirm it
        // still filters correctly once `list` has to unseal every entry.
        let dir = temp_dir("matching");
        let store = EncryptedFileWebhookStore::new(&dir, kms()).unwrap();
        store
            .register(WebhookSubscription::new(
                "acme",
                "https://a",
                vec!["plugin.*".into()],
                "s1",
            ))
            .unwrap();
        store
            .register(WebhookSubscription::new(
                "acme",
                "https://b",
                vec!["workflow.*".into()],
                "s2",
            ))
            .unwrap();

        let matched = store.matching("acme", "plugin.installed").unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].secret, "s1");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
