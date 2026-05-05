//! Integration tests for the tracking service
//! These tests require Docker (PostgreSQL via testcontainers)

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{email_links, emails};
    use uuid::Uuid;

    use crate::services::TrackingService;

    fn create_test_config_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_config::ConfigService> {
        // Create a minimal ServerConfig for tests
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "0.0.0.0:3000".to_string(),
            database_url: "postgres://localhost/test".to_string(),
            tls_address: None,
            console_address: "0.0.0.0:3001".to_string(),
            data_dir: std::path::PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-encryption-key-32bytes!!!!!".to_string(),
            api_base_url: "http://localhost:3000".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        });
        Arc::new(temps_config::ConfigService::new(server_config, db))
    }

    async fn setup_test_env() -> (TestDatabase, Arc<TrackingService>) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let config_service = create_test_config_service(db.db.clone());
        let tracking_service = Arc::new(TrackingService::with_base_url(
            db.db.clone(),
            config_service,
            "https://app.example.com".to_string(),
        ));
        (db, tracking_service)
    }

    /// Create a test email directly in the database
    async fn create_test_email(
        db: &Arc<sea_orm::DatabaseConnection>,
        track_opens: bool,
        track_clicks: bool,
    ) -> Uuid {
        let email_id = Uuid::new_v4();
        let email = emails::ActiveModel {
            id: Set(email_id),
            from_address: Set("sender@test.com".to_string()),
            to_addresses: Set(serde_json::json!(["recipient@test.com"])),
            subject: Set("Test email".to_string()),
            html_body: Set(Some(
                r#"<html><body><a href="https://example.com/page1">Link 1</a><a href="https://example.com/page2">Link 2</a></body></html>"#.to_string(),
            )),
            status: Set("sent".to_string()),
            track_opens: Set(track_opens),
            track_clicks: Set(track_clicks),
            open_count: Set(0),
            click_count: Set(0),
            ..Default::default()
        };
        email.insert(db.as_ref()).await.unwrap();
        email_id
    }

    /// Store test links for an email
    async fn create_test_links(db: &Arc<sea_orm::DatabaseConnection>, email_id: Uuid) {
        for (idx, url) in ["https://example.com/page1", "https://example.com/page2"]
            .iter()
            .enumerate()
        {
            let link = email_links::ActiveModel {
                email_id: Set(email_id),
                link_index: Set(idx as i32),
                original_url: Set(url.to_string()),
                click_count: Set(0),
                ..Default::default()
            };
            link.insert(db.as_ref()).await.unwrap();
        }
    }

    // ============================================
    // HTML Transformation Tests (use static methods)
    // ============================================

    const TEST_BASE_URL: &str = "https://app.example.com";

    #[test]
    fn test_transform_html_injects_pixel_and_rewrites_links() {
        let email_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let html = r#"<html><body><a href="https://example.com/pricing">Pricing</a><a href="https://example.com/docs">Docs</a></body></html>"#;

        let pixel_html = TrackingService::inject_tracking_pixel(TEST_BASE_URL, email_id, html);
        assert!(
            pixel_html.contains("/api/emails/550e8400-e29b-41d4-a716-446655440000/track/open"),
            "Missing tracking pixel"
        );

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);
        assert!(
            rewritten.contains("/track/click/0"),
            "First link not rewritten"
        );
        assert!(
            rewritten.contains("/track/click/1"),
            "Second link not rewritten"
        );
        assert!(
            !rewritten.contains(r#"href="https://example.com/pricing""#),
            "Original URL should be replaced"
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].original_url, "https://example.com/pricing");
        assert_eq!(links[1].original_url, "https://example.com/docs");
    }

    #[test]
    fn test_transform_html_only_opens() {
        let email_id = Uuid::new_v4();
        let html = r#"<a href="https://example.com">Link</a>"#;

        let pixel_html = TrackingService::inject_tracking_pixel(TEST_BASE_URL, email_id, html);
        assert!(pixel_html.contains("track/open"), "Should have pixel");
        assert!(
            !pixel_html.contains("track/click"),
            "Should NOT have click tracking"
        );
    }

    #[test]
    fn test_transform_html_only_clicks() {
        let email_id = Uuid::new_v4();
        let html = r#"<a href="https://example.com">Link</a>"#;

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);
        assert!(
            rewritten.contains("track/click"),
            "Should have click tracking"
        );
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_transform_preserves_mailto_and_anchor_links() {
        let email_id = Uuid::new_v4();
        let html = "<a href=\"mailto:test@example.com\">Email</a> <a href=\"#top\">Top</a> <a href=\"https://example.com\">Link</a>";

        let (rewritten, links) = TrackingService::rewrite_links(TEST_BASE_URL, email_id, html);
        assert!(
            rewritten.contains("mailto:test@example.com"),
            "mailto should be preserved"
        );
        assert!(rewritten.contains("#top"), "Anchor should be preserved");
        assert_eq!(links.len(), 1, "Only HTTP link should be tracked");
        assert_eq!(links[0].original_url, "https://example.com");
    }

    // ============================================
    // Integration Tests (Require Docker)
    // ============================================

    #[tokio::test]
    async fn test_record_open_increments_counter() {
        let (db, tracking) = setup_test_env().await;

        // Create email with open tracking
        let email_id = create_test_email(&db.db, true, false).await;

        // Record first open
        tracking
            .record_open(
                email_id,
                Some("1.2.3.4".to_string()),
                Some("TestAgent".to_string()),
            )
            .await
            .unwrap();

        // Verify email counter was updated
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.open_count, 1);
        assert!(email.first_opened_at.is_some());

        // Record second open
        tracking
            .record_open(
                email_id,
                Some("5.6.7.8".to_string()),
                Some("TestAgent2".to_string()),
            )
            .await
            .unwrap();

        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.open_count, 2);

        // Verify events recorded
        let events = tracking.get_events(email_id, Some("open")).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].ip_address, Some("1.2.3.4".to_string()));
        assert_eq!(events[1].ip_address, Some("5.6.7.8".to_string()));
    }

    #[tokio::test]
    async fn test_record_open_skips_when_tracking_disabled() {
        let (db, tracking) = setup_test_env().await;

        // Create email WITHOUT open tracking
        let email_id = create_test_email(&db.db, false, false).await;

        // Record open - should not fail but should not increment
        tracking
            .record_open(email_id, Some("1.2.3.4".to_string()), None)
            .await
            .unwrap();

        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            email.open_count, 0,
            "Should not increment when tracking disabled"
        );

        let events = tracking.get_events(email_id, Some("open")).await.unwrap();
        assert!(
            events.is_empty(),
            "Should not record event when tracking disabled"
        );
    }

    #[tokio::test]
    async fn test_record_click_returns_redirect_url() {
        let (db, tracking) = setup_test_env().await;

        let email_id = create_test_email(&db.db, false, true).await;
        create_test_links(&db.db, email_id).await;

        // Click link index 0
        let redirect_url = tracking
            .record_click(
                email_id,
                0,
                Some("1.2.3.4".to_string()),
                Some("Agent".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(redirect_url, "https://example.com/page1");

        // Click link index 1
        let redirect_url = tracking
            .record_click(email_id, 1, Some("1.2.3.4".to_string()), None)
            .await
            .unwrap();

        assert_eq!(redirect_url, "https://example.com/page2");

        // Verify counters
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.click_count, 2);
        assert!(email.first_clicked_at.is_some());

        // Verify link click counts
        let links = tracking.get_links(email_id).await.unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].click_count, 1);
        assert_eq!(links[1].click_count, 1);

        // Verify events
        let events = tracking.get_events(email_id, Some("click")).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].link_index, Some(0));
        assert_eq!(events[1].link_index, Some(1));
    }

    #[tokio::test]
    async fn test_record_click_invalid_link_index() {
        let (db, tracking) = setup_test_env().await;

        let email_id = create_test_email(&db.db, false, true).await;
        // No links stored

        let result = tracking.record_click(email_id, 999, None, None).await;

        assert!(result.is_err(), "Should fail for invalid link index");
    }

    #[tokio::test]
    async fn test_record_open_nonexistent_email() {
        let (_db, tracking) = setup_test_env().await;

        let result = tracking.record_open(Uuid::new_v4(), None, None).await;

        assert!(result.is_err(), "Should fail for nonexistent email");
    }

    #[tokio::test]
    async fn test_store_and_retrieve_links() {
        let (db, tracking) = setup_test_env().await;

        let email_id = create_test_email(&db.db, false, true).await;

        let links = vec![
            crate::services::ExtractedLink {
                index: 0,
                original_url: "https://example.com/a".to_string(),
            },
            crate::services::ExtractedLink {
                index: 1,
                original_url: "https://example.com/b".to_string(),
            },
        ];

        tracking.store_links(email_id, &links).await.unwrap();

        let stored = tracking.get_links(email_id).await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].original_url, "https://example.com/a");
        assert_eq!(stored[1].original_url, "https://example.com/b");
        assert_eq!(stored[0].click_count, 0);
    }

    #[tokio::test]
    async fn test_get_events_filtered_by_type() {
        let (db, tracking) = setup_test_env().await;

        let email_id = create_test_email(&db.db, true, true).await;
        create_test_links(&db.db, email_id).await;

        // Record mixed events
        tracking
            .record_open(email_id, Some("1.1.1.1".to_string()), None)
            .await
            .unwrap();
        tracking
            .record_click(email_id, 0, Some("2.2.2.2".to_string()), None)
            .await
            .unwrap();
        tracking
            .record_open(email_id, Some("3.3.3.3".to_string()), None)
            .await
            .unwrap();

        // Get all events
        let all_events = tracking.get_events(email_id, None).await.unwrap();
        assert_eq!(all_events.len(), 3);

        // Filter opens only
        let opens = tracking.get_events(email_id, Some("open")).await.unwrap();
        assert_eq!(opens.len(), 2);

        // Filter clicks only
        let clicks = tracking.get_events(email_id, Some("click")).await.unwrap();
        assert_eq!(clicks.len(), 1);
    }

    #[tokio::test]
    async fn test_multiple_clicks_on_same_link() {
        let (db, tracking) = setup_test_env().await;

        let email_id = create_test_email(&db.db, false, true).await;
        create_test_links(&db.db, email_id).await;

        // Click same link 3 times
        for _ in 0..3 {
            tracking
                .record_click(email_id, 0, Some("1.2.3.4".to_string()), None)
                .await
                .unwrap();
        }

        // Verify link click count
        let links = tracking.get_links(email_id).await.unwrap();
        let link_0 = links.iter().find(|l| l.link_index == 0).unwrap();
        assert_eq!(link_0.click_count, 3);

        // Verify email total click count
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.click_count, 3);

        // first_clicked_at should be set from first click only
        assert!(email.first_clicked_at.is_some());
    }
}
