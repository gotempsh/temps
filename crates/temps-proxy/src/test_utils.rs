use sea_orm::*;
use sea_orm_migration::MigratorTrait;
#[cfg(test)]
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::{custom_routes, deployments, environments, projects, project_custom_domains, request_logs, visitor};
use temps_migrations::Migrator;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};

/// Test database setup with TimescaleDB container
pub struct TestDBMockOperations {
    pub db: Arc<DbConnection>,
}

impl TestDBMockOperations {
    /// Create a new test database with TimescaleDB
    pub async fn new(db: Arc<DbConnection>) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(TestDBMockOperations {
            db,
        })
    }

    /// Create test project with environment and deployment
    pub async fn create_test_project(
        &self,
    ) -> Result<
        (projects::Model, environments::Model, deployments::Model),
        Box<dyn std::error::Error>,
    > {
        self.create_test_project_with_domain("test.example.com")
            .await
    }

    /// Create test project with custom domain
    pub async fn create_test_project_with_domain(
        &self,
        domain: &str,
    ) -> Result<
        (projects::Model, environments::Model, deployments::Model),
        Box<dyn std::error::Error>,
    > {
        use temps_entities::types::ProjectType;

        // Create project with unique name based on domain
        let project_name = format!("test-project-{}", domain.replace(".", "-"));
        let project = projects::ActiveModel {
            name: Set(project_name.clone()),
            custom_domain: Set(Some(domain.to_string())),
            is_web_app: Set(true),
            project_type: Set(ProjectType::Server),
            slug: Set(project_name.clone()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(self.db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("http://localhost:8080".to_string()),
            host: Set(domain.to_string()),
            upstreams: Set(sea_orm::JsonValue::Null),
            project_id: Set(project.id),
            use_default_wildcard: Set(true),
            ..Default::default()
        };
        let environment = environment.insert(self.db.as_ref()).await?;

        // Create deployment (basic fields only)
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("http://localhost:8080".to_string()),
            state: Set("running".to_string()),
            metadata: Set(sea_orm::JsonValue::Object(serde_json::Map::from_iter(vec![
                ("container_port".to_string(), serde_json::Value::Number(8080.into())),
            ]))),
            ..Default::default()
        };
        let deployment = deployment.insert(self.db.as_ref()).await?;

        // Update environment to point to the deployment
        let mut environment: environments::ActiveModel = environment.into();
        environment.current_deployment_id = Set(Some(deployment.id));
        let environment = environment.update(self.db.as_ref()).await?;

        // Create project_custom_domains entry for the route table to find
        let custom_domain_entry = project_custom_domains::ActiveModel {
            domain: Set(domain.to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            status: Set("active".to_string()),
            redirect_to: Set(None),
            status_code: Set(None),
            ..Default::default()
        };
        custom_domain_entry.insert(self.db.as_ref()).await?;

        Ok((project, environment, deployment))
    }

    /// Create test custom route
    pub async fn create_test_custom_route(
        &self,
        domain: &str,
    ) -> Result<custom_routes::Model, Box<dyn std::error::Error>> {
        let custom_route = custom_routes::ActiveModel {
            domain: Set(domain.to_string()),
            host: Set("localhost".to_string()),
            port: Set(8080),
            enabled: Set(true),
            ..Default::default()
        };
        let custom_route = custom_route.insert(self.db.as_ref()).await?;
        Ok(custom_route)
    }

    /// Create test visitor
    pub async fn create_test_visitor(
        &self,
        project_id: i32,
    ) -> Result<visitor::Model, Box<dyn std::error::Error>> {
        let visitor = visitor::ActiveModel {
            visitor_id: Set(uuid::Uuid::new_v4().to_string()),
            project_id: Set(project_id),
            environment_id: Set(1), // Default environment
            user_agent: Set(Some("Test User Agent".to_string())),
            is_crawler: Set(false),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            ..Default::default()
        };
        let visitor = visitor.insert(self.db.as_ref()).await?;
        Ok(visitor)
    }

    /// Clean up all test data
    pub async fn cleanup(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Delete in reverse dependency order
        let _ = request_logs::Entity::delete_many()
            .exec(self.db.as_ref())
            .await;
        let _ = visitor::Entity::delete_many().exec(self.db.as_ref()).await;
        let _ = custom_routes::Entity::delete_many()
            .exec(self.db.as_ref())
            .await;
        let _ = deployments::Entity::delete_many()
            .exec(self.db.as_ref())
            .await;
        let _ = environments::Entity::delete_many()
            .exec(self.db.as_ref())
            .await;
        let _ = projects::Entity::delete_many().exec(self.db.as_ref()).await;
        Ok(())
    }
}

/// Mock server config for testing
pub fn create_test_server_config() -> TestServerConfig {
    TestServerConfig {
        address: "127.0.0.1:8080".to_string(),
        console_address: "127.0.0.1:3000".to_string(),
        tls_address: None,
    }
}

/// Test server configuration (simplified version)
#[derive(Debug, Clone)]
pub struct TestServerConfig {
    pub address: String,
    pub console_address: String,
    pub tls_address: Option<String>,
}

/// Create test project context
pub fn create_test_project_context(
    project: projects::Model,
    environment: environments::Model,
    deployment: deployments::Model,
) -> crate::traits::ProjectContext {
    crate::traits::ProjectContext {
        project: Arc::new(project),
        environment: Arc::new(environment),
        deployment: Arc::new(deployment),
    }
}

/// Create test visitor
pub fn create_test_visitor_trait(visitor: visitor::Model) -> crate::traits::Visitor {
    crate::traits::Visitor {
        visitor_id: visitor.visitor_id,
        visitor_id_i32: visitor.id,
        is_crawler: visitor.is_crawler,
        crawler_name: visitor.crawler_name,
    }
}

#[cfg(test)]
/// Mock ProjectContextResolver for testing redirect functionality
pub struct MockProjectContextResolver {
    redirect_host: Option<String>,
    redirect_url: Option<String>,
    redirect_status: Option<u16>,
}

#[cfg(test)]
impl MockProjectContextResolver {
    pub fn new() -> Self {
        Self {
            redirect_host: None,
            redirect_url: None,
            redirect_status: None,
        }
    }

    pub fn new_with_redirect(host: &str, url: String, status: u16) -> Self {
        Self {
            redirect_host: Some(host.to_string()),
            redirect_url: Some(url),
            redirect_status: Some(status),
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl crate::traits::ProjectContextResolver for MockProjectContextResolver {
    async fn resolve_context(&self, _host: &str) -> Option<crate::traits::ProjectContext> {
        None
    }

    async fn is_static_deployment(&self, _host: &str) -> bool {
        false
    }

    async fn get_redirect_info(&self, host: &str) -> Option<(String, u16)> {
        if let Some(redirect_host) = &self.redirect_host {
            if host == redirect_host {
                return Some((
                    self.redirect_url.clone().unwrap(),
                    self.redirect_status.unwrap(),
                ));
            }
        }
        None
    }

    async fn get_static_path(&self, _host: &str) -> Option<String> {
        None
    }
}
