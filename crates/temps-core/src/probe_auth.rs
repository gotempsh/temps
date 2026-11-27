// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host-bound, short-lived authorization to expose diagnostic response markers.
//! This never authorizes application access. The installation key stays in memory;
//! callers send only a domain-separated MAC, never the key.
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub const HEADER: &str = "x-origin-probe-authorization";

fn mac(key: &str, host: &str, issued: u64) -> Option<Hmac<Sha256>> {
    if key.is_empty() {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(b"temps-response-probe-v1\0");
    mac.update(host.to_ascii_lowercase().as_bytes());
    mac.update(b"\0");
    mac.update(&issued.to_be_bytes());
    Some(mac)
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub fn sign(key: &str, host: &str, issued: u64) -> String {
    mac(key, host, issued).map_or_else(String::new, |mac| {
        format!("{issued}.{}", hex::encode(mac.finalize().into_bytes()))
    })
}

pub fn verify(key: &str, host: &str, token: &str, now: u64) -> bool {
    if token.len() > 85 {
        return false;
    }
    let Some((issued, signature)) = token.split_once('.') else {
        return false;
    };
    let Ok(issued) = issued.parse::<u64>() else {
        return false;
    };
    if issued > now || now - issued > 30 {
        return false;
    }
    let Ok(signature) = hex::decode(signature) else {
        return false;
    };
    mac(key, host, issued).is_some_and(|mac| mac.verify_slice(&signature).is_ok())
}

/// Where this machine asks its own proxy who it is (host facts): answered
/// only to a signed probe, so it says nothing to anyone else.
pub const PROXY_IDENTITY_PATH: &str = "/.well-known/temps/proxy-identity";
/// The host a proxy identity probe is signed for and sent to.
pub const PROXY_IDENTITY_HOST: &str = "proxy-identity.temps.invalid";
/// The random challenge a proxy identity probe carries.
pub const PROXY_IDENTITY_NONCE_HEADER: &str = "x-temps-proxy-identity-nonce";
/// The proxy's answer: a MAC of the challenge under the installation key.
pub const PROXY_IDENTITY_HEADER: &str = "x-temps-proxy-identity";

fn identity_mac(key: &str, nonce: &str) -> Option<Hmac<Sha256>> {
    // A challenge is 32 to 128 hex digits: nothing else is ever signed, so
    // the answer cannot be steered into any other token's shape.
    if key.is_empty()
        || !(32..=128).contains(&nonce.len())
        || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(b"temps-proxy-identity-v1\0");
    mac.update(nonce.to_ascii_lowercase().as_bytes());
    Some(mac)
}

/// A fresh challenge for a proxy identity probe.
pub fn identity_nonce() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// The proxy's answer to challenge `nonce`; `None` for a challenge that is
/// not one.
pub fn identity_answer(key: &str, nonce: &str) -> Option<String> {
    identity_mac(key, nonce).map(|mac| hex::encode(mac.finalize().into_bytes()))
}

/// Whether `answer` proves the proxy that gave it holds the installation
/// key: only Temps' own proxy can answer the challenge.
pub fn verify_identity(key: &str, nonce: &str, answer: &str) -> bool {
    let Ok(answer) = hex::decode(answer) else {
        return false;
    };
    identity_mac(key, nonce).is_some_and(|mac| mac.verify_slice(&answer).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_installation_key_answers_an_identity_challenge() {
        let nonce = identity_nonce();
        let answer = identity_answer("test-installation-key", &nonce).unwrap();
        assert!(verify_identity("test-installation-key", &nonce, &answer));
        assert!(!verify_identity("another-key", &nonce, &answer));
        assert!(!verify_identity(
            "test-installation-key",
            &identity_nonce(),
            &answer
        ));
        assert!(!verify_identity("test-installation-key", &nonce, "not-hex"));
        // Not a challenge: too short, not hex, or no key.
        assert_eq!(identity_answer("test-installation-key", "abc"), None);
        assert_eq!(
            identity_answer("test-installation-key", &"z".repeat(64)),
            None
        );
        assert_eq!(identity_answer("", &nonce), None);
        // An identity answer is never a probe token for any host.
        assert!(!verify(
            "test-installation-key",
            PROXY_IDENTITY_HOST,
            &answer,
            now()
        ));
    }

    #[test]
    fn probes_are_host_bound_expire_and_require_the_installation_key() {
        let token = sign("test-installation-key", "app.example.test", 100);
        assert!(verify(
            "test-installation-key",
            "APP.example.test",
            &token,
            130
        ));
        for (key, host, time) in [
            ("wrong", "app.example.test", 100),
            ("test-installation-key", "other.example.test", 100),
            ("test-installation-key", "app.example.test", 131),
            ("test-installation-key", "app.example.test", 99),
        ] {
            assert!(!verify(key, host, &token, time));
        }
        assert!(!verify(
            "",
            "app.example.test",
            &sign("", "app.example.test", 100),
            100
        ));
        assert!(!verify(
            "test-installation-key",
            "app.example.test",
            "Temps-Reachability/1.0",
            100
        ));
    }
}
