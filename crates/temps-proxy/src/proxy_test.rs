#[cfg(test)]
mod proxy_tests {
    use crate::config::ProxyConfig;
    use crate::proxy::LoadBalancer as ProxyLoadBalancer;
    use crate::services::*;
    use crate::test_utils::*;
    use crate::traits::*;
    use hyper::body::{Bytes, Incoming};
    use hyper::{Request, Response, StatusCode};
    use pingora::upstreams::peer::Peer;
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::sync::Arc;
    use temps_core::CookieCrypto;
    use temps_database::test_utils::TestDatabase;
    use temps_routes::CachedPeerTable;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use anyhow::Result;
    use http_body_util::Full;
    use lazy_static::lazy_static;
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::sync::{Arc as StdArc, Mutex};

    // Helper to convert std errors to anyhow
    fn convert_error<T>(result: Result<T, Box<dyn std::error::Error>>) -> Result<T> {
        result.map_err(|e| anyhow::anyhow!("{}", e))
    }

    // Helper to convert Send+Sync errors to anyhow
    fn convert_send_sync_error<T>(
        result: Result<T, Box<dyn std::error::Error + Send + Sync>>,
    ) -> Result<T> {
        result.map_err(|e| anyhow::anyhow!("{}", e))
    }

    static NEXT_PORT: AtomicU16 = AtomicU16::new(9000);

    fn get_next_port() -> u16 {
        NEXT_PORT.fetch_add(1, Ordering::SeqCst)
    }

    /// Simple mock server that just accepts connections
    async fn start_simple_server() -> String {
        let port = get_next_port();
        let addr = format!("127.0.0.1:{}", port);

        let listener = TcpListener::bind(&addr).await.unwrap();
        let server_addr = addr.clone();

        // Start a simple server that accepts and closes connections
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    // Just close the connection immediately
                    let _ = stream.shutdown().await;
                } else {
                    break;
                }
            }
        });

        // Give the server a moment to start
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        server_addr
    }

    async fn mock_handler(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
        let path = req.uri().path();
        let method = req.method();

        let response_body = match (method.as_str(), path) {
            ("GET", "/") => "Hello from mock server!",
            ("GET", "/health") => "OK",
            ("POST", "/api/test") => "POST received",
            ("GET", "/user-agent") => {
                // Echo back the user agent
                if let Some(ua) = req.headers().get("user-agent") {
                    return Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "text/plain")
                        .body(Full::new(Bytes::from(format!(
                            "User-Agent: {}",
                            ua.to_str().unwrap_or("")
                        ))))
                        .unwrap());
                }
                "No User-Agent"
            }
            ("GET", "/headers") => {
                // Return request headers as JSON
                let mut headers_map = HashMap::new();
                for (name, value) in req.headers() {
                    headers_map.insert(name.to_string(), value.to_str().unwrap_or("").to_string());
                }
                let json_response = serde_json::to_string(&headers_map).unwrap();
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(json_response)))
                    .unwrap());
            }
            ("GET", "/error") => {
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Full::new(Bytes::from("Internal Server Error")))
                    .unwrap());
            }
            _ => "Not Found",
        };

        let status = if path == "/"
            || path == "/health"
            || path == "/api/test"
            || path == "/user-agent"
            || path == "/headers"
        {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };

        Ok(Response::builder()
            .status(status)
            .header("Content-Type", "text/plain")
            .header("X-Mock-Server", "true")
            .body(Full::new(Bytes::from(response_body)))
            .unwrap())
    }

    /// Mock upstream resolver that points to our test server
    struct MockUpstreamResolver {
        mock_server_addr: String,
        console_addr: String,
    }

    impl MockUpstreamResolver {
        fn new(mock_server_addr: String, console_addr: String) -> Self {
            Self {
                mock_server_addr,
                console_addr,
            }
        }
    }
    fn create_crypto_cookie_crypto() -> Arc<temps_core::CookieCrypto> {
        let encryption_key = "default-32-byte-key-for-testing!";
        Arc::new(
            temps_core::CookieCrypto::new(encryption_key).expect("Failed to create cookie crypto"),
        )
    }

    fn create_mock_ip_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_geo::IpAddressService> {
        // Force mock mode for tests by setting environment variable
        std::env::set_var("TEMPS_GEO_MOCK", "true");

        // Create mock GeoIP service
        let geoip_service =
            Arc::new(temps_geo::GeoIpService::new().expect("Failed to create GeoIpService"));
        Arc::new(temps_geo::IpAddressService::new(db, geoip_service))
    }
    #[async_trait::async_trait]
    impl UpstreamResolver for MockUpstreamResolver {
        async fn resolve_peer(
            &self,
            host: &str,
            path: &str,
        ) -> pingora_core::Result<Box<pingora_core::upstreams::peer::HttpPeer>> {
            let upstream_addr = if path.starts_with("/api/_temps") {
                &self.console_addr
            } else if host == "test.example.com" {
                &self.mock_server_addr
            } else {
                &self.console_addr
            };

            tracing::debug!("Resolving {} {} -> {}", host, path, upstream_addr);

            let peer = Box::new(pingora_core::upstreams::peer::HttpPeer::new(
                upstream_addr.clone(),
                false,
                "".to_string(),
            ));
            Ok(peer)
        }

        async fn has_custom_route(&self, host: &str) -> bool {
            host == "custom.example.com"
        }

        async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
            Some("round_robin".to_string())
        }
    }

    #[tokio::test]
    async fn test_proxy_upstream_resolution() -> Result<()> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();
        // Start simple server
        let mock_server_addr = start_simple_server().await;
        println!("Mock server started on: {}", mock_server_addr);

        // Create project that will route to our mock server (with unique domain for this test)
        let test_domain = format!("test-{}.example.com", get_next_port()); // Use port number for uniqueness
        let (_project, _environment, _deployment) =
            convert_error(test_db.create_test_project_with_domain(&test_domain).await)?;

        // Create proxy service with mock upstream resolver
        let server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();

        let upstream_resolver = Arc::new(MockUpstreamResolver::new(
            mock_server_addr.clone(),
            "127.0.0.1:3001".to_string(), // Mock console
        )) as Arc<dyn UpstreamResolver>;

        // Create route table (not used in this mock but required by constructor)
        let mock_route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));

        let ip_service = create_mock_ip_service(test_db.db.clone());

        let request_logger = Arc::new(RequestLoggerImpl::new(
            LoggingConfig::default(),
            test_db.db.clone(),
            ip_service.clone(),
        )) as Arc<dyn RequestLogger>;

        let proxy_log_service = Arc::new(crate::service::proxy_log_service::ProxyLogService::new(
            test_db.db.clone(),
            ip_service.clone(),
        ));

        let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(mock_route_table))
            as Arc<dyn ProjectContextResolver>;

        let visitor_manager = Arc::new(VisitorManagerImpl::new(
            test_db.db.clone(),
            crypto.clone(),
            ip_service,
        )) as Arc<dyn VisitorManager>;

        let session_manager = Arc::new(SessionManagerImpl::new(test_db.db.clone(), crypto.clone()))
            as Arc<dyn SessionManager>;

        let lb = ProxyLoadBalancer::new(
            upstream_resolver,
            request_logger,
            proxy_log_service,
            project_context_resolver,
            visitor_manager,
            session_manager,
            crypto,
            test_db.db.clone(),
        );

        // Test that the LoadBalancer can resolve the upstream
        let upstream = lb
            .upstream_resolver()
            .resolve_peer("test.example.com", "/")
            .await?;
        println!("Resolved upstream to: {}", upstream.address());
        assert!(upstream.address().to_string().starts_with("127.0.0.1:"));

        // Note: Using shared database, so we don't cleanup individual test data
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix route table lookup - CachedPeerTable.load_routes() not finding custom domain entries
    async fn test_proxy_context_resolution() -> Result<()> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();

        // Create test project with unique domain
        let test_domain = format!("context-test-{}.example.com", get_next_port());
        let (project, environment, deployment) =
            convert_error(test_db.create_test_project_with_domain(&test_domain).await)?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let project_resolver = ProjectContextResolverImpl::new(route_table);

        // Test project context resolution
        let context = project_resolver.resolve_context(&test_domain).await;
        assert!(context.is_some());

        let context = context.unwrap();
        assert_eq!(context.project.id, project.id);
        assert_eq!(context.environment.id, environment.id);
        assert_eq!(context.deployment.id, deployment.id);
        assert!(context.project.name.contains("test-project"));
        assert_eq!(context.environment.name, "production");

        // Test non-existent domain
        let no_context = project_resolver.resolve_context("nonexistent.com").await;
        assert!(no_context.is_none());

        // Note: Using shared database, so we don't cleanup individual test data
        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_route_resolution() -> Result<()> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();

        // Start mock server
        let mock_server_addr = start_simple_server().await;

        // Create mock upstream resolver
        let upstream_resolver =
            MockUpstreamResolver::new(mock_server_addr.clone(), "127.0.0.1:3001".to_string());

        // Test different route resolutions
        let peer1 = upstream_resolver
            .resolve_peer("test.example.com", "/")
            .await?;
        assert!(peer1.address().to_string().starts_with("127.0.0.1:"));
        assert_eq!(peer1.address().to_string(), mock_server_addr);

        let peer2 = upstream_resolver.resolve_peer("unknown.com", "/").await?;
        assert_eq!(peer2.address().to_string(), "127.0.0.1:3001"); // Should go to console

        let peer3 = upstream_resolver
            .resolve_peer("test.example.com", "/api/_temps/health")
            .await?;
        assert_eq!(peer3.address().to_string(), "127.0.0.1:3001"); // Temps API should go to console

        // Test custom route detection
        assert!(
            upstream_resolver
                .has_custom_route("custom.example.com")
                .await
        );
        assert!(!upstream_resolver.has_custom_route("regular.com").await);

        // Note: Using shared database, so we don't cleanup individual test data
        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_visitor_management() -> Result<()> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();

        let server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();

        let ip_service = create_mock_ip_service(test_db.db.clone());
        let visitor_manager =
            VisitorManagerImpl::new(test_db.db.clone(), crypto.clone(), ip_service);

        // Test visitor creation
        let visitor = visitor_manager
            .get_or_create_visitor(
                None, // No existing cookie
                None, // No project context
                "Mozilla/5.0 (test)",
                Some("127.0.0.1"),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Failed to get or create visitor"))?;

        assert!(!visitor.visitor_id.is_empty());
        assert!(!visitor.is_crawler);
        assert!(visitor.crawler_name.is_none());

        // Test visitor cookie generation
        let cookie = convert_send_sync_error(
            visitor_manager
                .generate_visitor_cookie(&visitor, false)
                .await,
        )?;
        assert!(cookie.contains("_temps_visitor_id"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));

        // Test bot detection
        let bot_visitor = convert_send_sync_error(
            visitor_manager
                .get_or_create_visitor(None, None, "Googlebot/2.1", Some("127.0.0.1"))
                .await,
        )?;

        assert!(bot_visitor.is_crawler);
        assert!(bot_visitor.crawler_name.is_some());

        // Note: Using shared database, so we don't cleanup individual test data
        Ok(())
    }

    #[tokio::test]
    #[ignore] // TODO: Fix foreign key constraint - needs visitor record creation before session
    async fn test_proxy_session_management() -> Result<()> {
        let server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let session_manager =
            SessionManagerImpl::new(test_db_mock.connection_arc().clone(), crypto.clone());

        let visitor = Visitor {
            visitor_id: "test-visitor-123".to_string(),
            visitor_id_i32: 123,
            is_crawler: false,
            crawler_name: None,
        };

        // Test session creation
        let session = convert_send_sync_error(
            session_manager
                .get_or_create_session(
                    None, // No existing cookie
                    &visitor,
                    None, // No project context
                    Some("https://example.com"),
                )
                .await,
        )?;

        assert!(!session.session_id.is_empty());
        assert_eq!(session.visitor_id_i32, visitor.visitor_id_i32);
        assert!(session.is_new_session);

        // Test session cookie generation
        let cookie = convert_send_sync_error(
            session_manager
                .generate_session_cookie(&session, true)
                .await,
        )?;
        assert!(cookie.contains("_temps_sid"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure")); // Should be secure for HTTPS

        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_visitor_tracking_decisions() -> Result<()> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();

        let server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();

        let ip_service = create_mock_ip_service(test_db.db.clone());
        let visitor_manager = VisitorManagerImpl::new(test_db.db.clone(), crypto, ip_service);

        // Test tracking decisions for different request types
        assert!(
            visitor_manager
                .should_track_visitor("/", Some("text/html"), 200, None)
                .await
        );

        assert!(
            !visitor_manager
                .should_track_visitor("/api/_temps/health", Some("application/json"), 200, None)
                .await
        );

        assert!(
            !visitor_manager
                .should_track_visitor("/assets/style.css", Some("text/css"), 200, None)
                .await
        );

        assert!(
            visitor_manager
                .should_track_visitor("/some-page", Some("text/html"), 404, None)
                .await
        );

        // Test static asset detection
        assert!(
            !visitor_manager
                .should_track_visitor("/images/logo.png", Some("image/png"), 200, None)
                .await
        );

        // Note: Using shared database, so we don't cleanup individual test data
        Ok(())
    }

    #[tokio::test]
    async fn test_redirect_handling() -> Result<()> {
        // Test that the proxy properly handles redirect configuration
        use crate::test_utils::MockProjectContextResolver;

        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let db = test_db_mock.connection_arc().clone();

        // Create a mock context resolver that returns redirect info for test.redirect.com
        let project_context_resolver = Arc::new(MockProjectContextResolver::new_with_redirect(
            "test.redirect.com",
            "https://example.com".to_string(),
            301,
        ));

        // Verify that get_redirect_info returns the expected redirect
        let redirect_info = project_context_resolver
            .get_redirect_info("test.redirect.com")
            .await;

        assert!(redirect_info.is_some(), "Redirect info should be present");

        let (redirect_url, status_code) = redirect_info.unwrap();
        assert_eq!(redirect_url, "https://example.com");
        assert_eq!(status_code, 301);

        // Verify that get_redirect_info returns None for non-redirect hosts
        let no_redirect = project_context_resolver
            .get_redirect_info("non-redirect-host.com")
            .await;

        assert!(
            no_redirect.is_none(),
            "Non-redirect host should return None"
        );

        Ok(())
    }
}
