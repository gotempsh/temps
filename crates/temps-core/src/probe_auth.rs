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

#[cfg(test)]
mod tests {
    use super::*;
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
