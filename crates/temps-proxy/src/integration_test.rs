#[cfg(test)]
mod integration_tests {
    use std::sync::Arc;

    use temps_database::test_utils::TestDatabase;

    use crate::test_utils::*;
    use crate::*;

    fn create_crypto_cookie_crypto() -> Arc<temps_core::CookieCrypto> {
        let encryption_key = "default-32-byte-key-for-testing!";
        Arc::new(
            temps_core::CookieCrypto::new(encryption_key).expect("Failed to create cookie crypto"),
        )
    }

    fn create_test_server_config() -> Arc<temps_config::ServerConfig> {
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test@localhost/test".to_string(),
            None,
            None,
        )
        .expect("Failed to create test ServerConfig");
        Arc::new(config)
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_proxy_service_creation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();
        let server_config = ProxyConfig::default();
        let config = create_test_server_config();
        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Create the proxy service
        let proxy_service = create_proxy_service(
            test_db.db.clone(),
            server_config,
            create_crypto_cookie_crypto(),
            route_table,
            config,
        )?;

        // Verify the proxy service was created successfully
        assert_eq!(
            proxy_service
                .upstream_resolver()
                .get_lb_strategy("example.com")
                .await,
            Some("round_robin".to_string())
        );

        // Test that it doesn't crash on basic operations
        let has_route = proxy_service
            .upstream_resolver()
            .has_custom_route("nonexistent.com")
            .await;
        assert!(!has_route);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_upstream_resolution() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();
        let server_config = ProxyConfig::default();

        // Create test data
        let _custom_route = test_db
            .create_test_custom_route("custom.example.com")
            .await?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;
        let config = create_test_server_config();
        let proxy_service = create_proxy_service(
            test_db.db.clone(),
            server_config,
            create_crypto_cookie_crypto(),
            route_table,
            config,
        )?;

        // Test custom route resolution
        let has_custom = proxy_service
            .upstream_resolver()
            .has_custom_route("custom.example.com")
            .await;
        assert!(has_custom);

        let no_custom = proxy_service
            .upstream_resolver()
            .has_custom_route("nonexistent.com")
            .await;
        assert!(!no_custom);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_project_context_resolution() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();
        let server_config = ProxyConfig::default();
        let config = create_test_server_config();
        // Create test project
        let (project, environment, deployment) = test_db.create_test_project().await?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let proxy_service = create_proxy_service(
            test_db.db.clone(),
            server_config,
            create_crypto_cookie_crypto(),
            route_table,
            config,
        )?;

        // Test project context resolution
        let context = proxy_service
            .project_context_resolver()
            .resolve_context("test.example.com")
            .await;
        assert!(context.is_some());

        let context = context.unwrap();
        assert_eq!(context.project.id, project.id);
        assert_eq!(context.environment.id, environment.id);
        assert_eq!(context.deployment.id, deployment.id);

        // Test non-existent domain
        let no_context = proxy_service
            .project_context_resolver()
            .resolve_context("nonexistent.com")
            .await;
        assert!(no_context.is_none());

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_visitor_tracking() -> Result<(), Box<dyn std::error::Error>> {
        // Visitor tracking decisions are now a pure static function — no DB needed.
        use crate::proxy::LoadBalancer;

        assert!(LoadBalancer::should_track_page("/", Some("text/html"), 200));
        assert!(!LoadBalancer::should_track_page(
            "/api/_temps/health",
            Some("application/json"),
            200
        ));
        assert!(!LoadBalancer::should_track_page(
            "/assets/style.css",
            Some("text/css"),
            200
        ));
        assert!(LoadBalancer::should_track_page(
            "/some-page",
            Some("text/html"),
            404
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_cookie_generation() -> Result<(), Box<dyn std::error::Error>> {
        // Cookie generation is now handled by the stateless codec — no DB needed.
        use crate::service::cookie_codec::{
            make_v2_session_payload, parse_session_cookie, parse_visitor_cookie,
        };

        let crypto = create_crypto_cookie_crypto();

        // Visitor cookie: no cookie → new UUID, round-trips correctly
        let new_uuid = parse_visitor_cookie(None, &crypto);
        assert!(!new_uuid.is_empty());
        let encrypted = crypto.encrypt(&new_uuid)?;
        let reused = parse_visitor_cookie(Some(&encrypted), &crypto);
        assert_eq!(reused, new_uuid);

        // Session cookie: fresh → reuse; expired → new session
        let session_uuid = uuid::Uuid::new_v4().to_string();
        let now_ts = chrono::Utc::now().timestamp();
        let payload = make_v2_session_payload(&session_uuid, now_ts);
        let enc_payload = crypto.encrypt(&payload)?;
        let decision = parse_session_cookie(Some(&enc_payload), &crypto, 30);
        assert!(!decision.is_new_session);
        assert_eq!(decision.session_uuid, session_uuid);

        Ok(())
    }
}
