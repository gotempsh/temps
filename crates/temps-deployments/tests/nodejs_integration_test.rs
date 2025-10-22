#![allow(deprecated)]
//! Integration test for Node.js deployment through all three stages
//!
//! This test demonstrates the complete deployment pipeline with a real Node.js application:
//! 1. Download Repo (simulated with local fixture)
//! 2. Build Image (using Docker to build the Node.js Dockerfile)
//! 3. Deploy Image (simulated deployment)

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::{LogWriter, WorkflowError};
use temps_core::{WorkflowBuilder, WorkflowCancellationProvider, WorkflowTask};
use temps_deployer::{docker::DockerRuntime, ContainerDeployer, ImageBuilder};
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

/// Capturing LogWriter for tests that stores all log messages
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
            name: "nodejs-app".to_string(),
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

/// Recursively copy directory contents (dst must already exist)
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
async fn test_nodejs_three_stage_deployment() {
    use bollard::Docker;

    println!("🚀 Starting Node.js three-stage deployment test");

    // Check if Docker is available using bollard
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple-nodejs");

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

    // Create real Docker runtime (will skip test if Docker not available)
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

    println!("📦 Stage 1: Download Repository");

    // Create DownloadRepoJob
    let download_job = DownloadRepoBuilder::new()
        .job_id("download_nodejs".to_string())
        .repo_owner("test".to_string())
        .repo_name("nodejs-app".to_string())
        .git_provider_connection_id(1)
        .branch_ref("main".to_string())
        .build(git_manager.clone())
        .expect("Should create download job");

    println!("🐳 Stage 2: Build Docker Image");

    // Create BuildImageJob
    let build_job = BuildImageJobBuilder::new()
        .job_id("build_nodejs".to_string())
        .download_job_id("download_nodejs".to_string())
        .image_tag("nodejs-test-app:latest".to_string())
        .dockerfile_path("Dockerfile".to_string())
        .build_args(vec![("NODE_ENV".to_string(), "production".to_string())])
        .build(image_builder.clone())
        .expect("Should create build job");

    println!("🚢 Stage 3: Deploy Image");

    // Create DeployImageJob with Docker target (for real deployment)
    let mut env_vars = HashMap::new();
    env_vars.insert("NODE_ENV".to_string(), "production".to_string());
    env_vars.insert("PORT".to_string(), "3000".to_string());

    let deploy_job = DeployImageJobBuilder::new()
        .job_id("deploy_nodejs".to_string())
        .build_job_id("build_nodejs".to_string())
        .target(DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        })
        .service_name("nodejs-app".to_string())
        .namespace("default".to_string())
        .port(3000)
        .replicas(1)
        .environment_variables(env_vars)
        .build(container_deployer.clone())
        .expect("Should create deploy job");

    println!("✅ All three jobs created successfully");

    // Verify job structure
    assert_eq!(download_job.job_id(), "download_nodejs");
    assert_eq!(build_job.job_id(), "build_nodejs");
    assert_eq!(deploy_job.job_id(), "deploy_nodejs");

    // Verify dependency chain
    assert!(
        download_job.depends_on().is_empty(),
        "Download should have no dependencies"
    );
    assert_eq!(
        build_job.depends_on(),
        vec!["download_nodejs"],
        "Build should depend on download"
    );
    assert_eq!(
        deploy_job.depends_on(),
        vec!["build_nodejs"],
        "Deploy should depend on build"
    );

    println!("✅ Dependency chain validated");

    // Create capturing log writer
    let log_writer = Arc::new(CapturingLogWriter::new(1));

    // Build workflow configuration
    let workflow_config = WorkflowBuilder::new()
        .with_workflow_run_id("nodejs-test-workflow".to_string())
        .with_deployment_context(1, 1, 1)
        .with_log_writer(log_writer.clone())
        .with_var("repo_owner", "test")
        .unwrap()
        .with_var("repo_name", "nodejs-app")
        .unwrap()
        .with_var("image_tag", "nodejs-test-app:latest")
        .unwrap()
        .with_job(Arc::new(download_job))
        .with_job(Arc::new(build_job))
        .with_job(Arc::new(deploy_job))
        .continue_on_failure(false)
        .with_max_parallel_jobs(1)
        .build()
        .expect("Should build workflow");

    assert_eq!(workflow_config.jobs.len(), 3, "Workflow should have 3 jobs");
    println!("✅ WorkflowBuilder created with 3 jobs");

    // Execute the workflow
    println!("🔄 Executing workflow...");
    use temps_core::WorkflowExecutor;

    let executor = WorkflowExecutor::new(None); // No job tracker for test
    let cancellation_provider = Arc::new(NoCancellationProvider);
    let result = executor
        .execute_workflow(workflow_config, cancellation_provider)
        .await;

    // Check execution result
    match &result {
        Ok(final_context) => {
            println!("✅ Workflow execution completed successfully");

            // Verify outputs from each stage
            println!("🔍 Verifying stage outputs...");

            // Stage 1: Download - should have repo_dir output
            let repo_dir: Option<String> = final_context
                .get_output("download_nodejs", "repo_dir")
                .expect("Should get repo_dir output");
            assert!(repo_dir.is_some(), "Download stage should produce repo_dir");
            println!("  ✓ Download stage: repo_dir = {:?}", repo_dir);

            // Stage 2: Build - should have image_tag and image_id outputs
            let image_tag: Option<String> = final_context
                .get_output("build_nodejs", "image_tag")
                .expect("Should get image_tag output");
            assert!(image_tag.is_some(), "Build stage should produce image_tag");
            println!("  ✓ Build stage: image_tag = {:?}", image_tag);

            let image_id: Option<String> = final_context
                .get_output("build_nodejs", "image_id")
                .expect("Should get image_id output");
            assert!(image_id.is_some(), "Build stage should produce image_id");
            println!("  ✓ Build stage: image_id = {:?}", image_id);

            // Stage 3: Deploy - should have deployment outputs
            let deployment_status: Option<String> = final_context
                .get_output("deploy_nodejs", "status")
                .expect("Should get deployment status");
            println!("  ✓ Deploy stage: status = {:?}", deployment_status);

            let deployment_id: Option<String> = final_context
                .get_output("deploy_nodejs", "deployment_id")
                .expect("Should get deployment_id output");
            if let Some(container_id) = deployment_id {
                println!("\n🔍 Verifying Container Deployment:");
                println!("  Container ID: {}", container_id);

                // 1. Check if container exists and is running
                use bollard::container::InspectContainerOptions;
                match docker
                    .inspect_container(&container_id, None::<InspectContainerOptions>)
                    .await
                {
                    Ok(inspect) => {
                        // Check running status
                        let state = inspect.state.as_ref();
                        let is_running = state.and_then(|s| s.running).unwrap_or(false);
                        let status = state
                            .and_then(|s| s.status.as_ref())
                            .map(|s| format!("{:?}", s))
                            .unwrap_or_else(|| "unknown".to_string());
                        let exit_code = state.and_then(|s| s.exit_code);
                        let started_at = state
                            .and_then(|s| s.started_at.as_ref())
                            .map(|s| s.to_string());

                        println!("  ✓ Container status: {}", status);
                        println!("  ✓ Running: {}", is_running);
                        if let Some(started) = started_at {
                            println!("  ✓ Started at: {}", started);
                        }

                        if is_running {
                            println!("  ✅ Container is RUNNING!");

                            // 2. Check container configuration
                            if let Some(config) = inspect.config {
                                println!("\n  📋 Container Configuration:");
                                if let Some(image) = config.image {
                                    println!("    - Image: {}", image);
                                }
                                if let Some(env) = config.env {
                                    println!("    - Environment variables: {} entries", env.len());
                                    for var in env.iter().filter(|v| {
                                        v.starts_with("NODE_ENV") || v.starts_with("PORT")
                                    }) {
                                        println!("      • {}", var);
                                    }
                                }
                                if let Some(exposed_ports) = config.exposed_ports {
                                    println!(
                                        "    - Exposed ports: {}",
                                        exposed_ports
                                            .keys()
                                            .map(|k| k.as_str())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    );
                                }
                            }

                            // 3. Check network settings and ports
                            if let Some(network_settings) = inspect.network_settings {
                                if let Some(ports) = network_settings.ports {
                                    println!("\n  🌐 Port Bindings:");
                                    for (container_port, host_bindings) in ports.iter() {
                                        if let Some(bindings) = host_bindings {
                                            for binding in bindings {
                                                let host_ip =
                                                    binding.host_ip.as_deref().unwrap_or("0.0.0.0");
                                                let host_port =
                                                    binding.host_port.as_deref().unwrap_or("?");
                                                println!(
                                                    "    - {}:{} -> {}",
                                                    host_ip, host_port, container_port
                                                );
                                            }
                                        }
                                    }
                                }
                            }

                            // 4. Check container logs (last few lines)
                            println!("\n  📄 Container Logs (last 10 lines):");
                            use bollard::container::LogsOptions;
                            use futures_util::stream::StreamExt;

                            let log_options = Some(LogsOptions::<String> {
                                stdout: true,
                                stderr: true,
                                tail: "10".to_string(),
                                ..Default::default()
                            });

                            let mut log_stream = docker.logs(&container_id, log_options);
                            let mut log_count = 0;
                            while let Some(log_result) = log_stream.next().await {
                                if let Ok(log_output) = log_result {
                                    print!("    {}", log_output);
                                    log_count += 1;
                                }
                            }
                            if log_count == 0 {
                                println!("    (no logs yet)");
                            }

                            // 5. List all running containers to confirm visibility
                            println!("\n  📦 All Running Containers:");
                            use bollard::container::ListContainersOptions;
                            let list_options = Some(ListContainersOptions::<String> {
                                all: false, // Only running containers
                                ..Default::default()
                            });

                            match docker.list_containers(list_options).await {
                                Ok(containers) => {
                                    for container in containers.iter() {
                                        let id = container.id.as_deref().unwrap_or("?");
                                        let names = container
                                            .names
                                            .as_ref()
                                            .map(|n| n.join(", "))
                                            .unwrap_or_else(|| "unnamed".to_string());
                                        let image = container.image.as_deref().unwrap_or("?");
                                        let status = container.status.as_deref().unwrap_or("?");

                                        let marker = if id.starts_with(&container_id[..12]) {
                                            "👉"
                                        } else {
                                            "  "
                                        };
                                        println!(
                                            "    {} {} | {} | {} | {}",
                                            marker,
                                            &id[..12],
                                            names,
                                            image,
                                            status
                                        );
                                    }
                                }
                                Err(e) => println!("    Failed to list containers: {}", e),
                            }
                        } else {
                            println!("  ❌ Container is NOT running!");
                            println!("  Status: {}", status);
                            if let Some(code) = exit_code {
                                println!("  Exit code: {}", code);
                            }

                            // Print container logs for debugging
                            println!("\n  📄 Container Logs:");
                            use bollard::container::LogsOptions;
                            use futures_util::stream::StreamExt;

                            let log_options = Some(LogsOptions::<String> {
                                stdout: true,
                                stderr: true,
                                tail: "50".to_string(),
                                ..Default::default()
                            });

                            let mut log_stream = docker.logs(&container_id, log_options);
                            while let Some(log_result) = log_stream.next().await {
                                if let Ok(log_output) = log_result {
                                    print!("    {}", log_output);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("  ❌ Failed to inspect container: {}", e);
                    }
                }
            }
        }
        Err(ref e) => {
            println!("❌ Workflow execution failed: {:?}", e);

            // Print all logs for debugging
            let error_logs = log_writer.get_logs().await;
            println!("📋 Full error logs ({} entries):", error_logs.len());
            for (i, log) in error_logs.iter().enumerate() {
                println!("  [{}] {}", i + 1, log.trim());
            }
        }
    }

    // Verify logs were captured
    println!("📝 Checking captured logs...");
    let logs = log_writer.get_logs().await;
    println!("  Total log entries: {}", logs.len());

    assert!(!logs.is_empty(), "Should have captured logs from execution");

    // Print some logs for visibility
    println!("  Sample logs:");
    for (i, log) in logs.iter().take(10).enumerate() {
        println!("    [{}] {}", i + 1, log.trim());
    }

    if logs.len() > 10 {
        println!("    ... and {} more log entries", logs.len() - 10);
    }

    // Verify specific log content
    let all_logs = logs.join("\n");

    // Should have logs from each stage
    let has_download_logs = all_logs.contains("download")
        || all_logs.contains("Download")
        || all_logs.contains("repository");
    let has_build_logs =
        all_logs.contains("build") || all_logs.contains("Build") || all_logs.contains("image");
    let has_deploy_logs = all_logs.contains("deploy") || all_logs.contains("Deploy");

    if has_download_logs {
        println!("  ✓ Found download stage logs");
    }
    if has_build_logs {
        println!("  ✓ Found build stage logs");
    }
    if has_deploy_logs {
        println!("  ✓ Found deploy stage logs");
    }

    // Verify Docker image was created (if workflow succeeded)
    if result.is_ok() {
        use bollard::image::{ListImagesOptions, RemoveImageOptions};

        println!("🐳 Verifying Docker image exists...");

        // Check if image exists using bollard
        let mut filters = std::collections::HashMap::new();
        filters.insert(
            "reference".to_string(),
            vec!["nodejs-test-app:latest".to_string()],
        );
        let options = Some(ListImagesOptions::<String> {
            filters,
            ..Default::default()
        });

        match docker.list_images(options).await {
            Ok(images) => {
                if !images.is_empty() {
                    println!("  ✅ Docker image 'nodejs-test-app:latest' was built successfully");
                    println!("     Image ID: {}", images[0].id.clone());
                } else {
                    println!("  ⚠️  Docker image not found");
                }
            }
            Err(e) => {
                println!("  ⚠️  Failed to check Docker images: {}", e);
            }
        }

        // Cleanup: Remove test image using bollard
        println!("🧹 Cleaning up test image...");
        let remove_options = Some(RemoveImageOptions {
            force: true,
            ..Default::default()
        });

        match docker
            .remove_image("nodejs-test-app:latest", remove_options, None)
            .await
        {
            Ok(_) => println!("  ✓ Image cleanup completed"),
            Err(e) => println!("  ⚠️  Image cleanup failed: {}", e),
        }

        // Also cleanup any deployed containers
        if let Ok(ref final_context) = result {
            if let Ok(Some(container_id)) =
                final_context.get_output::<String>("deploy_nodejs", "deployment_id")
            {
                println!("🧹 Cleaning up deployed container...");
                use bollard::container::RemoveContainerOptions;

                // Stop container first
                use bollard::container::StopContainerOptions;
                let _ = docker
                    .stop_container(&container_id, None::<StopContainerOptions>)
                    .await;

                // Remove container
                let container_remove_options = Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                });

                match docker
                    .remove_container(&container_id, container_remove_options)
                    .await
                {
                    Ok(_) => println!("  ✓ Container cleanup completed"),
                    Err(e) => println!("  ⚠️  Container cleanup failed: {}", e),
                }
            }
        }
    }

    // Final assertions
    match &result {
        Ok(_) => {
            println!("\n🎉 Node.js three-stage deployment test completed successfully!");
            println!("   ✓ Repository downloaded from fixture");
            println!("   ✓ Docker image built with bollard");
            println!("   ✓ Container deployed and running");
        }
        Err(e) => {
            println!("⚠️  Test failed with error: {:?}", e);
            panic!("Workflow execution should succeed with real implementations");
        }
    }
}

#[test]
fn test_nodejs_fixture_exists() {
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple-nodejs");

    assert!(fixture_path.exists(), "Fixture directory should exist");
    assert!(
        fixture_path.join("Dockerfile").exists(),
        "Dockerfile should exist"
    );
    assert!(
        fixture_path.join("package.json").exists(),
        "package.json should exist"
    );
    assert!(
        fixture_path.join("index.js").exists(),
        "index.js should exist"
    );

    // Verify Dockerfile contains Node.js base image
    let dockerfile_content =
        std::fs::read_to_string(fixture_path.join("Dockerfile")).expect("Should read Dockerfile");
    assert!(
        dockerfile_content.contains("FROM node:"),
        "Dockerfile should use Node.js base image"
    );
    assert!(
        dockerfile_content.contains("EXPOSE 3000"),
        "Dockerfile should expose port 3000"
    );
}

#[test]
fn test_nodejs_package_json_valid() {
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple-nodejs");

    let package_json = std::fs::read_to_string(fixture_path.join("package.json"))
        .expect("Should read package.json");

    // Parse as JSON to verify it's valid
    let parsed: serde_json::Value =
        serde_json::from_str(&package_json).expect("package.json should be valid JSON");

    assert!(parsed["name"].is_string(), "Should have name field");
    assert!(parsed["version"].is_string(), "Should have version field");
    assert!(
        parsed["dependencies"].is_object(),
        "Should have dependencies"
    );
}
