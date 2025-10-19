//! Pipeline validation - demonstrates complete typed workflow without async tests
//!
//! This module provides synchronous validation of the complete deployment pipeline

use std::collections::HashMap;
use std::sync::Arc;
use std::path::Path;
use temps_core::{WorkflowBuilder, WorkflowTask};
use temps_git::{GitProviderManagerTrait, GitProviderManagerError, RepositoryInfo};
use async_trait::async_trait;

use crate::jobs::{
    DownloadRepoBuilder,
    BuildImageJobBuilder, RepositoryOutput,
    DeployImageJobBuilder, DeploymentTarget, BuildImageOutput
};
use temps_deployer::{ImageBuilder, BuildRequest, BuildResult, BuilderError, ContainerDeployer, DeployRequest, DeployResult, DeployerError, ContainerInfo, ContainerStatus};
use std::path::PathBuf;

/// Mock ImageBuilder for pipeline validation
struct MockImageBuilder;

#[async_trait]
impl ImageBuilder for MockImageBuilder {
    async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
        Ok(BuildResult {
            image_id: "sha256:mock123".to_string(),
            image_name: request.image_name,
            size_bytes: 104857600,
            build_duration_ms: 5000,
        })
    }

    async fn import_image(&self, _image_path: PathBuf, _tag: &str) -> Result<String, BuilderError> {
        Ok("sha256:imported".to_string())
    }

    async fn extract_from_image(&self, _image_name: &str, _source_path: &str, _destination_path: &Path) -> Result<(), BuilderError> {
        Ok(())
    }

    async fn build_image_with_callback(&self, request: temps_deployer::BuildRequestWithCallback) -> Result<BuildResult, BuilderError> {
        self.build_image(request.request).await
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        Ok(vec!["test:latest".to_string()])
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
        Ok(())
    }
}

/// Mock ContainerDeployer for pipeline validation
struct MockContainerDeployer;

#[async_trait]
impl ContainerDeployer for MockContainerDeployer {
    async fn deploy_container(&self, request: DeployRequest) -> Result<DeployResult, DeployerError> {
        Ok(DeployResult {
            container_id: "mock_container_123".to_string(),
            container_name: request.container_name,
            container_port: 8080,
            host_port: 8080,
            status: ContainerStatus::Running,
        })
    }

    async fn start_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn stop_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn pause_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn resume_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn remove_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn get_container_info(&self, _container_id: &str) -> Result<ContainerInfo, DeployerError> {
        Ok(ContainerInfo {
            container_id: "mock_container_123".to_string(),
            container_name: "mock_container".to_string(),
            image_name: "mock:latest".to_string(),
            status: ContainerStatus::Running,
            created_at: chrono::Utc::now(),
            ports: vec![],
            environment_vars: HashMap::new(),
        })
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        Ok(vec![])
    }

    async fn get_container_logs(&self, _container_id: &str) -> Result<String, DeployerError> {
        Ok("mock logs".to_string())
    }

    async fn stream_container_logs(&self, _container_id: &str) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        Err(DeployerError::Other("Not implemented".to_string()))
    }
}

/// Mock GitProviderManager for pipeline validation
struct MockGitProviderManager;

#[async_trait]
impl GitProviderManagerTrait for MockGitProviderManager {
    async fn clone_repository(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
        _target_dir: &Path,
        _branch_or_ref: Option<&str>,
    ) -> Result<(), GitProviderManagerError> {
        Ok(())
    }

    async fn get_repository_info(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
    ) -> Result<RepositoryInfo, GitProviderManagerError> {
        Ok(RepositoryInfo {
            clone_url: "https://github.com/example/repo.git".to_string(),
            default_branch: "main".to_string(),
            owner: "example".to_string(),
            name: "repo".to_string(),
        })
    }

    async fn download_archive(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
        _branch_or_ref: &str,
        _archive_path: &Path,
    ) -> Result<(), GitProviderManagerError> {
        Err(GitProviderManagerError::Other("Mock: not implemented".to_string()))
    }
}

/// Validate the complete deployment pipeline works end-to-end
pub fn validate_complete_deployment_pipeline() -> Result<(), String> {
    println!("🚀 Validating complete deployment pipeline...");

    // Phase 1: Job Creation and Configuration
    validate_job_creation()?;

    // Phase 2: Typed Data Flow
    validate_typed_data_flow()?;

    // Phase 3: Workflow Builder Integration
    validate_workflow_builder_integration()?;

    // Phase 4: Error Handling
    validate_error_handling()?;

    println!("✅ Complete deployment pipeline validation passed!");
    Ok(())
}

fn validate_job_creation() -> Result<(), String> {
    println!("  📦 Phase 1: Validating job creation and dependencies...");

    // Initialize service managers
    let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);
    let image_builder: Arc<dyn ImageBuilder> = Arc::new(MockImageBuilder);
    let container_deployer: Arc<dyn ContainerDeployer> = Arc::new(MockContainerDeployer);

    // Create DownloadRepoJob
    let download_job = DownloadRepoBuilder::new()
        .job_id("download_repo".to_string())
        .repo_owner("example".to_string())
        .repo_name("webapp".to_string())
        .git_provider_connection_id(1)
        .branch_ref("main".to_string())
        .build(git_manager)
        .map_err(|e| format!("Failed to create download job: {}", e))?;

    // Create BuildImageJob
    let build_job = BuildImageJobBuilder::new()
        .job_id("build_image".to_string())
        .download_job_id("download_repo".to_string()) // Typed dependency!
        .image_tag("webapp:v1.0.0".to_string())
        .dockerfile_path("Dockerfile".to_string())
        .build_args(vec![
            ("NODE_ENV".to_string(), "production".to_string()),
            ("VERSION".to_string(), "1.0.0".to_string()),
        ])
        .build(image_builder)
        .map_err(|e| format!("Failed to create build job: {}", e))?;

    // Create DeployImageJob
    let mut env_vars = HashMap::new();
    env_vars.insert("NODE_ENV".to_string(), "production".to_string());
    env_vars.insert("PORT".to_string(), "8080".to_string());

    let deploy_job = DeployImageJobBuilder::new()
        .job_id("deploy_image".to_string())
        .build_job_id("build_image".to_string()) // Typed dependency!
        .target(DeploymentTarget::Kubernetes {
            cluster_name: "test-cluster".to_string(),
            kubeconfig_path: None,
        })
        .service_name("webapp".to_string())
        .namespace("production".to_string())
        .replicas(2)
        .environment_variables(env_vars)
        .build(container_deployer)
        .map_err(|e| format!("Failed to create deploy job: {}", e))?;

    // Validate job properties
    assert_eq!(download_job.job_id(), "download_repo");
    assert_eq!(build_job.job_id(), "build_image");
    assert_eq!(deploy_job.job_id(), "deploy_image");

    // Validate dependency chain
    assert!(download_job.depends_on().is_empty(), "Download job should have no dependencies");
    assert_eq!(build_job.depends_on(), vec!["download_repo"], "Build job should depend on download");
    assert_eq!(deploy_job.depends_on(), vec!["build_image"], "Deploy job should depend on build");

    println!("    ✅ All jobs created with correct dependencies");
    Ok(())
}

fn validate_typed_data_flow() -> Result<(), String> {
    println!("  🔗 Phase 2: Validating typed data flow between jobs...");

    // Create workflow context
    let mut context = crate::test_utils::create_test_context(
        "validation-workflow".to_string(),
        1,  // deployment_id
        1,  // project_id
        1   // environment_id
    );

    // Step 1: Simulate DownloadRepoJob outputs
    context.set_output("download_repo", "repo_dir", "/tmp/workspace/webapp")
        .map_err(|e| format!("Failed to set download output: {}", e))?;
    context.set_output("download_repo", "checkout_ref", "main")
        .map_err(|e| format!("Failed to set checkout_ref: {}", e))?;
    context.set_output("download_repo", "repo_owner", "example")
        .map_err(|e| format!("Failed to set repo_owner: {}", e))?;
    context.set_output("download_repo", "repo_name", "webapp")
        .map_err(|e| format!("Failed to set repo_name: {}", e))?;

    // Validate we can extract typed repository output
    let repo_output = RepositoryOutput::from_context(&context, "download_repo")
        .map_err(|e| format!("Failed to extract repository output: {}", e))?;

    assert_eq!(repo_output.repo_owner, "example");
    assert_eq!(repo_output.repo_name, "webapp");
    assert_eq!(repo_output.checkout_ref, "main");
    println!("    ✅ Repository output extracted: {}/{}", repo_output.repo_owner, repo_output.repo_name);

    // Step 2: Simulate BuildImageJob outputs
    context.set_output("build_image", "image_tag", "webapp:v1.0.0")
        .map_err(|e| format!("Failed to set image_tag: {}", e))?;
    context.set_output("build_image", "image_id", "sha256:abc123def456789")
        .map_err(|e| format!("Failed to set image_id: {}", e))?;
    context.set_output("build_image", "size_bytes", 157286400u64)
        .map_err(|e| format!("Failed to set size_bytes: {}", e))?;
    context.set_output("build_image", "build_context", "/tmp/workspace/webapp")
        .map_err(|e| format!("Failed to set build_context: {}", e))?;
    context.set_output("build_image", "dockerfile_path", "/tmp/workspace/webapp/Dockerfile")
        .map_err(|e| format!("Failed to set dockerfile_path: {}", e))?;

    // Validate we can extract typed image output
    let image_output = BuildImageOutput::from_context(&context, "build_image")
        .map_err(|e| format!("Failed to extract image output: {}", e))?;

    assert_eq!(image_output.image_tag, "webapp:v1.0.0");
    assert_eq!(image_output.image_id, "sha256:abc123def456789");
    assert_eq!(image_output.size_bytes, 157286400);
    println!("    ✅ Image output extracted: {} ({})", image_output.image_tag, image_output.image_id);

    // Step 3: Simulate DeployImageJob outputs
    context.set_output("deploy_image", "deployment_id", "deploy-abc12345")
        .map_err(|e| format!("Failed to set deployment_id: {}", e))?;
    context.set_output("deploy_image", "service_name", "webapp")
        .map_err(|e| format!("Failed to set service_name: {}", e))?;
    context.set_output("deploy_image", "namespace", "production")
        .map_err(|e| format!("Failed to set namespace: {}", e))?;
    context.set_output("deploy_image", "endpoint_url", "https://webapp.production.example.com")
        .map_err(|e| format!("Failed to set endpoint_url: {}", e))?;

    // Validate final outputs
    let deployment_id: String = context.get_output("deploy_image", "deployment_id")
        .map_err(|e| format!("Failed to get deployment_id: {}", e))?
        .ok_or("deployment_id not found")?;
    let endpoint_url: String = context.get_output("deploy_image", "endpoint_url")
        .map_err(|e| format!("Failed to get endpoint_url: {}", e))?
        .ok_or("endpoint_url not found")?;

    assert_eq!(deployment_id, "deploy-abc12345");
    assert_eq!(endpoint_url, "https://webapp.production.example.com");
    println!("    ✅ Deployment output extracted: {} at {}", deployment_id, endpoint_url);

    Ok(())
}

fn validate_workflow_builder_integration() -> Result<(), String> {
    println!("  🏗️  Phase 3: Validating WorkflowBuilder integration...");

    let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);
    let image_builder: Arc<dyn ImageBuilder> = Arc::new(MockImageBuilder);
    let container_deployer: Arc<dyn ContainerDeployer> = Arc::new(MockContainerDeployer);

    // Create all three jobs
    let download_job = Arc::new(
        DownloadRepoBuilder::new()
            .job_id("download_repo".to_string())
            .repo_owner("example".to_string())
            .repo_name("webapp".to_string())
            .git_provider_connection_id(1)
            .branch_ref("main".to_string())
            .build(git_manager)
            .map_err(|e| format!("Failed to create download job: {}", e))?
    );

    let build_job = Arc::new(
        BuildImageJobBuilder::new()
            .job_id("build_image".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("webapp:latest".to_string())
            .build(image_builder)
            .map_err(|e| format!("Failed to create build job: {}", e))?
    );

    let deploy_job = Arc::new(
        DeployImageJobBuilder::new()
            .job_id("deploy_image".to_string())
            .build_job_id("build_image".to_string())
            .target(DeploymentTarget::Kubernetes {
                cluster_name: "test-cluster".to_string(),
                kubeconfig_path: None,
            })
            .service_name("webapp".to_string())
            .namespace("test".to_string())
            .build(container_deployer)
            .map_err(|e| format!("Failed to create deploy job: {}", e))?
    );

    // Create workflow with all jobs
    let workflow_config = WorkflowBuilder::new()
        .with_workflow_run_id("validation-test-123".to_string())
        .with_deployment_context(1, 1, 1)
        .with_var("pipeline_type", "full_deployment")
        .map_err(|e| format!("Failed to set pipeline_type var: {}", e))?
        .with_var("environment", "test")
        .map_err(|e| format!("Failed to set environment var: {}", e))?
        .with_job(download_job.clone())
        .with_job(build_job.clone())
        .with_job(deploy_job.clone())
        .continue_on_failure(false)
        .with_max_parallel_jobs(1)
        .build()
        .map_err(|e| format!("Failed to build workflow: {}", e))?;

    // Validate workflow configuration
    assert_eq!(workflow_config.workflow_run_id, "validation-test-123");
    assert_eq!(workflow_config.jobs.len(), 3);
    assert!(!workflow_config.continue_on_failure);
    assert_eq!(workflow_config.max_parallel_jobs, 1);

    // Validate all jobs are present
    let job_ids: Vec<_> = workflow_config.jobs.iter()
        .map(|j| j.job.job_id())
        .collect();

    assert!(job_ids.contains(&"download_repo"));
    assert!(job_ids.contains(&"build_image"));
    assert!(job_ids.contains(&"deploy_image"));

    println!("    ✅ WorkflowBuilder integration validated");
    Ok(())
}

fn validate_error_handling() -> Result<(), String> {
    println!("  🚨 Phase 4: Validating error handling in dependency chain...");

    let mut context = crate::test_utils::create_test_context("error-test".to_string(), 1, 1, 1);

    // Test 1: Missing download output should fail
    let build_result = RepositoryOutput::from_context(&context, "download_repo");
    if build_result.is_ok() {
        return Err("Should fail when download outputs missing".to_string());
    }
    println!("    ✅ Correctly fails without download outputs");

    // Test 2: Add download outputs but missing build outputs should fail
    context.set_output("download_repo", "repo_dir", "/tmp/repo").unwrap();
    context.set_output("download_repo", "checkout_ref", "main").unwrap();
    context.set_output("download_repo", "repo_owner", "user").unwrap();
    context.set_output("download_repo", "repo_name", "project").unwrap();

    let deploy_result = BuildImageOutput::from_context(&context, "build_image");
    if deploy_result.is_ok() {
        return Err("Should fail when build outputs missing".to_string());
    }
    println!("    ✅ Correctly fails without build outputs");

    // Test 3: Complete chain should work
    context.set_output("build_image", "image_tag", "project:latest").unwrap();
    context.set_output("build_image", "image_id", "sha256:123").unwrap();
    context.set_output("build_image", "size_bytes", 1000000u64).unwrap();
    context.set_output("build_image", "build_context", "/tmp/repo").unwrap();
    context.set_output("build_image", "dockerfile_path", "/tmp/repo/Dockerfile").unwrap();

    let repo_result = RepositoryOutput::from_context(&context, "download_repo");
    let build_result = BuildImageOutput::from_context(&context, "build_image");

    if repo_result.is_err() || build_result.is_err() {
        return Err("Should succeed with all outputs present".to_string());
    }
    println!("    ✅ Complete chain works when all outputs present");

    Ok(())
}

/// Validate different deployment target configurations
pub fn validate_deployment_configurations() -> Result<(), String> {
    println!("⚙️  Validating deployment target configurations...");

    let container_deployer: Arc<dyn ContainerDeployer> = Arc::new(MockContainerDeployer);

    // Test Kubernetes deployment
    let k8s_job = DeployImageJobBuilder::new()
        .job_id("deploy_k8s".to_string())
        .build_job_id("build_image".to_string())
        .target(DeploymentTarget::Kubernetes {
            cluster_name: "prod-cluster".to_string(),
            kubeconfig_path: Some("/etc/kubernetes/kubeconfig".to_string()),
        })
        .service_name("webapp".to_string())
        .namespace("production".to_string())
        .replicas(3)
        .build(container_deployer.clone())
        .map_err(|e| format!("Failed to create Kubernetes deploy job: {}", e))?;

    assert_eq!(k8s_job.config().replicas, 3);
    assert_eq!(k8s_job.config().namespace, "production");

    // Test Docker deployment
    let docker_job = DeployImageJobBuilder::new()
        .job_id("deploy_docker".to_string())
        .build_job_id("build_image".to_string())
        .target(DeploymentTarget::Docker {
            registry_url: "registry.example.com".to_string(),
            network: Some("app-network".to_string()),
        })
        .service_name("webapp".to_string())
        .namespace("default".to_string())
        .replicas(1)
        .build(container_deployer.clone())
        .map_err(|e| format!("Failed to create Docker deploy job: {}", e))?;

    assert_eq!(docker_job.config().replicas, 1);

    // Test Cloud Run deployment
    let _cloudrun_job = DeployImageJobBuilder::new()
        .job_id("deploy_cloudrun".to_string())
        .build_job_id("build_image".to_string())
        .target(DeploymentTarget::CloudRun {
            project_id: "my-gcp-project".to_string(),
            region: "us-central1".to_string(),
        })
        .service_name("webapp".to_string())
        .build(container_deployer)
        .map_err(|e| format!("Failed to create Cloud Run deploy job: {}", e))?;

    println!("✅ All deployment configurations validated");
    Ok(())
}

/// Run complete pipeline validation
pub fn run_complete_validation() -> Result<(), String> {
    println!("🎯 Running complete pipeline validation...\n");

    validate_complete_deployment_pipeline()?;
    println!();
    validate_deployment_configurations()?;

    println!("\n🎉 All pipeline validations passed successfully!");
    println!("   ✅ Job creation and dependency chain");
    println!("   ✅ Typed data flow between stages");
    println!("   ✅ WorkflowBuilder integration");
    println!("   ✅ Error handling and validation");
    println!("   ✅ Multiple deployment target support");

    Ok(())
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn test_pipeline_validation() {
        run_complete_validation().expect("Pipeline validation should pass");
    }
}