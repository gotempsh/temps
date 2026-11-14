// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Names and lookup rules for the console's authentication cookies.
//!
//! A cookie is scoped to a *domain*, not to a host: an app deployed on a
//! sibling subdomain of the console can set a cookie on the parent domain
//! with any name it likes, and the browser then sends both to the console
//! with no way to tell them apart. Planting `session` that way is a session
//! fixation: the console reads the first value that decrypts, so the
//! attacker's cookie can take over the browser's identity.
//!
//! The `__Host-` prefix is the browser-enforced answer. A cookie whose name
//! carries it is only accepted with `Secure`, `Path=/` and **no** `Domain`,
//! which makes it host-only: a sibling app cannot set one for the console's
//! host. The OIDC login-binding cookie already uses the prefix for exactly
//! this reason (see `oidc_handler::login_binding_cookie_name`).
//!
//! Browsers refuse a `__Host-` cookie that is not `Secure`, so a console
//! served over plain HTTP (local development, HTTP-only installs) still gets
//! the unprefixed name — [`issued_name`] picks per request, from the same
//! trusted `is_secure` decision that sets the `Secure` attribute
//! ([`temps_core::secure_cookie_for_request`]).
//!
//! [`read`] therefore accepts both, and **always prefers the prefixed one**:
//! that ordering is what stops a planted parent-domain cookie overriding a
//! host-only one. The unprefixed fallback is what a plain-HTTP install reads
//! normally, and what lets a session issued before this change keep working
//! until its holder next signs in.

use axum::http::{HeaderMap, HeaderValue};
use cookie::Cookie;
use temps_core::CookieCrypto;

/// Browser-enforced host-only cookie prefix (RFC 6265bis §4.1.3).
const HOST_PREFIX: &str = "__Host-";

/// The authenticated browser session.
pub(crate) const SESSION: &str = "session";
/// A pending first-factor sign-in awaiting its second factor.
pub(crate) const MFA_SESSION: &str = "mfa_session";
/// A forced password change after signing in with a temporary password.
pub(crate) const PASSWORD_CHANGE_SESSION: &str = "password_change_session";

/// The name to issue `base` under on a request whose cookies carry `Secure`.
///
/// Must be paired with `Secure`, `Path=/` and no `Domain`, or the browser
/// drops the cookie outright. [`builder`] applies all three.
pub(crate) fn issued_name(base: &str, secure: bool) -> String {
    if secure {
        format!("{HOST_PREFIX}{base}")
    } else {
        base.to_string()
    }
}

/// A cookie builder carrying every attribute the `__Host-` prefix requires,
/// so the name and the attributes can never drift apart.
pub(crate) fn builder(
    base: &str,
    value: impl Into<String>,
    secure: bool,
) -> cookie::CookieBuilder<'static> {
    Cookie::build((issued_name(base, secure), value.into()))
        .http_only(true)
        // `__Host-` requires exactly `/`, and a console behind a reverse
        // proxy under a URL prefix would never be sent a narrower cookie.
        .path("/")
        .same_site(cookie::SameSite::Strict)
        .secure(secure)
}

/// `Set-Cookie` values that remove `base` under **both** names.
///
/// Both, because the browser may hold either: the prefixed cookie this
/// release issues, or an unprefixed one issued before it (or by a
/// plain-HTTP install). Clearing only one would leave the other to be sent
/// on every later request, and a sign-out that leaves a cookie behind is how
/// a logout loop starts.
pub(crate) fn clearing_values(base: &str) -> Vec<HeaderValue> {
    [true, false]
        .into_iter()
        .filter_map(|secure| {
            builder(base, "", secure)
                .max_age(cookie::time::Duration::seconds(0))
                .build()
                .to_string()
                .parse()
                .ok()
        })
        .collect()
}

/// The raw value of `base`, preferring the host-only `__Host-` cookie.
///
/// Returns the first prefixed match if there is one, and only otherwise an
/// unprefixed match, so a parent-domain cookie planted by a sibling app can
/// never displace the console's own.
///
/// Deliberately *no* fallback when a prefixed cookie is present but turns
/// out to be unusable (it no longer decrypts after a key change, say): that
/// fallback is precisely what would let a planted cookie win, since a
/// sibling app cannot alter the host-only one but can always add another.
/// A browser holds at most one `__Host-` cookie per name and host, so there
/// is never a second prefixed candidate to try.
pub(crate) fn read(headers: &HeaderMap, base: &str) -> Option<String> {
    let prefixed = issued_name(base, true);
    let mut fallback = None;
    for cookie_header in headers.get_all("cookie") {
        let Ok(cookie_str) = cookie_header.to_str() else {
            continue;
        };
        for cookie in Cookie::split_parse(cookie_str).filter_map(Result::ok) {
            if cookie.name() == prefixed {
                return Some(cookie.value().to_string());
            }
            if cookie.name() == base && fallback.is_none() {
                fallback = Some(cookie.value().to_string());
            }
        }
    }
    fallback
}

/// [`read`] followed by decryption, which is how every caller uses it.
///
/// `None` when the cookie is absent or does not decrypt — a value the
/// console did not seal is not a session.
pub(crate) fn read_sealed(
    headers: &HeaderMap,
    base: &str,
    crypto: &CookieCrypto,
) -> Option<String> {
    crypto.decrypt(&read(headers, base)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_cookies(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("cookie", value.parse().expect("cookie header"));
        }
        headers
    }

    #[test]
    fn a_secure_request_is_issued_the_host_only_name() {
        assert_eq!(issued_name(SESSION, true), "__Host-session");
        assert_eq!(issued_name(SESSION, false), "session");
    }

    #[test]
    fn the_issued_cookie_carries_every_attribute_the_prefix_requires() {
        let cookie = builder(SESSION, "token", true)
            .max_age(cookie::time::Duration::days(7))
            .build();
        let text = cookie.to_string();

        // Browsers enforce these together for a `__Host-` cookie; without
        // them the cookie is dropped and the user cannot sign in at all.
        assert!(text.starts_with("__Host-session="), "{text}");
        assert!(text.contains("Secure"), "{text}");
        assert!(text.contains("Path=/"), "{text}");
        assert!(!text.contains("Domain"), "{text}");
        assert!(text.contains("HttpOnly"), "{text}");
        assert!(text.contains("SameSite=Strict"), "{text}");
    }

    #[test]
    fn a_plain_http_console_keeps_the_unprefixed_name() {
        // Browsers refuse `__Host-` without `Secure`, so an HTTP-only
        // install must not be issued one.
        let text = builder(SESSION, "token", false).build().to_string();
        assert!(text.starts_with("session="), "{text}");
        assert!(!text.contains("Secure"), "{text}");
    }

    #[test]
    fn the_host_only_cookie_wins_over_a_planted_one() {
        // A sibling app sets `session` on the parent domain; the browser
        // sends both, in whichever order it likes.
        assert_eq!(
            read(
                &with_cookies(&["session=planted; __Host-session=real"]),
                SESSION
            )
            .as_deref(),
            Some("real")
        );
        assert_eq!(
            read(
                &with_cookies(&["__Host-session=real; session=planted"]),
                SESSION
            )
            .as_deref(),
            Some("real")
        );
        // Also across separate Cookie headers.
        assert_eq!(
            read(
                &with_cookies(&["session=planted", "__Host-session=real"]),
                SESSION
            )
            .as_deref(),
            Some("real")
        );
    }

    #[test]
    fn an_unprefixed_cookie_is_read_when_it_is_the_only_one() {
        assert_eq!(
            read(&with_cookies(&["session=legacy"]), SESSION).as_deref(),
            Some("legacy")
        );
        assert_eq!(read(&with_cookies(&["other=x"]), SESSION), None);
        assert_eq!(read(&HeaderMap::new(), SESSION), None);
    }

    #[test]
    fn a_name_is_never_confused_with_another_cookies_name() {
        let headers = with_cookies(&["__Host-mfa_session=challenge; __Host-session=real"]);
        assert_eq!(read(&headers, SESSION).as_deref(), Some("real"));
        assert_eq!(read(&headers, MFA_SESSION).as_deref(), Some("challenge"));
        assert_eq!(read(&headers, PASSWORD_CHANGE_SESSION), None);
        // `session` must not match the tail of `mfa_session`.
        assert_eq!(read(&with_cookies(&["mfa_session=c"]), SESSION), None);
    }

    #[test]
    fn clearing_removes_both_names() {
        let cleared: Vec<String> = clearing_values(SESSION)
            .iter()
            .map(|value| value.to_str().expect("ascii").to_string())
            .collect();
        assert_eq!(cleared.len(), 2);
        assert!(cleared
            .iter()
            .any(|value| value.starts_with("__Host-session=")));
        assert!(cleared
            .iter()
            .any(|value| value.starts_with("session=") && !value.contains("Secure")));
        for value in &cleared {
            assert!(value.contains("Max-Age=0"), "{value}");
        }
    }

    #[test]
    fn reading_a_sealed_cookie_requires_this_consoles_seal() {
        let crypto = CookieCrypto::from_bytes(&[7; 32]);
        let sealed = crypto.encrypt("session-token").expect("seal");

        assert_eq!(
            read_sealed(
                &with_cookies(&[&format!("__Host-session={sealed}")]),
                SESSION,
                &crypto,
            )
            .as_deref(),
            Some("session-token")
        );
        assert_eq!(
            read_sealed(&with_cookies(&["__Host-session=forged"]), SESSION, &crypto),
            None
        );
    }
}
