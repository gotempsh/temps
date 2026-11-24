// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Binds a GitHub App manifest conversion to the signed-in user who started it.
//!
//! GitHub finishes the manifest flow by redirecting the browser to
//! `/webhook/git/github/auth?code=…&state=…`. That request carries no session:
//! the session cookie is `SameSite=Strict` and the redirect is a cross-site
//! navigation from github.com. Without another proof, anyone who obtained a
//! manifest `code` from an App *they* created on GitHub could hand it to that
//! endpoint and have Temps register their App as a Git provider.
//!
//! So the console first asks for a `state` while signed in. The server stores
//! that state (single-use, see `GitProviderManager::issue_github_manifest_state`)
//! and also seals `{state, user, expiry}` with [`CookieCrypto`] into an HttpOnly
//! `SameSite=Lax` cookie (Lax is sent on the top-level redirect back from
//! GitHub). The callback converts the manifest only when the sealed cookie is
//! present, unexpired, and names the same `state` as the query string, and the
//! server then consumes the stored state. The cookie cannot be forged without
//! the server key and a cross-site page cannot plant it, so a state that leaks
//! (it travels through github.com in a URL) is useless in another browser.

use axum::http::{header, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use temps_core::CookieCrypto;

/// How long a started manifest flow stays valid. Creating the App on GitHub
/// takes a few clicks; fifteen minutes is generous without leaving a long-lived
/// credential in the browser.
pub(crate) const MANIFEST_BINDING_TTL_SECS: i64 = 15 * 60;

/// Over https the `__Host-` prefix stops a sibling subdomain (for example a
/// deployed app on `*.example.com`) from planting the cookie. Browsers reject
/// that prefix over plain http, so the unprefixed name is used there.
const SECURE_COOKIE_NAME: &str = "__Host-temps_github_manifest";
const INSECURE_COOKIE_NAME: &str = "temps_github_manifest";

/// Contents sealed into the binding cookie.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ManifestBinding {
    /// The `state` handed to GitHub, echoed back on the callback.
    state: String,
    /// The user who started the flow, recorded for the audit trail.
    user_id: i32,
    /// Unix seconds after which the binding is refused.
    expires_at: i64,
}

/// Why a manifest callback was refused. Each variant says what to do next.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ManifestBindingError {
    #[error(
        "no GitHub App setup was started from this browser; start it again from Git Providers in the Temps console"
    )]
    MissingCookie,
    #[error(
        "the GitHub App setup cookie could not be read; start the setup again from Git Providers"
    )]
    Unreadable,
    #[error(
        "the GitHub App setup started more than 15 minutes ago; start it again from Git Providers"
    )]
    Expired,
    #[error("the GitHub App setup state does not match the one this browser started; start the setup again from Git Providers")]
    StateMismatch,
    #[error("the GitHub callback did not include a state parameter; start the setup again from Git Providers")]
    MissingState,
}

/// Seal `state`, with `user_id`, into the `Set-Cookie` header that binds the
/// manifest flow to this browser.
pub(crate) fn bind_state(
    crypto: &CookieCrypto,
    state: &str,
    user_id: i32,
    secure: bool,
    now_unix: i64,
) -> Result<HeaderValue, String> {
    if state.is_empty() {
        return Err(format!(
            "refusing to bind an empty GitHub manifest state for user {user_id}"
        ));
    }
    let binding = ManifestBinding {
        state: state.to_string(),
        user_id,
        expires_at: now_unix + MANIFEST_BINDING_TTL_SECS,
    };
    let payload = serde_json::to_string(&binding).map_err(|error| {
        format!("failed to encode GitHub manifest binding for user {user_id}: {error}")
    })?;
    let sealed = crypto.encrypt(&payload).map_err(|error| {
        format!("failed to seal GitHub manifest binding for user {user_id}: {error}")
    })?;
    cookie_header(&sealed, MANIFEST_BINDING_TTL_SECS, secure)
}

/// Check the callback's `state` against the sealed cookie in `headers`.
/// Returns the user who started the flow.
pub(crate) fn verify_binding(
    crypto: &CookieCrypto,
    headers: &HeaderMap,
    state: Option<&str>,
    now_unix: i64,
) -> Result<i32, ManifestBindingError> {
    let state = state
        .filter(|value| !value.is_empty())
        .ok_or(ManifestBindingError::MissingState)?;
    let sealed = find_cookie(headers).ok_or(ManifestBindingError::MissingCookie)?;
    let payload = crypto
        .decrypt(&sealed)
        .map_err(|_| ManifestBindingError::Unreadable)?;
    let binding: ManifestBinding =
        serde_json::from_str(&payload).map_err(|_| ManifestBindingError::Unreadable)?;
    if binding.expires_at < now_unix {
        return Err(ManifestBindingError::Expired);
    }
    let matches: bool = binding.state.as_bytes().ct_eq(state.as_bytes()).into();
    if !matches {
        return Err(ManifestBindingError::StateMismatch);
    }
    Ok(binding.user_id)
}

/// `Set-Cookie` headers that remove the binding under both names, so a used
/// or rejected binding cannot be replayed.
pub(crate) fn clear_binding_cookies() -> Vec<HeaderValue> {
    [true, false]
        .into_iter()
        .filter_map(|secure| cookie_header("", 0, secure).ok())
        .collect()
}

fn cookie_header(value: &str, max_age_secs: i64, secure: bool) -> Result<HeaderValue, String> {
    let name = if secure {
        SECURE_COOKIE_NAME
    } else {
        INSECURE_COOKIE_NAME
    };
    // `Lax`, not `Strict`: GitHub returns the browser with a cross-site
    // top-level navigation, and a `Strict` cookie is not sent on it.
    let mut cookie =
        format!("{name}={value}; Max-Age={max_age_secs}; Path=/; HttpOnly; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie)
        .map_err(|error| format!("failed to build GitHub manifest binding cookie: {error}"))
}

fn find_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, value)| {
            (*name == SECURE_COOKIE_NAME || *name == INSECURE_COOKIE_NAME) && !value.is_empty()
        })
        .map(|(_, value)| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn crypto() -> CookieCrypto {
        CookieCrypto::from_bytes(&[42u8; 32])
    }

    /// Headers carrying the cookie value from an issued `Set-Cookie`.
    struct Issued {
        state: String,
        set_cookie: HeaderValue,
    }

    fn issue(crypto: &CookieCrypto, user_id: i32, secure: bool) -> Issued {
        let state = format!("state-for-user-{user_id}");
        let set_cookie = bind_state(crypto, &state, user_id, secure, NOW).expect("bind");
        Issued { state, set_cookie }
    }

    #[test]
    fn an_empty_state_is_never_bound() {
        assert!(bind_state(&crypto(), "", 1, true, NOW).is_err());
    }

    fn headers_from(set_cookie: &HeaderValue) -> HeaderMap {
        let pair = set_cookie
            .to_str()
            .expect("cookie header is ascii")
            .split(';')
            .next()
            .expect("cookie has a name=value pair")
            .to_string();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(&pair).expect("valid"));
        headers
    }

    #[test]
    fn issued_binding_verifies_and_returns_the_starting_user() {
        let crypto = crypto();
        let issued = issue(&crypto, 7, true);
        let headers = headers_from(&issued.set_cookie);

        let user = verify_binding(&crypto, &headers, Some(&issued.state), NOW + 60);

        assert_eq!(user, Ok(7));
    }

    #[test]
    fn secure_cookie_uses_host_prefix_lax_and_httponly() {
        let issued = issue(&crypto(), 1, true);
        let text = issued.set_cookie.to_str().expect("ascii");

        assert!(text.starts_with("__Host-temps_github_manifest="), "{text}");
        assert!(text.contains("SameSite=Lax"), "{text}");
        assert!(text.contains("HttpOnly"), "{text}");
        assert!(text.contains("Secure"), "{text}");
        assert!(text.contains("Path=/"), "{text}");
    }

    #[test]
    fn insecure_cookie_drops_prefix_and_secure_flag() {
        let issued = issue(&crypto(), 1, false);
        let text = issued.set_cookie.to_str().expect("ascii");

        assert!(text.starts_with("temps_github_manifest="), "{text}");
        assert!(!text.contains("Secure"), "{text}");
    }

    #[test]
    fn callback_without_cookie_is_refused() {
        // The attack this guards against: an unauthenticated request that
        // supplies only a manifest code and a state of the attacker's choosing.
        let result = verify_binding(&crypto(), &HeaderMap::new(), Some("anything"), NOW);

        assert_eq!(result, Err(ManifestBindingError::MissingCookie));
    }

    #[test]
    fn callback_without_state_is_refused() {
        let crypto = crypto();
        let issued = issue(&crypto, 1, true);
        let headers = headers_from(&issued.set_cookie);

        assert_eq!(
            verify_binding(&crypto, &headers, None, NOW),
            Err(ManifestBindingError::MissingState)
        );
        assert_eq!(
            verify_binding(&crypto, &headers, Some(""), NOW),
            Err(ManifestBindingError::MissingState)
        );
    }

    #[test]
    fn state_mismatch_is_refused() {
        let crypto = crypto();
        let issued = issue(&crypto, 1, true);
        let headers = headers_from(&issued.set_cookie);

        let result = verify_binding(&crypto, &headers, Some("not-the-issued-state"), NOW);

        assert_eq!(result, Err(ManifestBindingError::StateMismatch));
    }

    #[test]
    fn expired_binding_is_refused() {
        let crypto = crypto();
        let issued = issue(&crypto, 1, true);
        let headers = headers_from(&issued.set_cookie);

        let result = verify_binding(
            &crypto,
            &headers,
            Some(&issued.state),
            NOW + MANIFEST_BINDING_TTL_SECS + 1,
        );

        assert_eq!(result, Err(ManifestBindingError::Expired));
    }

    #[test]
    fn cookie_sealed_with_another_key_is_refused() {
        let issued = issue(&crypto(), 1, true);
        let headers = headers_from(&issued.set_cookie);
        let other = CookieCrypto::from_bytes(&[9u8; 32]);

        let result = verify_binding(&other, &headers, Some(&issued.state), NOW);

        assert_eq!(result, Err(ManifestBindingError::Unreadable));
    }

    #[test]
    fn forged_plaintext_cookie_is_refused() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "temps_github_manifest={\"state\":\"s\",\"user_id\":1,\"expires_at\":9999999999}",
            ),
        );

        let result = verify_binding(&crypto(), &headers, Some("s"), NOW);

        assert_eq!(result, Err(ManifestBindingError::Unreadable));
    }

    #[test]
    fn binding_is_found_among_other_cookies() {
        let crypto = crypto();
        let issued = issue(&crypto, 3, false);
        let pair = issued
            .set_cookie
            .to_str()
            .expect("ascii")
            .split(';')
            .next()
            .expect("pair")
            .to_string();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("session=abc; {pair}; theme=dark")).expect("valid"),
        );

        assert_eq!(
            verify_binding(&crypto, &headers, Some(&issued.state), NOW),
            Ok(3)
        );
    }

    #[test]
    fn clearing_expires_both_cookie_names() {
        let cleared: Vec<String> = clear_binding_cookies()
            .iter()
            .map(|value| value.to_str().expect("ascii").to_string())
            .collect();

        assert_eq!(cleared.len(), 2);
        assert!(cleared
            .iter()
            .any(|c| c.starts_with("__Host-temps_github_manifest=;") && c.contains("Max-Age=0")));
        assert!(cleared
            .iter()
            .any(|c| c.starts_with("temps_github_manifest=;") && c.contains("Max-Age=0")));
    }
}
