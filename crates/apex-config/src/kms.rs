//! The platform KMS construction shared by `apex-server` and `apex-cli` —
//! previously a byte-for-byte duplicate of the same root-key + tenant-catalog
//! logic in each binary (`apex-server/src/lib.rs`'s `default_kms` and
//! `apex-cli/src/config.rs`'s `kms`).

use apex_kms::{FileKmsStore, InMemoryKmsStore, Kms, KmsStore, LocalKms};
use std::sync::Arc;

/// The platform KMS ([Encryption
/// §5](../../../docs/13-security/encryption.md#5-key-management)): sources a
/// root key from `APEX_KMS_ROOT_KEY` (hex) or, failing that,
/// generates-and-persists one at `~/.apex/kms/root.key` — shared by
/// `apex-server` and `apex-cli` so either process can decrypt the other's
/// sealed data — backing tenant keys with a [`FileKmsStore`] in the same
/// directory. Falls back to a fully ephemeral in-process key if neither is
/// available; anything sealed under it will not survive the process exiting,
/// so this is logged loudly rather than silently accepted.
pub fn build_kms() -> Arc<dyn Kms> {
    let dir = crate::paths::kms_dir().ok();
    let root_key = apex_kms::root::from_env("APEX_KMS_ROOT_KEY")
        .ok()
        .or_else(|| {
            dir.as_ref()
                .and_then(|d| apex_kms::root::from_file(d.join("root.key")).ok())
        });
    match (root_key, dir) {
        (Some(key), Some(dir)) => {
            let store: Arc<dyn KmsStore> = match FileKmsStore::new(dir) {
                Ok(s) => Arc::new(s),
                Err(_) => Arc::new(InMemoryKmsStore::new()),
            };
            Arc::new(LocalKms::new(key, store))
        }
        _ => {
            tracing::warn!(
                "no persistent KMS root key available (set APEX_KMS_ROOT_KEY or ensure \
                 HOME/USERPROFILE is set); using an ephemeral in-process key — anything \
                 sealed under it will not survive a restart"
            );
            let key = apex_kms::generate_key().expect("secure RNG available");
            Arc::new(LocalKms::new(key, Arc::new(InMemoryKmsStore::new())))
        }
    }
}
