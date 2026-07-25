//! Keyless (identity-based) plugin signing
//! ([distribution §4](../../docs/08-plugin-sdk/distribution.md#4-provenance--sbom),
//! [ADR-0009](../../docs/17-adr/ADR-0009-keyless-signing.md)).
//!
//! Sigstore-shaped, Wovyr-native: instead of a publisher holding a long-lived ed25519
//! key (the [`TrustStore`](crate::verify::TrustStore) mode), a **certificate
//! authority** issues a short-lived [`IdentityCert`] binding an OIDC-style identity
//! (issuer + subject) to an ephemeral signing key; the manifest is signed with the
//! ephemeral key, the signing event is appended to a **transparency log**, and the
//! ephemeral key is discarded. Verifiers hold only a pinned [`KeylessRoot`] (CA + log
//! public keys) and an [`IdentityPolicy`] saying which identities may sign for which
//! publisher namespaces.
//!
//! The verification side is **fully offline**: a [`KeylessBundle`] is self-contained
//! (certificate + signature + log entry), and the log's `integrated_time` — not a
//! local clock — anchors the certificate-validity check, so the core stays
//! deterministic per [coding-standards §7]. Only *signing* touches infrastructure,
//! via the [`CertificateAuthority`] / [`TransparencyLog`] ports:
//! [`InMemoryCa`]/[`InMemoryTransparencyLog`] are the deterministic in-process
//! implementations; a Rekor-backed log lives behind the `rekor` cargo feature (see
//! [`crate::rekor`], live-tested against `deployment/rekor/`).
//!
//! SET verification covers **both** log formats: a log that supplies its own
//! canonical entry `body` (Rekor) is checked against the RFC 8785 payload it signs
//! ([`rekor_set_payload`], ed25519 or ECDSA P-256 pinned keys — proven live against
//! a real Rekor); the Wovyr in-memory log signs this module's canonical entry form
//! ([`set_canonical_bytes`]). Deferred ([ADR-0009]): X.509/Fulcio certificate
//! compatibility and Merkle inclusion-proof checks.

use crate::verify::hex;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wovyr_common::{Error, Result};

/// Who signed: an OIDC-style identity, e.g. issuer `https://ci.example.com` and
/// subject `release@acme.dev`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerIdentity {
    /// The identity provider that attested the subject.
    pub issuer: String,
    /// The attested subject (email / workload identity).
    pub subject: String,
}

/// A short-lived certificate binding a [`SignerIdentity`] to an ephemeral ed25519
/// public key, signed by a certificate authority. All key/signature fields are
/// lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityCert {
    /// The ephemeral ed25519 public key (hex, 32 bytes).
    pub public_key: String,
    /// The identity the CA attested.
    pub identity: SignerIdentity,
    /// Validity window start (epoch ms).
    pub not_before_ms: u64,
    /// Validity window end (epoch ms).
    pub not_after_ms: u64,
    /// CA ed25519 signature (hex) over [`canonical_bytes`](Self::canonical_bytes).
    pub signature: String,
}

impl IdentityCert {
    /// The exact bytes the CA signs — a deterministic, delimiter-separated encoding
    /// of every field (so any mutation breaks the signature).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        format!(
            "wovyr-identity-cert.v1\n{}\n{}\n{}\n{}\n{}",
            self.identity.issuer,
            self.identity.subject,
            self.public_key,
            self.not_before_ms,
            self.not_after_ms
        )
        .into_bytes()
    }
}

/// A reference to an appended transparency-log entry: the proof a signing event was
/// witnessed at `integrated_time_ms`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntryRef {
    /// The log's entry identifier.
    pub uuid: String,
    /// Position in the log.
    pub log_index: u64,
    /// When the log integrated the entry (epoch ms) — the timestamp that anchors
    /// certificate-validity checks.
    pub integrated_time_ms: u64,
    /// Identifier of the log that witnessed the entry.
    pub log_id: String,
    /// The log's own canonicalized entry (base64), when the log defines one —
    /// **Rekor** does, and its SET signs over it (see [`rekor_set_payload`]). Empty
    /// for the Wovyr in-memory log, whose SET signs [`set_canonical_bytes`] instead.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// The log's signature (hex) over its SET payload — the signed entry
    /// timestamp (SET). Empty when the log does not provide one.
    #[serde(default)]
    pub signed_entry_timestamp: String,
}

/// The canonical bytes a log signs for its SET: entry coordinates plus a digest of
/// the signing event (artifact digest, signature, ephemeral key).
pub fn set_canonical_bytes(entry: &LogEntryRef, body_digest_hex: &str) -> Vec<u8> {
    format!(
        "wovyr-set.v1\n{}\n{}\n{}\n{}\n{}",
        entry.uuid, entry.log_index, entry.integrated_time_ms, entry.log_id, body_digest_hex
    )
    .into_bytes()
}

/// Digest of a signing event's body, committing the artifact, signature, and key.
pub fn body_digest_hex(artifact: &[u8], signature_hex: &str, public_key_hex: &str) -> String {
    let artifact_digest = hex::encode(Sha256::digest(artifact));
    hex::encode(Sha256::digest(
        format!("{artifact_digest}\n{signature_hex}\n{public_key_hex}").as_bytes(),
    ))
}

/// Escape a string for embedding in canonical JSON (RFC 8785 string rules for the
/// characters that can actually occur here — quotes, backslashes, controls).
fn jcs_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The exact bytes **Rekor** signs for its SET: the RFC 8785 (JCS)
/// canonicalization of `{body, integratedTime, logID, logIndex}` — keys in
/// lexicographic order, no whitespace, `integratedTime` in **seconds**.
pub fn rekor_set_payload(entry: &LogEntryRef) -> Vec<u8> {
    format!(
        r#"{{"body":"{}","integratedTime":{},"logID":"{}","logIndex":{}}}"#,
        jcs_escape(&entry.body),
        entry.integrated_time_ms / 1000,
        jcs_escape(&entry.log_id),
        entry.log_index
    )
    .into_bytes()
}

/// Verify `signature_hex` over `message` with a pinned log/CA key in any accepted
/// encoding: raw ed25519 (32 bytes), SPKI ed25519 (44 bytes), SPKI ECDSA P-256
/// (91 bytes, e.g. Rekor's `/api/v1/log/publicKey`), or a raw uncompressed P-256
/// point (65 bytes). ECDSA signatures are ASN.1-encoded (as Rekor emits them).
fn verify_with_pinned_key(key_hex: &str, message: &[u8], signature_hex: &str) -> Result<()> {
    let key = hex::decode(key_hex)?;
    let sig = hex::decode(signature_hex)?;
    /// DER prefix of a PKIX `SubjectPublicKeyInfo` for ed25519.
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    /// DER prefix of a PKIX `SubjectPublicKeyInfo` for ECDSA P-256 (prime256v1).
    const P256_SPKI_PREFIX: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    let (alg, raw): (&dyn ring::signature::VerificationAlgorithm, &[u8]) = match key.len() {
        32 => (&ED25519, &key),
        44 if key.starts_with(&ED25519_SPKI_PREFIX) => (&ED25519, &key[12..]),
        65 if key[0] == 0x04 => (&ring::signature::ECDSA_P256_SHA256_ASN1, &key),
        91 if key.starts_with(&P256_SPKI_PREFIX) => {
            (&ring::signature::ECDSA_P256_SHA256_ASN1, &key[26..])
        }
        n => {
            return Err(Error::invalid(format!(
                "unsupported pinned key encoding ({n} bytes)"
            )));
        }
    };
    UnparsedPublicKey::new(alg, raw)
        .verify(message, &sig)
        .map_err(|_| Error::invalid("signature verification failed against pinned key"))
}

/// A self-contained keyless signature over a plugin manifest: certificate +
/// ephemeral-key signature + transparency-log entry. Everything a verifier needs
/// besides the pinned [`KeylessRoot`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeylessBundle {
    /// The short-lived identity certificate.
    pub cert: IdentityCert,
    /// Ephemeral-key ed25519 signature (hex) over the manifest bytes.
    pub signature: String,
    /// The witnessed signing event, when a transparency log was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entry: Option<LogEntryRef>,
}

/// The pinned trust root a verifier holds: CA public keys (who may issue
/// certificates) and transparency-log public keys (whose SETs are checkable).
/// Serializable, so operators can vendor it like `trust.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeylessRoot {
    /// Trusted CA ed25519 public keys (hex).
    #[serde(default)]
    pub ca_public_keys: Vec<String>,
    /// Pinned transparency-log ed25519 public keys (hex). Empty ⇒ log entries are
    /// accepted without SET verification (dev mode — e.g. a local Rekor whose
    /// in-memory key rotates, or logs whose SET format is not yet supported).
    #[serde(default)]
    pub log_public_keys: Vec<String>,
}

/// One identity → publisher-namespace grant. `subject` and `publisher` support a
/// trailing `*` wildcard (`release@acme.*`, `acme-*`); `issuer` is exact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityRule {
    /// Exact issuer the identity must come from.
    pub issuer: String,
    /// Subject pattern (exact, or trailing-`*` prefix).
    pub subject: String,
    /// Publisher namespace pattern this identity may sign for (`*` = any).
    pub publisher: String,
}

/// Which identities may sign for which publishers — **fail-closed**: an empty policy
/// admits nobody, exactly like an empty [`TrustStore`](crate::verify::TrustStore).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityPolicy {
    /// The allowed identity → publisher grants.
    #[serde(default)]
    pub allow: Vec<IdentityRule>,
    /// Require a transparency-log entry (default `true`). Without one the
    /// certificate-validity window cannot be anchored, so disabling this accepts
    /// bundles on CA + policy trust alone (registry-witnessed mode).
    #[serde(default = "default_true")]
    pub require_transparency: bool,
}

fn default_true() -> bool {
    true
}

impl Default for IdentityPolicy {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            require_transparency: true,
        }
    }
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

impl IdentityPolicy {
    /// Whether `identity` may sign for `publisher` under this policy.
    pub fn allows(&self, identity: &SignerIdentity, publisher: &str) -> bool {
        self.allow.iter().any(|r| {
            r.issuer == identity.issuer
                && pattern_matches(&r.subject, &identity.subject)
                && pattern_matches(&r.publisher, publisher)
        })
    }
}

/// The certificate-issuing port (the Fulcio role). Implementations attest an
/// identity out of band (OIDC, operator fiat) and bind it to the ephemeral key.
pub trait CertificateAuthority {
    /// Issue a short-lived certificate for `identity` over `ephemeral_public_key`
    /// (hex), valid from `now_ms`.
    fn issue(
        &self,
        identity: &SignerIdentity,
        ephemeral_public_key: &str,
        now_ms: u64,
    ) -> Result<IdentityCert>;
}

/// The transparency-log port (the Rekor role): append a signing event, get back a
/// witnessed entry.
pub trait TransparencyLog {
    /// Append the signing event (`artifact` + hex `signature` + hex `public_key`),
    /// returning the witnessed entry.
    fn append(&self, artifact: &[u8], signature: &str, public_key: &str) -> Result<LogEntryRef>;
}

/// Default certificate lifetime: 10 minutes, mirroring Fulcio.
pub const DEFAULT_CERT_TTL_MS: u64 = 10 * 60 * 1000;

/// How far a certificate's `not_before` is backdated at issue, absorbing clock skew
/// between the CA and the transparency log — and second-granularity log timestamps
/// (Rekor reports whole seconds, which truncation can place just *before* a
/// same-instant millisecond-precision issue time).
pub const CLOCK_SKEW_ALLOWANCE_MS: u64 = 60 * 1000;

/// An in-process CA over an ed25519 keypair — the deterministic implementation for
/// tests and single-operator (dev) deployments.
pub struct InMemoryCa {
    key: Ed25519KeyPair,
    ttl_ms: u64,
}

impl InMemoryCa {
    /// A CA from pkcs8 key bytes (e.g. from [`generate_keypair`]).
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self> {
        let key = Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|_| Error::invalid("invalid ed25519 pkcs8 for CA"))?;
        Ok(Self {
            key,
            ttl_ms: DEFAULT_CERT_TTL_MS,
        })
    }

    /// Override the issued-certificate lifetime.
    pub fn with_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = ttl_ms;
        self
    }

    /// The CA's public key (hex) — what verifiers pin in [`KeylessRoot`].
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.key.public_key().as_ref())
    }
}

impl CertificateAuthority for InMemoryCa {
    fn issue(
        &self,
        identity: &SignerIdentity,
        ephemeral_public_key: &str,
        now_ms: u64,
    ) -> Result<IdentityCert> {
        let mut cert = IdentityCert {
            public_key: ephemeral_public_key.to_string(),
            identity: identity.clone(),
            // Backdated for clock skew (see [`CLOCK_SKEW_ALLOWANCE_MS`]).
            not_before_ms: now_ms.saturating_sub(CLOCK_SKEW_ALLOWANCE_MS),
            not_after_ms: now_ms + self.ttl_ms,
            signature: String::new(),
        };
        cert.signature = hex::encode(self.key.sign(&cert.canonical_bytes()).as_ref());
        Ok(cert)
    }
}

/// An in-process transparency log — deterministic (time injected at construction),
/// entries witnessed with the log's ed25519 key so SET verification is exercised
/// offline.
pub struct InMemoryTransparencyLog {
    key: Ed25519KeyPair,
    now_ms: u64,
    entries: std::sync::Mutex<Vec<String>>,
}

impl InMemoryTransparencyLog {
    /// A log from pkcs8 key bytes, integrating entries at `now_ms`.
    pub fn from_pkcs8(pkcs8: &[u8], now_ms: u64) -> Result<Self> {
        let key = Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|_| Error::invalid("invalid ed25519 pkcs8 for transparency log"))?;
        Ok(Self {
            key,
            now_ms,
            entries: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// The log's public key (hex) — what verifiers pin in [`KeylessRoot`].
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.key.public_key().as_ref())
    }
}

impl TransparencyLog for InMemoryTransparencyLog {
    fn append(&self, artifact: &[u8], signature: &str, public_key: &str) -> Result<LogEntryRef> {
        let body = body_digest_hex(artifact, signature, public_key);
        let mut entries = self.entries.lock().expect("log mutex poisoned");
        let log_id = hex::encode(Sha256::digest(self.key.public_key().as_ref()));
        let mut entry = LogEntryRef {
            uuid: hex::encode(Sha256::digest(
                format!("{}\n{}", entries.len(), body).as_bytes(),
            )),
            log_index: entries.len() as u64,
            integrated_time_ms: self.now_ms,
            log_id,
            body: String::new(), // Wovyr-native SET form (no log-defined body)
            signed_entry_timestamp: String::new(),
        };
        entry.signed_entry_timestamp =
            hex::encode(self.key.sign(&set_canonical_bytes(&entry, &body)).as_ref());
        entries.push(body);
        Ok(entry)
    }
}

/// Generate an ed25519 keypair, returning `(pkcs8_bytes, public_key_hex)`.
/// Publisher-/operator-side tooling: this is the randomness boundary — core
/// signing/verification below is deterministic over its inputs.
pub fn generate_keypair() -> Result<(Vec<u8>, String)> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| Error::Runtime("ed25519 keypair generation failed".into()))?;
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| Error::Runtime("generated pkcs8 did not parse".into()))?;
    let public = hex::encode(key.public_key().as_ref());
    Ok((pkcs8.as_ref().to_vec(), public))
}

/// Keyless-sign `manifest_bytes`: certify the ephemeral key (supplied as pkcs8 —
/// generate it with [`generate_keypair`] and discard it after this call), sign the
/// manifest, and witness the event in `log` when provided.
pub fn sign_keyless(
    manifest_bytes: &[u8],
    identity: &SignerIdentity,
    ephemeral_pkcs8: &[u8],
    ca: &dyn CertificateAuthority,
    log: Option<&dyn TransparencyLog>,
    now_ms: u64,
) -> Result<KeylessBundle> {
    let key = Ed25519KeyPair::from_pkcs8(ephemeral_pkcs8)
        .map_err(|_| Error::invalid("invalid ephemeral ed25519 pkcs8"))?;
    let public_hex = hex::encode(key.public_key().as_ref());
    let cert = ca.issue(identity, &public_hex, now_ms)?;
    let signature = hex::encode(key.sign(manifest_bytes).as_ref());
    let log_entry = match log {
        Some(log) => Some(log.append(manifest_bytes, &signature, &public_hex)?),
        None => None,
    };
    Ok(KeylessBundle {
        cert,
        signature,
        log_entry,
    })
}

fn verify_ed25519(public_key_hex: &str, message: &[u8], signature_hex: &str) -> Result<()> {
    let key = hex::decode(public_key_hex)?;
    let sig = hex::decode(signature_hex)?;
    UnparsedPublicKey::new(&ED25519, &key)
        .verify(message, &sig)
        .map_err(|_| Error::invalid("ed25519 signature verification failed"))
}

/// Verify a [`KeylessBundle`] over `manifest_bytes` for `publisher`, against the
/// pinned `root` and `policy`. Fail-closed at every step:
///
/// 1. the certificate must be signed by a pinned CA key;
/// 2. the certified identity must be allowed to sign for `publisher`;
/// 3. the manifest signature must verify with the certified ephemeral key;
/// 4. a transparency-log entry is required (unless the policy opts out), its
///    `integrated_time_ms` must fall inside the certificate's validity window, and —
///    when the root pins log keys — its SET must verify.
///
/// Returns the verified signer identity.
pub fn verify_keyless(
    manifest_bytes: &[u8],
    bundle: &KeylessBundle,
    root: &KeylessRoot,
    policy: &IdentityPolicy,
    publisher: &str,
) -> Result<SignerIdentity> {
    // 1. Certificate chains to a pinned CA.
    let cert_bytes = bundle.cert.canonical_bytes();
    let ca_ok = root
        .ca_public_keys
        .iter()
        .any(|ca| verify_ed25519(ca, &cert_bytes, &bundle.cert.signature).is_ok());
    if !ca_ok {
        return Err(Error::invalid(
            "keyless certificate is not signed by a pinned CA",
        ));
    }

    // 2. The identity may sign for this publisher.
    if !policy.allows(&bundle.cert.identity, publisher) {
        return Err(Error::Forbidden(format!(
            "identity `{}` (issuer `{}`) is not allowed to sign for publisher `{publisher}`",
            bundle.cert.identity.subject, bundle.cert.identity.issuer
        )));
    }

    // 3. The manifest signature verifies with the certified ephemeral key.
    verify_ed25519(&bundle.cert.public_key, manifest_bytes, &bundle.signature)
        .map_err(|_| Error::invalid("keyless manifest signature verification failed"))?;

    // 4. Transparency: entry required (by default), time-anchored, SET-checked.
    match &bundle.log_entry {
        None if policy.require_transparency => Err(Error::invalid(
            "keyless bundle carries no transparency-log entry, which this policy requires",
        )),
        None => Ok(bundle.cert.identity.clone()),
        Some(entry) => {
            if entry.integrated_time_ms < bundle.cert.not_before_ms
                || entry.integrated_time_ms > bundle.cert.not_after_ms
            {
                return Err(Error::invalid(
                    "transparency-log entry falls outside the certificate validity window",
                ));
            }
            if !root.log_public_keys.is_empty() {
                // Two SET formats: a log that supplies its own canonical `body`
                // (Rekor) signs the RFC 8785 payload over it; the Wovyr in-memory
                // log signs this module's canonical entry form.
                let set_bytes = if entry.body.is_empty() {
                    let body =
                        body_digest_hex(manifest_bytes, &bundle.signature, &bundle.cert.public_key);
                    set_canonical_bytes(entry, &body)
                } else {
                    rekor_set_payload(entry)
                };
                let set_ok = root.log_public_keys.iter().any(|k| {
                    verify_with_pinned_key(k, &set_bytes, &entry.signed_entry_timestamp).is_ok()
                });
                if !set_ok {
                    return Err(Error::invalid(
                        "transparency-log signed entry timestamp does not verify against a pinned log key",
                    ));
                }
            }
            Ok(bundle.cert.identity.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000_000;

    fn identity() -> SignerIdentity {
        SignerIdentity {
            issuer: "https://ci.example.com".into(),
            subject: "release@acme.dev".into(),
        }
    }

    fn policy() -> IdentityPolicy {
        IdentityPolicy {
            allow: vec![IdentityRule {
                issuer: "https://ci.example.com".into(),
                subject: "release@acme.dev".into(),
                publisher: "acme".into(),
            }],
            require_transparency: true,
        }
    }

    /// A full signing setup: CA, log, and a bundle over `manifest`, plus the root
    /// that pins both.
    fn signed(manifest: &[u8]) -> (KeylessBundle, KeylessRoot) {
        let (ca_pkcs8, _) = generate_keypair().unwrap();
        let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap();
        let (log_pkcs8, _) = generate_keypair().unwrap();
        let log = InMemoryTransparencyLog::from_pkcs8(&log_pkcs8, NOW + 1000).unwrap();
        let (eph, _) = generate_keypair().unwrap();
        let bundle = sign_keyless(manifest, &identity(), &eph, &ca, Some(&log), NOW).unwrap();
        let root = KeylessRoot {
            ca_public_keys: vec![ca.public_key_hex()],
            log_public_keys: vec![log.public_key_hex()],
        };
        (bundle, root)
    }

    #[test]
    fn round_trip_verifies_and_returns_the_identity() {
        let manifest = b"metadata: {name: x}";
        let (bundle, root) = signed(manifest);
        let id = verify_keyless(manifest, &bundle, &root, &policy(), "acme").unwrap();
        assert_eq!(id, identity());
    }

    #[test]
    fn tampered_manifest_fails() {
        let (bundle, root) = signed(b"original");
        assert!(verify_keyless(b"tampered", &bundle, &root, &policy(), "acme").is_err());
    }

    #[test]
    fn certificate_from_an_unpinned_ca_fails() {
        let (bundle, _) = signed(b"m");
        // A root pinning a *different* CA.
        let (other, _) = generate_keypair().unwrap();
        let root = KeylessRoot {
            ca_public_keys: vec![InMemoryCa::from_pkcs8(&other).unwrap().public_key_hex()],
            log_public_keys: vec![],
        };
        let err = verify_keyless(b"m", &bundle, &root, &policy(), "acme").unwrap_err();
        assert!(err.to_string().contains("pinned CA"), "{err}");
    }

    #[test]
    fn identity_and_publisher_are_policy_gated_fail_closed() {
        let manifest = b"m";
        let (bundle, root) = signed(manifest);

        // Empty policy admits nobody.
        let err = verify_keyless(manifest, &bundle, &root, &IdentityPolicy::default(), "acme")
            .unwrap_err();
        assert!(matches!(err, Error::Forbidden(_)));

        // The right identity for the wrong publisher namespace is refused.
        assert!(verify_keyless(manifest, &bundle, &root, &policy(), "evil").is_err());

        // Wildcards: subject prefix + any-publisher.
        let broad = IdentityPolicy {
            allow: vec![IdentityRule {
                issuer: "https://ci.example.com".into(),
                subject: "release@*".into(),
                publisher: "*".into(),
            }],
            require_transparency: true,
        };
        assert!(verify_keyless(manifest, &bundle, &root, &broad, "anyone").is_ok());
    }

    #[test]
    fn log_time_outside_the_cert_window_fails() {
        let manifest = b"m";
        let (ca_pkcs8, _) = generate_keypair().unwrap();
        let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap().with_ttl_ms(1000);
        let (log_pkcs8, _) = generate_keypair().unwrap();
        // The log integrates the entry *after* the 1s certificate expired.
        let log = InMemoryTransparencyLog::from_pkcs8(&log_pkcs8, NOW + 60_000).unwrap();
        let (eph, _) = generate_keypair().unwrap();
        let bundle = sign_keyless(manifest, &identity(), &eph, &ca, Some(&log), NOW).unwrap();
        let root = KeylessRoot {
            ca_public_keys: vec![ca.public_key_hex()],
            log_public_keys: vec![log.public_key_hex()],
        };
        let err = verify_keyless(manifest, &bundle, &root, &policy(), "acme").unwrap_err();
        assert!(err.to_string().contains("validity window"), "{err}");
    }

    #[test]
    fn tampered_set_or_time_fails_when_log_key_is_pinned() {
        let manifest = b"m";
        let (mut bundle, root) = signed(manifest);
        // Forge a later integration time (to sneak into the window) — SET breaks.
        bundle.log_entry.as_mut().unwrap().integrated_time_ms += 1;
        let err = verify_keyless(manifest, &bundle, &root, &policy(), "acme").unwrap_err();
        assert!(err.to_string().contains("signed entry timestamp"), "{err}");
    }

    #[test]
    fn missing_log_entry_is_rejected_unless_policy_opts_out() {
        let manifest = b"m";
        let (ca_pkcs8, _) = generate_keypair().unwrap();
        let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap();
        let (eph, _) = generate_keypair().unwrap();
        let bundle = sign_keyless(manifest, &identity(), &eph, &ca, None, NOW).unwrap();
        let root = KeylessRoot {
            ca_public_keys: vec![ca.public_key_hex()],
            log_public_keys: vec![],
        };

        // Default: transparency required.
        assert!(verify_keyless(manifest, &bundle, &root, &policy(), "acme").is_err());

        // Registry-witnessed mode: the operator opts out explicitly.
        let mut lax = policy();
        lax.require_transparency = false;
        assert!(verify_keyless(manifest, &bundle, &root, &lax, "acme").is_ok());
    }

    /// A test log that mimics **Rekor**: the entry carries the log's canonical
    /// `body` (base64) and the SET signs the RFC 8785 payload over it.
    struct RekorishLog<F: Fn(&[u8]) -> Vec<u8>> {
        sign: F,
        now_ms: u64,
    }

    impl<F: Fn(&[u8]) -> Vec<u8>> TransparencyLog for RekorishLog<F> {
        fn append(&self, _a: &[u8], _s: &str, _p: &str) -> Result<LogEntryRef> {
            let mut entry = LogEntryRef {
                uuid: "0791c72f88cf47564".into(),
                log_index: 7,
                integrated_time_ms: self.now_ms,
                log_id: "fcdb0cf103bc".into(),
                body: "eyJhcGlWZXJzaW9uIjoiMC4wLjEifQ==".into(),
                signed_entry_timestamp: String::new(),
            };
            entry.signed_entry_timestamp = hex::encode((self.sign)(&rekor_set_payload(&entry)));
            Ok(entry)
        }
    }

    /// Sign + verify with a Rekor-shaped log across every pinned-key encoding the
    /// verifier accepts: raw ed25519, and ECDSA P-256 as both SPKI DER and a raw
    /// uncompressed point (Rekor's memory signer is P-256).
    #[test]
    fn rekor_format_set_verifies_with_ed25519_and_p256_pins() {
        let manifest = b"m";
        let (ca_pkcs8, _) = generate_keypair().unwrap();
        let ca = InMemoryCa::from_pkcs8(&ca_pkcs8).unwrap();
        let (eph, _) = generate_keypair().unwrap();

        // — ed25519 log key, pinned raw —
        let (log_pkcs8, log_pub_hex) = generate_keypair().unwrap();
        let log_kp = Ed25519KeyPair::from_pkcs8(&log_pkcs8).unwrap();
        let log = RekorishLog {
            sign: move |m: &[u8]| log_kp.sign(m).as_ref().to_vec(),
            now_ms: NOW + 1000,
        };
        let bundle = sign_keyless(manifest, &identity(), &eph, &ca, Some(&log), NOW).unwrap();
        let root = KeylessRoot {
            ca_public_keys: vec![ca.public_key_hex()],
            log_public_keys: vec![log_pub_hex],
        };
        assert!(verify_keyless(manifest, &bundle, &root, &policy(), "acme").is_ok());

        // Tampering with any SET-covered coordinate breaks it.
        let mut forged = bundle.clone();
        forged.log_entry.as_mut().unwrap().log_index += 1;
        let err = verify_keyless(manifest, &forged, &root, &policy(), "acme").unwrap_err();
        assert!(err.to_string().contains("signed entry timestamp"), "{err}");

        // — ECDSA P-256 log key (what Rekor's memory signer uses) —
        use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
        let rng = ring::rand::SystemRandom::new();
        let p256_pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let p256 =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, p256_pkcs8.as_ref(), &rng)
                .unwrap();
        let point = p256.public_key().as_ref().to_vec(); // 65-byte uncompressed
        let rng2 = ring::rand::SystemRandom::new();
        let log = RekorishLog {
            sign: move |m: &[u8]| p256.sign(&rng2, m).unwrap().as_ref().to_vec(),
            now_ms: NOW + 1000,
        };
        let (eph2, _) = generate_keypair().unwrap();
        let bundle = sign_keyless(manifest, &identity(), &eph2, &ca, Some(&log), NOW).unwrap();

        // Pin as SPKI DER (PEM-decoded form) and as the raw point — both verify.
        let mut spki = vec![
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06,
            0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
        ];
        spki.extend_from_slice(&point);
        for pinned in [hex::encode(&spki), hex::encode(&point)] {
            let root = KeylessRoot {
                ca_public_keys: vec![ca.public_key_hex()],
                log_public_keys: vec![pinned],
            };
            assert!(
                verify_keyless(manifest, &bundle, &root, &policy(), "acme").is_ok(),
                "p256 pin should verify"
            );
        }
    }

    #[test]
    fn bundle_and_root_serde_round_trip() {
        let (bundle, root) = signed(b"m");
        let bundle2: KeylessBundle =
            serde_json::from_str(&serde_json::to_string(&bundle).unwrap()).unwrap();
        assert_eq!(bundle, bundle2);
        let root_json = serde_json::to_string(&root).unwrap();
        let root2: KeylessRoot = serde_json::from_str(&root_json).unwrap();
        assert_eq!(root.ca_public_keys, root2.ca_public_keys);
    }
}
