//! # apex-secrets — the platform secret vault
//!
//! Implements [Secret Management](../../docs/13-security/secret-management.md): secrets
//! are addressed by **reference** (`secret://<namespace>/<name>`), stored in a managed
//! vault, resolved at runtime by an authorized workload, **masked** everywhere, and
//! **rotatable** without redeploying consumers.
//!
//! - [`SecretRef`] — the reference type (parse/format, `secret:read:<name>` permission).
//! - [`SecretValue`] — a resolved value, masked in `Debug`/`Display` and deliberately
//!   non-serializable so it cannot leak into logs or responses.
//! - [`Secret`] / [`SecretMetadata`] — the stored record (value + rotation history) and
//!   its value-free projection.
//! - [`SecretStore`] — the durable port, with [`InMemorySecretStore`] +
//!   [`FileSecretStore`] backends.
//! - [`Vault`] — the access-controlled front: **tenant isolation** + a
//!   **`secret:read:<name>` grant** gate the resolution path, fail-closed.
//!
//! Depends only on `apex-common`, keeping the workspace dependency spine one-directional.
//! Sandbox/plugin **injection** of resolved values is a consumer of this crate (the Tool
//! Runtime / Plugin host), wired separately.

mod reference;
mod secret;
mod store;
mod vault;

pub use reference::{SCHEME, SecretRef};
pub use secret::{Secret, SecretMetadata, SecretValue};
pub use store::{FileSecretStore, InMemorySecretStore, SecretStore};
pub use vault::{SecretAccess, Vault};
