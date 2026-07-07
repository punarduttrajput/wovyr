//! RM-GA-P2 DUR-403 acceptance: "a test spawning two processes (or two `Store`
//! handles with real file locks) doing concurrent writes shows no lost update
//! and no corrupt file." `rotate_tenant_key` is exactly the read-increment-write
//! pattern DUR-403 exists to protect (read the current version, compute the
//! next one, write it back) — a lost update here means two racing rotations
//! both mint the same "next" version, one clobbering the other.
//!
//! Each thread gets its own independently-constructed `FileKmsStore`/`LocalKms`
//! pair (no shared `Arc`, no in-process cache) pointed at the same directory —
//! the same stand-in for "a separate process" the crash-recovery tests use.

use apex_kms::{FileKmsStore, Kms, KmsStore, LocalKms, root};
use std::sync::Arc;

#[test]
fn concurrent_rotations_from_independent_store_handles_produce_no_lost_update() {
    let dir = std::env::temp_dir().join(format!(
        "apex_kms_concurrent_rotation_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root_key_path = dir.join("root.key");
    let store_dir = dir.join("store");

    const THREADS: usize = 8;
    const ROTATIONS_PER_THREAD: usize = 15;

    // Provision the tenant (version 1) before the race, so every thread
    // starts from the same known baseline.
    {
        let kms = LocalKms::new(
            root::from_file(&root_key_path).unwrap(),
            Arc::new(FileKmsStore::new(&store_dir).unwrap()),
        );
        kms.generate_data_key("acme").unwrap();
    }

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let root_key_path = root_key_path.clone();
            let store_dir = store_dir.clone();
            std::thread::spawn(move || {
                // A fresh, independent store + KMS per thread — structurally
                // identical to a separate process opening the same directory.
                let kms = LocalKms::new(
                    root::from_file(&root_key_path).unwrap(),
                    Arc::new(FileKmsStore::new(&store_dir).unwrap()),
                );
                let mut versions = Vec::with_capacity(ROTATIONS_PER_THREAD);
                for _ in 0..ROTATIONS_PER_THREAD {
                    versions.push(kms.rotate_tenant_key("acme").unwrap());
                }
                versions
            })
        })
        .collect();

    let mut all_versions: Vec<u32> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();

    let expected_count = THREADS * ROTATIONS_PER_THREAD;
    assert_eq!(
        all_versions.len(),
        expected_count,
        "every rotation call returned a version"
    );

    all_versions.sort_unstable();
    all_versions.dedup();
    assert_eq!(
        all_versions.len(),
        expected_count,
        "no two rotations minted the same version number (a lost update would \
         show up as a duplicate here)"
    );

    // Versions 2..=expected_count+1 (version 1 was the initial provision) —
    // contiguous, no gaps, meaning no rotation silently vanished either.
    let expected: Vec<u32> = (2..=(expected_count as u32 + 1)).collect();
    assert_eq!(
        all_versions, expected,
        "rotated versions must be exactly contiguous from 2..=N+1, with no gaps"
    );

    // The store file itself is intact and parseable — a corrupted temp file
    // from two racing writers would show up as a load failure here.
    let final_store = FileKmsStore::new(&store_dir).unwrap();
    let record = final_store.get("acme").unwrap().unwrap();
    assert_eq!(record.versions.len(), expected_count + 1);

    let _ = std::fs::remove_dir_all(&dir);
}
