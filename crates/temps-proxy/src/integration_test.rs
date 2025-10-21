#[cfg(test)]
mod integration_tests {
    use std::sync::Arc;

    use temps_database::test_utils::TestDatabase;

    use crate::*;
    use crate::test_utils::*;
    fn create_crypto_cookie_crypto() -> Arc<temps_core::CookieCrypto> {
        let encryption_key = "default-32-byte-key-for-testing!";
        Arc::new(temps_core::CookieCrypto::new(encryption_key).expect("Failed to create cookie crypto"))
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_proxy_service_creation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone()).await.unwrap();
        let server_config = ProxyConfig::default();

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Create the proxy service
        let proxy_service = create_proxy_service(test_db.db.clone(), server_config, create_crypto_cookie_crypto(), route_table)?;

        // Verify the proxy service was created successfully
        assert_eq!(proxy_service.upstream_resolver().get_lb_strategy("example.com").await, Some("round_robin".to_string()));

        // Test that it doesn't crash on basic operations
        let has_route = proxy_service.upstream_resolver().has_custom_route("nonexistent.com").await;
        assert!(!has_route);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_upstream_resolution() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone()).await.unwrap();
        let server_config = ProxyConfig::default();

        // Create test data
        let _custom_route = test_db.create_test_custom_route("custom.example.com").await?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let proxy_service = create_proxy_service(test_db.db.clone(), server_config, create_crypto_cookie_crypto(), route_table)?;

        // Test custom route resolution
        let has_custom = proxy_service.upstream_resolver().has_custom_route("custom.example.com").await;
        assert!(has_custom);

        let no_custom = proxy_service.upstream_resolver().has_custom_route("nonexistent.com").await;
        assert!(!no_custom);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_project_context_resolution() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone()).await.unwrap();
        let server_config = ProxyConfig::default();

        // Create test project
        let (project, environment, deployment) = test_db.create_test_project().await?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let proxy_service = create_proxy_service(test_db.db.clone(), server_config, create_crypto_cookie_crypto(), route_table)?;

        // Test project context resolution
        let context = proxy_service.project_context_resolver().resolve_context("test.example.com").await;
        assert!(context.is_some());

        let context = context.unwrap();
        assert_eq!(context.project.id, project.id);
        assert_eq!(context.environment.id, environment.id);
        assert_eq!(context.deployment.id, deployment.id);

        // Test non-existent domain
        let no_context = proxy_service.project_context_resolver().resolve_context("nonexistent.com").await;
        assert!(no_context.is_none());

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_visitor_tracking() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone()).await.unwrap();
        let server_config = ProxyConfig::default();

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let proxy_service = create_proxy_service(test_db.db.clone(), server_config, create_crypto_cookie_crypto(), route_table)?;

        // Test visitor tracking decisions
        assert!(proxy_service.visitor_manager().should_track_visitor(
            "/",
            Some("text/html"),
            200,
            None
        ).await);

        assert!(!proxy_service.visitor_manager().should_track_visitor(
            "/api/_temps/health",
            Some("application/json"),
            200,
            None
        ).await);

        assert!(!proxy_service.visitor_manager().should_track_visitor(
            "/assets/style.css",
            Some("text/css"),
            200,
            None
        ).await);

        assert!(proxy_service.visitor_manager().should_track_visitor(
            "/some-page",
            Some("text/html"),
            404,
            None
        ).await);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix runtime nesting error
    async fn test_cookie_generation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone()).await.unwrap();
        let server_config = ProxyConfig::default();

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let proxy_service = create_proxy_service(test_db.db.clone(), server_config, create_crypto_cookie_crypto(), route_table)?;

        // Create test visitor
        let visitor = Visitor {
            visitor_id: "test-visitor".to_string(),
            visitor_id_i32: 123,
            is_crawler: false,
            crawler_name: None,
        };

        // Test visitor cookie generation
        let visitor_cookie = proxy_service.visitor_manager().generate_visitor_cookie(&visitor, false).await
            .map_err(|e| format!("Failed to generate visitor cookie: {:?}", e))?;
        assert!(visitor_cookie.contains("_temps_visitor_id"));
        assert!(visitor_cookie.contains("Path=/"));
        assert!(visitor_cookie.contains("HttpOnly"));

        // Test session cookie generation
        let session = crate::traits::Session {
            session_id: "test-session".to_string(),
            session_id_i32: 456,
            visitor_id_i32: 123,
            is_new_session: true,
        };

        let session_cookie = proxy_service.session_manager().generate_session_cookie(&session, false).await
            .map_err(|e| format!("Failed to generate session cookie: {:?}", e))?;
        assert!(session_cookie.contains("_temps_sid"));
        assert!(session_cookie.contains("Path=/"));
        assert!(session_cookie.contains("HttpOnly"));

        test_db.cleanup().await?;
        Ok(())
    }
}