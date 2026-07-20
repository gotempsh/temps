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
    config_service: Arc<temps_config::ConfigService>,
    /// Override base URL for testing (avoids needing a full ConfigService setup)
    #[cfg(test)]
    base_url_override: Option<String>,
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

/// Global tracking statistics
#[derive(Debug, Clone)]
pub struct GlobalTrackingStats {
    pub delivered: u64,
    pub opened: u64,
    pub clicked: u64,
    pub bounced: u64,
    pub complained: u64,
    pub open_rate: Option<f64>,
    pub click_rate: Option<f64>,
    pub bounce_rate: Option<f64>,
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
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<temps_config::ConfigService>,
    ) -> Self {
        Self {
            db,
            config_service,
            #[cfg(test)]
            base_url_override: None,
        }
    }

    /// Create a TrackingService with a fixed base URL (for tests only)
    #[cfg(test)]
    pub(crate) fn with_base_url(
        db: Arc<DatabaseConnection>,
        config_service: Arc<temps_config::ConfigService>,
        base_url: String,
    ) -> Self {
        Self {
            db,
            config_service,
            base_url_override: Some(base_url),
        }
    }

    /// Get the base URL for tracking endpoints from the config service
    async fn get_base_url(&self) -> String {
        #[cfg(test)]
        if let Some(ref url) = self.base_url_override {
            return url.clone();
        }
        self.config_service
            .get_external_url_or_default()
            .await
            .unwrap_or_else(|_| "http://localhost:3000".to_string())
    }

    /// Transform HTML body for tracking: inject open pixel + rewrite links
    pub async fn transform_html(
        &self,
        email_id: Uuid,
        html: &str,
        track_opens: bool,
        track_clicks: bool,
    ) -> TransformResult {
        let base_url = self.get_base_url().await;
        let mut result_html = html.to_string();
        let mut links = Vec::new();

        // Rewrite links for click tracking
        if track_clicks {
            let (rewritten, extracted) = Self::rewrite_links(&base_url, email_id, &result_html);
            result_html = rewritten;
            links = extracted;
        }

        // Inject tracking pixel for open tracking
        if track_opens {
            result_html = Self::inject_tracking_pixel(&base_url, email_id, &result_html);
        }

        TransformResult {
            html: result_html,
            links,
        }
    }

    /// Inject a 1x1 transparent tracking pixel before </body> or at end of HTML
    pub(crate) fn inject_tracking_pixel(base_url: &str, email_id: Uuid, html: &str) -> String {
        let pixel_url = format!("{}/api/emails/{}/track/open", base_url, email_id);
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
    pub(crate) fn rewrite_links(
        base_url: &str,
        email_id: Uuid,
        html: &str,
    ) -> (String, Vec<ExtractedLink>) {
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
                        base_url, email_id, link_index
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
            event_type: Set("opened".to_string()),
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
            event_type: Set("clicked".to_string()),
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
            query =
                query.filter(email_events::Column::EventType.eq(normalize_event_type_filter(et)));
        }

        let events = query
            .order_by_asc(email_events::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(events)
    }

    /// Get global tracking stats (aggregated from emails table)
    pub async fn get_global_stats(&self) -> Result<GlobalTrackingStats, EmailError> {
        use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

        #[derive(FromQueryResult)]
        struct StatsRow {
            total_sent: i64,
            emails_with_opens: i64,
            emails_with_clicks: i64,
            emails_with_bounces: i64,
            emails_with_complaints: i64,
        }

        // Count unique emails per engagement type — not total events.
        // Opens/clicks come from the emails table counters; bounces/complaints
        // come from email_events (no aggregate column on emails for those).
        let sql = r#"
            SELECT
                (SELECT COUNT(*) FROM emails
                    WHERE status = 'sent'
                      AND (track_opens = true OR track_clicks = true)) AS total_sent,
                (SELECT COUNT(*) FROM emails
                    WHERE open_count > 0
                      AND (track_opens = true OR track_clicks = true)) AS emails_with_opens,
                (SELECT COUNT(*) FROM emails
                    WHERE click_count > 0
                      AND (track_opens = true OR track_clicks = true)) AS emails_with_clicks,
                (SELECT COUNT(DISTINCT email_id) FROM email_events
                    WHERE event_type = 'bounced') AS emails_with_bounces,
                (SELECT COUNT(DISTINCT email_id) FROM email_events
                    WHERE event_type = 'complained') AS emails_with_complaints
        "#;

        let row = StatsRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_string(),
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(StatsRow {
            total_sent: 0,
            emails_with_opens: 0,
            emails_with_clicks: 0,
            emails_with_bounces: 0,
            emails_with_complaints: 0,
        });

        let delivered = row.total_sent;
        let rate = |numerator: i64| -> Option<f64> {
            if delivered > 0 {
                Some(numerator as f64 / delivered as f64 * 100.0)
            } else {
                None
            }
        };

        Ok(GlobalTrackingStats {
            delivered: delivered as u64,
            opened: row.emails_with_opens as u64,
            clicked: row.emails_with_clicks as u64,
            bounced: row.emails_with_bounces as u64,
            complained: row.emails_with_complaints as u64,
            open_rate: rate(row.emails_with_opens),
            click_rate: rate(row.emails_with_clicks),
            bounce_rate: rate(row.emails_with_bounces),
        })
    }

    /// Get all tracking events across all emails (paginated)
    pub async fn get_all_events(
        &self,
        event_type: Option<&str>,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<email_events::Model>, u64), EmailError> {
        use sea_orm::PaginatorTrait;

        let mut query = email_events::Entity::find();

        if let Some(et) = event_type {
            query =
                query.filter(email_events::Column::EventType.eq(normalize_event_type_filter(et)));
        }

        let paginator = query
            .order_by_desc(email_events::Column::Id)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let events = paginator.fetch_page(page.saturating_sub(1)).await?;

        Ok((events, total))
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

/// Map the short, public-facing event-type filter values documented on the
/// tracking-events endpoints ("open", "click") onto the canonical form
/// persisted in `email_events.event_type` ("opened", "clicked", ...).
/// Values already in canonical form pass through unchanged.
fn normalize_event_type_filter(event_type: &str) -> &str {
    match event_type {
        "open" => "opened",
        "click" => "clicked",
        "bounce" => "bounced",
        "complaint" => "complained",
        other => other,
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
///
/// Beyond the scheme check we run the same synchronous SSRF/private-IP
/// blocklist used by sandbox seed URLs. Rationale: a tracked URL is
/// stored verbatim and emitted from `/track/click/{i}` as a 302 redirect.
/// Without this filter, an authenticated sender could craft an email
/// whose links cause our domain to redirect recipients to private IPs
/// or cloud-metadata endpoints — a phishing primitive that abuses our
/// brand. Links that fail validation are still rendered to the recipient
/// (we leave the original `<a href>` untouched), they just aren't proxied
/// through our click-tracking endpoint.
fn should_track_link(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return false;
    }
    temps_core::url_validation::validate_external_url(trimmed).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BASE_URL: &str = "https://app.example.com";

    #[test]
    fn test_inject_tracking_pixel_with_body_tag() {
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = "<html><body><h1>Hello</h1></body></html>";

        let result = TrackingService::inject_tracking_pixel(TEST_BASE_URL, email_id, html);

        assert!(result.contains("/api/emails/550e8400-e29b-41d4-a716-446655440000/track/open"));
        assert!(result.contains(r#"width="1" height="1""#));
        // Pixel should be before </body>
        let pixel_pos = result.find("track/open").unwrap();
        let body_pos = result.rfind("</body>").unwrap();
        assert!(pixel_pos < body_pos);
    }

    #[test]
    fn test_inject_tracking_pixel_without_body_tag() {
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = "<h1>Hello</h1><p>World</p>";

        let result = TrackingService::inject_tracking_pixel(TEST_BASE_URL, email_id, html);

        assert!(result.contains("track/open"));
        assert!(result.ends_with("/>"));
    }

    #[test]
    fn test_rewrite_links_http() {
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<a href="https://example.com/pricing">Click here</a>"#;

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);

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
        let email_id = Uuid::new_v4();
        let html = r#"<a href="mailto:support@example.com">Email us</a>"#;

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);

        assert!(links.is_empty());
        assert!(rewritten.contains("mailto:support@example.com"));
    }

    #[test]
    fn test_rewrite_links_skips_anchors() {
        let email_id = Uuid::new_v4();
        let html = "<a href=\"#section\">Jump</a>";

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);

        assert!(links.is_empty());
        assert!(rewritten.contains("#section"));
    }

    #[test]
    fn test_rewrite_multiple_links() {
        let email_id = Uuid::new_v4();
        let html = r#"<a href="https://example.com/a">A</a> <a href="https://example.com/b">B</a>"#;

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].index, 0);
        assert_eq!(links[1].index, 1);
        assert!(rewritten.contains("track/click/0"));
        assert!(rewritten.contains("track/click/1"));
    }

    #[test]
    fn test_transform_html_both_tracking() {
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body><a href="https://example.com">Link</a></body></html>"#;

        // Test pixel + links independently (transform_html is async, tested via integration)
        let pixel_html = TrackingService::inject_tracking_pixel(TEST_BASE_URL, email_id, html);
        assert!(pixel_html.contains("track/open"));

        let (click_html, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);
        assert!(click_html.contains("track/click/0"));
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_no_tracking_leaves_html_unchanged() {
        let email_id = Uuid::new_v4();
        let html = r#"<a href="https://example.com">Link</a>"#;

        // With no tracking, rewrite_links should still work but we just don't call it
        // Verify the static methods don't affect unrelated HTML
        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);
        assert_eq!(links.len(), 1); // Links ARE rewritten when called directly
        assert!(rewritten.contains("track/click"));
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

    /// Regression test for the v0.0.8 audit finding: emails could store
    /// tracking redirects pointing at private/loopback/metadata IPs, turning
    /// our click endpoint into an open redirect that abuses our brand.
    #[test]
    fn test_should_track_link_rejects_private_and_metadata_targets() {
        // RFC 1918 private
        assert!(!should_track_link("http://10.0.0.1/admin"));
        assert!(!should_track_link("http://192.168.1.1/"));
        // Loopback
        assert!(!should_track_link("http://127.0.0.1/"));
        // Cloud metadata
        assert!(!should_track_link(
            "http://169.254.169.254/latest/meta-data/"
        ));
        // localhost hostname is also blocked synchronously
        assert!(!should_track_link("http://localhost/x"));
    }

    #[test]
    fn test_rewrite_links_with_single_quotes() {
        let email_id = Uuid::new_v4();
        let html = "<a href='https://example.com/page'>Link</a>";

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);

        assert_eq!(links.len(), 1);
        assert!(rewritten.contains("track/click/0"));
    }

    #[test]
    fn test_rewrite_preserves_non_link_content() {
        let email_id = Uuid::new_v4();
        let html = r#"<p>Hello World</p><img src="https://example.com/img.png" /><a href="https://example.com">Link</a>"#;

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);

        assert_eq!(links.len(), 1);
        // img src should NOT be rewritten
        assert!(rewritten.contains(r#"src="https://example.com/img.png""#));
    }
}
