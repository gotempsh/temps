// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a browser sent this request from the console's own origin.
//!
//! `SameSite=Strict` on the session cookie scopes it to the *registrable
//! domain*, not to one host: a page served from a sibling subdomain of the
//! console is same-site, so the browser attaches the session cookie to its
//! requests. A JSON content-type preflight does not help either — a request
//! with no body, a `multipart/form-data` body or a plain-text body is a
//! "simple" request and needs no preflight. So every state-changing request
//! that authenticated through the session cookie must additionally prove it
//! came from the console's own origin.
//!
//! [`request_is_same_origin`] is the single decision used for that, and the
//! equivalent of [`crate::websocket_origin::validate_websocket_origin`] for
//! ordinary HTTP methods.

use axum::http::{header, HeaderMap, Method};

/// Whether `method` can change state, i.e. needs the same-origin proof.
///
/// `GET`, `HEAD` and `OPTIONS` are the methods a browser may issue
/// cross-origin without consent, and the ones every cache and crawler
/// replays; handlers behind them must not have side effects anyway. `TRACE`
/// is included because it cannot carry one either.
pub fn is_state_changing_method(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// Whether this request came from the server's own origin.
///
/// Two signals, in order:
///
/// 1. **`Sec-Fetch-Site`**, when present, decides on its own. It is a
///    forbidden header, so no page can forge or suppress it, and it is the
///    only signal that distinguishes `same-origin` from `same-site` — exactly
///    the sibling-subdomain case `SameSite=Strict` lets through.
/// 2. **`Origin`**, otherwise, compared with the request's own `Host`.
///    Authority only (host and port, exactly, case-insensitively): TLS may
///    terminate in front of Temps, so the scheme the server sees is not
///    necessarily the one the browser used, and comparing it would 403 every
///    console write behind a reverse proxy that forwards no
///    `X-Forwarded-Proto`. The sibling-subdomain and other-port cases are
///    both a different authority, so they are still refused. An `Origin` that
///    cannot be compared — `null`, unparsable, carrying userinfo, or with no
///    `Host` to compare against — counts as cross-origin.
///
/// **Neither header present: same origin.** Browsers attach `Origin` to every
/// request whose method is not `GET`/`HEAD`, including form submissions and
/// `sendBeacon`, so a state-changing request without one did not come from a
/// page; it came from a non-browser client (the CLI, an acceptance test, an
/// in-process replay), which supplies its own cookies and cannot be driven by
/// a hostile site. This matches
/// [`crate::websocket_origin::validate_websocket_origin`] and the analytics
/// ingest classifier, so all three agree on what "same origin" means.
pub fn request_is_same_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site
            .to_str()
            .is_ok_and(|site| site.trim().eq_ignore_ascii_case("same-origin"));
    }

    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin.to_str().ok().and_then(origin_authority) else {
        return false;
    };
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| origin.eq_ignore_ascii_case(host.trim()))
}

/// The `host[:port]` part of an `Origin` value, exactly as written.
///
/// Compared as written rather than through a URL parser because
/// `Url::port()` drops a scheme's default port, which would make
/// `https://host:443` look portless and never match `Host: host:443`.
///
/// `None` for anything that is not a bare `scheme://host[:port]` origin — an
/// opaque `null`, a non-HTTP scheme, or one carrying userinfo — so it can
/// never match a `Host` header.
pub fn origin_authority(origin: &str) -> Option<&str> {
    let (scheme, rest) = origin.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(authority)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                value.parse().expect("header value"),
            );
        }
        headers
    }

    #[test]
    fn state_changing_methods_are_everything_but_the_safe_four() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
            assert!(!is_state_changing_method(&method), "{method}");
        }
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::CONNECT,
        ] {
            assert!(is_state_changing_method(&method), "{method}");
        }
    }

    #[test]
    fn sec_fetch_site_decides_on_its_own() {
        // Same-site is the sibling-subdomain case `SameSite=Strict` allows.
        for site in ["same-site", "cross-site", "none", "", "garbage"] {
            assert!(
                !request_is_same_origin(&headers(&[
                    ("sec-fetch-site", site),
                    ("origin", "https://console.example"),
                    ("host", "console.example"),
                ])),
                "{site}"
            );
        }
        assert!(request_is_same_origin(&headers(&[
            ("sec-fetch-site", "same-origin"),
            ("host", "console.example"),
        ])));
        assert!(request_is_same_origin(&headers(&[(
            "sec-fetch-site",
            "Same-Origin"
        )])));
    }

    #[test]
    fn sec_fetch_site_outranks_a_matching_origin() {
        // A sibling page cannot forge the header, so `same-site` is refused
        // even though it may set any `Origin` it likes.
        assert!(!request_is_same_origin(&headers(&[
            ("sec-fetch-site", "same-site"),
            ("origin", "https://console.example"),
            ("host", "console.example"),
        ])));
    }

    #[test]
    fn matching_origin_authority_is_same_origin() {
        assert!(request_is_same_origin(&headers(&[
            ("origin", "https://console.example"),
            ("host", "console.example"),
        ])));
        assert!(request_is_same_origin(&headers(&[
            ("origin", "https://CONSOLE.example:8443"),
            ("host", "console.example:8443"),
        ])));
        // TLS terminated in front of Temps: the scheme the server sees is
        // not the browser's, so only the authority is compared.
        assert!(request_is_same_origin(&headers(&[
            ("origin", "https://console.example"),
            ("host", "console.example"),
        ])));
    }

    #[test]
    fn sibling_subdomain_and_other_port_are_cross_origin() {
        assert!(!request_is_same_origin(&headers(&[
            ("origin", "https://app.console.example"),
            ("host", "console.example"),
        ])));
        assert!(!request_is_same_origin(&headers(&[
            ("origin", "https://app.example"),
            ("host", "console.example"),
        ])));
        assert!(!request_is_same_origin(&headers(&[
            ("origin", "http://localhost:5173"),
            ("host", "localhost:8080"),
        ])));
        // An explicit default port is not the same authority as none.
        assert!(!request_is_same_origin(&headers(&[
            ("origin", "https://console.example:443"),
            ("host", "console.example"),
        ])));
    }

    #[test]
    fn uncomparable_origins_are_cross_origin() {
        for origin in [
            "null",
            "",
            "console.example",
            "file:///etc/passwd",
            "https://user@console.example",
            "javascript:alert(1)",
        ] {
            assert!(
                !request_is_same_origin(&headers(&[
                    ("origin", origin),
                    ("host", "console.example"),
                ])),
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_with_no_host_to_compare_is_cross_origin() {
        assert!(!request_is_same_origin(&headers(&[(
            "origin",
            "https://console.example"
        )])));
    }

    #[test]
    fn neither_header_is_a_non_browser_client_and_passes() {
        assert!(request_is_same_origin(&HeaderMap::new()));
        assert!(request_is_same_origin(&headers(&[(
            "host",
            "console.example"
        )])));
    }

    #[test]
    fn origin_path_query_and_fragment_are_ignored_after_the_authority() {
        // Browsers never send these, but a hand-written client might; the
        // authority is what decides either way.
        assert!(request_is_same_origin(&headers(&[
            ("origin", "https://console.example/path"),
            ("host", "console.example"),
        ])));
        assert!(!request_is_same_origin(&headers(&[
            ("origin", "https://app.example/?x=console.example"),
            ("host", "console.example"),
        ])));
    }
}
