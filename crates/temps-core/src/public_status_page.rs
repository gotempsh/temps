// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire contract between the proxy and the console for public status pages
//! served on a custom hostname (#1412).
//!
//! The proxy owns the custom hostname: it allows exactly two paths, rewrites
//! them to the page's slug routes on the console's public-only ingress
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
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; \
     img-src https:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

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

/// What a request path on a custom status-page hostname maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusHostPath {
    /// `/`: the HTML page.
    Page,
    /// `/summary.json`: the JSON page.
    Summary,
}

impl StatusHostPath {
    /// The only two paths a custom hostname serves. Anything else, including
    /// `/index.html`, `//`, encoded variants and trailing slashes, is `None`.
    pub fn parse(path: &str) -> Option<Self> {
        match path {
            "/" => Some(Self::Page),
            "/summary.json" => Some(Self::Summary),
            _ => None,
        }
    }

    /// Path on the console's public router (mounted under `/api`) that serves
    /// this view of the page `slug`. `slug` is server-generated lowercase hex.
    pub fn console_path(self, slug: &str) -> String {
        match self {
            Self::Page => format!("/api/status/{slug}"),
            Self::Summary => format!("/api/status/{slug}/summary.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_paths_are_served() {
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
