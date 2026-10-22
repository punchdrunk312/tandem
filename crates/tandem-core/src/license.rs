//! Licence passes. cubecursor.shop signs a short pass for the one computer that is a
//! licence's "main" right now; it is valid for three days and renewed in the
//! background. Other computers check the main's pass when they connect: a pair of
//! computers works when at least one of them shows a valid pass.
//!
//! A pass is `base64url(json) "." base64url(ed25519 signature of the json bytes)`.
//! Only the website holds the signing key; the app only checks.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The website's public key (its private half is the LICENSE_SIGNING_KEY secret).
pub const PUBLIC_KEY: [u8; 32] = [
    0x34, 0xd9, 0x30, 0x66, 0x00, 0x31, 0x9e, 0x2c, 0x76, 0xef, 0xab, 0x63, 0xdd, 0xa2, 0x32, 0x43,
    0x72, 0xbd, 0x54, 0x84, 0x46, 0xdf, 0x09, 0xdf, 0x5a, 0xe3, 0x51, 0x8e, 0x13, 0x1f, 0xd6, 0x76,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pass {
    /// Which licence (an opaque id, not the key).
    pub lic: String,
    /// "paid" or "trial".
    pub kind: String,
    /// The CubeCursor id of the computer the pass is for (what its peers know it by).
    pub dev: String,
    /// A one-way code of that computer's hardware, checked by the computer itself.
    pub hw: String,
    /// Issued / expires, Unix seconds.
    pub iat: u64,
    pub exp: u64,
    /// How many computers the main may be connected to at once.
    pub peers: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invalid {
    Malformed,
    BadSignature,
    Expired,
}

/// Checks a pass's signature and expiry (not who it is for: see `Pass::dev` / `hw`).
pub fn verify(token: &str, key: &[u8; 32], now: u64) -> Result<Pass, Invalid> {
    let (body, sig) = token.trim().split_once('.').ok_or(Invalid::Malformed)?;
    let body = B64.decode(body).map_err(|_| Invalid::Malformed)?;
    let sig = B64.decode(sig).map_err(|_| Invalid::Malformed)?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| Invalid::Malformed)?;
    let vk = VerifyingKey::from_bytes(key).map_err(|_| Invalid::Malformed)?;
    vk.verify(&body, &Signature::from_bytes(&sig))
        .map_err(|_| Invalid::BadSignature)?;
    let pass: Pass = serde_json::from_slice(&body).map_err(|_| Invalid::Malformed)?;
    if now >= pass.exp {
        return Err(Invalid::Expired);
    }
    Ok(pass)
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Signs a pass (tests and tools only; the real key lives on the website).
pub fn sign(pass: &Pass, signing_key: &[u8; 32]) -> String {
    use ed25519_dalek::{Signer, SigningKey};
    let body = serde_json::to_vec(pass).expect("pass serialises");
    let sig = SigningKey::from_bytes(signing_key).sign(&body);
    format!("{}.{}", B64.encode(&body), B64.encode(sig.to_bytes()))
}

/// The public key for a signing key (tests).
pub fn public_of(signing_key: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(signing_key)
        .verifying_key()
        .to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(exp: u64) -> Pass {
        Pass {
            lic: "l1".into(),
            kind: "paid".into(),
            dev: "abc".into(),
            hw: "h".into(),
            iat: 100,
            exp,
            peers: 10,
        }
    }

    #[test]
    fn a_signed_pass_checks_out_until_it_expires() {
        let sk = [7u8; 32];
        let pk = public_of(&sk);
        let t = sign(&pass(1000), &sk);
        assert_eq!(verify(&t, &pk, 500).unwrap().dev, "abc");
        assert_eq!(verify(&t, &pk, 1000), Err(Invalid::Expired));
    }

    #[test]
    fn a_changed_or_foreign_pass_is_refused() {
        let sk = [7u8; 32];
        let pk = public_of(&sk);
        let t = sign(&pass(1000), &sk);
        // Someone edits the body (another computer's id) and keeps the signature.
        let (_, sig) = t.split_once('.').unwrap();
        let mut forged = pass(1000);
        forged.dev = "someone-else".into();
        let body = B64.encode(serde_json::to_vec(&forged).unwrap());
        assert_eq!(
            verify(&format!("{body}.{sig}"), &pk, 500),
            Err(Invalid::BadSignature)
        );
        // Signed with a different key.
        let other = sign(&pass(1000), &[9u8; 32]);
        assert_eq!(verify(&other, &pk, 500), Err(Invalid::BadSignature));
        assert_eq!(verify("garbage", &pk, 500), Err(Invalid::Malformed));
    }

    #[test]
    fn the_built_in_key_is_a_real_key() {
        assert!(VerifyingKey::from_bytes(&PUBLIC_KEY).is_ok());
    }
}
