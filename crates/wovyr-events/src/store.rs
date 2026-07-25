//! The webhook subscription catalog: a durable store of registered endpoints.
//!
//! [`InMemoryWebhookStore`] (tests/single-process) and [`FileWebhookStore`] (a single
//! `webhooks.json`) share their logic. Registering an already-present id is idempotent
//! (the subscription is replaced); deleting an absent one is
//! [`Error::NotFound`](wovyr_common::Error::NotFound).

use crate::subscription::WebhookSubscription;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use wovyr_common::{Error, Result};

/// The persisted subscription catalog (one document for [`FileWebhookStore`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WebhookState {
    /// Subscriptions by id.
    pub subscriptions: BTreeMap<String, WebhookSubscription>,
}

/// A durable catalog of webhook subscriptions.
pub trait WebhookStore: Send + Sync {
    /// Register a subscription (replaces any with the same derived id). Returns it.
    fn register(&self, sub: WebhookSubscription) -> Result<WebhookSubscription>;
    /// Look up a subscription by id.
    fn get(&self, id: &str) -> Result<Option<WebhookSubscription>>;
    /// All subscriptions for a tenant, sorted by id.
    fn list(&self, tenant: &str) -> Result<Vec<WebhookSubscription>>;
    /// Remove a subscription (not found if absent).
    fn delete(&self, id: &str) -> Result<()>;
    /// All active subscriptions in `tenant` that match `event_type` — the delivery set.
    fn matching(&self, tenant: &str, event_type: &str) -> Result<Vec<WebhookSubscription>> {
        Ok(self
            .list(tenant)?
            .into_iter()
            .filter(|s| s.matches(event_type))
            .collect())
    }
}

/// In-process webhook store (tests / single process).
#[derive(Default)]
pub struct InMemoryWebhookStore {
    state: Mutex<WebhookState>,
}

impl InMemoryWebhookStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl WebhookStore for InMemoryWebhookStore {
    fn register(&self, sub: WebhookSubscription) -> Result<WebhookSubscription> {
        let mut s = self.state.lock().expect("webhook mutex poisoned");
        s.subscriptions.insert(sub.id.clone(), sub.clone());
        Ok(sub)
    }
    fn get(&self, id: &str) -> Result<Option<WebhookSubscription>> {
        Ok(self
            .state
            .lock()
            .expect("webhook mutex poisoned")
            .subscriptions
            .get(id)
            .cloned())
    }
    fn list(&self, tenant: &str) -> Result<Vec<WebhookSubscription>> {
        Ok(self
            .state
            .lock()
            .expect("webhook mutex poisoned")
            .subscriptions
            .values()
            .filter(|s| s.tenant == tenant)
            .cloned()
            .collect())
    }
    fn delete(&self, id: &str) -> Result<()> {
        let mut s = self.state.lock().expect("webhook mutex poisoned");
        if s.subscriptions.remove(id).is_none() {
            return Err(Error::NotFound(format!("webhook `{id}` not found")));
        }
        Ok(())
    }
}

/// A durable webhook store backed by a single `webhooks.json` under a directory.
/// Mutations are serialized by a process-local lock **and** a cross-process advisory
/// file lock (RM-GA-P2 DUR-403), since the CLI and server share this directory.
pub struct FileWebhookStore {
    dir: PathBuf,
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileWebhookStore {
    /// Open (or create) a store under `dir`, holding `dir/webhooks.json`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("webhooks.json"),
            dir,
            lock: Mutex::new(()),
        })
    }

    fn load(&self) -> Result<WebhookState> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::invalid(format!("corrupt webhook store: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(WebhookState::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn save(&self, state: &WebhookState) -> Result<()> {
        wovyr_common::fs::atomic_write(&self.path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }
}

impl FileWebhookStore {
    /// Cross-process lock guarding a read-modify-write cycle (DUR-403).
    fn cross_process_lock(&self) -> Result<wovyr_common::fs::FileLock> {
        wovyr_common::fs::FileLock::acquire(&self.dir)
            .map_err(|e| Error::config(format!("lock webhook store: {e}")))
    }
}

impl WebhookStore for FileWebhookStore {
    fn register(&self, sub: WebhookSubscription) -> Result<WebhookSubscription> {
        let _g = self.lock.lock().expect("webhook file lock poisoned");
        let _flock = self.cross_process_lock()?;
        let mut state = self.load()?;
        state.subscriptions.insert(sub.id.clone(), sub.clone());
        self.save(&state)?;
        Ok(sub)
    }
    fn get(&self, id: &str) -> Result<Option<WebhookSubscription>> {
        let _g = self.lock.lock().expect("webhook file lock poisoned");
        Ok(self.load()?.subscriptions.get(id).cloned())
    }
    fn list(&self, tenant: &str) -> Result<Vec<WebhookSubscription>> {
        let _g = self.lock.lock().expect("webhook file lock poisoned");
        Ok(self
            .load()?
            .subscriptions
            .into_values()
            .filter(|s| s.tenant == tenant)
            .collect())
    }
    fn delete(&self, id: &str) -> Result<()> {
        let _g = self.lock.lock().expect("webhook file lock poisoned");
        let _flock = self.cross_process_lock()?;
        let mut state = self.load()?;
        if state.subscriptions.remove(id).is_none() {
            return Err(Error::NotFound(format!("webhook `{id}` not found")));
        }
        self.save(&state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(store: &dyn WebhookStore) {
        let sub = WebhookSubscription::new(
            "acme",
            "https://hooks.example.com/x",
            vec!["plugin.*".into()],
            "shh",
        );
        let id = store.register(sub.clone()).unwrap().id;
        assert_eq!(store.get(&id).unwrap().unwrap().url, sub.url);
        assert_eq!(store.list("acme").unwrap().len(), 1);
        assert!(store.list("other").unwrap().is_empty());

        // Delivery set: matches plugin.* in this tenant only.
        assert_eq!(store.matching("acme", "plugin.installed").unwrap().len(), 1);
        assert!(
            store
                .matching("acme", "workflow.completed")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .matching("other", "plugin.installed")
                .unwrap()
                .is_empty()
        );

        store.delete(&id).unwrap();
        assert!(store.get(&id).unwrap().is_none());
        assert!(store.delete(&id).is_err());
    }

    #[test]
    fn in_memory_round_trips() {
        exercise(&InMemoryWebhookStore::new());
    }

    #[test]
    fn file_round_trips_and_persists() {
        let dir = std::env::temp_dir().join(format!("wovyr_webhooks_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileWebhookStore::new(&dir).unwrap();
        let sub = WebhookSubscription::new("acme", "https://x", vec!["*".into()], "s");
        let id = store.register(sub).unwrap().id;
        // A fresh handle sees the persisted subscription.
        let reopened = FileWebhookStore::new(&dir).unwrap();
        assert!(reopened.get(&id).unwrap().is_some());
        // Clean up before the shared CRUD exercise (which expects an empty store).
        store.delete(&id).unwrap();
        exercise(&store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
