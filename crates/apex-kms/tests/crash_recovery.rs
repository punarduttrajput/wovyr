//! RM-GA-P2 DUR-401 acceptance: the KMS store is the highest-blast-radius
//! target for a torn whole-file write (losing/corrupting `kms.json` makes
//! every DEK ever wrapped under it unrecoverable). This proves the round
//! trip survives a simulated crash mid-write: a temp file left behind by an
//! interrupted `atomic_write` (the write landed, the rename never happened)
//! must not disturb the last *committed* `kms.json`, and a sealed record
//! must still decrypt correctly — including one sealed before a tenant-key
//! rotation that itself gets "interrupted" this way.
//!
//! No filesystem fault injection needed: `atomic_write`'s first phase (write
//! the temp file) and second phase (rename it over the target) are separate,
//! durable steps, so reproducing "phase one happened, phase two didn't" is
//! just writing the temp file directly and stopping there.

use apex_kms::{FileKmsStore, Kms, LocalKms, envelope, root};
use std::sync::Arc;

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "apex_kms_crash_recovery_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sealed_record_survives_a_torn_write_during_rotation() {
    let dir = scratch_dir("rotate");
    let key = root::from_file(dir.join("root.key")).unwrap();
    let store_dir = dir.join("store");
    let store = Arc::new(FileKmsStore::new(&store_dir).unwrap());
    let kms = LocalKms::new(key, store.clone());

    // Seal a payload under tenant key version 1, then rotate (bumps the
    // version and persists kms.json again) — the old wrapped DEK must
    // remain valid under its recorded version.
    let sealed = envelope::seal(&kms, "acme", b"top secret").unwrap();
    kms.rotate_tenant_key("acme").unwrap();
    assert_eq!(
        envelope::open(&kms, "acme", &sealed).unwrap(),
        b"top secret"
    );

    // Simulate a crash mid-*next* rotation: atomic_write's temp-file write
    // completed, but the process died before the rename that would have
    // made it live. Reproduce that exact on-disk state directly, bypassing
    // the rename step entirely.
    let tmp = store_dir.join("kms.json.tmp");
    std::fs::write(&tmp, b"{ this is a torn write in progress, not valid json").unwrap();

    // A fresh store/KMS pair opened against the same directory must load
    // the last *committed* kms.json — the abandoned temp file is inert —
    // and still unseal the value protected before the interrupted write.
    let reopened_store = Arc::new(FileKmsStore::new(&store_dir).unwrap());
    let reopened_kms = LocalKms::new(
        root::from_file(dir.join("root.key")).unwrap(),
        reopened_store,
    );
    assert_eq!(
        envelope::open(&reopened_kms, "acme", &sealed).unwrap(),
        b"top secret"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn root_key_generation_survives_a_torn_write_left_behind_by_a_prior_attempt() {
    let dir = scratch_dir("root_key");
    let root_path = dir.join("root.key");

    let key = root::from_file(&root_path).unwrap();

    // Simulate a crash between `atomic_write`'s temp-file write and its
    // rename during some *other*, hypothetical rewrite of the root key: the
    // temp file exists with garbage, but the committed file was never
    // touched.
    let tmp = dir.join("root.key.tmp");
    std::fs::write(&tmp, b"not-a-valid-hex-key").unwrap();

    // Loading again must still return the original, already-committed key —
    // unaffected by the abandoned temp file — not the garbage nor a freshly
    // generated one.
    let reloaded = root::from_file(&root_path).unwrap();
    assert_eq!(key, reloaded);

    let _ = std::fs::remove_dir_all(&dir);
}
