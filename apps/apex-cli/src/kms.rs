//! `apex kms` commands: manage the local platform KMS's tenant keys
//! ([Encryption §5](../../docs/13-security/encryption.md#5-key-management)).
//!
//! Operates directly on the same `~/.apex/kms` catalog the server uses — no server
//! process required, matching how `memory`/`plugin` commands work locally.

use crate::config;

/// `apex kms rotate --tenant <t>` — roll a new tenant-key version. Existing wrapped
/// data keys remain valid under their original version; nothing is re-encrypted.
pub fn rotate_cmd(tenant: &str) -> apex_common::Result<()> {
    let version = config::kms().rotate_tenant_key(tenant)?;
    println!("tenant `{tenant}` rotated to key version {version}");
    Ok(())
}

/// `apex kms destroy --tenant <t> --yes` — permanently **crypto-shred** a tenant's key
/// material. Irreversible: every secret/memory sealed under this tenant becomes
/// unrecoverable, past and future, until the tenant is silently re-provisioned by the
/// next seal (which starts a brand-new key hierarchy — old ciphertext stays unreadable).
pub fn destroy_cmd(tenant: &str, confirmed: bool) -> apex_common::Result<()> {
    if !confirmed {
        eprintln!(
            "refusing to destroy tenant `{tenant}`'s key without --yes: this is \
             IRREVERSIBLE — every secret/memory sealed under it becomes permanently \
             unrecoverable"
        );
        return Err(apex_common::Error::invalid("missing --yes confirmation"));
    }
    config::kms().destroy_tenant_key(tenant)?;
    println!("tenant `{tenant}`'s key material has been destroyed (crypto-shredded)");
    Ok(())
}
