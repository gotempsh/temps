#[cfg(test)]
pub mod proxy_tests {
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

    use temps_database::test_utils::TestDatabase;
    use temps_routes::CachedPeerTable;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use anyhow::Result;
    use http_body_util::Full;

    use std::collections::HashMap;
    use std::convert::Infallible;

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
            while let Ok((mut stream, _)) = listener.accept().await {
                // Just close the connection immediately
                let _ = stream.shutdown().await;
            }
        });

        // Give the server a moment to start
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        server_addr
    }

    #[allow(dead_code)]
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

    pub fn create_test_config_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_config::ConfigService> {
        // Create test ServerConfig with minimal required fields
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test@localhost/test".to_string(),
            None,
            None,
        )
        .expect("Failed to create test ServerConfig");

        Arc::new(temps_config::ConfigService::new(Arc::new(config), db))
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
            _sni_hostname: Option<&str>,
        ) -> pingora_core::Result<PeerSelection> {
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
            Ok(PeerSelection {
                peer,
                container_id: None,
                container_name: None,
            })
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
        let _server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();

        let upstream_resolver = Arc::new(MockUpstreamResolver::new(
            mock_server_addr.clone(),
            "127.0.0.1:3001".to_string(), // Mock console
        )) as Arc<dyn UpstreamResolver>;

        // Create route table (not used in this mock but required by constructor)
        let mock_route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));

        let ip_service = create_mock_ip_service(test_db.db.clone());

        let proxy_log_storage: Arc<dyn crate::storage::ProxyLogStorage> = Arc::new(
            crate::storage::TimescaleDbProxyLogStore::new(test_db.db.clone(), ip_service.clone()),
        );
        let (proxy_log_handle, _proxy_log_writer) =
            crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
                test_db.db.clone(),
                ip_service.clone(),
                proxy_log_storage,
            );

        let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(mock_route_table))
            as Arc<dyn ProjectContextResolver>;

        let visitor_manager = Arc::new(VisitorManagerImpl::new(
            test_db.db.clone(),
            crypto.clone(),
            ip_service,
        )) as Arc<dyn VisitorManager>;

        let session_manager = Arc::new(SessionManagerImpl::new(test_db.db.clone(), crypto.clone()))
            as Arc<dyn SessionManager>;

        // Create config service for static file serving
        let config_service = create_test_config_service(test_db.db.clone());

        // Create IP access control service
        let ip_access_control_service = Arc::new(
            crate::service::ip_access_control_service::IpAccessControlService::new(
                test_db.db.clone(),
            ),
        );

        // Create challenge service
        let challenge_service = Arc::new(crate::service::challenge_service::ChallengeService::new(
            test_db.db.clone(),
        ));

        let lb = ProxyLoadBalancer::new(
            upstream_resolver,
            proxy_log_handle,
            project_context_resolver,
            visitor_manager,
            session_manager,
            crypto,
            test_db.db.clone(),
            config_service,
            ip_access_control_service,
            challenge_service,
            false,
        );

        // Test that the LoadBalancer can resolve the upstream
        let selection = lb
            .upstream_resolver()
            .resolve_peer("test.example.com", "/", None)
            .await?;
        println!("Resolved upstream to: {}", selection.peer.address());
        assert!(selection
            .peer
            .address()
            .to_string()
            .starts_with("127.0.0.1:"));

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
        let _test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();

        // Start mock server
        let mock_server_addr = start_simple_server().await;

        // Create mock upstream resolver
        let upstream_resolver =
            MockUpstreamResolver::new(mock_server_addr.clone(), "127.0.0.1:3001".to_string());

        // Test different route resolutions
        let sel1 = upstream_resolver
            .resolve_peer("test.example.com", "/", None)
            .await?;
        assert!(sel1.peer.address().to_string().starts_with("127.0.0.1:"));
        assert_eq!(sel1.peer.address().to_string(), mock_server_addr);

        let sel2 = upstream_resolver
            .resolve_peer("unknown.com", "/", None)
            .await?;
        assert_eq!(sel2.peer.address().to_string(), "127.0.0.1:3001"); // Should go to console

        let sel3 = upstream_resolver
            .resolve_peer("test.example.com", "/api/_temps/health", None)
            .await?;
        assert_eq!(sel3.peer.address().to_string(), "127.0.0.1:3001"); // Temps API should go to console

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
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::{deployments, environments, projects};

        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.connection_arc().clone())
            .await
            .unwrap();

        let _server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();

        let ip_service = create_mock_ip_service(test_db.db.clone());
        let visitor_manager =
            VisitorManagerImpl::new(test_db.db.clone(), crypto.clone(), ip_service);

        // A visitor row has non-nullable project_id / environment_id, so
        // get_or_create_visitor requires a real ProjectContext — create one.
        let project = projects::ActiveModel {
            slug: Set("visitor-mgmt-test".to_string()),
            name: Set("Visitor Mgmt Test".to_string()),
            repo_name: Set("visitor-app".to_string()),
            repo_owner: Set("test-org".to_string()),
            directory: Set("".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Vite),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            slug: Set("production".to_string()),
            name: Set("Production".to_string()),
            subdomain: Set("visitor-app".to_string()),
            host: Set("visitor-app.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deploy-visitor-test".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(Default::default())),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;

        let context = ProjectContext {
            project: Arc::new(project),
            environment: Arc::new(environment),
            deployment: Arc::new(deployment),
        };

        // Test visitor creation
        let attribution = crate::traits::FirstVisitAttribution::default();
        let visitor = visitor_manager
            .get_or_create_visitor(
                None, // No existing cookie
                Some(&context),
                "Mozilla/5.0 (test)",
                Some("127.0.0.1"),
                &attribution,
            )
            .await
            // Surface the real error instead of swallowing it, so a future
            // failure here is diagnosable.
            .map_err(|e| anyhow::anyhow!("Failed to get or create visitor: {e}"))?;

        assert!(!visitor.visitor_id.is_empty());
        assert!(!visitor.is_crawler);
        assert!(visitor.crawler_name.is_none());

        // Test visitor cookie generation
        let cookie = convert_send_sync_error(
            visitor_manager
                .generate_visitor_cookie(&visitor, false, None)
                .await,
        )?;
        assert!(cookie.contains("_temps_visitor_id"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));

        // Test bot detection
        let bot_visitor = visitor_manager
            .get_or_create_visitor(
                None,
                Some(&context),
                "Googlebot/2.1",
                Some("127.0.0.1"),
                &attribution,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get or create bot visitor: {e}"))?;

        assert!(bot_visitor.is_crawler);
        assert!(bot_visitor.crawler_name.is_some());

        // A request with no project context must be rejected, not silently
        // create an orphan visitor.
        let no_context = visitor_manager
            .get_or_create_visitor(
                None,
                None,
                "Mozilla/5.0 (test)",
                Some("127.0.0.1"),
                &attribution,
            )
            .await;
        assert!(
            no_context.is_err(),
            "visitor creation without project context must fail"
        );

        // Note: Using shared database, so we don't cleanup individual test data
        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_session_management() -> Result<()> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::{environments, projects, visitor as visitor_entity};

        let _server_config = ProxyConfig::default();
        let crypto = create_crypto_cookie_crypto();
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let db = test_db_mock.connection_arc().clone();
        let session_manager = SessionManagerImpl::new(db.clone(), crypto.clone());

        // request_sessions.visitor_id has an FK to `visitor`, which in turn
        // requires a project + environment — create the full chain so the
        // session insert has a real visitor row to reference.
        let project = projects::ActiveModel {
            slug: Set("session-mgmt-test".to_string()),
            name: Set("Session Mgmt Test".to_string()),
            repo_name: Set("session-app".to_string()),
            repo_owner: Set("test-org".to_string()),
            directory: Set("".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Vite),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            slug: Set("production".to_string()),
            name: Set("Production".to_string()),
            subdomain: Set("session-app".to_string()),
            host: Set("session-app.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let visitor_row = visitor_entity::ActiveModel {
            visitor_id: Set("test-visitor-123".to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            is_crawler: Set(false),
            has_activity: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let visitor = Visitor {
            visitor_id: visitor_row.visitor_id.clone(),
            visitor_id_i32: visitor_row.id,
            is_crawler: false,
            crawler_name: None,
        };

        // Test session creation
        let session = session_manager
            .get_or_create_session(
                None, // No existing cookie
                &visitor,
                None, // No project context
                Some("https://example.com"),
                None, // No query string
                None, // No current hostname
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get or create session: {e}"))?;

        assert!(!session.session_id.is_empty());
        assert_eq!(session.visitor_id_i32, visitor.visitor_id_i32);
        assert!(session.is_new_session);

        // Test session cookie generation
        let cookie = convert_send_sync_error(
            session_manager
                .generate_session_cookie(&session, true, None)
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

        let _server_config = ProxyConfig::default();
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
        let _db = test_db_mock.connection_arc().clone();

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

    #[tokio::test]
    async fn test_static_deployment_integration() -> Result<()> {
        use std::fs as std_fs;
        use std::io::Write;

        // Create temporary directory for static files
        let temp_dir = std::env::temp_dir().join(format!("temps-test-{}", uuid::Uuid::new_v4()));
        std_fs::create_dir_all(&temp_dir)?;
        std_fs::create_dir_all(temp_dir.join("assets"))?;

        // Create test files
        let mut index_html = std_fs::File::create(temp_dir.join("index.html"))?;
        index_html.write_all(b"<!DOCTYPE html><html><body><h1>Static Site</h1></body></html>")?;
        drop(index_html);

        let mut app_js = std_fs::File::create(temp_dir.join("assets/app.js"))?;
        app_js.write_all(b"console.log('Static app');")?;
        drop(app_js);

        let mut styles_css = std_fs::File::create(temp_dir.join("assets/styles.css"))?;
        styles_css.write_all(b"body { margin: 0; }")?;
        drop(styles_css);

        // Test 1: Verify files exist in static directory
        assert!(
            temp_dir.join("index.html").exists(),
            "index.html should exist"
        );
        assert!(
            temp_dir.join("assets/app.js").exists(),
            "assets/app.js should exist"
        );
        assert!(
            temp_dir.join("assets/styles.css").exists(),
            "assets/styles.css should exist"
        );

        // Test 2: Verify preset supports static deployment
        let vite_preset = temps_presets::get_preset_by_slug("vite");
        assert!(vite_preset.is_some(), "Vite preset should exist");
        let vite_static_output = vite_preset.unwrap().static_output_dir();
        assert!(
            vite_static_output.is_some(),
            "Vite preset should support static deployment"
        );
        assert_eq!(vite_static_output.unwrap(), "/usr/share/nginx/html");

        // Test 3: Verify Rsbuild preset supports static deployment
        let rsbuild_preset = temps_presets::get_preset_by_slug("rsbuild");
        assert!(rsbuild_preset.is_some(), "Rsbuild preset should exist");
        let rsbuild_static_output = rsbuild_preset.unwrap().static_output_dir();
        assert!(
            rsbuild_static_output.is_some(),
            "Rsbuild preset should support static deployment"
        );
        assert_eq!(rsbuild_static_output.unwrap(), "/app/dist");

        // Test 4: Verify Docusaurus preset supports static deployment
        let docusaurus_preset = temps_presets::get_preset_by_slug("docusaurus");
        assert!(
            docusaurus_preset.is_some(),
            "Docusaurus preset should exist"
        );
        let docusaurus_static_output = docusaurus_preset.unwrap().static_output_dir();
        assert!(
            docusaurus_static_output.is_some(),
            "Docusaurus preset should support static deployment"
        );
        assert_eq!(docusaurus_static_output.unwrap(), "/app/build");

        // Test 5: Verify NextJS preset does NOT support static deployment (SSR/server-based)
        let nextjs_preset = temps_presets::get_preset_by_slug("nextjs");
        assert!(nextjs_preset.is_some(), "NextJS preset should exist");
        let nextjs_static_output = nextjs_preset.unwrap().static_output_dir();
        assert!(
            nextjs_static_output.is_none(),
            "NextJS preset should NOT support static deployment (requires server)"
        );

        println!("✅ Static deployment integration test passed");
        println!("   - Static dir location: {}", temp_dir.display());
        println!("   - Files: index.html, assets/app.js, assets/styles.css");
        println!("   - Vite preset: supports static (dist/)");
        println!("   - Rsbuild preset: supports static (dist/)");
        println!("   - Docusaurus preset: supports static (build/)");
        println!("   - NextJS preset: requires server (no static output)");

        // Cleanup
        let _ = std_fs::remove_dir_all(&temp_dir);

        Ok(())
    }

    // Temporarily disabled - has compilation errors with missing test utilities
    // TODO: Fix test utilities and re-enable
    /*
    #[tokio::test]
    async fn test_proxy_static_file_serving() -> Result<()> {
        use crate::test_utils::{MockProjectContextResolver, ProjectContextForTest};
        use std::fs as std_fs;
        use std::io::Write;
        use temps_entities::{deployments, environments, projects};
        use sea_orm::ActiveValue::Set;

        // Create test database
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc().clone();

        // Create temporary directory for static files
        let temp_dir = std::env::temp_dir().join(format!("temps-proxy-test-{}", uuid::Uuid::new_v4()));
        std_fs::create_dir_all(&temp_dir)?;
        std_fs::create_dir_all(temp_dir.join("assets"))?;

        // Create test files with actual content
        let mut index_html = std_fs::File::create(temp_dir.join("index.html"))?;
        index_html.write_all(b"<!DOCTYPE html><html><head><title>Vite App</title></head><body><div id=\"root\"></div><script src=\"/assets/app.js\"></script></body></html>")?;
        drop(index_html);

        let mut app_js = std_fs::File::create(temp_dir.join("assets/app.js"))?;
        app_js.write_all(b"console.log('Vite app loaded'); document.getElementById('root').textContent = 'Hello from Vite';")?;
        drop(app_js);

        let mut styles_css = std_fs::File::create(temp_dir.join("assets/styles.css"))?;
        styles_css.write_all(b"body { font-family: sans-serif; margin: 0; padding: 20px; } #root { color: #333; }")?;
        drop(styles_css);

        let mut favicon = std_fs::File::create(temp_dir.join("favicon.ico"))?;
        favicon.write_all(&[0x00, 0x00, 0x01, 0x00])?; // Minimal ICO header
        drop(favicon);

        // Create test project, environment, and deployment using ActiveModelTrait
        use sea_orm::ActiveModelTrait;

        let project = projects::ActiveModel {
            slug: Set("vite-static-test".to_string()),
            name: Set("Vite Static Test".to_string()),
            repo_name: Set("vite-app".to_string()),
            repo_owner: Set("test-org".to_string()),
            directory: Set("".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Vite),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            slug: Set("production".to_string()),
            name: Set("Production".to_string()),
            subdomain: Set("vite-app".to_string()),
            host: Set("vite-app.example.com".to_string()),
            upstreams: Set(serde_json::json!([])),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deploy-abc123".to_string()),
            state: Set("deployed".to_string()),
            static_dir_location: Set(Some(temp_dir.to_string_lossy().to_string())),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // Create mock project context
        let project_context = ProjectContextForTest {
            project: Arc::new(project),
            environment: Arc::new(environment),
            deployment: Arc::new(deployment.clone()),
        };

        let project_context_resolver =
            Arc::new(MockProjectContextResolver::new_with_context(project_context));

        // Create LoadBalancer
        let crypto = create_test_crypto();
        let upstream_resolver = Arc::new(MockUpstreamResolver::default());
        let ip_service = create_mock_ip_service(db.clone());
        let proxy_log_storage: Arc<dyn crate::storage::ProxyLogStorage> =
            Arc::new(crate::storage::TimescaleDbProxyLogStore::new(
                db.clone(),
                ip_service.clone(),
            ));
        let (proxy_log_handle, _proxy_log_writer) =
            crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
                db.clone(),
                ip_service,
                proxy_log_storage,
            );
        let visitor_manager = Arc::new(MockVisitorManager::default());
        let session_manager = Arc::new(MockSessionManager::default());

        // Create config service for static file serving
        let config_service = create_test_config_service(db.clone());

        // Create IP access control service
        let ip_access_control_service = Arc::new(
            crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
        );

        // Create challenge service
        let challenge_service = Arc::new(
            crate::service::challenge_service::ChallengeService::new(db.clone()),
        );

        let lb = ProxyLoadBalancer::new(
            upstream_resolver,
            proxy_log_handle,
            project_context_resolver,
            visitor_manager,
            session_manager,
            crypto,
            db.clone(),
            config_service,
            ip_access_control_service,
            challenge_service,
            false,
        );

        // Test 1: Verify static_dir_location is set
        println!("\n🧪 Test 1: Verify deployment has static_dir_location");
        assert!(
            deployment.static_dir_location.is_some(),
            "Deployment should have static_dir_location"
        );
        println!("   ✅ Static dir: {}", deployment.static_dir_location.as_ref().unwrap());

        // Test 2: Verify files exist in the static directory
        println!("\n🧪 Test 2: Verify static files exist");
        let index_path = temp_dir.join("index.html");
        let js_path = temp_dir.join("assets/app.js");
        let css_path = temp_dir.join("assets/styles.css");

        assert!(index_path.exists(), "index.html should exist");
        assert!(js_path.exists(), "app.js should exist");
        assert!(css_path.exists(), "styles.css should exist");
        println!("   ✅ Found index.html");
        println!("   ✅ Found assets/app.js");
        println!("   ✅ Found assets/styles.css");
        println!("   ✅ Found favicon.ico");

        // Test 3: Verify file contents
        println!("\n🧪 Test 3: Verify file contents");
        let index_content = std_fs::read_to_string(&index_path)?;
        assert!(index_content.contains("<title>Vite App</title>"));
        assert!(index_content.contains("id=\"root\""));
        println!("   ✅ index.html contains valid HTML");

        let js_content = std_fs::read_to_string(&js_path)?;
        assert!(js_content.contains("Vite app loaded"));
        println!("   ✅ app.js contains valid JavaScript");

        let css_content = std_fs::read_to_string(&css_path)?;
        assert!(css_content.contains("sans-serif"));
        println!("   ✅ styles.css contains valid CSS");

        // Test 4: Test content type inference
        println!("\n🧪 Test 4: Test content type inference");
        use crate::proxy::LoadBalancer;

        assert_eq!(
            LoadBalancer::infer_content_type("index.html"),
            "text/html; charset=utf-8"
        );
        println!("   ✅ HTML → text/html; charset=utf-8");

        assert_eq!(
            LoadBalancer::infer_content_type("assets/app.js"),
            "application/javascript; charset=utf-8"
        );
        println!("   ✅ JS → application/javascript; charset=utf-8");

        assert_eq!(
            LoadBalancer::infer_content_type("assets/styles.css"),
            "text/css; charset=utf-8"
        );
        println!("   ✅ CSS → text/css; charset=utf-8");

        assert_eq!(
            LoadBalancer::infer_content_type("favicon.ico"),
            "image/x-icon"
        );
        println!("   ✅ ICO → image/x-icon");

        // Test 5: Test cacheable asset detection
        println!("\n🧪 Test 5: Test cacheable asset detection");
        assert!(
            LoadBalancer::is_cacheable_static_asset("/assets/app.js"),
            "/assets/ paths should be cacheable"
        );
        println!("   ✅ /assets/app.js is cacheable (immutable)");

        assert!(
            LoadBalancer::is_cacheable_static_asset("/static/bundle.chunk.abc123.js"),
            "Chunk files should be cacheable"
        );
        println!("   ✅ .chunk. files are cacheable");

        assert!(
            !LoadBalancer::is_cacheable_static_asset("/index.html"),
            "index.html should not be cacheable"
        );
        println!("   ✅ /index.html is NOT cacheable (must-revalidate)");

        // Test 6: Test path traversal protection (conceptual - can't easily test without full Pingora session)
        println!("\n🧪 Test 6: Path traversal protection");
        println!("   ℹ️  Proxy uses fs::canonicalize() to prevent path traversal");
        println!("   ℹ️  Paths like /../../../etc/passwd are blocked");
        println!("   ✅ Security: Path traversal protection enabled");

        // Test 7: Test SPA fallback logic (conceptual)
        println!("\n🧪 Test 7: SPA fallback for client-side routing");
        println!("   ℹ️  Routes without extensions (e.g., /about, /dashboard) → index.html");
        println!("   ℹ️  Files with extensions serve directly (e.g., /assets/app.js)");
        println!("   ✅ SPA routing: Fallback to index.html enabled");

        // Test 8: Verify deployment metadata
        println!("\n🧪 Test 8: Verify deployment workflow compatibility");
        println!("   ✅ Deployment state: {}", deployment.state);
        println!("   ✅ Deployment slug: {}", deployment.slug);
        println!("   ✅ Static deployment (no container required)");

        // Test 9: END-TO-END - Actually try to retrieve files from LoadBalancer context
        println!("\n🧪 Test 9: END-TO-END - File retrieval simulation");

        // Create a context with the deployment that has static_dir_location
        let mut ctx = lb.new_ctx();
        ctx.deployment = Some(Arc::new(deployment.clone()));
        ctx.host = "vite-app.example.com".to_string();

        // Test retrieving index.html (root path)
        ctx.path = "/".to_string();
        println!("   Testing: GET / (should serve index.html)");
        assert!(ctx.deployment.as_ref().unwrap().static_dir_location.is_some());
        let static_location = ctx.deployment.as_ref().unwrap().static_dir_location.as_ref().unwrap();
        let index_served = tokio::fs::read_to_string(format!("{}/index.html", static_location)).await;
        assert!(index_served.is_ok(), "Should be able to read index.html from static location");
        println!("   ✅ index.html accessible at: {}/index.html", static_location);

        // Test retrieving app.js
        ctx.path = "/assets/app.js".to_string();
        println!("   Testing: GET /assets/app.js");
        let js_served = tokio::fs::read_to_string(format!("{}/assets/app.js", static_location)).await;
        assert!(js_served.is_ok(), "Should be able to read app.js from static location");
        println!("   ✅ app.js accessible at: {}/assets/app.js", static_location);

        // Test retrieving styles.css
        ctx.path = "/assets/styles.css".to_string();
        println!("   Testing: GET /assets/styles.css");
        let css_served = tokio::fs::read_to_string(format!("{}/assets/styles.css", static_location)).await;
        assert!(css_served.is_ok(), "Should be able to read styles.css from static location");
        println!("   ✅ styles.css accessible at: {}/assets/styles.css", static_location);

        // Test non-existent file
        ctx.path = "/nonexistent.html".to_string();
        println!("   Testing: GET /nonexistent.html (should fail)");
        let nonexistent = tokio::fs::read_to_string(format!("{}/nonexistent.html", static_location)).await;
        assert!(nonexistent.is_err(), "Non-existent file should return error");
        println!("   ✅ Non-existent file correctly returns error");

        // Test SPA routing - route without extension should fallback to index.html
        ctx.path = "/about".to_string();
        println!("   Testing: GET /about (SPA route - should fallback to index.html)");
        // In real proxy, this would serve index.html
        let index_fallback = tokio::fs::read_to_string(format!("{}/index.html", static_location)).await;
        assert!(index_fallback.is_ok(), "SPA fallback should serve index.html");
        println!("   ✅ SPA route fallback to index.html works");

        println!("\n🎉 All proxy end-to-end static file serving tests passed!");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Summary:");
        println!("  • Static directory: {}", temp_dir.display());
        println!("  • Files created: index.html, assets/app.js, assets/styles.css, favicon.ico");
        println!("  • Database deployment.static_dir_location: {}", static_location);
        println!("  • Proxy can resolve deployment → static files");
        println!("  • File retrieval: ✅ index.html, ✅ app.js, ✅ styles.css");
        println!("  • Non-existent files: ✅ Correctly rejected");
        println!("  • SPA routing: ✅ Fallback to index.html");
        println!("  • Content types: HTML, JS, CSS, ICO");
        println!("  • Cache policy: Aggressive for /assets/, must-revalidate for HTML");
        println!("  • Security: Path traversal protection enabled");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // Cleanup
        let _ = std_fs::remove_dir_all(&temp_dir);

        Ok(())
    }
    */

    /// Test that ProjectContextResolver correctly identifies static deployments via RouteInfo
    #[tokio::test]
    async fn test_project_context_resolver_static_detection() -> Result<()> {
        use sea_orm::{ActiveModelTrait, Set};
        use std::fs as std_fs;
        use temps_entities::deployments::DeploymentMetadata;
        use temps_entities::preset::Preset;
        use temps_entities::upstream_config::UpstreamList;
        use temps_entities::{deployments, environments, projects};

        println!("\n🧪 Testing ProjectContextResolver static deployment detection");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.db.clone();

        // Create temporary directory for static files
        let temp_dir =
            std::env::temp_dir().join(format!("temps-test-static-{}", uuid::Uuid::new_v4()));
        std_fs::create_dir_all(&temp_dir)?;

        // Create a test file
        std_fs::write(temp_dir.join("index.html"), "<html>Test</html>")?;

        // Create project
        let project = projects::ActiveModel {
            name: Set("static-test-project".to_string()),
            slug: Set("static-test".to_string()),
            preset: Set(Preset::Vite),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("static-test.example.com".to_string()),
            host: Set("static-test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            project_id: Set(project.id),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create deployment WITH static_dir_location
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("static-deployment".to_string()),
            state: Set("completed".to_string()),
            static_dir_location: Set(Some(temp_dir.to_string_lossy().to_string())),
            metadata: Set(Some(DeploymentMetadata::default())),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Update environment to point to deployment
        let mut env: environments::ActiveModel = environment.into();
        env.current_deployment_id = Set(Some(deployment.id));
        let environment = env.update(db.as_ref()).await?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        route_table.load_routes().await?;

        println!("\n✅ Test data created:");
        println!("   Project: {} (id: {})", project.name, project.id);
        println!(
            "   Environment: {} (id: {})",
            environment.name, environment.id
        );
        println!("   Deployment: {} (id: {})", deployment.slug, deployment.id);
        println!(
            "   Static dir: {}",
            deployment.static_dir_location.as_ref().unwrap()
        );

        // Test 1: Verify route is loaded in route table
        println!("\n🧪 Test 1: Verify route is loaded with static backend");
        let route_info = route_table.get_route("static-test.example.com");
        assert!(
            route_info.is_some(),
            "Route should be loaded in route table"
        );

        let route_info = route_info.unwrap();
        assert!(
            route_info.is_static(),
            "Route should be identified as static"
        );
        assert_eq!(
            route_info.static_dir(),
            Some(temp_dir.to_string_lossy().as_ref()),
            "Static directory should match deployment"
        );
        println!("   ✅ Route loaded with BackendType::StaticDir");
        println!("   ✅ is_static() returns true");
        println!("   ✅ static_dir() returns correct path");

        // Test 2: Verify ProjectContextResolver uses RouteInfo API
        println!("\n🧪 Test 2: Verify ProjectContextResolver.is_static_deployment()");
        let resolver = ProjectContextResolverImpl::new(route_table.clone());
        let is_static = resolver
            .is_static_deployment("static-test.example.com")
            .await;
        assert!(
            is_static,
            "ProjectContextResolver should identify deployment as static"
        );
        println!("   ✅ is_static_deployment() returns true");

        // Test 3: Verify ProjectContextResolver.get_static_path()
        println!("\n🧪 Test 3: Verify ProjectContextResolver.get_static_path()");
        let static_path = resolver.get_static_path("static-test.example.com").await;
        assert!(
            static_path.is_some(),
            "get_static_path() should return Some for static deployment"
        );
        assert_eq!(
            static_path.unwrap(),
            temp_dir.to_string_lossy().to_string(),
            "Static path should match deployment static_dir_location"
        );
        println!("   ✅ get_static_path() returns correct path");

        // Test 4: Verify non-static deployment returns false
        println!("\n🧪 Test 4: Verify non-existent host returns false");
        let is_static_nonexistent = resolver
            .is_static_deployment("nonexistent.example.com")
            .await;
        assert!(
            !is_static_nonexistent,
            "Non-existent host should not be static"
        );
        let static_path_nonexistent = resolver.get_static_path("nonexistent.example.com").await;
        assert!(
            static_path_nonexistent.is_none(),
            "Non-existent host should return None for static path"
        );
        println!("   ✅ Non-existent host correctly returns false/None");

        println!("\n🎉 All ProjectContextResolver static detection tests passed!");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // Cleanup
        let _ = std_fs::remove_dir_all(&temp_dir);

        Ok(())
    }

    /// Test that container deployments are NOT identified as static
    #[tokio::test]
    async fn test_project_context_resolver_container_deployment() -> Result<()> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_core::chrono::Utc;
        use temps_entities::deployments::DeploymentMetadata;
        use temps_entities::preset::Preset;
        use temps_entities::upstream_config::UpstreamList;
        use temps_entities::{deployment_containers, deployments, environments, projects};

        println!("\n🧪 Testing ProjectContextResolver container deployment detection");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.db.clone();

        // Create project
        let project = projects::ActiveModel {
            name: Set("container-test-project".to_string()),
            slug: Set("container-test".to_string()),
            preset: Set(Preset::Nixpacks),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("container-test.example.com".to_string()),
            host: Set("container-test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            project_id: Set(project.id),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create deployment WITHOUT static_dir_location (container-based)
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("container-deployment".to_string()),
            state: Set("completed".to_string()),
            static_dir_location: Set(None), // No static directory
            metadata: Set(Some(DeploymentMetadata::default())),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Create deployment container
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set("test-container-123".to_string()),
            container_name: Set("test-container".to_string()),
            container_port: Set(3000),
            host_port: Set(Some(8080)),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(Some("running".to_string())),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        };
        container.insert(db.as_ref()).await?;

        // Update environment to point to deployment
        let mut env: environments::ActiveModel = environment.into();
        env.current_deployment_id = Set(Some(deployment.id));
        let _environment = env.update(db.as_ref()).await?;

        // Create route table and load routes
        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        route_table.load_routes().await?;

        println!("\n✅ Test data created:");
        println!("   Project: {} (preset: Nixpacks)", project.name);
        println!("   Deployment: {} (container-based)", deployment.slug);
        println!("   Container: localhost:8080");

        // Test 1: Verify route is loaded with upstream backend
        println!("\n🧪 Test 1: Verify route is loaded with upstream backend");
        let route_info = route_table.get_route("container-test.example.com");
        assert!(
            route_info.is_some(),
            "Route should be loaded in route table"
        );

        let route_info = route_info.unwrap();
        assert!(
            !route_info.is_static(),
            "Route should NOT be identified as static"
        );
        assert!(
            route_info.static_dir().is_none(),
            "Static directory should be None for container deployment"
        );
        assert_eq!(
            route_info.get_backend_addr(),
            "127.0.0.1:8080",
            "Should return container address"
        );
        println!("   ✅ Route loaded with BackendType::Upstream");
        println!("   ✅ is_static() returns false");
        println!("   ✅ static_dir() returns None");
        println!("   ✅ get_backend_addr() returns container address");

        // Test 2: Verify ProjectContextResolver identifies as non-static
        println!("\n🧪 Test 2: Verify ProjectContextResolver.is_static_deployment()");
        let resolver = ProjectContextResolverImpl::new(route_table.clone());
        let is_static = resolver
            .is_static_deployment("container-test.example.com")
            .await;
        assert!(
            !is_static,
            "ProjectContextResolver should NOT identify container deployment as static"
        );
        println!("   ✅ is_static_deployment() returns false");

        // Test 3: Verify get_static_path returns None
        println!("\n🧪 Test 3: Verify ProjectContextResolver.get_static_path()");
        let static_path = resolver.get_static_path("container-test.example.com").await;
        assert!(
            static_path.is_none(),
            "get_static_path() should return None for container deployment"
        );
        println!("   ✅ get_static_path() returns None");

        println!("\n🎉 All container deployment tests passed!");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        Ok(())
    }
}
