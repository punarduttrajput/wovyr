//! Domain events and outbound webhooks for the Wovyr AI Platform
//! ([event-driven architecture](../../docs/02-architecture/event-driven-architecture.md),
//! [API overview §15](../../docs/09-api/overview.md#15-webhooks--events)).
//!
//! Platform mutations emit past-tense [`Event`]s; clients register
//! [`WebhookSubscription`]s to receive the ones they care about, delivered as
//! **signed**, **retried** HTTP callbacks. This crate is the transport-agnostic
//! foundation:
//!
//! - [`event`] — the [`Event`] record.
//! - [`subscription`] — [`WebhookSubscription`] + wildcard topic matching.
//! - [`sign`] — HMAC-SHA256 payload signing/verification (`X-Wovyr-Signature`).
//! - [`retry`] — the [`BackoffPolicy`] (pure delay computation).
//! - [`store`] — the durable [`WebhookStore`] catalog ([`InMemoryWebhookStore`] /
//!   [`FileWebhookStore`]), plus an at-rest-**encrypting**
//!   [`EncryptedFileWebhookStore`]
//!   ([Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption)):
//!   seals a subscription's `secret` (the HMAC signing key) through
//!   [`wovyr_kms`] before it reaches disk, keyed by the subscription's own
//!   `tenant` as the KMS tenant — `url`/`events`/`active` stay plaintext, no
//!   confidentiality need there. `webhooks.enc.json` never holds a plaintext
//!   secret; the plain `FileWebhookStore`'s `webhooks.json` does.
//!
//! Depends on `wovyr-common` and `wovyr-kms` (itself `wovyr-common`-only), keeping
//! the workspace dependency spine one-directional. The HTTP delivery worker
//! (sign → POST → backoff retry → dead-letter) and the server routes + event
//! emission on mutations build on this in a later slice.

mod encrypted_store;
pub mod event;
pub mod retry;
pub mod sign;
pub mod store;
pub mod subscription;

pub use encrypted_store::EncryptedFileWebhookStore;
pub use event::{EVENT_SCHEMA_VERSION, Event};
pub use retry::BackoffPolicy;
pub use store::{FileWebhookStore, InMemoryWebhookStore, WebhookState, WebhookStore};
pub use subscription::{WebhookSubscription, topic_matches};
