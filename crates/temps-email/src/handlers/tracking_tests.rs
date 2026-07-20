//! Integration tests for email tracking HTTP endpoints
//!
//! Tests the actual HTTP routes using tower::ServiceExt::oneshot.
//! Public endpoints (track/open, track/click) are tested without auth.
//! Authenticated endpoints (/tracking, /tracking/events, /tracking/links) are tested with auth middleware.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::middleware;
    use axum::Router;
    use http_body_util::BodyExt;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use temps_auth::{AuthContext, Role};
    use temps_core::{AuditLogger, AuditOperation, RequestMetadata};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{email_links, emails, users};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::handlers::tracking::{public_routes, routes};
    use crate::handlers::types::AppState;
    use crate::services::{
        DomainService, EmailService, ProviderService, SuppressionService, TrackingService,
        ValidationConfig, ValidationService,
    };

    // ============================================
    // Test Helpers
    // ============================================

    struct MockAuditLogger;

    #[async_trait::async_trait]
    impl AuditLogger for MockAuditLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn create_test_encryption_service() -> Arc<temps_core::EncryptionService> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(temps_core::EncryptionService::new(key).unwrap())
    }

    fn test_request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test-agent".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost:3000".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn test_user() -> users::Model {
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    async fn setup_test_env() -> (TestDatabase, Arc<AppState>) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let encryption_service = create_test_encryption_service();
        let provider_service = Arc::new(ProviderService::new(db.db.clone(), encryption_service));
        let domain_service = Arc::new(DomainService::new(db.db.clone(), provider_service.clone()));
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
        let config_service = Arc::new(temps_config::ConfigService::new(
            server_config,
            db.db.clone(),
        ));
        let tracking_setup_service = Arc::new(crate::services::TrackingSetupService::new(
            provider_service.clone(),
            db.db.clone(),
        ));
        let tracking_service = Arc::new(TrackingService::with_base_url(
            db.db.clone(),
            config_service.clone(),
            "http://localhost:3000".to_string(),
        ));
        let suppression_service = Arc::new(SuppressionService::new(db.db.clone()));
        let email_service = Arc::new(EmailService::new(
            db.db.clone(),
            provider_service.clone(),
            domain_service.clone(),
            tracking_service.clone(),
            suppression_service,
        ));
        let validation_service = Arc::new(ValidationService::new(ValidationConfig::default()));

        let app_state = Arc::new(AppState {
            provider_service,
            domain_service,
            email_service,
            validation_service,
            tracking_service,
            audit_service: Arc::new(MockAuditLogger),
            dns_provider_service: None,
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            tracking_setup_service,
            config_service,
        });

        (db, app_state)
    }

    /// Build public routes with RequestMetadata middleware (no auth)
    fn build_public_app(state: Arc<AppState>) -> Router {
        let metadata_middleware = middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(test_request_metadata());
                next.run(req).await
            },
        );

        public_routes().layer(metadata_middleware).with_state(state)
    }

    /// Build authenticated routes with auth + RequestMetadata middleware
    fn build_authed_app(state: Arc<AppState>) -> Router {
        let auth_middleware = middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let auth_context = AuthContext::new_session(test_user(), Role::Admin);
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(test_request_metadata());
                next.run(req).await
            },
        );

        routes().layer(auth_middleware).with_state(state)
    }

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
            html_body: Set(Some("<p>Hello</p>".to_string())),
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
    // Public Endpoint Tests: Track Open
    // ============================================

    #[tokio::test]
    async fn test_track_open_returns_gif_pixel() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, true, false).await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/open", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-type").unwrap(), "image/gif");

        let body = response.into_body().collect().await.unwrap().to_bytes();
        // GIF89a header
        assert_eq!(&body[..6], b"GIF89a");

        // Verify the open was recorded in the database
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.open_count, 1);
        assert!(email.first_opened_at.is_some());
    }

    #[tokio::test]
    async fn test_track_open_returns_gif_even_for_invalid_uuid() {
        let (_db, state) = setup_test_env().await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/emails/not-a-uuid/track/open")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should still return 200 with GIF (never leak info about email existence)
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-type").unwrap(), "image/gif");
    }

    #[tokio::test]
    async fn test_track_open_does_not_increment_when_tracking_disabled() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, false, false).await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/open", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Counter should NOT have been incremented
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.open_count, 0);
    }

    #[tokio::test]
    async fn test_track_open_sets_no_cache_headers() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, true, false).await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/open", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "no-store, no-cache, must-revalidate"
        );
    }

    // ============================================
    // Public Endpoint Tests: Track Click
    // ============================================

    #[tokio::test]
    async fn test_track_click_redirects_to_original_url() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, false, true).await;
        create_test_links(&db.db, email_id).await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/click/0", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "https://example.com/page1"
        );

        // Verify click was recorded
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.click_count, 1);
        assert!(email.first_clicked_at.is_some());
    }

    #[tokio::test]
    async fn test_track_click_second_link() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, false, true).await;
        create_test_links(&db.db, email_id).await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/click/1", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "https://example.com/page2"
        );
    }

    #[tokio::test]
    async fn test_track_click_invalid_link_index_returns_404() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, false, true).await;
        // No links stored

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/click/999", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_track_click_invalid_uuid_returns_400() {
        let (_db, state) = setup_test_env().await;

        let app = build_public_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/emails/not-a-uuid/track/click/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ============================================
    // Authenticated Endpoint Tests: Get Tracking Summary
    // ============================================

    #[tokio::test]
    async fn test_get_email_tracking_returns_summary() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, true, true).await;
        create_test_links(&db.db, email_id).await;

        // Record some opens and clicks via the service directly
        state
            .tracking_service
            .record_open(
                email_id,
                Some("1.1.1.1".to_string()),
                Some("Chrome".to_string()),
            )
            .await
            .unwrap();
        state
            .tracking_service
            .record_open(
                email_id,
                Some("2.2.2.2".to_string()),
                Some("Firefox".to_string()),
            )
            .await
            .unwrap();
        state
            .tracking_service
            .record_click(email_id, 0, Some("1.1.1.1".to_string()), None)
            .await
            .unwrap();

        let app = build_authed_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/tracking", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["email_id"], email_id.to_string());
        assert_eq!(json["track_opens"], true);
        assert_eq!(json["track_clicks"], true);
        assert_eq!(json["open_count"], 2);
        assert_eq!(json["click_count"], 1);
        assert_eq!(json["unique_opens"], 2); // 2 different IPs
        assert_eq!(json["unique_clicks"], 1);
        assert!(json["first_opened_at"].is_string());
        assert!(json["first_clicked_at"].is_string());
        assert_eq!(json["links"].as_array().unwrap().len(), 2);
        assert_eq!(json["links"][0]["click_count"], 1);
        assert_eq!(json["links"][1]["click_count"], 0);
    }

    #[tokio::test]
    async fn test_get_email_tracking_invalid_uuid_returns_400() {
        let (_db, state) = setup_test_env().await;

        let app = build_authed_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/emails/not-a-uuid/tracking")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ============================================
    // Authenticated Endpoint Tests: Get Tracking Events
    // ============================================

    #[tokio::test]
    async fn test_get_email_events_returns_all_events() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, true, true).await;
        create_test_links(&db.db, email_id).await;

        // Record events
        state
            .tracking_service
            .record_open(
                email_id,
                Some("1.1.1.1".to_string()),
                Some("Chrome".to_string()),
            )
            .await
            .unwrap();
        state
            .tracking_service
            .record_click(email_id, 0, Some("2.2.2.2".to_string()), None)
            .await
            .unwrap();

        let app = build_authed_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/tracking/events", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event_type"], "opened");
        assert_eq!(events[0]["ip_address"], "1.1.1.1");
        assert_eq!(events[1]["event_type"], "clicked");
        assert_eq!(events[1]["ip_address"], "2.2.2.2");
        assert_eq!(events[1]["link_index"], 0);
        assert_eq!(events[1]["link_url"], "https://example.com/page1");
    }

    #[tokio::test]
    async fn test_get_email_events_filtered_by_type() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, true, true).await;
        create_test_links(&db.db, email_id).await;

        state
            .tracking_service
            .record_open(email_id, None, None)
            .await
            .unwrap();
        state
            .tracking_service
            .record_click(email_id, 0, None, None)
            .await
            .unwrap();
        state
            .tracking_service
            .record_open(email_id, None, None)
            .await
            .unwrap();

        let app = build_authed_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/emails/{}/tracking/events?event_type=open",
                        email_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

        assert_eq!(events.len(), 2, "Should only return open events");
        assert!(events.iter().all(|e| e["event_type"] == "opened"));
    }

    // ============================================
    // Authenticated Endpoint Tests: Get Tracking Links
    // ============================================

    #[tokio::test]
    async fn test_get_email_links_returns_tracked_links() {
        let (db, state) = setup_test_env().await;
        let email_id = create_test_email(&db.db, false, true).await;
        create_test_links(&db.db, email_id).await;

        // Click link 0 twice
        state
            .tracking_service
            .record_click(email_id, 0, None, None)
            .await
            .unwrap();
        state
            .tracking_service
            .record_click(email_id, 0, None, None)
            .await
            .unwrap();

        let app = build_authed_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/tracking/links", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let links: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["original_url"], "https://example.com/page1");
        assert_eq!(links[0]["click_count"], 2);
        assert_eq!(links[1]["original_url"], "https://example.com/page2");
        assert_eq!(links[1]["click_count"], 0);
    }

    // ============================================
    // Full E2E Flow: Send email with tracking -> open -> click -> verify
    // ============================================

    #[tokio::test]
    async fn test_full_tracking_flow_open_then_click() {
        let (db, state) = setup_test_env().await;

        // Step 1: Create email with both tracking enabled
        let email_id = create_test_email(&db.db, true, true).await;
        create_test_links(&db.db, email_id).await;

        // Step 2: Simulate email open (tracking pixel loaded)
        let public_app = build_public_app(state.clone());
        let open_response = public_app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/open", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(open_response.status(), StatusCode::OK);

        // Step 3: Simulate link click
        let public_app = build_public_app(state.clone());
        let click_response = public_app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/track/click/0", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(click_response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            click_response.headers().get("location").unwrap(),
            "https://example.com/page1"
        );

        // Step 4: Query tracking summary via authenticated endpoint
        let authed_app = build_authed_app(state.clone());
        let tracking_response = authed_app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/tracking", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tracking_response.status(), StatusCode::OK);

        let body = tracking_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let tracking: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(tracking["open_count"], 1);
        assert_eq!(tracking["click_count"], 1);
        assert_eq!(tracking["unique_opens"], 1);
        assert_eq!(tracking["unique_clicks"], 1);
        assert!(tracking["first_opened_at"].is_string());
        assert!(tracking["first_clicked_at"].is_string());

        // Step 5: Query events via authenticated endpoint
        let authed_app = build_authed_app(state.clone());
        let events_response = authed_app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/emails/{}/tracking/events", email_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events_response.status(), StatusCode::OK);

        let body = events_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event_type"], "opened");
        assert_eq!(events[0]["ip_address"], "127.0.0.1"); // from RequestMetadata
        assert_eq!(events[0]["user_agent"], "test-agent");
        assert_eq!(events[1]["event_type"], "clicked");
        assert_eq!(events[1]["link_url"], "https://example.com/page1");

        // Step 6: Verify the database state directly
        let email = emails::Entity::find_by_id(email_id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(email.open_count, 1);
        assert_eq!(email.click_count, 1);
        assert!(email.first_opened_at.is_some());
        assert!(email.first_clicked_at.is_some());
    }
}
