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
}
