//! The secret vault construction shared by `wovyr-server` and `wovyr-cli` —
//! previously a byte-for-byte duplicate (`wovyr-server/src/lib.rs`'s
//! `default_secrets_vault` and `wovyr-cli/src/plugin.rs`'s `secrets_vault`).

use std::sync::Arc;
use wovyr_kms::Kms;
use wovyr_secrets::{
    EncryptedFileSecretStore, FileSecretStore, InMemorySecretStore, SecretStore, Vault,
};

/// A secret [`Vault`] over the durable store at `~/.wovyr/secrets` (shared by
/// `wovyr-server` and `wovyr-cli`, so both must agree on which file is live).
///
/// **Encrypted-at-rest is the default (RM-AIM-P1 SEC-101):** values are sealed
/// through `kms` before they reach disk (a distinct `secrets.enc.json`, never
/// mixed with the plaintext `secrets.json`). A legacy plaintext `secrets.json`
/// left over from before the default flipped is **migrated automatically** on
/// first construction — re-sealed into the encrypted store, then retired to
/// `secrets.json.migrated.bak` with a loud warning — so existing secrets stay
/// visible instead of silently vanishing behind the filename switch.
/// `WOVYR_SECRETS_PLAINTEXT=1` is the explicit opt-out for the old plaintext
/// behavior.
pub fn build_secrets_vault(kms: Arc<dyn Kms>) -> Vault {
    let dir = crate::paths::secrets_dir().ok();
    let encrypt_at_rest = crate::env::secrets_encrypt_at_rest();
    let store: Arc<dyn SecretStore> = match dir {
        Some(d) if encrypt_at_rest => match EncryptedFileSecretStore::new(d, kms) {
            Ok(s) => {
                // One-time re-seal of a pre-SEC-101 plaintext catalog. Failure is
                // loud but non-fatal: nothing is written on error (all-or-nothing),
                // the plaintext file stays for a retry on the next construction,
                // and the vault still serves newly-created (sealed) secrets.
                if let Err(e) = s.migrate_plaintext() {
                    tracing::error!(error = %e,
                        "failed to migrate plaintext secrets.json to encrypted \
                         storage; legacy secrets stay in secrets.json until a \
                         retry succeeds");
                }
                Arc::new(s)
            }
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
