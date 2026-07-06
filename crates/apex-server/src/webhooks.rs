//! Outbound webhook delivery + management routes
//! ([API overview §15](../../docs/09-api/overview.md#15-webhooks--events)).
//!
//! Platform mutations call [`emit`], which builds a past-tense [`Event`] and
//! **dispatches** it to every active subscription in the tenant whose topic filter
//! matches ([`apex_events`]). Each delivery is HMAC-signed (`X-Apex-Signature`) and
//! POSTed to the subscriber; failures retry with exponential backoff + jitter and are
//! dead-lettered (logged + metered) after the policy's max attempts — at-least-once
//! delivery ([retry §18](../../docs/02-architecture/event-driven-architecture.md#18-retry-strategy)).
//!
//! Delivery is fire-and-forget (spawned), so a slow/broken subscriber never blocks the
//! request that produced the event.

use crate::{ApiError, AppState};
use apex_events::{BackoffPolicy, Event, WebhookSubscription, sign::sign};
use apex_telemetry::Metrics;
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Posts a signed webhook payload to a subscriber. Abstracted so the delivery logic is
/// unit-testable without real HTTP.
#[async_trait]
pub(crate) trait WebhookSender: Send + Sync {
    /// POST `body` to `url` with the signature + event-type headers, returning the HTTP
    /// status (or a transport error string).
    async fn post(
        &self,
        url: &str,
        signature: &str,
        event_type: &str,
        body: &[u8],
    ) -> Result<u16, String>;
}

/// The production [`WebhookSender`] over `reqwest` (10s timeout per attempt).
pub(crate) struct ReqwestSender {
    client: reqwest::Client,
}

impl Default for ReqwestSender {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl WebhookSender for ReqwestSender {
    async fn post(
        &self,
        url: &str,
        signature: &str,
        event_type: &str,
        body: &[u8],
    ) -> Result<u16, String> {
        self.client
            .post(url)
            .header("content-type", "application/json")
            .header("x-apex-signature", signature)
            .header("x-apex-event", event_type)
            .body(body.to_vec())
            .send()
            .await
            .map(|r| r.status().as_u16())
            .map_err(|e| e.to_string())
    }
}

/// Deliver `event` to one subscription, signing the body and retrying per `policy`.
/// Returns whether it was accepted (2xx) within the attempt budget; a final failure is
/// dead-lettered (logged + metered).
async fn deliver(
    sender: &dyn WebhookSender,
    policy: BackoffPolicy,
    metrics: &Metrics,
    sub: &WebhookSubscription,
    event: &Event,
) -> bool {
    let body = serde_json::to_vec(event).unwrap_or_default();
    let signature = sign(sub.secret.as_bytes(), &body);
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let ok = matches!(
            sender.post(&sub.url, &signature, &event.event_type, &body).await,
            Ok(status) if (200..300).contains(&status)
        );
        if ok {
            metrics.counter_inc("apex_webhook_deliveries_total", &[("result", "delivered")]);
            return true;
        }
        if !policy.should_retry(attempt) {
            metrics.counter_inc("apex_webhook_deliveries_total", &[("result", "failed")]);
            tracing::warn!(
                event = %event.event_type,
                url = %sub.url,
                attempts = attempt,
                "webhook delivery failed; dead-lettered"
            );
            return false;
        }
        let delay = policy
            .delay_ms(attempt)
            .saturating_add(jitter_ms(policy.base_ms));
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
}

/// Small non-cryptographic jitter in `[0, base_ms)` derived from the wall clock, to
/// de-correlate retries across deliveries (added only at this I/O boundary).
fn jitter_ms(base_ms: u64) -> u64 {
    if base_ms == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % base_ms
}

/// Dispatch `event` to all matching subscriptions, spawning one delivery task each.
/// Returns the join handles (production callers drop them; tests await them).
pub(crate) fn dispatch(state: &Arc<AppState>, event: Event) -> Vec<tokio::task::JoinHandle<bool>> {
    let subs = state
        .webhooks
        .matching(&event.tenant, &event.event_type)
        .unwrap_or_default();
    subs.into_iter()
        .map(|sub| {
            let sender = state.webhook_sender.clone();
            let policy = state.webhook_policy;
            let metrics = state.metrics.clone();
            let event = event.clone();
            tokio::spawn(async move { deliver(&*sender, policy, &metrics, &sub, &event).await })
        })
        .collect()
}

/// Emit a domain event for a tenant mutation and dispatch it to subscribers
/// (fire-and-forget). Called from mutation handlers.
pub(crate) fn emit(state: &Arc<AppState>, event_type: &str, tenant: &str, payload: Value) {
    let id = format!("evt_{}", state.event_counter.fetch_add(1, Ordering::SeqCst));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let event = Event::new(id, event_type, tenant, ts, payload);
    // Fire-and-forget: a slow subscriber must not block the producing request.
    let _handles = dispatch(state, event);
}

// --- management routes -----------------------------------------------------------

/// The webhook sub-router, merged into the main app router.
pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/webhooks",
            get(list_webhooks).post(register_webhook),
        )
        .route("/api/v1/webhooks/{id}", delete(delete_webhook))
}

#[derive(Deserialize)]
struct RegisterWebhookRequest {
    url: String,
    events: Vec<String>,
    secret: String,
}

async fn list_webhooks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<crate::hardening::PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::tenancy::context(&state, &headers, None);
    ctx.authorize("projects:read")?;
    // Redact secrets from the listing.
    let items: Vec<Value> = state
        .webhooks
        .list(&ctx.tenant)?
        .into_iter()
        .map(|h| json!({ "id": h.id, "url": h.url, "events": h.events, "active": h.active }))
        .collect();
    Ok(Json(crate::hardening::paginate(items, &page.page())))
}

async fn register_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RegisterWebhookRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = crate::tenancy::context(&state, &headers, None);
    // Webhooks are tenant-wide config → org-level authority.
    ctx.authorize("org.admin")?;
    let sub = WebhookSubscription::new(&ctx.tenant, req.url, req.events, req.secret);
    let saved = state.webhooks.register(sub)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": saved.id, "url": saved.url, "events": saved.events })),
    ))
}

async fn delete_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let ctx = crate::tenancy::context(&state, &headers, None);
    ctx.authorize("org.admin")?;
    state.webhooks.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_events::{InMemoryWebhookStore, WebhookStore};
    use std::sync::Mutex;

    /// A test sender that records deliveries and returns scripted statuses (the last is
    /// repeated once exhausted).
    struct MockSender {
        statuses: Vec<u16>,
        calls: Mutex<Vec<(String, String, Vec<u8>)>>, // (signature, event_type, body)
    }

    #[async_trait]
    impl WebhookSender for MockSender {
        async fn post(
            &self,
            _url: &str,
            signature: &str,
            event_type: &str,
            body: &[u8],
        ) -> Result<u16, String> {
            let mut calls = self.calls.lock().unwrap();
            let status = self
                .statuses
                .get(calls.len())
                .copied()
                .or_else(|| self.statuses.last().copied())
                .unwrap_or(200);
            calls.push((signature.to_string(), event_type.to_string(), body.to_vec()));
            Ok(status)
        }
    }

    fn event() -> Event {
        Event::new(
            "evt_1",
            "project.created",
            "acme",
            1,
            json!({"id": "prj-x"}),
        )
    }

    fn fast_policy() -> BackoffPolicy {
        BackoffPolicy {
            base_ms: 0,
            max_attempts: 3,
            max_delay_ms: 0,
        }
    }

    #[tokio::test]
    async fn delivers_with_a_valid_signature() {
        let sender = MockSender {
            statuses: vec![200],
            calls: Mutex::new(Vec::new()),
        };
        let sub = WebhookSubscription::new("acme", "https://x", vec!["project.*".into()], "shh");
        let metrics = Metrics::new();
        let ev = event();
        assert!(deliver(&sender, fast_policy(), &metrics, &sub, &ev).await);

        let calls = sender.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (sig, et, body) = &calls[0];
        assert_eq!(et, "project.created");
        // The recorded body verifies against the subscription secret.
        assert!(apex_events::sign::verify(b"shh", body, sig));
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let sender = MockSender {
            statuses: vec![500, 503, 200], // fail twice, then accept
            calls: Mutex::new(Vec::new()),
        };
        let sub = WebhookSubscription::new("acme", "https://x", vec!["*".into()], "s");
        let metrics = Metrics::new();
        assert!(deliver(&sender, fast_policy(), &metrics, &sub, &event()).await);
        assert_eq!(sender.calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn dead_letters_after_max_attempts() {
        let sender = MockSender {
            statuses: vec![500], // always fails
            calls: Mutex::new(Vec::new()),
        };
        let sub = WebhookSubscription::new("acme", "https://x", vec!["*".into()], "s");
        let metrics = Metrics::new();
        assert!(!deliver(&sender, fast_policy(), &metrics, &sub, &event()).await);
        assert_eq!(sender.calls.lock().unwrap().len(), 3); // max_attempts
    }

    #[tokio::test]
    async fn mutation_route_emits_and_delivers_a_signed_event() {
        use axum::http::Request;
        use tower::ServiceExt;

        // SAFETY: single-threaded test; bootstrap a platform admin.
        unsafe { std::env::set_var("APEX_PLATFORM_ADMINS", "root") };

        let store = Arc::new(InMemoryWebhookStore::new());
        store
            .register(WebhookSubscription::new(
                "acme",
                "https://hooks.example.com/x",
                vec!["organization.*".into()],
                "topsecret",
            ))
            .unwrap();
        let sender = Arc::new(MockSender {
            statuses: vec![200],
            calls: Mutex::new(Vec::new()),
        });
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_tenancy(Arc::new(apex_tenancy::InMemoryTenancyStore::new()))
                .with_webhooks(store)
                .with_webhook_sender(sender.clone())
                .with_webhook_policy(fast_policy()),
        );

        // Creating an org (as the platform admin, tenant acme) emits organization.created.
        let resp = crate::router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/organizations")
                    .header("content-type", "application/json")
                    .header("x-apex-tenant", "acme")
                    .header("x-apex-principal", "root")
                    .body(axum::body::Body::from(
                        json!({"name":"Platform"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // Delivery is fire-and-forget; poll briefly for the spawned task to land it.
        for _ in 0..100 {
            if !sender.calls.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let calls = sender.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "expected one webhook delivery");
        let (sig, event_type, body) = &calls[0];
        assert_eq!(event_type, "organization.created");
        assert!(apex_events::sign::verify(b"topsecret", body, sig));
    }

    #[tokio::test]
    async fn dispatch_fans_out_to_matching_subscriptions_only() {
        let store = InMemoryWebhookStore::new();
        store
            .register(WebhookSubscription::new(
                "acme",
                "https://a",
                vec!["project.*".into()],
                "s",
            ))
            .unwrap();
        store
            .register(WebhookSubscription::new(
                "acme",
                "https://b",
                vec!["workflow.*".into()],
                "s",
            ))
            .unwrap();
        let sender = Arc::new(MockSender {
            statuses: vec![200],
            calls: Mutex::new(Vec::new()),
        });
        let state = Arc::new(
            AppState::from_env()
                .await
                .with_webhooks(Arc::new(store))
                .with_webhook_sender(sender.clone())
                .with_webhook_policy(fast_policy()),
        );

        let handles = dispatch(&state, event()); // project.created
        let results = futures::future::join_all(handles).await;
        // Only the project.* subscription matched and was delivered.
        assert_eq!(results.len(), 1);
        assert!(results[0].as_ref().unwrap());
        assert_eq!(sender.calls.lock().unwrap().len(), 1);
    }
}
