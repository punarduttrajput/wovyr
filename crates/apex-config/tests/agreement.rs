//! Proves `apex-server` and `apex-cli` agree on the `~/.apex` layout and the
//! KMS root key + tenant catalog after HLTH-903's extraction — the
//! acceptance criterion for centralizing backend construction into one
//! crate instead of two independently-maintained copies.

use apex_kms::envelope;
use std::sync::Mutex;

/// Every test below mutates the process-global `HOME`/`USERPROFILE` env vars
/// (`apex_config::apex_dir()` reads them, and there is no injectable
/// override). Rust's test harness runs tests in this binary on multiple
/// threads by default, so without serializing them a second test's
/// `set_var` can land between a first test's own calls and make it observe
/// a different `HOME` mid-test — confirmed in practice: this raced under
/// `cargo test --workspace` even though each test used its own scratch
/// directory. Hold this lock for a test's whole body instead.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn scratch_home(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "apex_config_agreement_{tag}_{}",
        std::process::id()
    ))
}

#[test]
fn resource_paths_agree_with_the_apex_dir_join() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = scratch_home("paths");
    // SAFETY: test-only env mutation, serialized against the other tests in
    // this binary by `ENV_LOCK`.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
    }

    let apex_dir = apex_config::apex_dir().unwrap();
    assert_eq!(apex_dir, home.join(".apex"));
    assert_eq!(apex_config::paths::kms_dir().unwrap(), apex_dir.join("kms"));
    assert_eq!(
        apex_config::paths::secrets_dir().unwrap(),
        apex_dir.join("secrets")
    );
    assert_eq!(
        apex_config::paths::staging_dir().unwrap(),
        apex_dir.join("plugins").join("staging")
    );
    assert_eq!(
        apex_config::paths::definitions_dir().unwrap(),
        apex_dir.join("workflows").join("definitions")
    );
}

/// Simulates a "CLI instance" and a "server instance" independently calling
/// `build_kms()` against the same `~/.apex` — the exact scenario HLTH-903
/// exists to keep correct. Before the extraction this was two hand-copied
/// implementations that could drift; now it's one function called twice.
#[test]
fn build_kms_agrees_across_independently_constructed_instances() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = scratch_home("kms");
    let _ = std::fs::remove_dir_all(&home);
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::remove_var("APEX_KMS_ROOT_KEY");
    }

    let cli_instance = apex_config::kms::build_kms();
    let server_instance = apex_config::kms::build_kms();

    let sealed = envelope::seal(cli_instance.as_ref(), "tenant-a", b"agreement-check").unwrap();
    let opened = envelope::open(server_instance.as_ref(), "tenant-a", &sealed).unwrap();
    assert_eq!(opened, b"agreement-check");

    let _ = std::fs::remove_dir_all(&home);
}

/// The secrets-vault construction must agree the same way: an
/// `EncryptedFileSecretStore` built by one "instance" is readable by another
/// built the same way, both pointed at the shared directory.
#[test]
fn build_secrets_vault_agrees_across_independently_constructed_instances() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = scratch_home("secrets");
    let _ = std::fs::remove_dir_all(&home);
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::remove_var("APEX_KMS_ROOT_KEY");
        std::env::set_var("APEX_SECRETS_ENCRYPT_AT_REST", "1");
    }

    let kms = apex_config::kms::build_kms();
    let writer = apex_config::secrets::build_secrets_vault(kms.clone());
    writer
        .create("agreement-tenant", "api-key", "s3cr3t")
        .unwrap();

    // A fresh "reader instance" built the same way, over the same directory.
    let reader = apex_config::secrets::build_secrets_vault(kms);
    let access = apex_secrets::SecretAccess::new(
        "agreement-tenant",
        vec!["secret:read:api-key".to_string()],
    );
    let value = reader
        .resolve_str("secret://agreement-tenant/api-key", &access)
        .unwrap();
    assert_eq!(value.expose(), "s3cr3t");

    unsafe {
        std::env::remove_var("APEX_SECRETS_ENCRYPT_AT_REST");
    }
    let _ = std::fs::remove_dir_all(&home);
}
