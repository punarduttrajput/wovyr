//! # wovyr-kms — envelope-encryption key management
//!
//! Implements the key hierarchy from
//! [Encryption §5](../../docs/13-security/encryption.md#5-key-management):
//!
//! ```text
//! Root key (KMS / HSM)
//!    │ wraps
//! Tenant key (versioned)
//!    │ wraps
//! Data encryption keys (DEKs)
//! ```
//!
//! - [`Kms`] is the port every consumer codes against: mint a DEK, unwrap
//!   one, rewrap a DEK onto the tenant's current key version (rotation
//!   without re-encrypting the data it protects), rotate the tenant key, and
//!   crypto-shred a tenant (permanently destroy its key material — a
//!   fail-closed data-deletion guarantee).
//! - [`LocalKms`] is the default implementation: a root key held in this
//!   process (see [`root`] for how to source one) wraps versioned tenant
//!   keys in a [`KmsStore`] (`InMemoryKmsStore` / `FileKmsStore`). A
//!   cloud-KMS- or HSM-backed root is a later slice — only what wraps/
//!   unwraps *tenant* keys would need to change, since `Kms` is the trait
//!   boundary.
//! - [`envelope::seal`]/[`envelope::open`] are the one-call helper for
//!   protecting a single payload (a memory record's content, a config field —
//!   [Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption))
//!   without a caller ever touching key material directly.
//!
//! Depends only on `wovyr-common`, keeping the workspace dependency spine
//! one-directional. Wiring this into a real consumer (`wovyr-memory`'s
//! sensitive-record flag, `wovyr-secrets`), server routes/CLI commands, and
//! `wovyr-audit` integration for key lifecycle events are later slices.

mod crypto;
mod kms;
mod model;
mod store;

pub mod envelope;
pub mod root;

pub use crypto::{KeyBytes, Sealed, generate_key};
pub use envelope::SealedData;
pub use kms::{Kms, LocalKms};
pub use model::{DataKey, TenantKeyRecord, TenantKeyVersion, WrappedDataKey};
pub use store::{FileKmsStore, InMemoryKmsStore, KmsStore};
