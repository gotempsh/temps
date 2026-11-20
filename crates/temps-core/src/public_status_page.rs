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

/// Length of a subscription token in a path: 64 lowercase hex characters.
pub const SUBSCRIPTION_TOKEN_LEN: usize = 64;

fn is_subscription_token(candidate: &str) -> bool {
    candidate.len() == SUBSCRIPTION_TOKEN_LEN
        && candidate
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a request path on a custom status-page hostname maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusHostPath {
    /// `/`: the HTML page.
    Page,
    /// `/summary.json`: the JSON page.
    Summary,
    /// `/subscribe`: the subscribe form's post (ADR 108 D2).
    Subscribe,
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
            "/subscribe" => return Some(Self::Subscribe),
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
            Self::Page | Self::Summary => matches!(method, "GET" | "HEAD"),
            Self::Subscribe => method == "POST",
            Self::Confirm(_) | Self::Unsubscribe(_) => matches!(method, "GET" | "HEAD" | "POST"),
        }
    }

    /// Path on the console's public router (mounted under `/api`) that serves
    /// this view of the page `slug`. `slug` is server-generated lowercase hex.
    pub fn console_path(&self, slug: &str) -> String {
        match self {
            Self::Page => format!("/api/status/{slug}"),
            Self::Summary => format!("/api/status/{slug}/summary.json"),
            Self::Subscribe => format!("/api/status/{slug}/subscribe"),
            Self::Confirm(token) => format!("/api/status/{slug}/confirm/{token}"),
            Self::Unsubscribe(token) => format!("/api/status/{slug}/unsubscribe/{token}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for view in [StatusHostPath::Page, StatusHostPath::Summary] {
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
        for path in [
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
    }
}
