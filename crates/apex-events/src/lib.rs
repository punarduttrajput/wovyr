//! Domain events and outbound webhooks for the Apex AI Platform
//! ([event-driven architecture](../../docs/02-architecture/event-driven-architecture.md),
//! [API overview §15](../../docs/09-api/overview.md#15-webhooks--events)).
//!
//! Platform mutations emit past-tense [`Event`]s; clients register
//! [`WebhookSubscription`]s to receive the ones they care about, delivered as
//! **signed**, **retried** HTTP callbacks. This crate is the transport-agnostic
//! foundation (it depends only on `apex-common`):
//!
//! - [`event`] — the [`Event`] record.
//! - [`subscription`] — [`WebhookSubscription`] + wildcard topic matching.
//! - [`sign`] — HMAC-SHA256 payload signing/verification (`X-Apex-Signature`).
//! - [`retry`] — the [`BackoffPolicy`] (pure delay computation).
//! - [`store`] — the durable [`WebhookStore`] catalog ([`InMemoryWebhookStore`] /
//!   [`FileWebhookStore`]).
//!
//! The HTTP delivery worker (sign → POST → backoff retry → dead-letter) and the server
//! routes + event emission on mutations build on this in a later slice.

pub mod event;
pub mod retry;
pub mod sign;
pub mod store;
pub mod subscription;

pub use event::{EVENT_SCHEMA_VERSION, Event};
pub use retry::BackoffPolicy;
pub use store::{FileWebhookStore, InMemoryWebhookStore, WebhookState, WebhookStore};
pub use subscription::{WebhookSubscription, topic_matches};
