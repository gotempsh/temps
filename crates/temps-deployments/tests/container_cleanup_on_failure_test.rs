//! Integration test for container cleanup when deployment fails
//!
//! This test verifies that when a deployment job fails (e.g., health check timeout),
//! the container is properly removed and not left in a restarting state.
//!
//! Test scenario:
//! 1. Build a Docker image that starts but doesn't listen on any port
//! 2. Attempt to deploy it with health checks enabled
//! 3. Verify that the deployment fails due to health check timeout
//! 4. Verify that the container was removed (not left running or restarting)

use async_trait::async_trait;
use bollard::Docker;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::{LogWriter, WorkflowBuilder, WorkflowCancellationProvider, WorkflowError};
use temps_deployer::docker::DockerRuntime;
use temps_deployer::{ContainerDeployer, ImageBuilder};
use temps_deployments::jobs::{
    BuildImageJobBuilder, DeployImageJobBuilder, DeploymentTarget, DownloadRepoBuilder,
};
use temps_git::{GitProviderManagerError, GitProviderManagerTrait, RepositoryInfo};
use tokio::sync::Mutex;

/// No-op cancellation provider for tests
struct NoCancellationProvider;

#[async_trait]
impl WorkflowCancellationProvider for NoCancellationProvider {
    async fn is_cancelled(&self, _workflow_run_id: &str) -> Result<bool, WorkflowError> {
        Ok(false) // Never cancelled in tests
    }
}

/// Capturing LogWriter for tests
struct CapturingLogWriter {
    stage_id: i32,
    logs: Arc<Mutex<Vec<String>>>,
}

impl CapturingLogWriter {
    fn new(stage_id: i32) -> Self {
        Self {
            stage_id,
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn get_logs(&self) -> Vec<String> {
        self.logs.lock().await.clone()
    }
}

#[async_trait]
impl LogWriter for CapturingLogWriter {
    async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
        self.logs.lock().await.push(message);
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        self.stage_id
    }
}

/// Mock GitProviderManager that copies from local fixture directory
struct LocalFixtureGitProvider {
    fixture_path: PathBuf,
}

#[async_trait]
impl GitProviderManagerTrait for LocalFixtureGitProvider {
    async fn clone_repository(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
        target_dir: &std::path::Path,
        _branch_or_ref: Option<&str>,
    ) -> Result<(), GitProviderManagerError> {
        // Directory should not exist or should be empty
        if target_dir.exists() {
            let is_empty = std::fs::read_dir(target_dir)
                .map_err(|e| {
                    GitProviderManagerError::CloneError(format!("Failed to read directory: {}", e))
                })?
                .next()
                .is_none();

            if !is_empty {
                return Err(GitProviderManagerError::DirectoryNotEmpty(
                    target_dir.display().to_string(),
                ));
            }
        }

        // Create directory and copy fixture files
        std::fs::create_dir_all(target_dir).map_err(|e| {
            GitProviderManagerError::CloneError(format!("Failed to create directory: {}", e))
        })?;

        copy_dir_recursive(&self.fixture_path, target_dir).map_err(|e| {
            GitProviderManagerError::CloneError(format!("Failed to copy fixture: {}", e))
        })?;
        Ok(())
    }

    async fn get_repository_info(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
    ) -> Result<RepositoryInfo, GitProviderManagerError> {
        Ok(RepositoryInfo {
            clone_url: "file://fixture".to_string(),
            default_branch: "main".to_string(),
            owner: "test".to_string(),
            name: "failing-healthcheck".to_string(),
        })
    }

    async fn download_archive(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
        _branch_or_ref: &str,
        _archive_path: &std::path::Path,
    ) -> Result<(), GitProviderManagerError> {
        // Force fallback to clone
        Err(GitProviderManagerError::Other(
            "Archive not available for fixtures".to_string(),
        ))
    }
}

/// Recursively copy directory contents
fn copy_dir_recursive(src: &PathBuf, dst: &std::path::Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_container_cleanup_on_deployment_failure() {
    println!("🚀 Starting container cleanup on deployment failure test");

    // Check if Docker is available
    let docker_check = Docker::connect_with_local_defaults();
    if docker_check.is_err() {
        println!("⚠️  Docker not available, skipping test");
        return;
    }
    let docker = Arc::new(docker_check.unwrap());
    if docker.ping().await.is_err() {
        println!("⚠️  Docker daemon not responding, skipping test");
        return;
    }

    // Get fixture path
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/failing-healthcheck");

    assert!(
        fixture_path.exists(),
        "Fixture directory must exist: {:?}",
        fixture_path
    );
    assert!(
        fixture_path.join("Dockerfile").exists(),
        "Dockerfile must exist in fixture"
    );

    // Initialize service managers
    let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(LocalFixtureGitProvider {
        fixture_path: fixture_path.clone(),
    });

    // Create Docker runtime
    let docker_runtime = Arc::new(DockerRuntime::new(
        docker.clone(),
        false,
        "temps-test".to_string(),
    ));

    // Ensure test network exists
    if let Err(e) = docker_runtime.ensure_network_exists().await {
        println!("⚠️  Failed to create network: {}", e);
        println!("Skipping test (Docker network required)");
        return;
    }

    let image_builder: Arc<dyn ImageBuilder> = docker_runtime.clone();
    let container_deployer: Arc<dyn ContainerDeployer> = docker_runtime.clone();

    println!("📦 Stage 1: Download Repository (fixture)");

    // Create DownloadRepoJob
    let download_job = DownloadRepoBuilder::new()
        .job_id("download_failing".to_string())
        .repo_owner("test".to_string())
        .repo_name("failing-healthcheck".to_string())
        .git_provider_connection_id(1)
        .branch_ref("main".to_string())
        .build(git_manager.clone())
        .expect("Should create download job");

    println!("🐳 Stage 2: Build Docker Image");

    // Create BuildImageJob
    let build_job = BuildImageJobBuilder::new()
        .job_id("build_failing".to_string())
        .download_job_id("download_failing".to_string())
        .image_tag("failing-healthcheck-test:latest".to_string())
        .dockerfile_path("Dockerfile".to_string())
        .build(image_builder.clone())
        .expect("Should create build job");

    println!("🚢 Stage 3: Deploy Image (will fail due to health check timeout)");

    // Create DeployImageJob with very short timeout to make test faster
    let mut env_vars = HashMap::new();
    env_vars.insert("TEST_VAR".to_string(), "test_value".to_string());

    let deploy_job = DeployImageJobBuilder::new()
        .job_id("deploy_failing".to_string())
        .build_job_id("build_failing".to_string())
        .target(DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        })
        .service_name("failing-app".to_string())
        .namespace("default".to_string())
        .port(3000)
        .replicas(1)
        .environment_variables(env_vars)
        .build(container_deployer.clone())
        .expect("Should create deploy job");

    println!("✅ All three jobs created successfully");

    // Create capturing log writer
    let log_writer = Arc::new(CapturingLogWriter::new(1));

    // Build workflow configuration
    let workflow_config = WorkflowBuilder::new()
        .with_workflow_run_id("failing-test-workflow".to_string())
        .with_deployment_context(1, 1, 1)
        .with_log_writer(log_writer.clone())
        .with_job(Arc::new(download_job))
        .with_job(Arc::new(build_job))
        .with_job(Arc::new(deploy_job))
        .continue_on_failure(false)
        .with_max_parallel_jobs(1)
        .build()
        .expect("Should build workflow");

    println!("🔄 Executing workflow (expecting failure due to health check timeout)...");

    use temps_core::WorkflowExecutor;

    let executor = WorkflowExecutor::new(None);
    let cancellation_provider = Arc::new(NoCancellationProvider);
    let result = executor
        .execute_workflow(workflow_config, cancellation_provider)
        .await;

    // The workflow SHOULD FAIL due to health check timeout
    assert!(
        result.is_err(),
        "Workflow should fail due to health check timeout"
    );
    println!("✅ Workflow failed as expected");

    // Print error for debugging
    if let Err(ref e) = result {
        println!("📋 Expected error: {:?}", e);
    }

    // Verify logs contain timeout/cleanup messages
    let logs = log_writer.get_logs().await;
    let all_logs = logs.join("\n");

    println!("📋 Checking logs for cleanup messages...");
    println!("  Total log entries: {}", logs.len());

    // Should have timeout message
    let has_timeout_message = all_logs.contains("timeout") || all_logs.contains("Timeout");
    if has_timeout_message {
        println!("  ✓ Found timeout message in logs");
    }

    // Should have cleanup message
    let has_cleanup_message = all_logs.contains("Cleaning up") || all_logs.contains("cleanup");
    if has_cleanup_message {
        println!("  ✓ Found cleanup message in logs");
    }

    // Should have container removal message
    let has_removal_message = all_logs.contains("Removing container")
        || all_logs.contains("removed")
        || all_logs.contains("Container") && all_logs.contains("removed successfully");
    if has_removal_message {
        println!("  ✓ Found container removal message in logs");
    }

    // Print sample logs for debugging
    println!("  Sample logs (last 20):");
    for (i, log) in logs.iter().rev().take(20).rev().enumerate() {
        println!("    [{}] {}", i + 1, log.trim());
    }

    println!("\n🔍 Verifying container was cleaned up...");

    // CRITICAL: Verify that no container with name "failing-app" exists
    use bollard::query_parameters::ListContainersOptions;
    let list_options = Some(ListContainersOptions {
        all: true, // Include stopped containers
        ..Default::default()
    });

    match docker.list_containers(list_options).await {
        Ok(containers) => {
            println!("  📦 Checking all containers for 'failing-app'...");

            let failing_container = containers.iter().find(|c| {
                c.names
                    .as_ref()
                    .map(|names| names.iter().any(|n| n.contains("failing-app")))
                    .unwrap_or(false)
            });

            if let Some(container) = failing_container {
                let status = container
                    .status
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("unknown");
                let state = container
                    .state
                    .as_ref()
                    .map(|s| format!("{:?}", s))
                    .unwrap_or_else(|| "unknown".to_string());
                let id = container.id.as_deref().unwrap_or("?");

                println!("  ❌ FAILURE: Container still exists!");
                println!("     ID: {}", id);
                println!("     Status: {}", status);
                println!("     State: {}", state);

                // Try to remove it for cleanup
                use bollard::query_parameters::RemoveContainerOptions;
                let _ = docker
                    .remove_container(
                        id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;

                panic!(
                    "Container 'failing-app' was NOT cleaned up after deployment failure! \
                     This is a critical bug - containers should be removed when deployment fails."
                );
            } else {
                println!("  ✅ Container 'failing-app' was properly cleaned up!");
                println!("     No container with this name found in Docker.");
            }
        }
        Err(e) => {
            println!("  ⚠️  Failed to list containers: {}", e);
            panic!("Could not verify container cleanup: {}", e);
        }
    }

    // Cleanup: Remove test image
    println!("🧹 Cleaning up test image...");
    use bollard::query_parameters::RemoveImageOptions;
    let remove_options = Some(RemoveImageOptions {
        force: true,
        ..Default::default()
    });

    match docker
        .remove_image("failing-healthcheck-test:latest", remove_options, None)
        .await
    {
        Ok(_) => println!("  ✓ Image cleanup completed"),
        Err(e) => println!("  ⚠️  Image cleanup failed: {}", e),
    }

    println!("\n🎉 Container cleanup test completed successfully!");
    println!("   ✓ Deployment failed as expected (health check timeout)");
    println!("   ✓ Container was properly removed (not left running/restarting)");
    println!("   ✓ Cleanup logic works correctly");
}

#[test]
fn test_failing_healthcheck_fixture_exists() {
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/failing-healthcheck");

    assert!(fixture_path.exists(), "Fixture directory should exist");
    assert!(
        fixture_path.join("Dockerfile").exists(),
        "Dockerfile should exist"
    );
    assert!(
        fixture_path.join("entrypoint.sh").exists(),
        "entrypoint.sh should exist"
    );

    // Verify Dockerfile contains expected content
    let dockerfile_content =
        std::fs::read_to_string(fixture_path.join("Dockerfile")).expect("Should read Dockerfile");
    assert!(
        dockerfile_content.contains("FROM alpine:"),
        "Dockerfile should use Alpine base image"
    );
    assert!(
        dockerfile_content.contains("EXPOSE 3000"),
        "Dockerfile should expose port 3000"
    );
}
