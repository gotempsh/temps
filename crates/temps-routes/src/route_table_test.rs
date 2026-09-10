// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for route table and listen/notify mechanism

#[cfg(test)]
mod route_table_tests {
    use crate::route_table::CachedPeerTable;
    use crate::test_utils::TestDBMockOperations;
    use sea_orm::{ActiveModelTrait, Set};
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{custom_routes, environment_domains, project_custom_domains};

    #[tokio::test]
    async fn test_route_table_basic_operations() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let db = test_db_mock.db.clone();

        // Create route table
        let route_table = Arc::new(CachedPeerTable::new(db));

        // Initially empty
        assert_eq!(route_table.len(), 0);
        assert!(route_table.is_empty());

        // Load routes
        route_table.load_routes().await?;

        // Still empty (no routes in database)
        assert_eq!(route_table.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_loads_custom_routes() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create a custom route
        let custom_route = custom_routes::ActiveModel {
            domain: Set("api.example.com".to_string()),
            host: Set("localhost".to_string()),
            port: Set(8080),
            enabled: Set(true),
            ..Default::default()
        };
        custom_route.insert(test_db.db.as_ref()).await?;

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify route was loaded
        assert_eq!(route_table.len(), 1);
        let route_info = route_table.get_route("api.example.com");
        assert!(route_info.is_some());

        let route_info = route_info.unwrap();
        assert_eq!(route_info.get_backend_addr(), "localhost:8080");
        assert!(route_info.project.is_none()); // Custom routes don't have projects
        assert!(route_info.environment.is_none());
        assert!(route_info.deployment.is_none());

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_loads_environment_domains() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create project, environment, and deployment
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;

        // Create deployment container with port 9000
        test_db
            .create_deployment_container(deployment.id, 9000, None)
            .await?;

        // Create environment domain
        let env_domain = environment_domains::ActiveModel {
            domain: Set("preview-123.example.com".to_string()),
            environment_id: Set(environment.id),
            ..Default::default()
        };
        env_domain.insert(test_db.db.as_ref()).await?;

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify route was loaded
        assert!(!route_table.is_empty());
        let route_info = route_table.get_route("preview-123.example.com");
        assert!(route_info.is_some());

        let route_info = route_info.unwrap();
        assert_eq!(route_info.get_backend_addr(), "127.0.0.1:9000");
        assert!(route_info.project.is_some());
        assert_eq!(route_info.project.as_ref().unwrap().id, project.id);
        assert!(route_info.environment.is_some());
        assert_eq!(route_info.environment.as_ref().unwrap().id, environment.id);
        assert!(route_info.deployment.is_some());
        assert_eq!(route_info.deployment.as_ref().unwrap().id, deployment.id);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_loads_project_custom_domains(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create project, environment, and deployment
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;

        // Create deployment container with port 9001
        test_db
            .create_deployment_container(deployment.id, 9001, None)
            .await?;

        // Create project custom domain
        let custom_domain = project_custom_domains::ActiveModel {
            domain: Set("mycustomdomain.com".to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            status: Set("active".to_string()),
            redirect_to: Set(None),
            status_code: Set(None),
            ..Default::default()
        };
        custom_domain.insert(test_db.db.as_ref()).await?;

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify route was loaded
        assert!(!route_table.is_empty());
        let route_info = route_table.get_route("mycustomdomain.com");
        assert!(route_info.is_some());

        let route_info = route_info.unwrap();
        assert_eq!(route_info.get_backend_addr(), "127.0.0.1:9001");
        assert!(route_info.project.is_some());
        assert_eq!(route_info.project.as_ref().unwrap().id, project.id);
        assert!(route_info.redirect_to.is_none());
        assert!(route_info.status_code.is_none());

        test_db.cleanup().await?;
        Ok(())
    }

    /// Issue #478: a project custom domain that matches the console hostname
    /// (`external_url`) must never win the route, otherwise the operator is
    /// locked out of the console and can only recover over the public IP.
    /// Installs that stored such a row before the API-level guard existed
    /// self-heal on the next route reload.
    #[tokio::test]
    async fn test_route_table_skips_console_hostname() -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::{ActiveModelBehavior, EntityTrait};
        use temps_core::AppSettings;
        use temps_entities::settings;

        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Configure the console to live on console.example.com
        let app_settings = AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            ..Default::default()
        };
        settings::Entity::insert(settings::ActiveModel {
            id: Set(1),
            data: Set(app_settings.to_json()),
            ..settings::ActiveModel::new()
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(settings::Column::Id)
                .update_column(settings::Column::Data)
                .to_owned(),
        )
        .exec(test_db.db.as_ref())
        .await?;

        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;
        test_db
            .create_deployment_container(deployment.id, 9010, None)
            .await?;

        // A pre-existing row claiming the console hostname, plus a normal one
        for domain in ["console.example.com", "app.example.com"] {
            project_custom_domains::ActiveModel {
                domain: Set(domain.to_string()),
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                status: Set("active".to_string()),
                redirect_to: Set(None),
                status_code: Set(None),
                ..Default::default()
            }
            .insert(test_db.db.as_ref())
            .await?;
        }

        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        assert!(
            route_table.get_route("console.example.com").is_none(),
            "console hostname must not be routed to a project (issue #478)"
        );
        assert!(
            route_table.get_route("app.example.com").is_some(),
            "ordinary custom domains must still route"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_with_redirect() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create project, environment, and deployment
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;

        // Create deployment container with port 9002
        test_db
            .create_deployment_container(deployment.id, 9002, None)
            .await?;

        // Create project custom domain with redirect
        let custom_domain = project_custom_domains::ActiveModel {
            domain: Set("old-domain.com".to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            status: Set("active".to_string()),
            redirect_to: Set(Some("https://new-domain.com".to_string())),
            status_code: Set(Some(301)),
            ..Default::default()
        };
        custom_domain.insert(test_db.db.as_ref()).await?;

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify route was loaded with redirect info
        let route_info = route_table.get_route("old-domain.com");
        assert!(route_info.is_some());

        let route_info = route_info.unwrap();
        assert_eq!(
            route_info.redirect_to,
            Some("https://new-domain.com".to_string())
        );
        assert_eq!(route_info.status_code, Some(301));

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_reload() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create route table and load (initially empty)
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;
        assert_eq!(route_table.len(), 0);

        // Add a custom route
        let custom_route = custom_routes::ActiveModel {
            domain: Set("new-route.com".to_string()),
            host: Set("localhost".to_string()),
            port: Set(8888),
            enabled: Set(true),
            ..Default::default()
        };
        custom_route.insert(test_db.db.as_ref()).await?;

        // Reload routes
        route_table.load_routes().await?;

        // Verify new route is loaded
        assert_eq!(route_table.len(), 1);
        let route_info = route_table.get_route("new-route.com");
        assert!(route_info.is_some());

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_handles_multiple_routes() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create multiple custom routes
        for i in 0..5 {
            let custom_route = custom_routes::ActiveModel {
                domain: Set(format!("route-{}.com", i)),
                host: Set("localhost".to_string()),
                port: Set(8000 + i),
                enabled: Set(true),
                ..Default::default()
            };
            custom_route.insert(test_db.db.as_ref()).await?;
        }

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify all routes are loaded
        assert_eq!(route_table.len(), 5);
        for i in 0..5 {
            let route_info = route_table.get_route(&format!("route-{}.com", i));
            assert!(route_info.is_some());
            let route_info = route_info.unwrap();
            assert_eq!(
                route_info.get_backend_addr(),
                format!("localhost:{}", 8000 + i)
            );
        }

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_disabled_custom_routes_not_loaded(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create disabled custom route
        let custom_route = custom_routes::ActiveModel {
            domain: Set("disabled-route.com".to_string()),
            host: Set("localhost".to_string()),
            port: Set(8080),
            enabled: Set(false), // Disabled
            ..Default::default()
        };
        custom_route.insert(test_db.db.as_ref()).await?;

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify route is NOT loaded
        assert_eq!(route_table.len(), 0);
        let route_info = route_table.get_route("disabled-route.com");
        assert!(route_info.is_none());

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_inactive_custom_domains_not_loaded(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create project, environment, and deployment
        let (project, environment, _deployment) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;

        // Create inactive project custom domain
        let custom_domain = project_custom_domains::ActiveModel {
            domain: Set("inactive-domain.com".to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            status: Set("pending".to_string()), // Not active
            redirect_to: Set(None),
            status_code: Set(None),
            ..Default::default()
        };
        custom_domain.insert(test_db.db.as_ref()).await?;

        // Create route table and load
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        // Verify route is NOT loaded
        let route_info = route_table.get_route("inactive-domain.com");
        assert!(route_info.is_none());

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_deployment_updates() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create project, environment, and deployment
        let (_project, environment, deployment) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;

        // Create deployment container with port 9000
        let container = test_db
            .create_deployment_container(deployment.id, 9000, None)
            .await?;

        // Create environment domain
        let env_domain = environment_domains::ActiveModel {
            domain: Set("app.example.com".to_string()),
            environment_id: Set(environment.id),
            ..Default::default()
        };
        env_domain.insert(test_db.db.as_ref()).await?;

        // Load routes initially
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let route_info = route_table.get_route("app.example.com").unwrap();
        assert_eq!(route_info.get_backend_addr(), "127.0.0.1:9000");

        // Update container to different port
        use temps_entities::deployment_containers;
        let mut container: deployment_containers::ActiveModel = container.into();
        container.container_port = Set(9999);
        let _container = container.update(test_db.db.as_ref()).await?;

        // Reload routes
        route_table.load_routes().await?;

        // Verify route points to new port
        let route_info = route_table.get_route("app.example.com").unwrap();
        assert_eq!(route_info.get_backend_addr(), "127.0.0.1:9999");

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_environment_current_deployment_changes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // Create project, environment, and deployment
        let (project, environment, deployment1) = test_db
            .create_test_project_with_domain("test.example.com")
            .await?;

        // Create deployment container with port 9000
        test_db
            .create_deployment_container(deployment1.id, 9000, None)
            .await?;

        // Create environment domain
        let env_domain = environment_domains::ActiveModel {
            domain: Set("app.example.com".to_string()),
            environment_id: Set(environment.id),
            ..Default::default()
        };
        env_domain.insert(test_db.db.as_ref()).await?;

        // Load routes initially
        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let route_info = route_table.get_route("app.example.com").unwrap();
        assert_eq!(route_info.deployment.as_ref().unwrap().id, deployment1.id);

        // Create second deployment
        let deployment2 = temps_entities::deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("http://localhost:9001".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        };
        let deployment2 = deployment2.insert(test_db.db.as_ref()).await?;

        // Create deployment container for second deployment with port 9001
        test_db
            .create_deployment_container(deployment2.id, 9001, None)
            .await?;

        // Update environment to point to new deployment
        let mut environment: temps_entities::environments::ActiveModel = environment.into();
        environment.current_deployment_id = Set(Some(deployment2.id));
        let _environment = environment.update(test_db.db.as_ref()).await?;

        // Reload routes
        route_table.load_routes().await?;

        // Verify route now points to new deployment
        let route_info = route_table.get_route("app.example.com").unwrap();
        assert_eq!(route_info.deployment.as_ref().unwrap().id, deployment2.id);
        assert_eq!(route_info.get_backend_addr(), "127.0.0.1:9001");

        test_db.cleanup().await?;
        Ok(())
    }

    /// Regression coverage for the routability status filter added alongside
    /// pause/resume (`load_routes`'s `deployment_containers.status IN (NULL,
    /// 'running')` condition): before this, every test here only ever
    /// created "running" containers, so nothing DB-backed proved the filter
    /// itself works. A deployment whose only container is "stopped" (e.g.
    /// after `pause_deployment`, or a manual per-container stop) must be
    /// skipped entirely rather than routed to a dead container.
    #[tokio::test]
    async fn test_route_table_excludes_stopped_only_container(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;

        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        let (_project, environment, deployment) = test_db
            .create_test_project_with_domain("stopped-only.example.com")
            .await?;

        // Only container for this deployment is "stopped" — not soft-deleted,
        // just not currently up.
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(format!("test-container-stopped-{}", deployment.id)),
            container_name: Set(format!("test-container-stopped-{}", deployment.id)),
            container_port: Set(9500),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(Some("stopped".to_string())),
            deployed_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        container.insert(test_db.db.as_ref()).await?;

        let env_domain = environment_domains::ActiveModel {
            domain: Set("stopped-only.preview.example.com".to_string()),
            environment_id: Set(environment.id),
            ..Default::default()
        };
        env_domain.insert(test_db.db.as_ref()).await?;

        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        assert!(
            route_table
                .get_route("stopped-only.preview.example.com")
                .is_none(),
            "a deployment whose only container is 'stopped' must not be routed to"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// Companion to `test_route_table_excludes_stopped_only_container`: a
    /// deployment with a mix of a "running" container, a NULL-status
    /// container (never had a status recorded — treated as up), and a
    /// "stopped" container must route only to the two live ones.
    #[tokio::test]
    async fn test_route_table_includes_running_and_null_status_containers(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;

        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        let (_project, environment, deployment) = test_db
            .create_test_project_with_domain("mixed-status.example.com")
            .await?;

        let now = chrono::Utc::now();

        let running = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(format!("test-container-running-{}", deployment.id)),
            container_name: Set(format!("test-container-running-{}", deployment.id)),
            container_port: Set(9600),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(Some("running".to_string())),
            deployed_at: Set(now),
            ..Default::default()
        };
        running.insert(test_db.db.as_ref()).await?;

        let null_status = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(format!("test-container-nullstatus-{}", deployment.id)),
            container_name: Set(format!("test-container-nullstatus-{}", deployment.id)),
            container_port: Set(9601),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(None),
            deployed_at: Set(now),
            ..Default::default()
        };
        null_status.insert(test_db.db.as_ref()).await?;

        let stopped = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(format!("test-container-stopped-{}", deployment.id)),
            container_name: Set(format!("test-container-stopped-{}", deployment.id)),
            container_port: Set(9602),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(Some("stopped".to_string())),
            deployed_at: Set(now),
            ..Default::default()
        };
        stopped.insert(test_db.db.as_ref()).await?;

        let env_domain = environment_domains::ActiveModel {
            domain: Set("mixed-status.preview.example.com".to_string()),
            environment_id: Set(environment.id),
            ..Default::default()
        };
        env_domain.insert(test_db.db.as_ref()).await?;

        let route_table = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        route_table.load_routes().await?;

        let route_info = route_table
            .get_route("mixed-status.preview.example.com")
            .expect("deployment with at least one live container must be routed to");

        let addresses = match &route_info.backend {
            crate::route_table::BackendType::Upstream { backends, .. } => backends
                .iter()
                .map(|b| b.address.clone())
                .collect::<Vec<_>>(),
            crate::route_table::BackendType::StaticDir { .. } => {
                panic!("expected an Upstream backend")
            }
        };

        assert_eq!(
            addresses.len(),
            2,
            "only the running and NULL-status containers should be routable, got {:?}",
            addresses
        );
        assert!(addresses.contains(&"127.0.0.1:9600".to_string()));
        assert!(addresses.contains(&"127.0.0.1:9601".to_string()));
        assert!(
            !addresses.contains(&"127.0.0.1:9602".to_string()),
            "the stopped container's port must not appear in the routable backends"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    // ── Traefik-discovered routes (section 6 of load_routes) ─────────

    /// Docker network the discovery tests adopt from. `load_routes()` only
    /// loads discovered rows for the network this process is configured for, so
    /// every discovery test has to state it explicitly.
    const DISCOVERY_NETWORK: &str = "temps";

    /// Seed one `traefik_discovered_routes` row on [`DISCOVERY_NETWORK`].
    async fn seed_discovered(
        db: &sea_orm::DatabaseConnection,
        host: &str,
        container: &str,
        port: i32,
        host_port: Option<i32>,
        tls: bool,
        enabled: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        seed_discovered_on(
            db,
            host,
            container,
            port,
            host_port,
            tls,
            enabled,
            DISCOVERY_NETWORK,
        )
        .await
    }

    /// Seed one `traefik_discovered_routes` row on an explicit network.
    #[allow(clippy::too_many_arguments)]
    async fn seed_discovered_on(
        db: &sea_orm::DatabaseConnection,
        host: &str,
        container: &str,
        port: i32,
        host_port: Option<i32>,
        tls: bool,
        enabled: bool,
        network: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        temps_entities::traefik_discovered_routes::ActiveModel {
            host: Set(host.to_string()),
            router_name: Set("app".to_string()),
            target_container_id: Set(format!("{container}-id")),
            target_container_name: Set(container.to_string()),
            target_port: Set(port),
            target_host_port: Set(host_port),
            network: Set(network.to_string()),
            tls: Set(tls),
            enabled: Set(enabled),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(())
    }

    /// A route table with Traefik label discovery enabled for
    /// [`DISCOVERY_NETWORK`], as `temps serve` configures it when
    /// `TEMPS_TRAEFIK_DISCOVERY_ENABLED=true`.
    fn discovery_enabled_table(db: Arc<sea_orm::DatabaseConnection>) -> Arc<CachedPeerTable> {
        let table = Arc::new(CachedPeerTable::new(db));
        table.set_traefik_discovery_network(Some(DISCOVERY_NETWORK.to_string()));
        table
    }

    #[tokio::test]
    async fn test_route_table_loads_traefik_discovered_routes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        seed_discovered(
            test_db.db.as_ref(),
            "legacy-stack.example.com",
            "whoami",
            8000,
            Some(18000),
            true,
            true,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        route_table.load_routes().await?;

        let route = route_table
            .get_route("legacy-stack.example.com")
            .expect("a discovered route must be served");
        assert!(
            route.deployment.is_none() && route.project.is_none() && route.environment.is_none(),
            "a discovered container has no Temps deployment context"
        );
        assert!(
            !route.cert_eligible,
            "a container-supplied tls label must NOT drive ACME issuance: the labels belong to a \
             workload Temps did not deploy, so honouring them would let any container on the \
             watched network mint certificates for hostnames it chose"
        );

        match &route.backend {
            crate::route_table::BackendType::Upstream { backends, .. } => {
                assert_eq!(backends.len(), 1);
                assert_eq!(
                    backends[0].container_name.as_deref(),
                    Some("whoami"),
                    "container metadata must reach the backend entry"
                );
                assert_eq!(
                    backends[0].container_id.as_deref(),
                    Some("whoami-id"),
                    "container id must reach the backend entry"
                );
            }
            other => panic!("expected an Upstream backend, got {other:?}"),
        }

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_route_table_skips_disabled_traefik_discovered_routes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        seed_discovered(
            test_db.db.as_ref(),
            "switched-off.example.com",
            "whoami",
            8000,
            None,
            false,
            false,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        route_table.load_routes().await?;

        assert!(
            route_table.get_route("switched-off.example.com").is_none(),
            "the operator kill-switch must keep a discovered route out of the table"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// A discovered row must never displace a real custom route, even if a
    /// racing writer managed to persist it. `load_routes` is the last line of
    /// defence for that precedence rule.
    #[tokio::test]
    async fn test_traefik_discovered_route_never_clobbers_a_custom_route(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        custom_routes::ActiveModel {
            domain: Set("contested.example.com".to_string()),
            host: Set("10.0.0.1".to_string()),
            port: Set(9999),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;

        seed_discovered(
            test_db.db.as_ref(),
            "contested.example.com",
            "squatter",
            3000,
            None,
            false,
            true,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        route_table.load_routes().await?;

        let route = route_table
            .get_route_by_host("contested.example.com")
            .expect("the legitimate custom route must still resolve");
        assert_eq!(
            route.get_backend_addr(),
            "10.0.0.1:9999",
            "the operator's custom route must win the host, not the discovered container"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// A wildcard custom route covering the host also wins: otherwise the
    /// discovered entry would sit in the table unreachable behind the
    /// wildcard, or worse, shadow it after a lookup-order change.
    #[tokio::test]
    async fn test_traefik_discovered_route_loses_to_a_wildcard_custom_route(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        custom_routes::ActiveModel {
            domain: Set("*.wild.example.com".to_string()),
            host: Set("10.0.0.2".to_string()),
            port: Set(7777),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;

        seed_discovered(
            test_db.db.as_ref(),
            "api.wild.example.com",
            "squatter",
            3000,
            None,
            false,
            true,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        route_table.load_routes().await?;

        assert!(
            route_table.get_route("api.wild.example.com").is_none(),
            "a wildcard custom route must keep the discovered entry out of the table"
        );
        let route = route_table
            .get_route_by_host("api.wild.example.com")
            .expect("the wildcard custom route must still resolve the host");
        assert_eq!(route.get_backend_addr(), "10.0.0.2:7777");

        test_db.cleanup().await?;
        Ok(())
    }

    /// The console hostname is reserved: a discovered container claiming it
    /// must not lock the operator out of the console (issue #478).
    #[tokio::test]
    async fn test_traefik_discovered_route_cannot_take_the_console_hostname(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::{ActiveModelBehavior, EntityTrait};
        use temps_core::AppSettings;
        use temps_entities::settings;

        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        let app_settings = AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            ..Default::default()
        };
        settings::Entity::insert(settings::ActiveModel {
            id: Set(1),
            data: Set(app_settings.to_json()),
            ..settings::ActiveModel::new()
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(settings::Column::Id)
                .update_column(settings::Column::Data)
                .to_owned(),
        )
        .exec(test_db.db.as_ref())
        .await?;

        // A published host port, so the row is rejected for being the console
        // hostname and not merely for being unreachable.
        seed_discovered(
            test_db.db.as_ref(),
            "console.example.com",
            "squatter",
            3000,
            Some(13000),
            false,
            true,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        route_table.load_routes().await?;

        assert!(
            route_table.get_route("console.example.com").is_none(),
            "a discovered container must never take the console hostname"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// The SSRF half of the port-label finding, at the merge site.
    ///
    /// On baremetal, `build_container_backend_addr` falls back to
    /// `127.0.0.1:<container port>` when there is no published host port —
    /// pointing the discovered hostname at an unrelated service on the Temps
    /// host. Such a row must be skipped outright, not routed to a guess.
    // `DEPLOYMENT_MODE` is process-global and `load_routes()` reads it, so the
    // lock has to span the await. It is a test-only guard over a std `Mutex`
    // that no production code ever takes, held by at most one test at a time
    // and never re-entered — there is nothing to deadlock against.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn test_traefik_discovered_route_without_a_host_port_is_skipped_on_baremetal(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _lock = crate::route_table::DEPLOYMENT_MODE_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        // 5432 is the point: a naive fallback would route this host at the
        // host's PostgreSQL port.
        seed_discovered(
            test_db.db.as_ref(),
            "unreachable.example.com",
            "internal-only",
            5432,
            None,
            false,
            true,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "baremetal") };
        let load = route_table.load_routes().await;
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
        load?;

        assert!(
            route_table.get_route("unreachable.example.com").is_none(),
            "a discovered container with no published host port is unreachable from a baremetal \
             install and must not be routed to 127.0.0.1:<container port>"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// Turning discovery off must actually stop serving what it adopted. The
    /// rows outlive the configuration, and the reconciler that would delete
    /// them is no longer running, so the reader has to enforce this.
    #[tokio::test]
    async fn test_traefik_discovered_routes_are_not_loaded_when_discovery_is_disabled(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        seed_discovered(
            test_db.db.as_ref(),
            "adopted.example.com",
            "whoami",
            8000,
            Some(18001),
            false,
            true,
        )
        .await?;

        // Enabled: the row is served.
        let enabled = discovery_enabled_table(test_db.db.clone());
        enabled.load_routes().await?;
        assert!(
            enabled.get_route("adopted.example.com").is_some(),
            "precondition: the row is served while discovery is enabled"
        );

        // Same database, discovery off (the default). Nothing is served, and a
        // reload on the still-running table drops what it had adopted.
        let disabled = Arc::new(CachedPeerTable::new(test_db.db.clone()));
        disabled.load_routes().await?;
        assert!(
            disabled.get_route("adopted.example.com").is_none(),
            "a node with discovery disabled must serve no discovered routes"
        );

        enabled.set_traefik_discovery_network(None);
        enabled.load_routes().await?;
        assert!(
            enabled.get_route("adopted.example.com").is_none(),
            "disabling discovery must remove previously-loaded routes on the next reload"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// Repointing `TEMPS_TRAEFIK_DISCOVERY_NETWORK` must stop serving the old
    /// network's adopted rows immediately, without waiting for the reconciler
    /// to get around to pruning them.
    #[tokio::test]
    async fn test_traefik_discovered_routes_from_another_network_are_not_loaded(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await?;
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await?;

        seed_discovered_on(
            test_db.db.as_ref(),
            "old-network.example.com",
            "leftover",
            8000,
            Some(18002),
            false,
            true,
            "previous_stack_default",
        )
        .await?;
        seed_discovered(
            test_db.db.as_ref(),
            "current-network.example.com",
            "whoami",
            8000,
            Some(18003),
            false,
            true,
        )
        .await?;

        let route_table = discovery_enabled_table(test_db.db.clone());
        route_table.load_routes().await?;

        assert!(
            route_table
                .get_route("current-network.example.com")
                .is_some(),
            "rows for the configured network are still served"
        );
        assert!(
            route_table.get_route("old-network.example.com").is_none(),
            "rows adopted from a network this node no longer watches must not be served"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// A statement-level trigger in Postgres fires even when the statement
    /// matched zero rows. The discovery reconciler reacts to every container
    /// event on the host and routinely issues delete-by-container statements
    /// that match nothing, so a statement-level trigger here would NOTIFY —
    /// and force a full `load_routes()` on every control plane node — on
    /// container churn that has nothing to do with Temps. Assert against the
    /// live catalog, which is the deployed truth rather than the SQL text.
    #[tokio::test]
    async fn test_traefik_discovered_routes_triggers_are_row_level(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::{ConnectionTrait, Statement};

        let test_db_mock = TestDatabase::with_migrations().await?;
        let db = test_db_mock.db.clone();

        // pg_trigger.tgtype bit 0 (value 1) is set for FOR EACH ROW triggers
        // and clear for FOR EACH STATEMENT ones. `tgisinternal` filters out
        // constraint-implementation triggers.
        let rows = db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT tgname, (tgtype & 1) AS is_row_level \
                 FROM pg_trigger \
                 WHERE tgrelid = 'traefik_discovered_routes'::regclass AND NOT tgisinternal \
                 ORDER BY tgname",
            ))
            .await?;

        assert_eq!(
            rows.len(),
            2,
            "expected the insert/delete and update triggers to exist"
        );
        for row in &rows {
            let name: String = row.try_get("", "tgname")?;
            let is_row_level: i32 = row.try_get("", "is_row_level")?;
            assert_eq!(
                is_row_level, 1,
                "trigger '{name}' must be FOR EACH ROW: a statement-level trigger NOTIFYs even \
                 when the statement affected zero rows, reloading every node's route table on \
                 unrelated container churn"
            );
        }

        Ok(())
    }
}
