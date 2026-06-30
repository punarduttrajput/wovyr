//! Webhook payload signing: deliveries are HMAC-SHA256 signed so receivers can verify
//! authenticity ([API overview §15](../../docs/09-api/overview.md#15-webhooks--events)).
//!
//! The signature is sent in an `X-Apex-Signature: sha256=<hex>` header; the receiver
//! recomputes it over the raw body with the shared subscription secret.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256 of `body` under `secret`, formatted as the `sha256=<hex>` signature
/// header value.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

/// Constant-time-ish verification that `signature` matches `body` under `secret`.
pub fn verify(secret: &[u8], body: &[u8], signature: &str) -> bool {
    let expected = sign(secret, body);
    // Length-then-byte compare; both are fixed-length hex for a given algorithm.
    expected.len() == signature.len()
        && expected
            .bytes()
            .zip(signature.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rfc4231_test_case_2() {
        // RFC 4231 §4.3: key="Jefe", data="what do ya want for nothing?".
        let sig = sign(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            sig,
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn verify_accepts_valid_and_rejects_tampered() {
        let body = br#"{"type":"project.created"}"#;
        let sig = sign(b"shh", body);
        assert!(verify(b"shh", body, &sig));
        assert!(!verify(b"shh", br#"{"type":"project.deleted"}"#, &sig));
        assert!(!verify(b"wrong", body, &sig));
    }
}
