//! Email tracking service for open tracking (pixel) and click tracking (link rewriting)

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use std::sync::Arc;
use temps_entities::{email_events, email_links, emails};
use tracing::debug;
use uuid::Uuid;

use crate::errors::EmailError;

/// Service for email tracking (opens, clicks)
pub struct TrackingService {
    db: Arc<DatabaseConnection>,
    /// Base URL for tracking endpoints (e.g., "https://app.example.com")
    base_url: String,
}

/// Result of transforming HTML for tracking
#[derive(Debug, Clone)]
pub struct TransformResult {
    /// Transformed HTML with tracking pixel and rewritten links
    pub html: String,
    /// Extracted links with their indices
    pub links: Vec<ExtractedLink>,
}

/// A link extracted during HTML transformation
#[derive(Debug, Clone)]
pub struct ExtractedLink {
    pub index: i32,
    pub original_url: String,
}

/// Event recorded for tracking
#[derive(Debug, Clone)]
pub struct TrackingEvent {
    pub email_id: Uuid,
    pub event_type: String,
    pub link_url: Option<String>,
    pub link_index: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

impl TrackingService {
    pub fn new(db: Arc<DatabaseConnection>, base_url: String) -> Self {
        Self { db, base_url }
    }

    /// Transform HTML body for tracking: inject open pixel + rewrite links
    pub fn transform_html(
        &self,
        email_id: Uuid,
        html: &str,
        track_opens: bool,
        track_clicks: bool,
    ) -> TransformResult {
        let mut result_html = html.to_string();
        let mut links = Vec::new();

        // Rewrite links for click tracking
        if track_clicks {
            let (rewritten, extracted) = self.rewrite_links(email_id, &result_html);
            result_html = rewritten;
            links = extracted;
        }

        // Inject tracking pixel for open tracking
        if track_opens {
            result_html = self.inject_tracking_pixel(email_id, &result_html);
        }

        TransformResult {
            html: result_html,
            links,
        }
    }

    /// Inject a 1x1 transparent tracking pixel before </body> or at end of HTML
    fn inject_tracking_pixel(&self, email_id: Uuid, html: &str) -> String {
        let pixel_url = format!("{}/api/emails/{}/track/open", self.base_url, email_id);
        let pixel_tag = format!(
            r#"<img src="{}" width="1" height="1" alt="" style="display:none;width:1px;height:1px;border:0;" />"#,
            pixel_url
        );

        // Insert before </body> if present, otherwise append
        if let Some(pos) = html.to_lowercase().rfind("</body>") {
            let mut result = html.to_string();
            result.insert_str(pos, &pixel_tag);
            result
        } else {
            format!("{}{}", html, pixel_tag)
        }
    }

    /// Rewrite all <a href="..."> links to go through the click tracking endpoint
    fn rewrite_links(&self, email_id: Uuid, html: &str) -> (String, Vec<ExtractedLink>) {
        let mut links = Vec::new();
        let mut result = String::with_capacity(html.len() + 256);
        let mut link_index: i32 = 0;

        let mut remaining = html;
        while let Some(href_start) = find_href_start(remaining) {
            // Copy everything before href="
            result.push_str(&remaining[..href_start.offset]);

            let after_href = &remaining[href_start.offset + href_start.prefix_len..];

            // Find the closing quote
            if let Some(end_pos) = after_href.find(href_start.quote) {
                let original_url = &after_href[..end_pos];

                // Only track http/https links, skip mailto:, tel:, #, javascript:
                if should_track_link(original_url) {
                    let tracking_url = format!(
                        "{}/api/emails/{}/track/click/{}",
                        self.base_url, email_id, link_index
                    );

                    links.push(ExtractedLink {
                        index: link_index,
                        original_url: original_url.to_string(),
                    });

                    result.push_str(&format!(
                        "href={}{}{}",
                        href_start.quote, tracking_url, href_start.quote
                    ));
                    link_index += 1;
                } else {
                    // Keep original
                    result.push_str(&format!(
                        "href={}{}{}",
                        href_start.quote, original_url, href_start.quote
                    ));
                }

                remaining = &after_href[end_pos + 1..];
            } else {
                // Malformed href, copy as-is
                result.push_str(
                    &remaining[href_start.offset..href_start.offset + href_start.prefix_len],
                );
                remaining = after_href;
            }
        }

        // Copy the rest
        result.push_str(remaining);

        (result, links)
    }

    /// Store extracted links in the database
    pub async fn store_links(
        &self,
        email_id: Uuid,
        links: &[ExtractedLink],
    ) -> Result<(), EmailError> {
        for link in links {
            let model = email_links::ActiveModel {
                email_id: Set(email_id),
                link_index: Set(link.index),
                original_url: Set(link.original_url.clone()),
                click_count: Set(0),
                ..Default::default()
            };
            model.insert(self.db.as_ref()).await?;
        }
        Ok(())
    }

    /// Record an open event and return the email_id if valid
    pub async fn record_open(
        &self,
        email_id: Uuid,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<(), EmailError> {
        // Verify email exists and has tracking enabled
        let email = emails::Entity::find_by_id(email_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| EmailError::EmailNotFound(email_id.to_string()))?;

        if !email.track_opens {
            debug!("Open tracking not enabled for email {}", email_id);
            return Ok(());
        }

        // Record the event
        let event = email_events::ActiveModel {
            email_id: Set(email_id),
            event_type: Set("open".to_string()),
            ip_address: Set(ip_address),
            user_agent: Set(user_agent),
            ..Default::default()
        };
        event.insert(self.db.as_ref()).await?;

        // Update email counters
        let mut active: emails::ActiveModel = email.into();
        let current_count = active.open_count.clone().unwrap();
        active.open_count = Set(current_count + 1);
        if current_count == 0 {
            active.first_opened_at = Set(Some(Utc::now()));
        }
        active.update(self.db.as_ref()).await?;

        debug!("Recorded open event for email {}", email_id);
        Ok(())
    }

    /// Record a click event and return the redirect URL
    pub async fn record_click(
        &self,
        email_id: Uuid,
        link_index: i32,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<String, EmailError> {
        // Look up the original URL from the links table
        let link = email_links::Entity::find()
            .filter(email_links::Column::EmailId.eq(email_id))
            .filter(email_links::Column::LinkIndex.eq(link_index))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                EmailError::Validation(format!(
                    "Link index {} not found for email {}",
                    link_index, email_id
                ))
            })?;

        let redirect_url = link.original_url.clone();

        // Record the event
        let event = email_events::ActiveModel {
            email_id: Set(email_id),
            event_type: Set("click".to_string()),
            link_url: Set(Some(redirect_url.clone())),
            link_index: Set(Some(link_index)),
            ip_address: Set(ip_address),
            user_agent: Set(user_agent),
            ..Default::default()
        };
        event.insert(self.db.as_ref()).await?;

        // Update link click count
        let mut active_link: email_links::ActiveModel = link.into();
        let current = active_link.click_count.clone().unwrap();
        active_link.click_count = Set(current + 1);
        active_link.update(self.db.as_ref()).await?;

        // Update email click counters
        let email = emails::Entity::find_by_id(email_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| EmailError::EmailNotFound(email_id.to_string()))?;

        let mut active_email: emails::ActiveModel = email.into();
        let current_count = active_email.click_count.clone().unwrap();
        active_email.click_count = Set(current_count + 1);
        if current_count == 0 {
            active_email.first_clicked_at = Set(Some(Utc::now()));
        }
        active_email.update(self.db.as_ref()).await?;

        debug!(
            "Recorded click event for email {}, link_index {}",
            email_id, link_index
        );
        Ok(redirect_url)
    }

    /// Get tracking events for an email
    pub async fn get_events(
        &self,
        email_id: Uuid,
        event_type: Option<&str>,
    ) -> Result<Vec<email_events::Model>, EmailError> {
        let mut query =
            email_events::Entity::find().filter(email_events::Column::EmailId.eq(email_id));

        if let Some(et) = event_type {
            query = query.filter(email_events::Column::EventType.eq(et));
        }

        let events = query
            .order_by_asc(email_events::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(events)
    }

    /// Get tracked links for an email
    pub async fn get_links(&self, email_id: Uuid) -> Result<Vec<email_links::Model>, EmailError> {
        let links = email_links::Entity::find()
            .filter(email_links::Column::EmailId.eq(email_id))
            .all(self.db.as_ref())
            .await?;
        Ok(links)
    }
}

/// Information about where an href= attribute starts
struct HrefMatch {
    offset: usize,
    prefix_len: usize,
    quote: char,
}

/// Find the next href="..." or href='...' in the string
fn find_href_start(s: &str) -> Option<HrefMatch> {
    let lower = s.to_lowercase();
    let patterns = ["href=\"", "href='", "href =\"", "href ='"];

    let mut best: Option<(usize, usize, char)> = None;

    for pattern in &patterns {
        if let Some(pos) = lower.find(pattern) {
            let quote = if pattern.ends_with('"') { '"' } else { '\'' };
            match best {
                Some((best_pos, _, _)) if pos < best_pos => {
                    best = Some((pos, pattern.len(), quote));
                }
                None => {
                    best = Some((pos, pattern.len(), quote));
                }
                _ => {}
            }
        }
    }

    best.map(|(offset, prefix_len, quote)| HrefMatch {
        offset,
        prefix_len,
        quote,
    })
}

/// Should this link be rewritten for click tracking?
fn should_track_link(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Only track http and https links
    trimmed.starts_with("http://") || trimmed.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_service() -> TrackingService {
        // Create a mock DB connection for unit tests
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        TrackingService::new(Arc::new(db), "https://app.example.com".to_string())
    }

    #[test]
    fn test_inject_tracking_pixel_with_body_tag() {
        let service = create_service();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = "<html><body><h1>Hello</h1></body></html>";

        let result = service.inject_tracking_pixel(email_id, html);

        assert!(result.contains("/api/emails/550e8400-e29b-41d4-a716-446655440000/track/open"));
        assert!(result.contains(r#"width="1" height="1""#));
        // Pixel should be before </body>
        let pixel_pos = result.find("track/open").unwrap();
        let body_pos = result.rfind("</body>").unwrap();
        assert!(pixel_pos < body_pos);
    }

    #[test]
    fn test_inject_tracking_pixel_without_body_tag() {
        let service = create_service();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = "<h1>Hello</h1><p>World</p>";

        let result = service.inject_tracking_pixel(email_id, html);

        assert!(result.contains("track/open"));
        assert!(result.ends_with("/>"));
    }

    #[test]
    fn test_rewrite_links_http() {
        let service = create_service();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<a href="https://example.com/pricing">Click here</a>"#;

        let (rewritten, links) = service.rewrite_links(email_id, html);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].index, 0);
        assert_eq!(links[0].original_url, "https://example.com/pricing");
        assert!(
            rewritten.contains("/api/emails/550e8400-e29b-41d4-a716-446655440000/track/click/0")
        );
        assert!(!rewritten.contains("https://example.com/pricing"));
    }

    #[test]
    fn test_rewrite_links_skips_mailto() {
        let service = create_service();
        let email_id = Uuid::new_v4();
        let html = r#"<a href="mailto:support@example.com">Email us</a>"#;

        let (rewritten, links) = service.rewrite_links(email_id, html);

        assert!(links.is_empty());
        assert!(rewritten.contains("mailto:support@example.com"));
    }

    #[test]
    fn test_rewrite_links_skips_anchors() {
        let service = create_service();
        let email_id = Uuid::new_v4();
        let html = "<a href=\"#section\">Jump</a>";

        let (rewritten, links) = service.rewrite_links(email_id, html);

        assert!(links.is_empty());
        assert!(rewritten.contains("#section"));
    }

    #[test]
    fn test_rewrite_multiple_links() {
        let service = create_service();
        let email_id = Uuid::new_v4();
        let html = r#"<a href="https://example.com/a">A</a> <a href="https://example.com/b">B</a>"#;

        let (rewritten, links) = service.rewrite_links(email_id, html);

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].index, 0);
        assert_eq!(links[1].index, 1);
        assert!(rewritten.contains("track/click/0"));
        assert!(rewritten.contains("track/click/1"));
    }

    #[test]
    fn test_transform_html_both_tracking() {
        let service = create_service();
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body><a href="https://example.com">Link</a></body></html>"#;

        let result = service.transform_html(email_id, html, true, true);

        assert!(result.html.contains("track/open"));
        assert!(result.html.contains("track/click/0"));
        assert_eq!(result.links.len(), 1);
    }

    #[test]
    fn test_transform_html_no_tracking() {
        let service = create_service();
        let email_id = Uuid::new_v4();
        let html = r#"<a href="https://example.com">Link</a>"#;

        let result = service.transform_html(email_id, html, false, false);

        assert!(!result.html.contains("track/open"));
        assert!(!result.html.contains("track/click"));
        assert!(result.links.is_empty());
    }

    #[test]
    fn test_should_track_link() {
        assert!(should_track_link("https://example.com"));
        assert!(should_track_link("http://example.com"));
        assert!(!should_track_link("mailto:test@example.com"));
        assert!(!should_track_link("tel:+1234567890"));
        assert!(!should_track_link("#section"));
        assert!(!should_track_link("javascript:void(0)"));
        assert!(!should_track_link(""));
    }

    #[test]
    fn test_rewrite_links_with_single_quotes() {
        let service = create_service();
        let email_id = Uuid::new_v4();
        let html = "<a href='https://example.com/page'>Link</a>";

        let (rewritten, links) = service.rewrite_links(email_id, html);

        assert_eq!(links.len(), 1);
        assert!(rewritten.contains("track/click/0"));
    }

    #[test]
    fn test_rewrite_preserves_non_link_content() {
        let service = create_service();
        let email_id = Uuid::new_v4();
        let html = r#"<p>Hello World</p><img src="https://example.com/img.png" /><a href="https://example.com">Link</a>"#;

        let (rewritten, links) = service.rewrite_links(email_id, html);

        assert_eq!(links.len(), 1);
        // img src should NOT be rewritten
        assert!(rewritten.contains(r#"src="https://example.com/img.png""#));
    }
}
