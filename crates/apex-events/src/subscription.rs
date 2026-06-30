//! Webhook subscriptions and event-topic matching
//! ([API overview §15](../../docs/09-api/overview.md#15-webhooks--events)).
//!
//! A subscription registers a delivery `url` for a set of event-type patterns within a
//! tenant. Patterns are dotted topics with a trailing `*` wildcard: `*` matches every
//! event, `plugin.*` matches `plugin.installed`/`plugin.enabled`/…, and an exact
//! `project.created` matches only that type.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A registered webhook endpoint for a tenant's events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookSubscription {
    /// Derived id, `wh-<hex>` (stable for a given tenant/url/topics).
    pub id: String,
    /// The tenant whose events this subscription receives.
    pub tenant: String,
    /// Destination URL deliveries POST to.
    pub url: String,
    /// Event-type patterns this endpoint subscribes to (e.g. `["plugin.*"]`).
    pub events: Vec<String>,
    /// Shared secret used to HMAC-sign delivery payloads.
    pub secret: String,
    /// Whether deliveries are active.
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

impl WebhookSubscription {
    /// A new active subscription with a derived id.
    pub fn new(
        tenant: impl Into<String>,
        url: impl Into<String>,
        events: Vec<String>,
        secret: impl Into<String>,
    ) -> Self {
        let tenant = tenant.into();
        let url = url.into();
        let id = derive_id(&tenant, &url, &events);
        Self {
            id,
            tenant,
            url,
            events,
            secret: secret.into(),
            active: true,
        }
    }

    /// Whether this (active) subscription should receive `event_type`.
    pub fn matches(&self, event_type: &str) -> bool {
        self.active && self.events.iter().any(|p| topic_matches(p, event_type))
    }
}

/// Whether topic `pattern` (exact, `*`, or `prefix.*`) matches `event_type`.
pub fn topic_matches(pattern: &str, event_type: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        // `plugin.*` matches `plugin.installed`; the prefix includes the trailing dot.
        return event_type.starts_with(prefix);
    }
    pattern == event_type
}

/// A stable `wh-<hex>` id derived from the subscription's tenant, url, and sorted topics.
fn derive_id(tenant: &str, url: &str, events: &[String]) -> String {
    let mut topics = events.to_vec();
    topics.sort();
    let mut hasher = Sha256::new();
    hasher.update(tenant.as_bytes());
    hasher.update(b"|");
    hasher.update(url.as_bytes());
    hasher.update(b"|");
    hasher.update(topics.join(",").as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("wh-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_patterns_match_as_specified() {
        assert!(topic_matches("*", "anything.happened"));
        assert!(topic_matches("plugin.*", "plugin.installed"));
        assert!(!topic_matches("plugin.*", "project.created"));
        assert!(topic_matches("project.created", "project.created"));
        assert!(!topic_matches("project.created", "project.updated"));
    }

    #[test]
    fn subscription_matches_only_when_active() {
        let mut sub = WebhookSubscription::new(
            "acme",
            "https://hooks.example.com/x",
            vec!["plugin.*".into(), "project.created".into()],
            "shh",
        );
        assert!(sub.matches("plugin.enabled"));
        assert!(sub.matches("project.created"));
        assert!(!sub.matches("workflow.completed"));
        sub.active = false;
        assert!(!sub.matches("plugin.enabled"));
    }

    #[test]
    fn id_is_stable_and_topic_order_independent() {
        let a =
            WebhookSubscription::new("acme", "https://x", vec!["a.*".into(), "b.*".into()], "s");
        let b =
            WebhookSubscription::new("acme", "https://x", vec!["b.*".into(), "a.*".into()], "s");
        assert_eq!(a.id, b.id);
        assert!(a.id.starts_with("wh-"));
    }
}
