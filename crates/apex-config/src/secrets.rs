//! The secret vault construction shared by `apex-server` and `apex-cli` —
//! previously a byte-for-byte duplicate (`apex-server/src/lib.rs`'s
//! `default_secrets_vault` and `apex-cli/src/plugin.rs`'s `secrets_vault`).

use apex_kms::Kms;
use apex_secrets::{
    EncryptedFileSecretStore, FileSecretStore, InMemorySecretStore, SecretStore, Vault,
};
use std::sync::Arc;

/// A secret [`Vault`] over the durable store at `~/.apex/secrets` (shared by
/// `apex-server` and `apex-cli`, so both must agree on which file is live).
/// Seals values through `kms` before they reach disk (a distinct
/// `secrets.enc.json`, never mixed with the plaintext `secrets.json`) when
/// `APEX_SECRETS_ENCRYPT_AT_REST` is set — **opt-in**: flipping it makes any
/// already-plaintext secrets invisible via this vault rather than
/// transparently migrating them.
pub fn build_secrets_vault(kms: Arc<dyn Kms>) -> Vault {
    let dir = crate::paths::secrets_dir().ok();
    let encrypt_at_rest = crate::env::secrets_encrypt_at_rest();
    let store: Arc<dyn SecretStore> = match dir {
        Some(d) if encrypt_at_rest => match EncryptedFileSecretStore::new(d, kms) {
            Ok(s) => Arc::new(s),
            Err(_) => Arc::new(InMemorySecretStore::new()),
        },
        Some(d) => match FileSecretStore::new(d) {
            Ok(s) => Arc::new(s),
            Err(_) => Arc::new(InMemorySecretStore::new()),
        },
        None => Arc::new(InMemorySecretStore::new()),
    };
    Vault::new(store)
}
