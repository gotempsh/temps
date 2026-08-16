//! Streaming HTML rewriter for email tracking
//!
//! Uses lol_html (Cloudflare's streaming HTML rewriter) to:
//! - Inject a 1x1 tracking pixel before </body>
//! - Rewrite <a href="..."> links to click tracking URLs

use lol_html::{element, HtmlRewriter, Settings};
use uuid::Uuid;

use temps_email::services::TrackingRewriter;

use crate::errors::EmailTrackingError;
use crate::hmac::generate_tracking_hmac;

/// HTML tracking rewriter that injects pixels and rewrites links
pub struct HtmlTrackingRewriter {
    tracking_base_url: String,
    hmac_key: Vec<u8>,
}

impl HtmlTrackingRewriter {
    pub fn new(tracking_base_url: String, hmac_key: Vec<u8>) -> Self {
        Self {
            tracking_base_url,
            hmac_key,
        }
    }

    /// Rewrite HTML to add tracking pixel and wrap links for click tracking.
    ///
    /// - Links: `<a href="https://...">` → `<a href="{base}/t/click/{email_id}/{hmac}/{encoded_url}">`
    /// - Pixel: appends `<img src="{base}/t/pixel/{email_id}/{hmac}.gif" .../>` inside `<body>`
    pub fn rewrite(&self, email_id: &Uuid, html: &str) -> Result<String, EmailTrackingError> {
        let mut output = Vec::with_capacity(html.len() + 256);
        let email_id_str = email_id.to_string();
        let base_url = self.tracking_base_url.clone();
        let hmac_key = self.hmac_key.clone();

        let base_url_click = base_url.clone();
        let hmac_key_click = hmac_key.clone();
        let email_id_click = email_id_str.clone();

        let base_url_pixel = base_url.clone();
        let hmac_key_pixel = hmac_key.clone();
        let email_id_pixel = email_id_str.clone();

        let mut rewriter = HtmlRewriter::new(
            Settings::new()
                // Rewrite <a href="..."> to click tracking URL
                .append_element_content_handler(element!("a[href]", move |el| {
                    if let Some(href) = el.get_attribute("href") {
                        // Only rewrite http/https links — skip mailto:, tel:, #anchors, etc.
                        if href.starts_with("http://") || href.starts_with("https://") {
                            let sig =
                                generate_tracking_hmac(&hmac_key_click, &email_id_click, &href);
                            let encoded_url = urlencoding::encode(&href);
                            let tracking_url = format!(
                                "{}/t/click/{}/{}/{}",
                                base_url_click, email_id_click, sig, encoded_url
                            );
                            el.set_attribute("href", &tracking_url)
                                .map_err(|e| format!("Failed to set href: {}", e))?;
                        }
                    }
                    Ok(())
                }))
                // Inject tracking pixel before </body>
                .append_element_content_handler(element!("body", move |el| {
                    let pixel_sig =
                        generate_tracking_hmac(&hmac_key_pixel, &email_id_pixel, "open");
                    let pixel_url = format!(
                        "{}/t/pixel/{}/{}.gif",
                        base_url_pixel, email_id_pixel, pixel_sig
                    );
                    el.append(
                        &format!(
                            r#"<img src="{}" width="1" height="1" alt="" style="display:none" />"#,
                            pixel_url
                        ),
                        lol_html::html_content::ContentType::Html,
                    );
                    Ok(())
                })),
            |c: &[u8]| output.extend_from_slice(c),
        );

        rewriter
            .write(html.as_bytes())
            .map_err(|e| EmailTrackingError::HtmlRewrite {
                email_id: email_id_str.clone(),
                reason: format!("Write failed: {}", e),
            })?;
        rewriter
            .end()
            .map_err(|e| EmailTrackingError::HtmlRewrite {
                email_id: email_id_str,
                reason: format!("End failed: {}", e),
            })?;

        String::from_utf8(output).map_err(|e| EmailTrackingError::HtmlRewrite {
            email_id: email_id.to_string(),
            reason: format!("UTF-8 conversion failed: {}", e),
        })
    }
}

impl TrackingRewriter for HtmlTrackingRewriter {
    fn rewrite(&self, email_id: &Uuid, html: &str) -> Result<String, String> {
        self.rewrite(email_id, html).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rewriter() -> HtmlTrackingRewriter {
        HtmlTrackingRewriter::new(
            "https://track.example.com/api".to_string(),
            b"test-hmac-key-for-email-tracking".to_vec(),
        )
    }

    #[test]
    fn test_pixel_injection() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = "<html><body><p>Hello</p></body></html>";

        let result = rewriter.rewrite(&email_id, html).unwrap();

        assert!(result.contains(r#"<img src="https://track.example.com/api/t/pixel/550e8400-e29b-41d4-a716-446655440000/"#));
        assert!(result.contains(r#"width="1" height="1""#));
        assert!(result.contains(r#"style="display:none""#));
        assert!(result.contains(".gif"));
    }

    #[test]
    fn test_link_rewriting() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body><a href="https://example.com/page">Click</a></body></html>"#;

        let result = rewriter.rewrite(&email_id, html).unwrap();

        assert!(result.contains("/t/click/550e8400-e29b-41d4-a716-446655440000/"));
        assert!(result.contains("https%3A%2F%2Fexample.com%2Fpage"));
        assert!(!result.contains(r#"href="https://example.com/page""#));
    }

    #[test]
    fn test_mailto_links_not_rewritten() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body><a href="mailto:test@example.com">Email</a></body></html>"#;

        let result = rewriter.rewrite(&email_id, html).unwrap();

        assert!(result.contains(r#"href="mailto:test@example.com""#));
    }

    #[test]
    fn test_anchor_links_not_rewritten() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r##"<html><body><a href="#section">Jump</a></body></html>"##;

        let result = rewriter.rewrite(&email_id, html).unwrap();

        assert!(result.contains(r##"href="#section""##));
    }

    #[test]
    fn test_tel_links_not_rewritten() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body><a href="tel:+1234567890">Call</a></body></html>"#;

        let result = rewriter.rewrite(&email_id, html).unwrap();

        assert!(result.contains(r#"href="tel:+1234567890""#));
    }

    #[test]
    fn test_multiple_links_rewritten() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body>
            <a href="https://example.com/a">A</a>
            <a href="https://example.com/b">B</a>
        </body></html>"#;

        let result = rewriter.rewrite(&email_id, html).unwrap();

        assert!(result.contains("example.com%2Fa"));
        assert!(result.contains("example.com%2Fb"));
    }

    #[test]
    fn test_html_without_body_tag() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = "<p>Hello World</p>";

        // Should not fail — just won't inject pixel since there's no <body>
        let result = rewriter.rewrite(&email_id, html).unwrap();
        assert!(result.contains("Hello World"));
    }

    #[test]
    fn test_empty_html() {
        let rewriter = test_rewriter();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        let result = rewriter.rewrite(&email_id, "").unwrap();
        assert_eq!(result, "");
    }
}
