//! Integration tests for the tracking service
//! These tests require Docker (PostgreSQL via testcontainers)

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use sea_orm::{
        ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait,
        MockDatabase, Statement,
    };
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{email_events, email_links, emails};
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
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
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

    async fn setup_test_env() -> Option<(TestDatabase, Arc<TrackingService>)> {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if error.to_string().contains("failed to create a container") => {
                eprintln!("Docker is unavailable; skipping PostgreSQL integration test: {error}");
                return None;
            }
            Err(error) => panic!("PostgreSQL test database setup failed: {error}"),
        };
        temps_database::run_post_migration_indexes(&db.database_url)
            .await
            .unwrap();
        let config_service = create_test_config_service(db.db.clone());
        let tracking_service = Arc::new(TrackingService::with_base_url(
            db.db.clone(),
            config_service,
            "https://app.example.com".to_string(),
        ));
        Some((db, tracking_service))
    }

    macro_rules! require_test_env {
        () => {
            match setup_test_env().await {
                Some(environment) => environment,
                None => return,
            }
        };
    }

    #[tokio::test]
    async fn test_tracking_retention_index_is_created_with_expected_columns() {
        let (db, _) = require_test_env!();
        let row = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT indexes.indexdef, index.indisvalid, index.indisready \
                 FROM pg_indexes AS indexes \
                 JOIN pg_class AS class ON class.oid = to_regclass(indexes.indexname) \
                 JOIN pg_index AS index ON index.indexrelid = class.oid \
                 WHERE indexes.schemaname = current_schema() \
                   AND indexes.indexname = 'idx_email_events_tracking_retention'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .expect("retention index should exist");
        let definition: String = row.try_get("", "indexdef").unwrap();
        assert!(definition.contains("(created_at, id)"));
        assert!(definition.contains("ip_address IS NOT NULL"));
        assert!(definition.contains("user_agent IS NOT NULL"));
        assert!(row.try_get::<bool>("", "indisvalid").unwrap());
        assert!(row.try_get::<bool>("", "indisready").unwrap());
    }

    #[tokio::test]
    async fn test_post_migration_indexes_repairs_invalid_retention_index() {
        let (db, _) = require_test_env!();
        let (_single_connection_pool, _backend_pid) =
            temps_database::connect_for_migrate(&db.database_url)
                .await
                .unwrap();
        db.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "UPDATE pg_index SET indisvalid = FALSE, indisready = FALSE \
                 WHERE indexrelid = to_regclass('idx_email_events_tracking_retention')"
                    .to_string(),
            ))
            .await
            .unwrap();

        temps_database::run_post_migration_indexes(&db.database_url)
            .await
            .unwrap();

        let row = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT indisvalid, indisready FROM pg_index \
                 WHERE indexrelid = to_regclass('idx_email_events_tracking_retention')"
                    .to_string(),
            ))
            .await
            .unwrap()
            .expect("repaired retention index should exist");
        assert!(row.try_get::<bool>("", "indisvalid").unwrap());
        assert!(row.try_get::<bool>("", "indisready").unwrap());
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
        let (db, tracking) = require_test_env!();

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
        let (db, tracking) = require_test_env!();

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
        let (db, tracking) = require_test_env!();

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
        let (db, tracking) = require_test_env!();

        let email_id = create_test_email(&db.db, false, true).await;
        // No links stored

        let result = tracking.record_click(email_id, 999, None, None).await;

        assert!(result.is_err(), "Should fail for invalid link index");
    }

    #[tokio::test]
    async fn test_record_open_nonexistent_email() {
        let (_db, tracking) = require_test_env!();

        let result = tracking.record_open(Uuid::new_v4(), None, None).await;

        assert!(result.is_err(), "Should fail for nonexistent email");
    }

    #[tokio::test]
    async fn test_store_and_retrieve_links() {
        let (db, tracking) = require_test_env!();

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
        let (db, tracking) = require_test_env!();

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
        let (db, tracking) = require_test_env!();

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

    #[tokio::test]
    async fn test_redact_connection_metadata_older_than_redacts_only_stale_rows() {
        let (db, tracking) = require_test_env!();

        let email_id = create_test_email(&db.db, true, true).await;

        let old_event = email_events::ActiveModel {
            email_id: Set(email_id),
            event_type: Set("opened".to_string()),
            provider_message_id: Set(Some("provider-old-1".to_string())),
            recipient: Set(Some("old-recipient@example.com".to_string())),
            metadata: Set(Some(serde_json::json!({"campaign": "private-segment"}))),
            link_url: Set(Some("https://example.com/private-link".to_string())),
            ip_address: Set(Some("1.1.1.1".to_string())),
            user_agent: Set(Some("old-ua".to_string())),
            created_at: Set(chrono::Utc::now() - chrono::Duration::days(100)),
            ..Default::default()
        };
        old_event.insert(db.db.as_ref()).await.unwrap();

        let recent_event = email_events::ActiveModel {
            email_id: Set(email_id),
            event_type: Set("opened".to_string()),
            ip_address: Set(Some("2.2.2.2".to_string())),
            user_agent: Set(Some("recent-ua".to_string())),
            created_at: Set(chrono::Utc::now() - chrono::Duration::days(1)),
            ..Default::default()
        };
        recent_event.insert(db.db.as_ref()).await.unwrap();

        let redacted = tracking
            .redact_connection_metadata_older_than(90)
            .await
            .unwrap();
        assert_eq!(redacted, 1, "should only redact the 100-day-old event");

        let remaining = tracking.get_events(email_id, None).await.unwrap();
        assert_eq!(remaining.len(), 2, "retention must preserve event facts");
        assert_eq!(remaining[0].ip_address, None);
        assert_eq!(remaining[0].user_agent, None);
        assert_eq!(
            remaining[0].recipient.as_deref(),
            Some("old-recipient@example.com")
        );
        assert!(remaining[0].metadata.is_some());
        assert_eq!(
            remaining[0].link_url.as_deref(),
            Some("https://example.com/private-link")
        );
        assert_eq!(remaining[0].event_type, "opened");
        assert_eq!(
            remaining[0].provider_message_id.as_deref(),
            Some("provider-old-1")
        );
        assert_eq!(remaining[1].ip_address.as_deref(), Some("2.2.2.2"));
        assert_eq!(remaining[1].user_agent.as_deref(), Some("recent-ua"));
    }

    #[tokio::test]
    async fn test_redact_connection_metadata_before_preserves_exact_cutoff_and_recent_rows() {
        let (db, tracking) = require_test_env!();
        let email_id = create_test_email(&db.db, true, true).await;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(90);

        for (created_at, user_agent) in [
            (cutoff - chrono::Duration::seconds(1), "stale"),
            (cutoff, "at-cutoff"),
            (cutoff + chrono::Duration::seconds(1), "recent"),
        ] {
            email_events::ActiveModel {
                email_id: Set(email_id),
                event_type: Set("opened".to_string()),
                user_agent: Set(Some(user_agent.to_string())),
                created_at: Set(created_at),
                ..Default::default()
            }
            .insert(db.db.as_ref())
            .await
            .unwrap();
        }

        let redacted = tracking
            .redact_connection_metadata_before(cutoff)
            .await
            .unwrap();
        assert_eq!(redacted, 1);

        let remaining = email_events::Entity::find()
            .all(db.db.as_ref())
            .await
            .unwrap();
        let mut agents: Vec<_> = remaining
            .iter()
            .filter_map(|event| event.user_agent.as_deref())
            .collect();
        agents.sort_unstable();
        assert_eq!(agents, vec!["at-cutoff", "recent"]);
        assert_eq!(remaining.len(), 3, "redaction must not delete events");
    }

    #[tokio::test]
    async fn test_redact_connection_metadata_before_processes_more_than_one_batch() {
        let (db, tracking) = require_test_env!();
        let email_id = create_test_email(&db.db, true, true).await;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(90);

        db.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO email_events (email_id, event_type, created_at, ip_address) \
                 SELECT $1, 'opened', $2, '192.0.2.1' FROM generate_series(1, 5001)",
                [
                    email_id.into(),
                    (cutoff - chrono::Duration::seconds(1)).into(),
                ],
            ))
            .await
            .unwrap();

        let redacted = tracking
            .redact_connection_metadata_before(cutoff)
            .await
            .unwrap();
        assert_eq!(redacted, 5_001);
        let events = email_events::Entity::find()
            .all(db.db.as_ref())
            .await
            .unwrap();
        assert_eq!(events.len(), 5_001);
        assert!(events.iter().all(|event| event.ip_address.is_none()));
    }

    #[tokio::test]
    async fn test_redact_connection_metadata_rejects_invalid_days_without_querying() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let tracking = TrackingService::with_base_url(
            db.clone(),
            create_test_config_service(db),
            "https://app.example.com".to_string(),
        );

        let error = tracking
            .redact_connection_metadata_older_than(0)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::errors::EmailError::InvalidTrackingRetentionDays { days: 0 }
        ));

        let error = tracking
            .redact_connection_metadata_older_than(u32::MAX)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::errors::EmailError::InvalidTrackingRetentionDays { days: u32::MAX }
        ));
    }

    #[tokio::test]
    async fn test_redact_connection_metadata_before_propagates_database_error() {
        let ready_row = BTreeMap::from([("ready", true.into())]);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![ready_row]])
                .append_exec_errors([DbErr::Custom("retention delete failed".to_string())])
                .into_connection(),
        );
        let tracking = TrackingService::with_base_url(
            db.clone(),
            create_test_config_service(db),
            "https://app.example.com".to_string(),
        );

        let error = tracking
            .redact_connection_metadata_before(chrono::Utc::now())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::errors::EmailError::TrackingRetentionRedaction {
                source: DbErr::Custom(message),
                batch_size: 5_000,
                ..
            } if message == "retention delete failed"
        ));
    }

    #[tokio::test]
    async fn test_redaction_defers_when_retention_index_is_not_ready() {
        let not_ready_row = BTreeMap::from([("ready", false.into())]);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![not_ready_row]])
                .into_connection(),
        );
        let tracking = TrackingService::with_base_url(
            db.clone(),
            create_test_config_service(db),
            "https://app.example.com".to_string(),
        );

        let error = tracking
            .redact_connection_metadata_before(chrono::Utc::now())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::errors::EmailError::TrackingRetentionIndexUnavailable
        ));
    }

    #[tokio::test]
    async fn test_retention_scheduler_does_not_keep_service_alive_after_shutdown() {
        let not_ready_row = BTreeMap::from([("ready", false.into())]);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![not_ready_row]])
                .into_connection(),
        );
        let tracking = Arc::new(TrackingService::with_base_url(
            db.clone(),
            create_test_config_service(db),
            "https://app.example.com".to_string(),
        ));
        tracking
            .start_connection_metadata_retention(
                90,
                std::time::Duration::from_secs(3_600),
                std::time::Duration::from_secs(3_600),
            )
            .unwrap();
        let weak = Arc::downgrade(&tracking);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        drop(tracking);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert!(
            weak.upgrade().is_none(),
            "retention task must not create an Arc cycle or outlive the service"
        );
    }
}
