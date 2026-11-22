// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The signed access cookie of a protected status page.
//!
//! Value: `v1.{page id}.{password version}.{expiry, unix seconds}.{hmac}`,
//! the HMAC-SHA256 of everything before it under a key derived from the
//! install's master key. It names its page, so it opens no other page, and
//! its version, so changing the password invalidates it. It holds nothing
//! secret: the password itself never leaves the unlock request.

use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use temps_core::public_status_page::STATUS_ACCESS_COOKIE;
use temps_core::{EncryptionService, UtcDateTime};

type HmacSha256 = Hmac<Sha256>;

/// How long an unlocked page stays open in a browser.
pub const ACCESS_LIFETIME: chrono::Duration = chrono::Duration::days(7);

/// Key domain for [`EncryptionService::derive_subkey`].
const KEY_DOMAIN: &str = "status-page-access-cookie-v1";

#[derive(Clone)]
pub struct AccessKey {
    key: [u8; 32],
}

impl AccessKey {
    pub fn new(encryption: &EncryptionService) -> Self {
        Self {
            key: encryption.derive_subkey(KEY_DOMAIN),
        }
    }

    fn mac(&self) -> HmacSha256 {
        #[allow(
            clippy::expect_used,
            reason = "HMAC accepts keys of any length, so new_from_slice cannot fail"
        )]
        HmacSha256::new_from_slice(&self.key).expect("HMAC takes a key of any size")
    }

    /// A cookie value for `status_page_id` at `version`, valid until
    /// `now + ACCESS_LIFETIME`.
    pub fn mint(&self, status_page_id: i32, version: i32, now: UtcDateTime) -> String {
        let expires = (now + ACCESS_LIFETIME).timestamp();
        let payload = format!("v1.{status_page_id}.{version}.{expires}");
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        format!("{payload}.{}", hex::encode(mac.finalize().into_bytes()))
    }

    /// Whether `value` opens `status_page_id` at `version` at `now`.
    pub fn verify(&self, value: &str, status_page_id: i32, version: i32, now: UtcDateTime) -> bool {
        let Some((payload, signature)) = value.rsplit_once('.') else {
            return false;
        };
        let mut parts = payload.split('.');
        let (Some("v1"), Some(page), Some(ver), Some(expires), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return false;
        };
        if page.parse::<i32>().ok() != Some(status_page_id)
            || ver.parse::<i32>().ok() != Some(version)
        {
            return false;
        }
        let Ok(expires) = expires.parse::<i64>() else {
            return false;
        };
        if expires <= now.timestamp() {
            return false;
        }
        let Ok(signature) = hex::decode(signature) else {
            return false;
        };
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature).is_ok()
    }
}

/// Every value of the access cookie in `headers` (a browser may send more
/// than one `Cookie` header, and the same name at several paths).
pub fn access_cookie_values(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|field| field.split(';'))
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == STATUS_ACCESS_COOKIE).then_some(value.trim_matches('"'))
        })
}

/// The `Set-Cookie` value for an unlocked page. `path` scopes it to the
/// page (`/api/status/{slug}` or `/` on a custom hostname).
pub fn set_cookie_header(value: &str, path: &str, secure: bool) -> String {
    format!(
        "{STATUS_ACCESS_COOKIE}={value}; Path={path}; Max-Age={}; HttpOnly; SameSite=Lax{}",
        ACCESS_LIFETIME.num_seconds(),
        if secure { "; Secure" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn now() -> UtcDateTime {
        Utc.with_ymd_and_hms(2026, 3, 2, 10, 0, 0)
            .single()
            .expect("valid time")
    }

    fn key(password: &str) -> AccessKey {
        AccessKey::new(&EncryptionService::new_from_password(password))
    }

    #[test]
    fn a_minted_cookie_opens_its_page_at_its_version_until_it_expires() {
        let key = key("access-tests");
        let value = key.mint(7, 3, now());
        assert!(
            temps_core::public_status_page::is_well_formed_access_cookie(&value),
            "{value}"
        );
        assert!(key.verify(&value, 7, 3, now()));
        assert!(!key.verify(&value, 8, 3, now()), "another page");
        assert!(!key.verify(&value, 7, 4, now()), "the password changed");
        assert!(
            !key.verify(&value, 7, 3, now() + ACCESS_LIFETIME),
            "expired"
        );
    }

    #[test]
    fn a_forged_or_foreign_cookie_is_refused() {
        let ours = key("access-tests");
        let theirs = key("another-install");
        assert!(!ours.verify(&theirs.mint(7, 3, now()), 7, 3, now()));
        let value = ours.mint(7, 3, now());
        let (payload, _) = value.rsplit_once('.').expect("signed");
        let tampered = payload.replace(".3.", ".4.");
        assert!(!ours.verify(&format!("{tampered}.{}", "00".repeat(32)), 7, 4, now()));
        for junk in [
            "",
            "v1",
            "v1.7.3.x.00",
            "v2.7.3.99999999999.00",
            "v1.7.3.1.2.3",
        ] {
            assert!(!ours.verify(junk, 7, 3, now()), "{junk}");
        }
    }

    #[test]
    fn cookie_values_are_found_among_others_and_the_header_is_scoped() {
        let mut headers = HeaderMap::new();
        headers.append(
            "cookie",
            "a=1; temps_status_access=one".parse().expect("header"),
        );
        headers.append(
            "cookie",
            "temps_status_access=\"two\"".parse().expect("header"),
        );
        let values: Vec<&str> = access_cookie_values(&headers).collect();
        assert_eq!(values, ["one", "two"]);
        let header = set_cookie_header("v", "/api/status/ab", true);
        assert!(header.contains("Path=/api/status/ab"));
        assert!(header.contains("HttpOnly") && header.contains("SameSite=Lax"));
        assert!(header.ends_with("; Secure"));
        assert!(!set_cookie_header("v", "/", false).contains("Secure"));
    }
}
