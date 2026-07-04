//! Secret values and records.
//!
//! [`SecretValue`] is the **masked** carrier for a resolved secret: its `Debug`/`Display`
//! render `****`, and it deliberately implements **neither `Serialize` nor `Deserialize`**
//! so a value can never be accidentally logged or serialized into an API response
//! ([secret-management §9 Masking](../../docs/13-security/secret-management.md#9-masking)).
//! Call [`SecretValue::expose`] only at the point of use (sandbox injection, gateway call).
//!
//! [`Secret`] is the stored record (value + rotation history); [`SecretMetadata`] is the
//! value-free projection safe to return from list/get endpoints.

use crate::reference::SecretRef;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A resolved secret value, masked in all debug/display output and never serializable.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wrap a raw value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value — call only at the point of use. Naming it `expose` keeps call
    /// sites greppable for audit.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(****)")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("****")
    }
}

/// A stored secret: the current value plus the one prior value retained for a
/// rotation/verification window ([secret-management §7](../../docs/13-security/secret-management.md#7-rotation)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Secret {
    /// Isolation boundary (tenant id or `platform`).
    pub namespace: String,
    /// Secret name within the namespace.
    pub name: String,
    /// The current value.
    value: String,
    /// The immediately-previous value, retained across one rotation (verification
    /// window); `None` for a freshly-created secret or after it ages out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<String>,
    /// Monotonic version, bumped on each rotation (starts at 1).
    pub version: u32,
}

impl Secret {
    /// A new secret at version 1.
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            value: value.into(),
            previous: None,
            version: 1,
        }
    }

    /// Rotate to `new_value`: the current value becomes the retained previous, and the
    /// version bumps. Consumers reading by reference pick up the new value automatically.
    pub fn rotate(&mut self, new_value: impl Into<String>) {
        let old = std::mem::replace(&mut self.value, new_value.into());
        self.previous = Some(old);
        self.version += 1;
    }

    /// The current value, masked.
    pub fn value(&self) -> SecretValue {
        SecretValue::new(self.value.clone())
    }

    /// The retained previous value (rotation window), masked, if any.
    pub fn previous_value(&self) -> Option<SecretValue> {
        self.previous.as_ref().map(|p| SecretValue::new(p.clone()))
    }

    /// This secret's reference.
    pub fn reference(&self) -> SecretRef {
        SecretRef {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
        }
    }

    /// The value-free projection safe to expose over an API.
    pub fn metadata(&self) -> SecretMetadata {
        SecretMetadata {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            version: self.version,
            reference: self.reference().to_string(),
        }
    }

    /// Reconstruct a secret from its raw parts — crate-internal, used by an
    /// encrypting store rehydrating from sealed storage (it never has a
    /// `SecretValue` to build from, only recovered plaintext).
    pub(crate) fn from_parts(
        namespace: String,
        name: String,
        value: String,
        previous: Option<String>,
        version: u32,
    ) -> Self {
        Self {
            namespace,
            name,
            value,
            previous,
            version,
        }
    }

    /// The raw current value — crate-internal. Sealing needs the plaintext
    /// bytes; external consumers use the masked [`value`](Self::value).
    pub(crate) fn raw_value(&self) -> &str {
        &self.value
    }

    /// The raw previous value, if any — crate-internal.
    pub(crate) fn raw_previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }
}

/// A secret's metadata — everything *except* the value. Safe to list/return.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMetadata {
    /// Isolation boundary (tenant id or `platform`).
    pub namespace: String,
    /// Secret name within the namespace.
    pub name: String,
    /// Current version.
    pub version: u32,
    /// The canonical `secret://…` reference.
    pub reference: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_masked_in_debug_and_display() {
        let v = SecretValue::new("super-secret");
        assert_eq!(format!("{v}"), "****");
        assert_eq!(format!("{v:?}"), "SecretValue(****)");
        // But the raw value is reachable at the point of use.
        assert_eq!(v.expose(), "super-secret");
    }

    #[test]
    fn rotation_bumps_version_and_retains_previous() {
        let mut s = Secret::new("acme", "token", "v1");
        assert_eq!(s.version, 1);
        assert!(s.previous_value().is_none());
        s.rotate("v2");
        assert_eq!(s.version, 2);
        assert_eq!(s.value().expose(), "v2");
        assert_eq!(s.previous_value().unwrap().expose(), "v1");
    }

    #[test]
    fn metadata_omits_the_value() {
        let s = Secret::new("acme", "token", "shh");
        let json = serde_json::to_string(&s.metadata()).unwrap();
        assert!(
            !json.contains("shh"),
            "metadata must not carry the value: {json}"
        );
        assert!(json.contains("secret://acme/token"));
    }
}
