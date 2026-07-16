//! Sandbox preview password wall.
//!
//! Renders the HTML login form shown when an unauthenticated user hits a
//! sandbox preview host. The cryptographic bits (cookie minting,
//! verification, rate limiting) live in [`crate::preview_auth`]; this module
//! only handles HTML rendering.
//!
//! Login flow (replaces HTTP Basic auth):
//!   1. GET `ws-<hex>-<port>.<preview_domain>/anything` without a valid
//!      `temps_preview_sbx_<hex>` cookie → proxy issues a 303 to
//!      `/__temps/preview/login?next=<encoded path>`.
//!   2. GET `/__temps/preview/login` → this form.
//!   3. POST `/__temps/preview/login` with `password` + `next` → proxy
//!      verifies with argon2, mints the cookie, 303s back to `next`.
//!   4. POST `/__temps/preview/logout` → 303 `/` with an expired cookie.
//!
//! Why not Basic auth: browsers cache Basic credentials unpredictably across
//! subdomains, show native prompts that can't be dismissed, and some HTTP
//! clients refuse to pass them over plain HTTP. Form + cookie is reliable
//! across both http/https and survives subdomain hops (cookie scoped to the
//! parent preview domain).

/// Path that the proxy intercepts to serve the login form and accept
/// credentials. Kept under a `/__temps/` prefix to avoid colliding with any
/// realistic dev-server route.
pub const PREVIEW_LOGIN_PATH: &str = "/__temps/preview/login";

/// Path that clears the preview cookie.
pub const PREVIEW_LOGOUT_PATH: &str = "/__temps/preview/logout";

const PREVIEW_FORM_HTML: &str = include_str!("../../preview_wall/preview_form.html");

/// Render the login form with a display label (e.g. `sandbox sbx_abc…`).
/// `next` is the path the user will be redirected to after a successful
/// login — always sanitized by the caller.
pub fn generate_preview_form_html_labeled(
    label: &str,
    port: u16,
    next: &str,
    show_error: bool,
) -> String {
    // The template historically used `{{SESSION_ID}}` substituted into
    // `session #{{SESSION_ID}}`. We replace the whole legacy phrase with the
    // provided label, then clear any remaining `{{SESSION_ID}}` tokens.
    let escaped_label = html_escape(label);
    let with_label = PREVIEW_FORM_HTML.replace("session #{{SESSION_ID}}", &escaped_label);
    with_label
        .replace("{{SESSION_ID}}", "")
        .replace("{{PORT}}", &port.to_string())
        .replace("{{REDIRECT_PATH}}", &html_escape(next))
        .replace(
            "{{ERROR_DISPLAY}}",
            if show_error { "flex" } else { "none" },
        )
        .replace(
            "{{ERROR_INPUT_CLASS}}",
            if show_error { "input-error" } else { "" },
        )
}

/// Build an expired Set-Cookie header for a standalone sandbox logout.
/// Matches the scope of the live cookie so the browser actually drops it.
/// `secure` must match the scheme used when the live cookie was set.
/// Expire the obsolete HTTPS host-only, unpartitioned variant.
pub fn build_logout_cookie_sandbox_unpartitioned(public_id_suffix: &str) -> String {
    format!(
        "{}{}=; Path=/; HttpOnly; Secure; SameSite=None; Max-Age=0",
        crate::preview_auth::PREVIEW_SANDBOX_COOKIE_PREFIX,
        public_id_suffix,
    )
}

pub fn build_logout_cookie_sandbox(
    public_id_suffix: &str,
    preview_domain: &str,
    secure: bool,
) -> String {
    if secure {
        return format!(
            "{}{}=; Path=/; HttpOnly; Secure; SameSite=None; Partitioned; Max-Age=0",
            crate::preview_auth::PREVIEW_SANDBOX_COOKIE_PREFIX,
            public_id_suffix,
        );
    }
    let domain = preview_domain.trim_start_matches("*.");
    format!(
        "{}{}=; Domain=.{domain}; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        crate::preview_auth::PREVIEW_SANDBOX_COOKIE_PREFIX,
        public_id_suffix,
    )
}

/// Sanitize a `next` redirect target to prevent open-redirect abuse. Only
/// allow paths that start with `/` and don't start with `//` (which browsers
/// interpret as a scheme-relative URL to another host).
pub fn sanitize_next(next: &str) -> String {
    if next.starts_with('/') && !next.starts_with("//") {
        next.to_string()
    } else {
        "/".to_string()
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_substitutes_label_port_and_next() {
        let html = generate_preview_form_html_labeled("sandbox sbx_abc", 3000, "/foo/bar", false);
        assert!(html.contains("sandbox sbx_abc"));
        assert!(html.contains("port 3000"));
        assert!(html.contains("value=\"/foo/bar\""));
        assert!(html.contains("display: none"));
    }

    #[test]
    fn form_shows_error_state() {
        let html = generate_preview_form_html_labeled("x", 8080, "/", true);
        assert!(html.contains("display: flex"));
        assert!(html.contains("input-error"));
    }

    #[test]
    fn form_escapes_next_to_prevent_xss() {
        let html = generate_preview_form_html_labeled("x", 3000, "/\"><script>x</script>", false);
        assert!(!html.contains("<script>x</script>"));
        assert!(html.contains("&quot;"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn sanitize_next_accepts_absolute_path() {
        assert_eq!(sanitize_next("/dashboard"), "/dashboard");
        assert_eq!(sanitize_next("/a?b=c"), "/a?b=c");
    }

    #[test]
    fn sanitize_next_rejects_scheme_relative() {
        assert_eq!(sanitize_next("//evil.example.com"), "/");
    }

    #[test]
    fn sanitize_next_rejects_absolute_url() {
        assert_eq!(sanitize_next("https://evil.example.com"), "/");
        assert_eq!(sanitize_next("javascript:alert(1)"), "/");
    }

    #[test]
    fn sanitize_next_rejects_relative() {
        assert_eq!(sanitize_next("foo"), "/");
        assert_eq!(sanitize_next(""), "/");
    }

    #[test]
    fn secure_logout_cookie_matches_partitioned_host_only_scope() {
        let c = build_logout_cookie_sandbox("abc", "*.localho.st", true);
        assert!(c.starts_with("temps_preview_sbx_abc="));
        assert!(!c.contains("Domain="));
        assert!(c.contains("Max-Age=0"));
        assert!(c.contains("; Secure"));
        assert!(c.contains("SameSite=None"));
        assert!(c.contains("Partitioned"));
    }

    #[test]
    fn unpartitioned_logout_cookie_targets_obsolete_https_scope() {
        let c = build_logout_cookie_sandbox_unpartitioned("abc");
        assert!(c.starts_with("temps_preview_sbx_abc="));
        assert!(c.contains("; Secure"));
        assert!(c.contains("SameSite=None"));
        assert!(!c.contains("Partitioned"));
        assert!(!c.contains("Domain="));
        assert!(c.contains("Max-Age=0"));
    }

    #[test]
    fn logout_cookie_omits_secure_on_http() {
        let c = build_logout_cookie_sandbox("abc", "localho.st", false);
        assert!(!c.contains("Secure"));
    }
}
