//! RM-GA-P2 DR-1002 acceptance: root-key escrow (`WOVYR_KMS_ROOT_KEY`) plus a
//! restored tenant-key catalog together let a completely fresh `LocalKms`
//! instance decrypt data sealed by another one that no longer exists — the
//! disaster-recovery story `docs/13-security/encryption.md` documents as a
//! mandatory install step, proven end to end rather than just asserted in
//! prose.
//!
//! Mirrors the ticket's "seal a record, back up + escrow, wipe, restore the
//! key + data, decrypt" acceptance criterion literally: the tenant-key
//! catalog directory is copied the same way `wovyr admin backup`/`restore`
//! (DR-1001) would copy `~/.wovyr/kms`, and the root key is round-tripped
//! through the exact `WOVYR_KMS_ROOT_KEY`-shaped env var `root::from_env`
//! reads in production.

use std::sync::Arc;
use wovyr_kms::envelope::{open, seal};
use wovyr_kms::{FileKmsStore, KmsStore, LocalKms, generate_key, root};

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wovyr_kms_escrow_restore_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Copy every file in `from` into `to` (flat — a `FileKmsStore` catalog
/// directory has no subdirectories), skipping DUR-403's `.lock` file the same
/// way `wovyr admin backup` does: lock-machinery state, not data.
fn copy_catalog(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".lock" {
            continue;
        }
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn escrowed_root_key_plus_restored_catalog_decrypts_data_sealed_by_a_gone_instance() {
    let original_catalog = scratch_dir("original");
    let restored_catalog = scratch_dir("restored");

    // --- The original host: generates its own root key (nothing escrowed
    // yet) and seals a record for tenant "acme". ---
    let root_key = generate_key().unwrap();
    let sealed = {
        let store: Arc<dyn KmsStore> = Arc::new(FileKmsStore::new(&original_catalog).unwrap());
        let kms = LocalKms::new(root_key, store);
        let sealed = seal(&kms, "acme", b"sensitive memory content").unwrap();
        assert_eq!(
            open(&kms, "acme", &sealed).unwrap(),
            b"sensitive memory content",
            "sanity check: the original instance can read back its own seal"
        );
        sealed
    };

    // --- Escrow (DR-1002's mandatory install step): export the root key as
    // the hex string an operator would store in a secrets manager / HSM /
    // sealed document, and back up the tenant-key catalog directory (DR-1001's
    // `wovyr admin backup`, stood in here by a plain copy since wovyr-kms has no
    // CLI dependency to call the real command). ---
    let escrowed_root_key_hex = hex::encode(root_key);
    copy_catalog(&original_catalog, &restored_catalog);

    // --- The host is lost. Nothing from the original instance survives except
    // the escrowed hex string and the backed-up catalog copy. ---
    let _ = std::fs::remove_dir_all(&original_catalog);

    // --- Restore: a fresh instance sources the root key exactly the way
    // production does (`WOVYR_KMS_ROOT_KEY`, DR-1002's documented mandatory
    // mode) and opens the restored catalog. ---
    const ENV_VAR: &str = "WOVYR_KMS_ROOT_KEY_DR1002_TEST";
    // SAFETY: test-only; this integration test file is its own process, and no
    // other test in it touches this var name.
    unsafe { std::env::set_var(ENV_VAR, &escrowed_root_key_hex) };
    let restored_root_key = root::from_env(ENV_VAR).unwrap();
    unsafe { std::env::remove_var(ENV_VAR) };
    assert_eq!(
        restored_root_key, root_key,
        "the escrowed key round-trips through the env var exactly"
    );

    let restored_store: Arc<dyn KmsStore> = Arc::new(FileKmsStore::new(&restored_catalog).unwrap());
    let restored_kms = LocalKms::new(restored_root_key, restored_store);

    // The actual DR-1002 acceptance criterion: a completely fresh instance,
    // built only from the escrowed key and the restored catalog, decrypts data
    // sealed by an instance that no longer exists.
    assert_eq!(
        open(&restored_kms, "acme", &sealed).unwrap(),
        b"sensitive memory content"
    );

    let _ = std::fs::remove_dir_all(&restored_catalog);
}

/// Restoring the catalog but sourcing the WRONG root key must fail closed —
/// otherwise the escrow story would be indistinguishable from "any key
/// works," which would defeat the entire point of the root key existing.
#[test]
fn restoring_with_the_wrong_root_key_fails_closed() {
    let original_catalog = scratch_dir("wrongkey_original");
    let restored_catalog = scratch_dir("wrongkey_restored");

    let root_key = generate_key().unwrap();
    let sealed = {
        let store: Arc<dyn KmsStore> = Arc::new(FileKmsStore::new(&original_catalog).unwrap());
        let kms = LocalKms::new(root_key, store);
        seal(&kms, "acme", b"top secret").unwrap()
    };
    copy_catalog(&original_catalog, &restored_catalog);
    let _ = std::fs::remove_dir_all(&original_catalog);

    let wrong_root_key = generate_key().unwrap();
    let restored_store: Arc<dyn KmsStore> = Arc::new(FileKmsStore::new(&restored_catalog).unwrap());
    let restored_kms = LocalKms::new(wrong_root_key, restored_store);

    assert!(
        open(&restored_kms, "acme", &sealed).is_err(),
        "a restore with an un-escrowed (wrong) root key must not decrypt anything"
    );

    let _ = std::fs::remove_dir_all(&restored_catalog);
}
