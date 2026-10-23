//! Updates: cubecursor.shop says which version is newest for each system, with a link
//! to its installer and the installer's SHA-256, all signed with the website's key (the
//! same key that signs licence passes, kept apart by the text signed). The app only
//! installs what it can prove came from there and arrived whole.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// What the website answers when there is something newer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    /// What's new, a few short lines.
    #[serde(default)]
    pub notes: String,
    /// The installer (https only).
    pub url: String,
    /// Its SHA-256, as lowercase hex.
    pub sha256: String,
    /// base64url Ed25519 signature of `signed_text`.
    pub sig: String,
}

/// What the signature covers.
pub fn signed_text(os: &str, r: &Release) -> String {
    format!("cubecursor-update-v1\n{os}\n{}\n{}\n{}", r.version, r.url, r.sha256)
}

/// Is `r` really from cubecursor.shop, for this system, and well formed?
pub fn verify(os: &str, r: &Release, key: &[u8; 32]) -> bool {
    let shaped = r.url.starts_with("https://")
        && r.url.len() <= 1024
        && r.sha256.len() == 64
        && r.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && parse(&r.version).is_some();
    if !shaped {
        return false;
    }
    let Ok(sig) = B64.decode(r.sig.trim()) else {
        return false;
    };
    let Ok(sig): Result<[u8; 64], _> = sig.try_into() else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(key) else {
        return false;
    };
    vk.verify(signed_text(os, r).as_bytes(), &Signature::from_bytes(&sig)).is_ok()
}

/// "1.2.3" (a leading "v" and anything after a "-" or "+" ignored).
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let a = it.next()??;
    let b = it.next().unwrap_or(Some(0))?;
    let c = it.next().unwrap_or(Some(0))?;
    Some((a, b, c))
}

/// Is `candidate` a newer version than `current`?
pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn release(key: &SigningKey, os: &str) -> Release {
        let mut r = Release {
            version: "0.2.0".into(),
            notes: "Second monitor mode".into(),
            url: "https://cubecursor.shop/download/CubeCursor_0.2.0_x64-setup.exe".into(),
            sha256: "a".repeat(64),
            sig: String::new(),
        };
        r.sig = B64.encode(key.sign(signed_text(os, &r).as_bytes()).to_bytes());
        r
    }

    #[test]
    fn a_signed_release_checks_out_and_a_changed_one_does_not() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pubkey = key.verifying_key().to_bytes();
        let r = release(&key, "windows");
        assert!(verify("windows", &r, &pubkey));
        // For another system, another link, another file: no.
        assert!(!verify("macos", &r, &pubkey));
        assert!(!verify("windows", &Release { url: "https://evil.example/x.exe".into(), ..r.clone() }, &pubkey));
        assert!(!verify("windows", &Release { sha256: "b".repeat(64), ..r.clone() }, &pubkey));
        // Not over https: no, even if signed.
        let mut plain = release(&key, "windows");
        plain.url = "http://cubecursor.shop/x.exe".into();
        plain.sig = B64.encode(key.sign(signed_text("windows", &plain).as_bytes()).to_bytes());
        assert!(!verify("windows", &plain, &pubkey));
        // Another key: no.
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes();
        assert!(!verify("windows", &r, &other));
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("v1.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("banana", "0.1.0"));
    }
}

#[cfg(test)]
mod cross {
    /// A release signed by the website's code (Node), from CC_SIG_JSON (see the site's lib/release.ts).
    #[test]
    #[ignore]
    fn the_websites_signature_checks_out() {
        let raw = std::fs::read_to_string(std::env::var("CC_SIG_JSON").unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let r: super::Release = serde_json::from_value(v.clone()).unwrap();
        let hex = v["pub"].as_str().unwrap();
        let key: Vec<u8> = (0..32).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()).collect();
        assert!(super::verify("windows", &r, &key.try_into().unwrap()));
    }
}
