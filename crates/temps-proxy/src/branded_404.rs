//! Branded 404 page rendered by the proxy when no app is routed to a host.
//!
//! Used by the admin gate when an unknown host hits a non-admin client IP:
//! instead of returning a bare `<h1>404 - Not Found</h1>` (the kind of thing
//! nginx/haproxy emit), we serve a small self-contained HTML page with the
//! Temps brand mark, the offending host, and the request ID so support can
//! correlate logs.
//!
//! Constraints:
//! - Zero external assets (no remote fonts, images, or scripts). The proxy
//!   may serve this for hosts that are still in DNS propagation, so we
//!   cannot assume the requester has connectivity to anything beyond the
//!   proxy itself.
//! - All dynamic content (host, request_id) is HTML-escaped to avoid
//!   reflected XSS through attacker-controlled `Host` headers.
//! - Page is dark-mode native; the Temps brand mark is the same `t` squircle
//!   used in `web/public/svg/temps-icon.svg`.

/// HTML-escape a string for safe interpolation into element text or
/// double-quoted attributes. Covers the OWASP "context: HTML body / attr"
/// set plus single quotes, since `request_id` ends up in JS-free text but
/// `host` is user-controlled and we want defense in depth.
fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render the branded 404 HTML for `host` and `request_id`.
///
/// The returned string is a complete, self-contained HTML document. It is
/// safe to send as the response body with `Content-Type: text/html; charset=utf-8`.
pub fn render(host: &str, request_id: &str) -> String {
    let host = escape(host);
    let request_id = escape(request_id);

    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow">
<title>404 — No deployment for this host · Temps</title>
<style>
  :root {{
    color-scheme: dark light;
    --bg: #09090b;
    --fg: #fafafa;
    --muted: #a1a1aa;
    --line: #27272a;
    --chip-bg: #18181b;
    --accent: #fafafa;
  }}
  @media (prefers-color-scheme: light) {{
    :root {{
      --bg: #fafafa;
      --fg: #09090b;
      --muted: #52525b;
      --line: #e4e4e7;
      --chip-bg: #f4f4f5;
      --accent: #09090b;
    }}
  }}
  * {{ box-sizing: border-box; }}
  html, body {{ height: 100%; }}
  body {{
    margin: 0;
    background: var(--bg);
    color: var(--fg);
    font: 15px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto,
          Inter, "Helvetica Neue", Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
    text-rendering: optimizeLegibility;
    display: grid;
    place-items: center;
    padding: 24px;
  }}
  main {{
    width: 100%;
    max-width: 520px;
    text-align: center;
  }}
  .mark {{
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 56px;
    height: 56px;
    border-radius: 12px;
    background: var(--accent);
    color: var(--bg);
    font-weight: 900;
    font-size: 36px;
    line-height: 1;
    letter-spacing: -0.04em;
    margin-bottom: 28px;
    user-select: none;
  }}
  h1 {{
    margin: 0 0 10px;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }}
  p.lede {{
    margin: 0 0 28px;
    color: var(--muted);
    font-size: 15px;
  }}
  .chip {{
    display: inline-block;
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--chip-bg);
    color: var(--muted);
    font: 12px/1 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    letter-spacing: 0.04em;
    margin-bottom: 24px;
  }}
  dl.meta {{
    margin: 0 auto 28px;
    padding: 14px 16px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--chip-bg);
    text-align: left;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
    font: 12.5px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }}
  dl.meta dt {{ color: var(--muted); }}
  dl.meta dd {{
    margin: 0;
    color: var(--fg);
    overflow-wrap: anywhere;
  }}
  a.docs {{
    color: var(--fg);
    border-bottom: 1px solid var(--line);
    text-decoration: none;
    padding-bottom: 1px;
  }}
  a.docs:hover {{ border-bottom-color: var(--fg); }}
  footer {{
    margin-top: 32px;
    color: var(--muted);
    font-size: 12px;
  }}
  footer a {{ color: var(--muted); text-decoration: none; }}
  footer a:hover {{ color: var(--fg); }}
</style>
</head>
<body>
<main>
  <div class="mark" aria-hidden="true">t</div>
  <div class="chip">DEPLOYMENT_NOT_FOUND · 404</div>
  <h1>No deployment is routed to this host</h1>
  <p class="lede">
    The host you requested isn't currently bound to a Temps deployment.
    If you just configured a domain, DNS may still be propagating.
  </p>
  <dl class="meta">
    <dt>Host</dt><dd>{host}</dd>
    <dt>Request ID</dt><dd>{request_id}</dd>
  </dl>
  <p>
    <a class="docs" href="https://temps.sh/docs/domains" rel="noopener noreferrer">
      How domains work on Temps →
    </a>
  </p>
  <footer>
    Served by <a href="https://temps.sh" rel="noopener noreferrer">Temps</a>
  </footer>
</main>
</body>
</html>
"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_handles_html_metacharacters() {
        assert_eq!(escape("a&b"), "a&amp;b");
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert_eq!(escape("\"x'y"), "&quot;x&#x27;y");
        assert_eq!(escape("plain"), "plain");
    }

    #[test]
    fn render_embeds_host_and_request_id() {
        let html = render("app1.temps.kfs.es", "abc-123");
        assert!(html.contains("<dt>Host</dt><dd>app1.temps.kfs.es</dd>"));
        assert!(html.contains("<dt>Request ID</dt><dd>abc-123</dd>"));
        assert!(html.contains("DEPLOYMENT_NOT_FOUND"));
        assert!(html.contains("<title>404"));
    }

    #[test]
    fn render_escapes_malicious_host() {
        let html = render("<img src=x onerror=alert(1)>", "rid");
        assert!(
            !html.contains("<img src=x onerror=alert(1)>"),
            "raw payload must not appear in output: {}",
            html
        );
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn render_escapes_quotes_in_request_id() {
        let html = render("example.com", "\"';--");
        assert!(html.contains("&quot;&#x27;;--"));
    }

    #[test]
    fn render_is_self_contained_no_external_assets() {
        let html = render("example.com", "rid");
        // No remote font, image, or script references.
        assert!(!html.contains("http://"));
        // Only allowed external URLs are the two documentation links.
        let https_count = html.matches("https://").count();
        assert_eq!(
            https_count, 2,
            "expected exactly two https references (docs link + footer), found {}",
            https_count
        );
        assert!(html.contains("https://temps.sh/docs/domains"));
        assert!(html.contains("https://temps.sh"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn render_marks_noindex() {
        let html = render("example.com", "rid");
        assert!(html.contains(r#"name="robots" content="noindex,nofollow""#));
    }
}
