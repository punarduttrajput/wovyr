//! An at-rest-**encrypting** [`MemoryStore`] decorator: seals a record's
//! `content` through [`wovyr_kms`] before it reaches the inner store, when the
//! record is flagged [`sensitive`](crate::record::MemoryRecord::sensitive) —
//! [Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption)'s
//! "memory records flagged sensitive". Keyed by the record's own `namespace`
//! as the KMS tenant, and transparent to callers either way: `put` still
//! takes a `MemoryRecord` with plaintext `content`, and every read path
//! (`all`/`get`) returns plaintext `content` — only the bytes the inner
//! store actually persists are ciphertext for sensitive records.
//!
//! Wraps *any* `MemoryStore`, including a pushdown-capable one
//! ([`TieredStore`](crate::TieredStore), behind the `tiered` feature) — but
//! retrieval pushdown needs the plaintext to score against (Postgres
//! `tsvector`, Qdrant ANN), which a purpose-built index only has if it's
//! *given* the plaintext. Since the whole point here is that it never is,
//! this wrapper always reports [`MemoryStore::supports_pushdown`] as
//! `false` — the engine falls back to its in-process path over `all()`,
//! which *is* decrypted, so retrieval still works, just without ANN/
//! full-text acceleration for the records this wraps.

use crate::record::MemoryRecord;
use crate::store::MemoryStore;
use async_trait::async_trait;
use std::sync::Arc;
use wovyr_common::{Error, Result};
use wovyr_kms::{Kms, SealedData, envelope};

/// Wraps `inner`, sealing/unsealing `content` through `kms` for any record
/// with `sensitive: true`; non-sensitive records pass through untouched.
pub struct EncryptingMemoryStore {
    inner: Arc<dyn MemoryStore>,
    kms: Arc<dyn Kms>,
}

impl EncryptingMemoryStore {
    /// Wrap `inner`, sealing/unsealing sensitive records' content through `kms`.
    pub fn new(inner: Arc<dyn MemoryStore>, kms: Arc<dyn Kms>) -> Self {
        Self { inner, kms }
    }

    fn seal_if_sensitive(&self, mut record: MemoryRecord) -> Result<MemoryRecord> {
        if record.sensitive {
            let sealed = envelope::seal(
                self.kms.as_ref(),
                &record.namespace,
                record.content.as_bytes(),
            )?;
            record.content = serde_json::to_string(&sealed)
                .map_err(|e| Error::invalid(format!("encode sealed memory content: {e}")))?;
        }
        Ok(record)
    }

    fn unseal_if_sensitive(&self, mut record: MemoryRecord) -> Result<MemoryRecord> {
        if record.sensitive {
            let sealed: SealedData = serde_json::from_str(&record.content)
                .map_err(|e| Error::invalid(format!("corrupt sealed memory content: {e}")))?;
            let plaintext = envelope::open(self.kms.as_ref(), &record.namespace, &sealed)?;
            record.content = String::from_utf8(plaintext)
                .map_err(|_| Error::invalid("decrypted memory content is not valid UTF-8"))?;
        }
        Ok(record)
    }
}

#[async_trait]
impl MemoryStore for EncryptingMemoryStore {
    async fn put(&self, record: MemoryRecord) -> Result<String> {
        let sealed = self.seal_if_sensitive(record)?;
        self.inner.put(sealed).await
    }

    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>> {
        self.inner
            .all(namespace)
            .await?
            .into_iter()
            .map(|r| self.unseal_if_sensitive(r))
            .collect()
    }

    async fn get(&self, ids: &[String]) -> Result<Vec<MemoryRecord>> {
        self.inner
            .get(ids)
            .await?
            .into_iter()
            .map(|r| self.unseal_if_sensitive(r))
            .collect()
    }

    async fn delete(&self, ids: &[String]) -> Result<()> {
        self.inner.delete(ids).await
    }

    async fn update(&self, record: MemoryRecord) -> Result<()> {
        // Same sealing discipline as `put`: a sensitive record's content is
        // re-sealed before the rewrite reaches disk (RAG-301 migration passes
        // plaintext content, since it read through `all`'s unsealing).
        let sealed = self.seal_if_sensitive(record)?;
        self.inner.update(sealed).await
    }

    fn supports_pushdown(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::MemoryType;
    use crate::store::FileStore;
    use wovyr_kms::{InMemoryKmsStore, LocalKms, generate_key};

    fn kms() -> Arc<dyn Kms> {
        Arc::new(LocalKms::new(
            generate_key().unwrap(),
            Arc::new(InMemoryKmsStore::new()),
        ))
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wovyr_memory_enc_test_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn record(namespace: &str, content: &str, sensitive: bool) -> MemoryRecord {
        MemoryRecord {
            id: String::new(),
            namespace: namespace.to_string(),
            content: content.to_string(),
            embedding: vec![0.1, 0.2, 0.3],
            embedding_model: String::new(),
            memory_type: MemoryType::Semantic,
            importance: 0.5,
            tags: Vec::new(),
            required_scopes: Vec::new(),
            sensitive,
            parent_id: None,
            is_parent: false,
            created_ms: 0,
            seq: 0,
        }
    }

    #[tokio::test]
    async fn sensitive_record_round_trips_through_put_all_get() {
        let dir = temp_dir("roundtrip");
        let inner: Arc<dyn MemoryStore> = Arc::new(FileStore::new(&dir).unwrap());
        let store = EncryptingMemoryStore::new(inner, kms());

        let id = store
            .put(record("acme", "sensitive fact", true))
            .await
            .unwrap();

        let all = store.all(Some("acme")).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content, "sensitive fact");

        let got = store.get(&[id]).await.unwrap();
        assert_eq!(got[0].content, "sensitive fact");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn non_sensitive_record_passes_through_untouched() {
        let dir = temp_dir("passthrough");
        let inner: Arc<dyn MemoryStore> = Arc::new(FileStore::new(&dir).unwrap());
        let store = EncryptingMemoryStore::new(inner, kms());

        store
            .put(record("acme", "public fact", false))
            .await
            .unwrap();
        let all = store.all(Some("acme")).await.unwrap();
        assert_eq!(all[0].content, "public fact");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn on_disk_bytes_never_contain_the_plaintext_for_a_sensitive_record() {
        let dir = temp_dir("plaintext-check");
        let inner: Arc<dyn MemoryStore> = Arc::new(FileStore::new(&dir).unwrap());
        let store = EncryptingMemoryStore::new(inner, kms());

        store
            .put(record("acme", "s3cr3t-plaintext-marker", true))
            .await
            .unwrap();

        let path = dir.join("acme.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("s3cr3t-plaintext-marker"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cannot_decrypt_with_a_different_kms_instance() {
        let dir = temp_dir("wrong-kms");
        {
            let inner: Arc<dyn MemoryStore> = Arc::new(FileStore::new(&dir).unwrap());
            let store = EncryptingMemoryStore::new(inner, kms());
            store.put(record("acme", "top secret", true)).await.unwrap();
        }
        let inner: Arc<dyn MemoryStore> = Arc::new(FileStore::new(&dir).unwrap());
        // A different KMS instance (independent root + tenant keys) cannot
        // recover the value — fails closed rather than returning garbage.
        let reopened = EncryptingMemoryStore::new(inner, kms());
        assert!(reopened.all(Some("acme")).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pushdown_is_always_disabled() {
        let inner: Arc<dyn MemoryStore> = Arc::new(FileStore::new(temp_dir("pushdown")).unwrap());
        let store = EncryptingMemoryStore::new(inner, kms());
        assert!(!store.supports_pushdown());
    }
}
