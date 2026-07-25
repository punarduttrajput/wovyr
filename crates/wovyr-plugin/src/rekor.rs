//! A [Rekor](https://github.com/sigstore/rekor)-backed [`TransparencyLog`] for
//! keyless signing ([ADR-0009](../../docs/17-adr/ADR-0009-keyless-signing.md)),
//! behind the `rekor` cargo feature.
//!
//! [`RekorLog::append`] uploads the signing event as a `rekord` entry (artifact
//! content + ed25519 signature + PKIX-PEM public key) to `POST
//! /api/v1/log/entries`; Rekor re-verifies the signature server-side and returns the
//! witnessed entry (uuid, index, integration time, log id, SET). Run a local stack
//! with `deployment/rekor/` and gate live tests on `WOVYR_REKOR_URL`.
//!
//! Bundles from this log are **fully SET-verifiable offline**: the entry carries
//! Rekor's canonicalized `body`, and
//! [`verify_keyless`](crate::keyless::verify_keyless) reproduces the RFC 8785
//! payload Rekor signs — pin the log's key
//! ([`RekorLog::server_public_key_hex`]) in
//! [`KeylessRoot::log_public_keys`](crate::keyless::KeylessRoot). A dev log with
//! the in-memory signer rotates its key on restart, so re-pin after `compose up`.

use crate::keyless::{LogEntryRef, TransparencyLog};
use crate::verify::hex;
use serde_json::{Value, json};
use wovyr_common::{Error, Result};

/// Minimal standard-alphabet base64 (with padding) for the Rekor wire format —
/// mirrors the crate's hand-rolled hex codec to avoid a dependency for two uses.
mod base64 {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 0x3f] as char);
            out.push(ALPHABET[(n >> 12) as usize & 0x3f] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 0x3f] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 0x3f] as char
            } else {
                '='
            });
        }
        out
    }

    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let val = |c: u8| ALPHABET.iter().position(|&a| a == c).map(|p| p as u32);
        let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let mut out = Vec::with_capacity(clean.len() / 4 * 3);
        for chunk in clean.chunks(4) {
            if chunk.len() != 4 {
                return None;
            }
            let pads = chunk.iter().filter(|&&c| c == b'=').count();
            let mut n: u32 = 0;
            for &c in &chunk[..4 - pads] {
                n = (n << 6) | val(c)?;
            }
            n <<= 6 * pads as u32;
            out.push((n >> 16) as u8);
            if pads < 2 {
                out.push((n >> 8) as u8);
            }
            if pads < 1 {
                out.push(n as u8);
            }
        }
        Some(out)
    }
}

/// DER prefix of a PKIX `SubjectPublicKeyInfo` for an ed25519 key
/// (`SEQUENCE { AlgorithmIdentifier { id-Ed25519 }, BIT STRING (32 bytes) }`).
const ED25519_PKIX_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Wrap a raw 32-byte ed25519 public key as a PKIX PEM `PUBLIC KEY` block (the
/// format Rekor's `x509` signature handler accepts).
pub fn ed25519_pkix_pem(raw_public_key: &[u8]) -> Result<String> {
    if raw_public_key.len() != 32 {
        return Err(Error::invalid(format!(
            "ed25519 public key must be 32 bytes, got {}",
            raw_public_key.len()
        )));
    }
    let mut der = Vec::with_capacity(44);
    der.extend_from_slice(&ED25519_PKIX_PREFIX);
    der.extend_from_slice(raw_public_key);
    let b64 = base64::encode(&der);
    Ok(format!(
        "-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n"
    ))
}

/// A transparency log backed by a Rekor server.
pub struct RekorLog {
    base_url: String,
    http: reqwest::blocking::Client,
}

impl RekorLog {
    /// A client for the Rekor server at `base_url` (e.g. `http://localhost:3000`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::new(),
        }
    }
}

impl TransparencyLog for RekorLog {
    fn append(&self, artifact: &[u8], signature: &str, public_key: &str) -> Result<LogEntryRef> {
        let pem = ed25519_pkix_pem(&hex::decode(public_key)?)?;
        let entry = json!({
            "apiVersion": "0.0.1",
            "kind": "rekord",
            "spec": {
                "data": { "content": base64::encode(artifact) },
                "signature": {
                    "content": base64::encode(&hex::decode(signature)?),
                    "format": "x509",
                    "publicKey": { "content": base64::encode(pem.as_bytes()) },
                },
            },
        });

        let resp = self
            .http
            .post(format!("{}/api/v1/log/entries", self.base_url))
            .json(&entry)
            .send()
            .map_err(|e| Error::provider(format!("rekor unreachable: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .map_err(|e| Error::provider(format!("rekor returned non-JSON: {e}")))?;
        if status.as_u16() != 201 {
            return Err(Error::provider(format!(
                "rekor rejected the entry ({status}): {body}"
            )));
        }

        // Response shape: `{ "<uuid>": { logIndex, integratedTime, logID,
        // verification: { signedEntryTimestamp } } }`.
        let (uuid, e) = body
            .as_object()
            .and_then(|o| o.iter().next())
            .ok_or_else(|| Error::provider("rekor response has no entry"))?;
        let set_b64 = e["verification"]["signedEntryTimestamp"]
            .as_str()
            .unwrap_or_default();
        let set_hex = match base64::decode(set_b64) {
            Some(bytes) => hex::encode(bytes),
            None => String::new(),
        };
        Ok(LogEntryRef {
            uuid: uuid.clone(),
            log_index: e["logIndex"].as_u64().unwrap_or(0),
            // Rekor reports seconds; the bundle carries milliseconds.
            integrated_time_ms: e["integratedTime"].as_u64().unwrap_or(0) * 1000,
            log_id: e["logID"].as_str().unwrap_or_default().to_string(),
            // Rekor's canonicalized entry — what its SET signs over (with the
            // coordinates), so offline verifiers can check the SET.
            body: e["body"].as_str().unwrap_or_default().to_string(),
            signed_entry_timestamp: set_hex,
        })
    }
}

impl RekorLog {
    /// Fetch the log's public key (`GET /api/v1/log/publicKey`, PEM) as SPKI DER
    /// hex — the encoding [`KeylessRoot::log_public_keys`]
    /// (crate::keyless::KeylessRoot) pins for SET verification. Note: a dev log
    /// with the in-memory signer rotates this key on restart.
    pub fn server_public_key_hex(&self) -> Result<String> {
        let pem = self
            .http
            .get(format!("{}/api/v1/log/publicKey", self.base_url))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
            .map_err(|e| Error::provider(format!("rekor public key fetch failed: {e}")))?;
        let der_b64: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<Vec<_>>()
            .join("");
        let der = base64::decode(&der_b64)
            .ok_or_else(|| Error::provider("rekor public key PEM is not valid base64"))?;
        Ok(hex::encode(der))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_all_pad_lengths() {
        for input in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let enc = base64::encode(input);
            assert_eq!(base64::decode(&enc).unwrap(), input, "{enc}");
        }
        assert_eq!(base64::encode(b"foobar"), "Zm9vYmFy"); // RFC 4648 vector
        assert_eq!(base64::encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn pkix_pem_wraps_a_32_byte_key() {
        let pem = ed25519_pkix_pem(&[0u8; 32]).unwrap();
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
        assert!(pem.ends_with("-----END PUBLIC KEY-----\n"));
        assert!(ed25519_pkix_pem(&[0u8; 31]).is_err());
    }
}
