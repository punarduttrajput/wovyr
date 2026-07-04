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
//!   [`FileSecretStore`] backends, plus an at-rest-**encrypting**
//!   [`EncryptedFileSecretStore`]
//!   ([Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption)):
//!   seals a secret's value (and retained previous value) through
//!   [`apex_kms`] before it reaches disk, keyed by the secret's own
//!   `namespace` as the KMS tenant — the plain `FileSecretStore`'s
//!   `secrets.json` holds plaintext, this one's `secrets.enc.json` never
//!   does. `list` stays value-free either way, so it never needs to unseal.
//! - [`Vault`] — the access-controlled front: **tenant isolation** + a
//!   **`secret:read:<name>` grant** gate the resolution path, fail-closed.
//!
//! Depends on `apex-common` and `apex-kms` (itself `apex-common`-only), keeping
//! the workspace dependency spine one-directional. Sandbox/plugin
//! **injection** of resolved values is a consumer of this crate (the Tool
//! Runtime / Plugin host), wired separately.

mod encrypted_store;
mod reference;
mod secret;
mod store;
mod vault;

pub use encrypted_store::EncryptedFileSecretStore;
pub use reference::{SCHEME, SecretRef};
pub use secret::{Secret, SecretMetadata, SecretValue};
pub use store::{FileSecretStore, InMemorySecretStore, SecretStore};
pub use vault::{SecretAccess, Vault};
