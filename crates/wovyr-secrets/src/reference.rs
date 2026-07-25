//! Secret **references** — the `secret://<namespace>/<name>` addresses that appear in
//! manifests, plugin permissions, and configs ([secret-management §3](../../docs/13-security/secret-management.md#3-secret-vault)).
//!
//! A reference names a secret without embedding its value. The `namespace` is the
//! isolation boundary (a tenant id, or `platform` for platform-wide secrets); the `name`
//! identifies the secret within it (and may itself contain `/`, e.g.
//! `secret://platform/llm/openai-key`).

use std::fmt;
use wovyr_common::{Error, Result};

/// The URI scheme every reference carries.
pub const SCHEME: &str = "secret://";

/// A parsed secret reference: `secret://<namespace>/<name>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SecretRef {
    /// Isolation boundary — a tenant id, or `platform`.
    pub namespace: String,
    /// Secret name within the namespace (may contain `/`).
    pub name: String,
}

impl SecretRef {
    /// A reference from its parts (fail-closed on empty segments).
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Result<Self> {
        let namespace = namespace.into();
        let name = name.into();
        if namespace.is_empty() || name.is_empty() {
            return Err(Error::invalid(
                "secret reference needs a non-empty namespace and name",
            ));
        }
        if namespace.contains('/') {
            return Err(Error::invalid("secret namespace must not contain `/`"));
        }
        Ok(Self { namespace, name })
    }

    /// Parse a `secret://<namespace>/<name>` reference.
    pub fn parse(s: &str) -> Result<Self> {
        let rest = s.strip_prefix(SCHEME).ok_or_else(|| {
            Error::invalid(format!("secret reference must start with `{SCHEME}`"))
        })?;
        let (namespace, name) = rest.split_once('/').ok_or_else(|| {
            Error::invalid("secret reference must be `secret://<namespace>/<name>`")
        })?;
        Self::new(namespace, name)
    }

    /// The `secret:read:<name>` permission a workload must hold to resolve this reference
    /// ([permissions](../../docs/08-plugin-sdk/permissions.md)).
    pub fn read_permission(&self) -> String {
        format!("secret:read:{}", self.name)
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}/{}", self.namespace, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_round_trips() {
        let r = SecretRef::parse("secret://acme/github-token").unwrap();
        assert_eq!(r.namespace, "acme");
        assert_eq!(r.name, "github-token");
        assert_eq!(r.to_string(), "secret://acme/github-token");
        assert_eq!(r.read_permission(), "secret:read:github-token");
    }

    #[test]
    fn name_may_contain_slashes() {
        let r = SecretRef::parse("secret://platform/llm/openai-key").unwrap();
        assert_eq!(r.namespace, "platform");
        assert_eq!(r.name, "llm/openai-key");
        assert_eq!(r.read_permission(), "secret:read:llm/openai-key");
    }

    #[test]
    fn rejects_malformed() {
        assert!(SecretRef::parse("acme/github-token").is_err()); // no scheme
        assert!(SecretRef::parse("secret://acme").is_err()); // no name
        assert!(SecretRef::parse("secret://acme/").is_err()); // empty name
        assert!(SecretRef::parse("secret:///token").is_err()); // empty namespace
    }
}
