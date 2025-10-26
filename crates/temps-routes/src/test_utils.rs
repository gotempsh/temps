//! Test utilities for route table tests

use sea_orm::*;
use std::sync::Arc;
use temps_core::chrono::Utc;
use temps_database::DbConnection;
use temps_entities::{
    custom_routes, deployment_containers, deployments, environments, project_custom_domains,
    projects, upstream_config::UpstreamList,
};
use temps_entities::deployments::DeploymentMetadata;

/// Test database mock operations for route table tests
pub struct TestDBMockOperations {
    pub db: Arc<DbConnection>,
}

impl TestDBMockOperations {
    /// Create a new test database mock operations instance
    pub async fn new(db: Arc<DbConnection>) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(TestDBMockOperations { db })
    }

    /// Create test project with environment and deployment
    #[allow(dead_code)]
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
        use temps_entities::preset::Preset;

        // Create project with unique name based on domain
        let project_name = format!("test-project-{}", domain.replace(".", "-"));
        let project = projects::ActiveModel {
            name: Set(project_name.clone()),
            preset: Set(Preset::Nixpacks), // Default to Nixpacks for tests
            slug: Set(project_name.clone()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        let project = project.insert(self.db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("http://localhost:8080".to_string()),
            host: Set(domain.to_string()),
            upstreams: Set(UpstreamList::default()),
            project_id: Set(project.id),
            ..Default::default()
        };
        let environment = environment.insert(self.db.as_ref()).await?;

        // Create deployment (basic fields only)
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("http://localhost:8080".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(DeploymentMetadata::default())), // Required NOT NULL field
            ..Default::default()
        };
        let deployment = deployment.insert(self.db.as_ref()).await?;

        // Update environment to point to the deployment
        let mut environment: environments::ActiveModel = environment.into();
        environment.current_deployment_id = Set(Some(deployment.id));
        let environment = environment.update(self.db.as_ref()).await?;

        Ok((project, environment, deployment))
    }

    /// Create a deployment container for a deployment with a specific port
    pub async fn create_deployment_container(
        &self,
        deployment_id: i32,
        container_port: i32,
        host_port: Option<i32>,
    ) -> Result<deployment_containers::Model, Box<dyn std::error::Error>> {
        let container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment_id),
            container_id: Set(format!("test-container-{}", deployment_id)),
            container_name: Set(format!("test-container-{}", deployment_id)),
            container_port: Set(container_port),
            host_port: Set(host_port),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(Some("running".to_string())),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        };
        let container = container.insert(self.db.as_ref()).await?;
        Ok(container)
    }

    /// Clean up all test data
    pub async fn cleanup(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Delete in reverse dependency order
        let _ = project_custom_domains::Entity::delete_many()
            .exec(self.db.as_ref())
            .await;
        let _ = custom_routes::Entity::delete_many()
            .exec(self.db.as_ref())
            .await;
        let _ = deployment_containers::Entity::delete_many()
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
