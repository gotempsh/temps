// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire contract between the proxy and the console for public status pages
//! served on a custom hostname (#1412).
//!
//! The proxy owns the custom hostname: it allows a fixed set of paths, each
//! with its own methods ([`StatusHostPath::allows_method`]), rewrites them to
//! the page's slug routes on the console's public-only ingress
//! listener, and tags the request with [`STATUS_HOST_HEADER`]. The console
//! serves the page only if that hostname is still attached and verified, so a
//! removed domain or an unpublished page answers 404 immediately, before the
//! proxy's route table has reloaded.
//!
//! Both sides answer misses with the byte-identical bodies and headers below,
//! so a visitor cannot tell an unknown, unverified, unpublished or removed
//! hostname, or a path that is not served, apart.

/// Request header the proxy sets to the normalized custom hostname. The proxy
/// strips any client-supplied copy from every request first. The header can
/// only ever narrow what the console serves, never widen it.
pub const STATUS_HOST_HEADER: &str = "x-temps-status-host";

/// `Content-Security-Policy` for every public status-page response. No
/// scripts at all; images only over HTTPS, for the logo an admin may set
/// (sent with `no-referrer`, so the image host never learns the page URL).
/// Forms may post only to the page's own origin: the subscribe form and the
/// confirm and unsubscribe buttons (ADR 108 D2).
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; \
     img-src https:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Headers sent with every public status-page response, hit or miss.
pub const SECURITY_HEADERS: [(&str, &str); 6] = [
    ("cache-control", "no-store"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
    ("content-security-policy", CONTENT_SECURITY_POLICY),
    ("x-robots-tag", "noindex, nofollow"),
];

/// Body of every HTML 404 for a status page.
pub const NOT_FOUND_HTML: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"robots\" content=\"noindex,nofollow\"><title>Status page not found</title></head>\
<body style=\"font-family:system-ui,sans-serif;max-width:640px;margin:80px auto;padding:0 20px\">\
<h1>Status page not found</h1><p>This status page does not exist or is no longer published.</p>\
</body></html>";

/// Body of every JSON 404 for a status page (`application/problem+json`).
pub const NOT_FOUND_JSON: &str = r#"{"type":"about:blank","title":"Status Page Not Found","status":404,"detail":"This status page does not exist or is no longer published."}"#;

/// Cookie that proves a visitor typed a password-protected page's password
/// (ADR 108 D2). On a custom hostname the proxy strips every other cookie
/// and forwards only this one, and only when its value is well formed.
pub const STATUS_ACCESS_COOKIE: &str = "temps_status_access";

/// Longest access cookie value accepted.
pub const STATUS_ACCESS_COOKIE_MAX_LEN: usize = 160;

/// Whether `value` has the shape of an access cookie: short, and only the
/// characters the console mints (`[A-Za-z0-9._-]`). Its signature is checked
/// by the console; this only keeps anything else from being forwarded.
pub fn is_well_formed_access_cookie(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= STATUS_ACCESS_COOKIE_MAX_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Length of a subscription token in a path: 64 lowercase hex characters.
pub const SUBSCRIPTION_TOKEN_LEN: usize = 64;

fn is_subscription_token(candidate: &str) -> bool {
    candidate.len() == SUBSCRIPTION_TOKEN_LEN
        && candidate
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What replaces a subscription token in a logged path.
pub const REDACTED_TOKEN: &str = "[REDACTED]";

/// `path` with the token of a subscription link blanked, for logs.
///
/// Confirmation and unsubscribe links carry their capability in the path
/// (`/confirm/{token}`, `/unsubscribe/{token}` on a custom hostname, and
/// `/api/status/{slug}/confirm/{token}` and `.../unsubscribe/{token}` on the
/// console), so a stored access log would let anyone who reads it confirm or
/// cancel someone else's subscription. Only those two routes are rewritten;
/// every other path is returned borrowed and unchanged. A query string, if
/// one is attached, is kept as it is.
pub fn redact_subscription_token_path(path: &str) -> std::borrow::Cow<'_, str> {
    let (route, query) = match path.find('?') {
        Some(at) => path.split_at(at),
        None => (path, ""),
    };
    let token_at = ["/confirm/", "/unsubscribe/"].iter().find_map(|marker| {
        let at = route.rfind(marker)?;
        let prefix = &route[..at];
        let token = &route[at + marker.len()..];
        let is_status_route = prefix.is_empty()
            || prefix
                .strip_prefix("/api/status/")
                .is_some_and(|slug| !slug.is_empty() && !slug.contains('/'));
        (is_status_route && is_subscription_token(token)).then_some(at + marker.len())
    });
    match token_at {
        Some(at) => std::borrow::Cow::Owned(format!("{}{REDACTED_TOKEN}{query}", &route[..at])),
        None => std::borrow::Cow::Borrowed(path),
    }
}

/// What a request path on a custom status-page hostname maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusHostPath {
    /// `/`: the HTML page.
    Page,
    /// `/summary.json`: the JSON page.
    Summary,
    /// `/feed.atom`: the page's Atom feed (ADR 108 D2).
    Feed,
    /// `/subscribe`: the subscribe form's post (ADR 108 D2).
    Subscribe,
    /// `/unlock`: the password form's post of a protected page (ADR 108 D2).
    Unlock,
    /// `/confirm/{token}`: a subscription's confirmation link.
    Confirm(String),
    /// `/unsubscribe/{token}`: a subscriber's unsubscribe link.
    Unsubscribe(String),
}

impl StatusHostPath {
    /// The only paths a custom hostname serves. Anything else, including
    /// `/index.html`, `//`, encoded variants, trailing slashes and a token
    /// that is not 64 lowercase hex characters, is `None`.
    pub fn parse(path: &str) -> Option<Self> {
        match path {
            "/" => return Some(Self::Page),
            "/summary.json" => return Some(Self::Summary),
            "/feed.atom" => return Some(Self::Feed),
            "/subscribe" => return Some(Self::Subscribe),
            "/unlock" => return Some(Self::Unlock),
            _ => {}
        }
        if let Some(token) = path.strip_prefix("/confirm/") {
            return is_subscription_token(token).then(|| Self::Confirm(token.to_string()));
        }
        if let Some(token) = path.strip_prefix("/unsubscribe/") {
            return is_subscription_token(token).then(|| Self::Unsubscribe(token.to_string()));
        }
        None
    }

    /// Whether `method` is served on this path: the page views are read-only,
    /// the subscribe form only posts, and a link answers `GET` with a button
    /// that posts back to it.
    pub fn allows_method(&self, method: &str) -> bool {
        match self {
            Self::Page | Self::Summary | Self::Feed => matches!(method, "GET" | "HEAD"),
            Self::Subscribe | Self::Unlock => method == "POST",
            Self::Confirm(_) | Self::Unsubscribe(_) => matches!(method, "GET" | "HEAD" | "POST"),
        }
    }

    /// Path on the console's public router (mounted under `/api`) that serves
    /// this view of the page `slug`. `slug` is server-generated lowercase hex.
    pub fn console_path(&self, slug: &str) -> String {
        match self {
            Self::Page => format!("/api/status/{slug}"),
            Self::Summary => format!("/api/status/{slug}/summary.json"),
            Self::Feed => format!("/api/status/{slug}/feed.atom"),
            Self::Subscribe => format!("/api/status/{slug}/subscribe"),
            Self::Unlock => format!("/api/status/{slug}/unlock"),
            Self::Confirm(token) => format!("/api/status/{slug}/confirm/{token}"),
            Self::Unsubscribe(token) => format!("/api/status/{slug}/unsubscribe/{token}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_tokens_are_blanked_in_logged_paths_and_nothing_else_is() {
        let token = "c".repeat(SUBSCRIPTION_TOKEN_LEN);
        for (path, logged) in [
            (
                format!("/confirm/{token}"),
                "/confirm/[REDACTED]".to_string(),
            ),
            (
                format!("/unsubscribe/{token}"),
                "/unsubscribe/[REDACTED]".to_string(),
            ),
            (
                format!("/api/status/ab12/confirm/{token}"),
                "/api/status/ab12/confirm/[REDACTED]".to_string(),
            ),
            (
                format!("/api/status/ab12/unsubscribe/{token}?x=1"),
                "/api/status/ab12/unsubscribe/[REDACTED]?x=1".to_string(),
            ),
        ] {
            assert_eq!(redact_subscription_token_path(&path), logged, "{path}");
        }
        for path in [
            "/".to_string(),
            "/api/status/ab12".to_string(),
            "/api/status/ab12/subscribe".to_string(),
            format!("/app/confirm/{token}"),
            format!("/api/status/ab12/extra/confirm/{token}"),
            "/confirm/not-a-token".to_string(),
        ] {
            let kept = redact_subscription_token_path(&path);
            assert!(matches!(kept, std::borrow::Cow::Borrowed(_)), "{path}");
            assert_eq!(kept, path);
        }
    }

    #[test]
    fn the_unlock_form_only_posts_and_maps_to_its_slug_route() {
        assert_eq!(
            StatusHostPath::parse("/unlock"),
            Some(StatusHostPath::Unlock)
        );
        assert_eq!(StatusHostPath::parse("/unlock/"), None);
        assert!(StatusHostPath::Unlock.allows_method("POST"));
        assert!(!StatusHostPath::Unlock.allows_method("GET"));
        assert_eq!(
            StatusHostPath::Unlock.console_path("ab12"),
            "/api/status/ab12/unlock"
        );
    }

    #[test]
    fn only_well_formed_access_cookies_are_forwarded() {
        assert!(is_well_formed_access_cookie("v1.7.3.1767225600.0a1B-_"));
        assert!(!is_well_formed_access_cookie(""));
        assert!(!is_well_formed_access_cookie("a;b"));
        assert!(!is_well_formed_access_cookie("a b"));
        assert!(!is_well_formed_access_cookie(&"a".repeat(161)));
    }

    #[test]
    fn subscription_paths_carry_a_well_formed_token_only() {
        let token = "a".repeat(SUBSCRIPTION_TOKEN_LEN);
        assert_eq!(
            StatusHostPath::parse("/subscribe"),
            Some(StatusHostPath::Subscribe)
        );
        assert_eq!(
            StatusHostPath::parse(&format!("/confirm/{token}")),
            Some(StatusHostPath::Confirm(token.clone()))
        );
        assert_eq!(
            StatusHostPath::parse(&format!("/unsubscribe/{token}")),
            Some(StatusHostPath::Unsubscribe(token.clone()))
        );
        for path in [
            "/subscribe/".to_string(),
            "/confirm/".to_string(),
            format!("/confirm/{}", "A".repeat(SUBSCRIPTION_TOKEN_LEN)),
            format!("/confirm/{token}/"),
            format!("/confirm/{token}a"),
            format!("/unsubscribe/../{token}"),
            format!("/unsubscribe/{}", &token[1..]),
        ] {
            assert_eq!(StatusHostPath::parse(&path), None, "{path}");
        }
        assert_eq!(
            StatusHostPath::Confirm(token.clone()).console_path("ab12"),
            format!("/api/status/ab12/confirm/{token}")
        );
        assert_eq!(
            StatusHostPath::Subscribe.console_path("ab12"),
            "/api/status/ab12/subscribe"
        );
    }

    #[test]
    fn each_path_allows_only_its_methods() {
        let token = "b".repeat(SUBSCRIPTION_TOKEN_LEN);
        for view in [
            StatusHostPath::Page,
            StatusHostPath::Summary,
            StatusHostPath::Feed,
        ] {
            assert!(view.allows_method("GET") && view.allows_method("HEAD"));
            assert!(!view.allows_method("POST") && !view.allows_method("PUT"));
        }
        assert!(StatusHostPath::Subscribe.allows_method("POST"));
        assert!(!StatusHostPath::Subscribe.allows_method("GET"));
        for view in [
            StatusHostPath::Confirm(token.clone()),
            StatusHostPath::Unsubscribe(token),
        ] {
            assert!(view.allows_method("GET") && view.allows_method("POST"));
            assert!(!view.allows_method("DELETE") && !view.allows_method("PUT"));
        }
    }

    #[test]
    fn forms_may_post_only_to_the_page_origin() {
        assert!(CONTENT_SECURITY_POLICY.contains("form-action 'self'"));
        assert!(CONTENT_SECURITY_POLICY.contains("default-src 'none'"));
    }

    #[test]
    fn only_the_known_paths_are_served() {
        assert_eq!(StatusHostPath::parse("/"), Some(StatusHostPath::Page));
        assert_eq!(
            StatusHostPath::parse("/summary.json"),
            Some(StatusHostPath::Summary)
        );
        assert_eq!(
            StatusHostPath::parse("/feed.atom"),
            Some(StatusHostPath::Feed)
        );
        for path in [
            "/feed.atom/",
            "/feed.xml",
            "",
            "//",
            "/index.html",
            "/summary.json/",
            "/Summary.json",
            "/%2F",
            "/api/status/abc",
            "/api/projects",
            "/.well-known/acme-challenge/x",
            "/../api",
        ] {
            assert_eq!(StatusHostPath::parse(path), None, "{path}");
        }
    }

    #[test]
    fn console_paths_are_pinned_to_the_slug_routes() {
        assert_eq!(
            StatusHostPath::Page.console_path("ab12"),
            "/api/status/ab12"
        );
        assert_eq!(
            StatusHostPath::Summary.console_path("ab12"),
            "/api/status/ab12/summary.json"
        );
        assert_eq!(
            StatusHostPath::Feed.console_path("ab12"),
            "/api/status/ab12/feed.atom"
        );
    }
}
