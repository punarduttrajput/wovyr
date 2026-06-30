//! The [`Vault`] — the access-controlled front over a [`SecretStore`].
//!
//! It enforces the two access rules from
//! [secret-management](../../docs/13-security/secret-management.md): **tenant isolation**
//! (a workload reads/manages only secrets in its own namespace — principle §2.3
//! "least privilege") and a **`secret:read:<name>` grant** for resolution (§5 injection).
//! Both are fail-closed → [`apex_common::Error::Forbidden`].
//!
//! Management (create/rotate/delete/list/metadata) is tenant-scoped: the caller's tenant
//! *is* the namespace, so a caller can never name another tenant's secret. Resolution
//! additionally requires the holder to present grants covering the reference.

use crate::reference::SecretRef;
use crate::secret::{Secret, SecretMetadata, SecretValue};
use crate::store::SecretStore;
use apex_common::{Error, Result};
use std::sync::Arc;

/// The access a workload presents when **resolving** a secret reference: the tenant it
/// acts in and the permission grants it holds (e.g. `secret:read:vpn-admin-token`,
/// possibly wildcarded as `secret:read:*`).
#[derive(Clone, Debug, Default)]
pub struct SecretAccess {
    /// The tenant (namespace) the workload may read from.
    pub tenant: String,
    /// The permission grants the workload holds.
    pub granted: Vec<String>,
}

impl SecretAccess {
    /// Construct an access context.
    pub fn new(tenant: impl Into<String>, granted: Vec<String>) -> Self {
        Self {
            tenant: tenant.into(),
            granted,
        }
    }
}

/// The access-controlled secret vault.
#[derive(Clone)]
pub struct Vault {
    store: Arc<dyn SecretStore>,
}

impl Vault {
    /// A vault over `store`.
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }

    /// Create a secret in `tenant`'s namespace (conflict if one already exists — use
    /// [`rotate`](Self::rotate) to change a value). Returns its (value-free) metadata.
    pub fn create(&self, tenant: &str, name: &str, value: &str) -> Result<SecretMetadata> {
        let r = SecretRef::new(tenant, name)?;
        if self.store.get(&r.namespace, &r.name)?.is_some() {
            return Err(Error::conflict(format!("secret `{r}` already exists")));
        }
        let secret = Secret::new(&r.namespace, &r.name, value);
        let meta = secret.metadata();
        self.store.put(secret)?;
        Ok(meta)
    }

    /// Rotate an existing secret's value (not found if absent): bumps the version and
    /// retains the prior value for the verification window.
    pub fn rotate(&self, tenant: &str, name: &str, new_value: &str) -> Result<SecretMetadata> {
        let mut secret = self.store.get(tenant, name)?.ok_or_else(|| {
            Error::NotFound(format!("secret `secret://{tenant}/{name}` not found"))
        })?;
        secret.rotate(new_value);
        let meta = secret.metadata();
        self.store.put(secret)?;
        Ok(meta)
    }

    /// Delete a secret in `tenant`'s namespace; returns whether it existed.
    pub fn delete(&self, tenant: &str, name: &str) -> Result<bool> {
        self.store.delete(tenant, name)
    }

    /// A secret's metadata (no value) within `tenant`'s namespace.
    pub fn metadata(&self, tenant: &str, name: &str) -> Result<Option<SecretMetadata>> {
        Ok(self.store.get(tenant, name)?.map(|s| s.metadata()))
    }

    /// List the metadata of every secret in `tenant`'s namespace.
    pub fn list(&self, tenant: &str) -> Result<Vec<SecretMetadata>> {
        self.store.list(tenant)
    }

    /// **Resolve** a reference to its (masked) value for a workload — the injection path.
    /// Fail-closed: the reference's namespace must match the workload's tenant, the
    /// workload must hold a grant covering `secret:read:<name>`, and the secret must
    /// exist. The returned [`SecretValue`] is masked in logs and never serializable.
    pub fn resolve(&self, reference: &SecretRef, access: &SecretAccess) -> Result<SecretValue> {
        if reference.namespace != access.tenant {
            return Err(Error::forbidden(format!(
                "secret `{reference}` is outside tenant `{}`",
                access.tenant
            )));
        }
        let wanted = reference.read_permission();
        if !access.granted.iter().any(|g| grant_covers(g, &wanted)) {
            return Err(Error::forbidden(format!(
                "missing grant `{wanted}` to read secret `{reference}`"
            )));
        }
        let secret = self
            .store
            .get(&reference.namespace, &reference.name)?
            .ok_or_else(|| Error::NotFound(format!("secret `{reference}` not found")))?;
        Ok(secret.value())
    }

    /// Convenience: parse and [`resolve`](Self::resolve) a `secret://…` string.
    pub fn resolve_str(&self, reference: &str, access: &SecretAccess) -> Result<SecretValue> {
        self.resolve(&SecretRef::parse(reference)?, access)
    }
}

/// Whether `granted` (which may use `*` or trailing-`*` wildcards) covers the `wanted`
/// permission. Mirrors the plugin permission model's matcher, kept local so this crate
/// depends only on `apex-common` (the dependency spine stays one-directional).
fn grant_covers(granted: &str, wanted: &str) -> bool {
    let g: Vec<&str> = granted.split(':').collect();
    let w: Vec<&str> = wanted.split(':').collect();
    if g.len() != w.len() {
        return false;
    }
    g.iter().zip(&w).all(|(gs, ws)| match gs.strip_suffix('*') {
        Some(prefix) => ws.starts_with(prefix),
        None => gs == ws,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemorySecretStore;

    fn vault() -> Vault {
        Vault::new(Arc::new(InMemorySecretStore::new()))
    }

    #[test]
    fn create_then_resolve_with_grant() {
        let v = vault();
        v.create("acme", "vpn-admin-token", "t0p").unwrap();
        let access = SecretAccess::new("acme", vec!["secret:read:vpn-admin-token".into()]);
        let value = v
            .resolve_str("secret://acme/vpn-admin-token", &access)
            .unwrap();
        assert_eq!(value.expose(), "t0p");
    }

    #[test]
    fn create_is_conflict_when_present() {
        let v = vault();
        v.create("acme", "token", "a").unwrap();
        assert!(matches!(
            v.create("acme", "token", "b").unwrap_err(),
            Error::Conflict(_)
        ));
    }

    #[test]
    fn resolve_without_grant_is_forbidden() {
        let v = vault();
        v.create("acme", "token", "a").unwrap();
        let no_grant = SecretAccess::new("acme", vec![]);
        assert!(matches!(
            v.resolve_str("secret://acme/token", &no_grant).unwrap_err(),
            Error::Forbidden(_)
        ));
        // A wildcard grant covers it.
        let wild = SecretAccess::new("acme", vec!["secret:read:*".into()]);
        assert!(v.resolve_str("secret://acme/token", &wild).is_ok());
    }

    #[test]
    fn cross_tenant_resolution_is_forbidden() {
        let v = vault();
        v.create("acme", "token", "secret").unwrap();
        // Even with a covering grant, a beta workload cannot read acme's secret.
        let beta = SecretAccess::new("beta", vec!["secret:read:*".into()]);
        assert!(matches!(
            v.resolve_str("secret://acme/token", &beta).unwrap_err(),
            Error::Forbidden(_)
        ));
    }

    #[test]
    fn rotate_serves_new_value_and_bumps_version() {
        let v = vault();
        v.create("acme", "token", "v1").unwrap();
        let meta = v.rotate("acme", "token", "v2").unwrap();
        assert_eq!(meta.version, 2);
        let access = SecretAccess::new("acme", vec!["secret:read:token".into()]);
        assert_eq!(
            v.resolve_str("secret://acme/token", &access)
                .unwrap()
                .expose(),
            "v2"
        );
    }

    #[test]
    fn rotate_missing_is_not_found() {
        let v = vault();
        assert!(matches!(
            v.rotate("acme", "ghost", "x").unwrap_err(),
            Error::NotFound(_)
        ));
    }

    #[test]
    fn list_and_delete_are_tenant_scoped() {
        let v = vault();
        v.create("acme", "a", "1").unwrap();
        v.create("acme", "b", "2").unwrap();
        v.create("beta", "a", "3").unwrap();
        assert_eq!(v.list("acme").unwrap().len(), 2);
        assert_eq!(v.list("beta").unwrap().len(), 1);
        assert!(v.delete("acme", "a").unwrap());
        assert_eq!(v.list("acme").unwrap().len(), 1);
    }
}
