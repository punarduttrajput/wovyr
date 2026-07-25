//! Durable webhook delivery outbox + dead-letter queue (RM-AIM-P1 SRV-103).
//!
//! Before this, webhook delivery was pure fire-and-forget: a `tokio::spawn`'d retry
//! loop with in-process `tokio::sleep` backoff, and an exhausted delivery was only
//! *logged*. A crash dropped every pending retry, and dead-letters vanished with the
//! process. This module makes both durable:
//!
//! - **Outbox** — every dispatched delivery is journaled (`enqueue`) before its
//!   delivery task runs, and removed (`remove`) only on success. A delivery still
//!   pending when the process dies survives in the journal; on the next start,
//!   `webhooks::recover_outbox` re-dispatches it (at-least-once).
//! - **DLQ** — an exhausted delivery is moved to a persisted dead-letter queue
//!   (`dead_letter`), queryable per-tenant (`dead_letters`) rather than lost to a log.
//!
//! The entry stores the subscription **id**, not the subscription itself — the secret
//! is re-resolved from the webhook store at send time, so it is never duplicated into
//! this journal (which would leak it even when the encrypted webhook store is in use).
//!
//! Persistence follows the workspace's DUR pattern: the whole `{pending, dlq}` document
//! is rewritten via `wovyr_common::fs::atomic_write` on every mutation, and `path: None`
//! (what most tests use) is a purely in-memory store.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use wovyr_events::Event;

/// A pending webhook delivery: enough to re-attempt it (the subscription secret is
/// re-resolved by `sub_id` at send time, never stored here).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct OutboxEntry {
    /// Unique delivery id (`<event id>::<subscription id>`).
    pub delivery_id: String,
    pub tenant: String,
    /// Subscription to deliver to — re-resolved via `WebhookStore::get` at send time.
    pub sub_id: String,
    /// The event to deliver.
    pub event: Event,
    pub enqueued_at_ms: u64,
}

/// An exhausted delivery, retained for inspection (no secret — a redacted view).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct DeadLetter {
    pub delivery_id: String,
    pub tenant: String,
    pub sub_id: String,
    pub url: String,
    pub event_type: String,
    pub attempts: u32,
    pub failed_at_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct OutboxDoc {
    pending: Vec<OutboxEntry>,
    dlq: Vec<DeadLetter>,
}

#[derive(Default)]
struct OutboxInner {
    pending: BTreeMap<String, OutboxEntry>,
    dlq: Vec<DeadLetter>,
}

/// The durable outbox + DLQ. Cheap to `clone` the handle is not offered — it lives in
/// `AppState` behind an `Arc`.
pub(crate) struct WebhookOutbox {
    inner: Mutex<OutboxInner>,
    path: Option<PathBuf>,
}

impl WebhookOutbox {
    /// Open the outbox, loading any persisted pending deliveries + DLQ from `path`
    /// (best-effort: a missing/corrupt file starts empty). `path: None` is in-memory.
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        let mut inner = OutboxInner::default();
        if let Some(path) = &path
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(doc) = serde_json::from_slice::<OutboxDoc>(&bytes)
        {
            for e in doc.pending {
                inner.pending.insert(e.delivery_id.clone(), e);
            }
            inner.dlq = doc.dlq;
        }
        Self {
            inner: Mutex::new(inner),
            path,
        }
    }

    /// Journal a delivery as pending before its task runs.
    pub(crate) fn enqueue(&self, entry: OutboxEntry) {
        let mut inner = self.inner.lock().expect("outbox poisoned");
        inner.pending.insert(entry.delivery_id.clone(), entry);
        self.persist(&inner);
    }

    /// Remove a pending delivery (delivered successfully, or its subscription is gone).
    pub(crate) fn remove(&self, delivery_id: &str) {
        let mut inner = self.inner.lock().expect("outbox poisoned");
        if inner.pending.remove(delivery_id).is_some() {
            self.persist(&inner);
        }
    }

    /// Move a pending delivery to the DLQ after its attempt budget is exhausted.
    pub(crate) fn dead_letter(
        &self,
        delivery_id: &str,
        url: &str,
        event_type: &str,
        attempts: u32,
        failed_at_ms: u64,
    ) {
        let mut inner = self.inner.lock().expect("outbox poisoned");
        if let Some(entry) = inner.pending.remove(delivery_id) {
            inner.dlq.push(DeadLetter {
                delivery_id: entry.delivery_id,
                tenant: entry.tenant,
                sub_id: entry.sub_id,
                url: url.to_string(),
                event_type: event_type.to_string(),
                attempts,
                failed_at_ms,
            });
            self.persist(&inner);
        }
    }

    /// Every pending delivery — the set `recover_outbox` re-dispatches on startup.
    pub(crate) fn pending(&self) -> Vec<OutboxEntry> {
        let inner = self.inner.lock().expect("outbox poisoned");
        inner.pending.values().cloned().collect()
    }

    /// `(pending deliveries, dead letters across every tenant)` — the outbox depth
    /// and DLQ size gauges (OBS-301), recomputed at every scrape. Counts only; the
    /// tenant-scoped [`Self::dead_letters`] stays the inspection surface.
    pub(crate) fn depths(&self) -> (usize, usize) {
        let inner = self.inner.lock().expect("outbox poisoned");
        (inner.pending.len(), inner.dlq.len())
    }

    /// Dead-letters for a tenant, most-recent first.
    pub(crate) fn dead_letters(&self, tenant: &str) -> Vec<DeadLetter> {
        let inner = self.inner.lock().expect("outbox poisoned");
        let mut out: Vec<DeadLetter> = inner
            .dlq
            .iter()
            .filter(|d| d.tenant == tenant)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    fn persist(&self, inner: &OutboxInner) {
        let Some(path) = &self.path else {
            return;
        };
        let doc = OutboxDoc {
            pending: inner.pending.values().cloned().collect(),
            dlq: inner.dlq.clone(),
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(error = %e, "failed to create webhook outbox directory");
            return;
        }
        match serde_json::to_vec_pretty(&doc) {
            Ok(bytes) => {
                if let Err(e) = wovyr_common::fs::atomic_write(path, bytes) {
                    tracing::error!(error = %e, "failed to persist webhook outbox");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to encode webhook outbox"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str) -> OutboxEntry {
        OutboxEntry {
            delivery_id: id.to_string(),
            tenant: "acme".to_string(),
            sub_id: "wh-1".to_string(),
            event: Event::new("evt_1", "project.created", "acme", 1, json!({})),
            enqueued_at_ms: 0,
        }
    }

    /// SRV-103: a pending delivery survives a reopen (the "restart" stand-in) so the
    /// startup recovery can re-dispatch it — it isn't dropped on crash.
    #[test]
    fn pending_delivery_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("wovyr_outbox_pending_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("outbox.json");

        {
            let outbox = WebhookOutbox::new(Some(path.clone()));
            outbox.enqueue(entry("d1"));
            outbox.enqueue(entry("d2"));
            outbox.remove("d1"); // d1 delivered; d2 still pending when we "crash"
        }

        let reopened = WebhookOutbox::new(Some(path));
        let pending = reopened.pending();
        assert_eq!(pending.len(), 1, "only the undelivered entry should remain");
        assert_eq!(pending[0].delivery_id, "d2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SRV-103: an exhausted delivery lands in the persisted DLQ and survives reopen.
    #[test]
    fn dead_letter_is_persisted_and_queryable() {
        let dir = std::env::temp_dir().join(format!("wovyr_outbox_dlq_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("outbox.json");

        {
            let outbox = WebhookOutbox::new(Some(path.clone()));
            outbox.enqueue(entry("d1"));
            outbox.dead_letter("d1", "https://x", "project.created", 3, 42);
        }

        let reopened = WebhookOutbox::new(Some(path));
        assert!(
            reopened.pending().is_empty(),
            "dead-lettered entry left pending"
        );
        let dlq = reopened.dead_letters("acme");
        assert_eq!(dlq.len(), 1);
        assert_eq!(dlq[0].attempts, 3);
        assert_eq!(dlq[0].url, "https://x");
        // Tenant-scoped: another tenant sees nothing.
        assert!(reopened.dead_letters("other").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
