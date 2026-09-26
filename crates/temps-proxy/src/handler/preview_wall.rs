// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    console_url: Option<&str>,
) -> String {
    // The template historically used `{{SESSION_ID}}` substituted into
    // `session #{{SESSION_ID}}`. We replace the whole legacy phrase with the
    // provided label, then clear any remaining `{{SESSION_ID}}` tokens.
    let escaped_label = html_escape(label);
    let with_label = PREVIEW_FORM_HTML.replace("session #{{SESSION_ID}}", &escaped_label);
    let managed = console_url.and_then(|base| console_handoff_url(base, label, port, next));
    let (managed_link, managed_script) = if let Some((url, origin)) = managed {
        let js_url = js_string(&url);
        let js_origin = js_string(&origin);
        let js_next = js_string(next);
        let key = js_string(&format!("temps:preview-handoff:{label}:{port}"));
        (
            format!("<p class=\"managed-help\">If you use Temps, <a id=\"console-link\" href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">open or sign in to Temps</a> to restore access. You can also use the preview password below.</p><p id=\"handoff-status\" class=\"managed-help\" hidden>Automatic sign-in did not complete. Check that preview cookies are enabled, or use the link above to retry.</p>", html_escape(&url)),
            format!(r#"<script>
(() => {{
  const destination = {js_url};
  const consoleOrigin = {js_origin};
  const nextPath = {js_next};
  const key = {key};
  const link = document.getElementById('console-link');
  const status = document.getElementById('handoff-status');
  if (window.parent !== window) {{
    window.parent.postMessage({{ type: 'temps:preview-auth-required', path: nextPath }}, consoleOrigin);
    return;
  }}
  try {{
    const lastAttempt = Number(sessionStorage.getItem(key) || 0);
    if (!{show_error} && (!lastAttempt || Date.now() - lastAttempt > 60000)) {{
      sessionStorage.setItem(key, String(Date.now()));
      window.location.replace(destination);
      return;
    }}
    status.hidden = false;
    link.addEventListener('click', () => sessionStorage.removeItem(key));
  }} catch (_) {{
    status.hidden = false;
  }}
}})();
</script>"#),
        )
    } else {
        (
            "<p class=\"managed-help\">This preview is protected. Enter its password below. To enable automatic sign-in through Temps, an administrator must configure the console external URL.</p>".to_string(),
            r#"<script>
if (window.parent !== window) {
  // A wildcard target must never receive the preview path or query.
  // The console verifies the sender and derives its own active iframe path.
  window.parent.postMessage({ type: 'temps:preview-auth-required' }, '*');
}
</script>"#.to_string(),
        )
    };
    with_label
        .replace("{{SESSION_ID}}", "")
        .replace("{{PORT}}", &port.to_string())
        .replace("{{REDIRECT_PATH}}", &html_escape(next))
        .replace("{{MANAGED_LINK}}", &managed_link)
        .replace("{{MANAGED_SCRIPT}}", &managed_script)
        .replace(
            "{{ERROR_DISPLAY}}",
            if show_error { "flex" } else { "none" },
        )
        .replace(
            "{{ERROR_INPUT_CLASS}}",
            if show_error { "input-error" } else { "" },
        )
}

/// Only the configured console origin is trusted; preview request headers are never used.
fn console_handoff_url(base: &str, label: &str, port: u16, next: &str) -> Option<(String, String)> {
    let mut url = url::Url::parse(base).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let hex = label.strip_prefix("sandbox sbx_")?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    url.set_path("/sandbox-preview");
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("sandbox", &format!("sbx_{hex}"))
        .append_pair("port", &port.to_string())
        .append_pair("path", &sanitize_next(next));
    Some((url.to_string(), origin))
}

fn js_string(value: &str) -> String {
    serde_json::json!(value)
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// Render the auto-submit bridge that exchanges a share link for a cookie.
///
/// A minted `session_grant` arrives in the URL fragment, which browsers never
/// send in the HTTP request target or Referer. This bridge reads it locally,
/// removes it from browser history, and submits it to the existing POST branch
/// where verification, rate limiting, and cookie minting already live.
///
/// The grant is deliberately not placed anywhere the sandbox can read it. It
/// never appears in generated markup, and the POST exchanges it for the
/// ordinary preview cookie.
pub fn generate_preview_bridge_html(label: &str, next: &str) -> String {
    let fallback = format!(
        "{}?next={}",
        PREVIEW_LOGIN_PATH,
        url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>()
    );
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="robots" content="noindex, nofollow">
<meta name="referrer" content="no-referrer">
<title>Opening preview…</title>
<style>
  body {{ font: 15px/1.5 system-ui, -apple-system, sans-serif; margin: 0;
         min-height: 100vh; display: flex; align-items: center;
         justify-content: center; color: #1f2328; background: #f6f8fa; }}
  .card {{ text-align: center; padding: 2rem 2.5rem; }}
  p {{ margin: 0 0 1rem; }}
  .muted {{ color: #656d76; font-size: 13px; }}
</style>
</head>
<body>
<div class="card">
  <p>Opening the preview for {label}…</p>
  <form id="bridge" method="POST" action="{action}">
    <input id="session-grant" type="hidden" name="session_grant" value="">
    <input type="hidden" name="next" value="{next}">
  </form>
  <p class="muted">This link signs you in to the preview. It expires.</p>
  <noscript><p><a href="{fallback}">Use the preview password instead</a></p></noscript>
</div>
<script>
(() => {{
  const current = new URL(window.location.href);
  const grant = new URLSearchParams(current.hash.slice(1)).get('session_grant');
  current.hash = '';
  current.searchParams.delete('grant');
  const cleanUrl = current.pathname + current.search;
  if (!grant) {{
    window.location.replace(cleanUrl);
    return;
  }}
  window.history.replaceState(null, '', cleanUrl);
  document.getElementById('session-grant').value = grant;
  document.getElementById('bridge').submit();
}})();
</script>
</body>
</html>
"#,
        label = html_escape(label),
        action = PREVIEW_LOGIN_PATH,
        next = html_escape(next),
        fallback = html_escape(&fallback),
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
    temps_core::sanitize_preview_next(next)
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
        let html =
            generate_preview_form_html_labeled("sandbox sbx_abc", 3000, "/foo/bar", false, None);
        assert!(html.contains("sandbox sbx_abc"));
        assert!(html.contains("port 3000"));
        assert!(html.contains("value=\"/foo/bar\""));
        assert!(html.contains("display: none"));
    }

    #[test]
    fn form_shows_error_state() {
        let html = generate_preview_form_html_labeled("x", 8080, "/", true, None);
        assert!(html.contains("display: flex"));
        assert!(html.contains("input-error"));
    }

    #[test]
    fn form_escapes_next_to_prevent_xss() {
        let html =
            generate_preview_form_html_labeled("x", 3000, "/\"><script>x</script>", false, None);
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
    fn managed_handoff_uses_configured_origin_and_preserves_path() {
        let html = generate_preview_form_html_labeled(
            "sandbox sbx_ab12",
            3000,
            "/docs?q=a&x=b",
            false,
            Some("https://console.example.test/settings?ignored=1"),
        );
        assert!(html.contains(
            "sandbox-preview?sandbox=sbx_ab12&amp;port=3000&amp;path=%2Fdocs%3Fq%3Da%26x%3Db"
        ));
        assert!(html.contains("temps:preview-auth-required"));
        assert!(html.contains("sessionStorage.setItem"));
        assert!(html.contains("Date.now() - lastAttempt > 60000"));
        assert!(html.contains("window.parent !== window"));
    }

    #[test]
    fn missing_or_untrusted_console_url_only_notifies_embedded_parent() {
        for base in [
            "javascript:alert(1)",
            "https://user:pass@example.test",
            "//example.test",
        ] {
            let html = generate_preview_form_html_labeled(
                "sandbox sbx_ab12",
                3000,
                "/",
                false,
                Some(base),
            );
            assert!(html.contains("postMessage({ type: 'temps:preview-auth-required' }, '*')"));
            assert!(!html.contains("path: \"/\""));
            assert!(!html.contains("window.location.replace"));
            assert!(html.contains("administrator must configure the console external URL"));
            assert!(html.contains("name=\"password\""));
        }
        let html =
            generate_preview_form_html_labeled("sandbox sbx_ab12", 3000, "/docs?x=1", false, None);
        assert!(html.contains("postMessage({ type: 'temps:preview-auth-required' }, '*')"));
        assert!(!html.contains("path: \"/docs?x=1\""));
        assert!(!html.contains("window.location.replace"));
    }

    #[test]
    fn sanitize_next_rejects_scheme_relative() {
        assert_eq!(sanitize_next("//evil.example.com"), "/");
    }

    #[test]
    fn sanitize_next_rejects_backslash_redirects_and_control_characters() {
        assert_eq!(sanitize_next("/\\evil.example.com"), "/");
        assert_eq!(sanitize_next("/safe\r\nLocation: //evil.example.com"), "/");
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
    fn bridge_reads_fragment_without_embedding_the_grant() {
        let html = generate_preview_bridge_html("sandbox sbx_abc", "/pricing");
        assert!(html.contains("current.hash.slice(1)"));
        assert!(html.contains("history.replaceState"));
        assert!(html.contains("name=\"referrer\" content=\"no-referrer\""));
        assert!(html.contains("name=\"session_grant\" value=\"\""));
        assert!(html.contains("name=\"next\" value=\"/pricing\""));
        assert!(!html.contains("session_grant=secret"));
    }

    #[test]
    fn bridge_escapes_dynamic_html() {
        let html = generate_preview_bridge_html("<script>alert(1)</script>", "/?q=\"");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("value=\"/?q=&quot;\""));
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
