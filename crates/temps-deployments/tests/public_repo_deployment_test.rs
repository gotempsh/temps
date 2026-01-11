//! Integration test for deploying a public GitHub repository
//!
//! This test demonstrates deploying a real public repository through the full pipeline:
//! 1. Create project with public git URL (no git provider connection needed)
//! 2. Create environment
//! 3. Trigger deployment
//! 4. Verify build and deployment succeed
//!
//! Uses a small public Next.js starter template for testing

#[cfg(test)]
mod public_repo_tests {
    use async_trait::async_trait;
    use bollard::Docker;
    use std::collections::HashMap;
    use std::sync::Arc;
    use temps_core::{
        LogWriter, WorkflowBuilder, WorkflowCancellationProvider, WorkflowError, WorkflowExecutor,
        WorkflowTask,
    };
    use temps_deployer::docker::DockerRuntime;
    use temps_deployer::{ContainerDeployer, ImageBuilder};
    use temps_deployments::jobs::{
        BuildImageJobBuilder, DeployImageJobBuilder, DeploymentTarget, DownloadRepoBuilder,
    };
    use temps_git::{GitProviderManagerError, GitProviderManagerTrait, RepositoryInfo};
    use tokio::sync::Mutex;

    /// Simple Git provider that clones from public URLs
    struct PublicGitProvider;

    #[async_trait]
    impl GitProviderManagerTrait for PublicGitProvider {
        async fn clone_repository(
            &self,
            _connection_id: i32,
            repo_owner: &str,
            repo_name: &str,
            target_dir: &std::path::Path,
            branch_or_ref: Option<&str>,
        ) -> Result<(), GitProviderManagerError> {
            // Build the public clone URL
            let clone_url = format!("https://github.com/{}/{}.git", repo_owner, repo_name);
            let branch = branch_or_ref.unwrap_or("main");

            println!(
                "📥 Cloning {} (branch: {}) to {:?}",
                clone_url, branch, target_dir
            );

            // Ensure target directory exists
            std::fs::create_dir_all(target_dir).map_err(|e| {
                GitProviderManagerError::CloneError(format!("Failed to create directory: {}", e))
            })?;

            // Use git command to clone
            let output = std::process::Command::new("git")
                .arg("clone")
                .arg("--depth=1")
                .arg("--branch")
                .arg(branch)
                .arg(&clone_url)
                .arg(target_dir)
                .output()
                .map_err(|e| {
                    GitProviderManagerError::CloneError(format!("Git clone failed: {}", e))
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(GitProviderManagerError::CloneError(format!(
                    "Git clone failed: {}",
                    stderr
                )));
            }

            println!("  ✓ Repository cloned successfully");
            Ok(())
        }

        async fn get_repository_info(
            &self,
            _connection_id: i32,
            repo_owner: &str,
            repo_name: &str,
        ) -> Result<RepositoryInfo, GitProviderManagerError> {
            Ok(RepositoryInfo {
                clone_url: format!("https://github.com/{}/{}.git", repo_owner, repo_name),
                default_branch: "main".to_string(),
                owner: repo_owner.to_string(),
                name: repo_name.to_string(),
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
                "Archive not available, use clone".to_string(),
            ))
        }
    }

    /// No-op cancellation provider for tests
    struct NoCancellationProvider;

    #[async_trait]
    impl WorkflowCancellationProvider for NoCancellationProvider {
        async fn is_cancelled(&self, _workflow_run_id: &str) -> Result<bool, WorkflowError> {
            Ok(false)
        }
    }

    /// Capturing LogWriter for test verification
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
            println!("  [LOG] {}", message.trim());
            self.logs.lock().await.push(message);
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            self.stage_id
        }
    }

    /// Test case for public repository deployment
    struct PublicRepoTestCase {
        /// Name of the test (for display)
        name: &'static str,
        /// GitHub repository owner
        repo_owner: &'static str,
        /// GitHub repository name
        repo_name: &'static str,
        /// Branch to clone
        branch: &'static str,
        /// Expected preset to be detected
        expected_preset: &'static str,
        /// Port the application runs on
        port: u16,
    }

    /// List of public repositories to test deployment with auto-detection
    const TEST_REPOSITORIES: &[PublicRepoTestCase] = &[
        PublicRepoTestCase {
            name: "Vite + React",
            repo_owner: "SafdarJamal",
            repo_name: "vite-template-react",
            branch: "main",
            expected_preset: "vite",
            port: 3000,
        },
        PublicRepoTestCase {
            name: "Next.js Blog (Tailwind)",
            repo_owner: "timlrx",
            repo_name: "tailwind-nextjs-starter-blog",
            branch: "main",
            expected_preset: "nextjs",
            port: 3000,
        },
        PublicRepoTestCase {
            name: "Next.js Commerce",
            repo_owner: "vercel",
            repo_name: "commerce",
            branch: "main",
            expected_preset: "nextjs",
            port: 3000,
        },
        PublicRepoTestCase {
            name: "Next.js App Router Playground",
            repo_owner: "vercel",
            repo_name: "app-playground",
            branch: "main",
            expected_preset: "nextjs",
            port: 3000,
        },
        PublicRepoTestCase {
            name: "Docusaurus v2",
            repo_owner: "chainlaunch",
            repo_name: "chainlaunch-docs",
            branch: "main",
            expected_preset: "docusaurus",
            port: 3000,
        },
        // Add more test cases here:
        // PublicRepoTestCase {
        //     name: "NestJS Starter",
        //     repo_owner: "nestjs",
        //     repo_name: "typescript-starter",
        //     branch: "master",
        //     expected_preset: "nestjs",
        //     port: 3000,
        // },
        // PublicRepoTestCase {
        //     name: "Angular Tour of Heroes",
        //     repo_owner: "johnpapa",
        //     repo_name: "angular-tour-of-heroes",
        //     branch: "main",
        //     expected_preset: "angular",
        //     port: 4200,
        // },
    ];

    #[tokio::test]
    async fn test_deploy_public_repositories() {
        // Enable verbose Docker build output
        // Set RUST_LOG to show debug logs from temps-deployer
        std::env::set_var("RUST_LOG", "temps_deployer=debug,temps_deployments=info");
        let _ = env_logger::try_init();

        // Allow filtering to a single test case via environment variable
        // Usage: TEST_REPO="vite" cargo test test_deploy_public_repositories
        //        TEST_REPO="nextjs" cargo test test_deploy_public_repositories
        //        TEST_REPO="docusaurus" cargo test test_deploy_public_repositories
        let filter = std::env::var("TEST_REPO").ok();

        let tests_to_run: Vec<usize> = if let Some(ref filter_name) = filter {
            println!("\n🔍 Filtering tests to match: '{}'", filter_name);
            TEST_REPOSITORIES
                .iter()
                .enumerate()
                .filter_map(|(idx, tc)| {
                    if tc.name.to_lowercase().contains(&filter_name.to_lowercase())
                        || tc
                            .repo_name
                            .to_lowercase()
                            .contains(&filter_name.to_lowercase())
                        || tc
                            .expected_preset
                            .to_lowercase()
                            .contains(&filter_name.to_lowercase())
                    {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            (0..TEST_REPOSITORIES.len()).collect()
        };

        if tests_to_run.is_empty() {
            panic!("No tests matched filter: {:?}", filter);
        }

        println!("\n📋 Running {} test(s):", tests_to_run.len());
        for &idx in &tests_to_run {
            let tc = &TEST_REPOSITORIES[idx];
            println!("   • {} ({}/{})", tc.name, tc.repo_owner, tc.repo_name);
        }
        println!();

        for &idx in &tests_to_run {
            let test_case = &TEST_REPOSITORIES[idx];
            println!("\n{}", "=".repeat(80));
            println!("🚀 Testing deployment: {}", test_case.name);
            println!(
                "   Repository: {}/{}",
                test_case.repo_owner, test_case.repo_name
            );
            println!("   Expected preset: {}", test_case.expected_preset);
            println!("{}\n", "=".repeat(80));

            if let Err(e) = run_deployment_test(test_case).await {
                panic!("Test failed for {}: {:?}", test_case.name, e);
            }

            println!("\n✅ Test passed: {}\n", test_case.name);
        }

        println!(
            "\n🎉 All {} repository deployment tests passed!",
            tests_to_run.len()
        );
    }

    /// Run a single deployment test for a public repository
    async fn run_deployment_test(test_case: &PublicRepoTestCase) -> Result<(), String> {
        println!("   This test will:");
        println!("   1. Clone a public repository from GitHub");
        println!("   2. AUTO-DETECT the project type from repository files");
        println!("   3. Build a Docker image using the detected preset");
        println!("   4. Deploy the container");

        // Check if Docker is available
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("⚠️  Docker not available, skipping test");
                return Ok(());
            }
        };

        if docker.ping().await.is_err() {
            println!("⚠️  Docker daemon not responding, skipping test");
            return Ok(());
        }

        // Check if git is available
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            println!("⚠️  Git not available, skipping test");
            return Ok(());
        }

        // Initialize services
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(PublicGitProvider);
        let docker_runtime = Arc::new(DockerRuntime::new(
            docker.clone(),
            true, // Enable BuildKit for faster builds and caching
            "temps-test".to_string(),
        ));

        // Ensure test network exists
        if let Err(e) = docker_runtime.ensure_network_exists().await {
            println!("⚠️  Failed to create network: {}", e);
            return Err(format!("Failed to create network: {}", e));
        }

        let image_builder: Arc<dyn ImageBuilder> = docker_runtime.clone();
        let container_deployer: Arc<dyn ContainerDeployer> = docker_runtime;

        // Generate unique identifiers for this test
        let test_id = format!("{}-{}", test_case.repo_name, chrono::Utc::now().timestamp());
        let image_tag = format!(
            "temps-test-{}:latest",
            test_case.repo_name.replace("/", "-")
        );
        let service_name = format!("temps-test-{}", test_case.repo_name);

        println!("\n📦 Stage 1: Download Public Repository");

        let download_job = DownloadRepoBuilder::new()
            .job_id("download_public_repo".to_string())
            .repo_owner(test_case.repo_owner.to_string())
            .repo_name(test_case.repo_name.to_string())
            .git_provider_connection_id(0)
            .branch_ref(test_case.branch.to_string())
            .build(git_manager)
            .map_err(|e| format!("Failed to create download job: {}", e))?;

        println!("\n🐳 Stage 2: Build Docker Image (with auto-detection)");

        let build_job = BuildImageJobBuilder::new()
            .job_id("build_public_repo".to_string())
            .download_job_id("download_public_repo".to_string())
            .image_tag(image_tag.clone())
            // NO PRESET - auto-detect from repository
            .build_args(vec![("NODE_ENV".to_string(), "production".to_string())])
            .build(image_builder)
            .map_err(|e| format!("Failed to create build job: {}", e))?;

        println!("\n🚢 Stage 3: Deploy Container");

        let mut env_vars = HashMap::new();
        env_vars.insert("NODE_ENV".to_string(), "production".to_string());
        env_vars.insert("PORT".to_string(), test_case.port.to_string());
        env_vars.insert("HOST".to_string(), "0.0.0.0".to_string());
        env_vars.insert("HOSTNAME".to_string(), "0.0.0.0".to_string());

        let deploy_job = DeployImageJobBuilder::new()
            .job_id("deploy_public_repo".to_string())
            .build_job_id("build_public_repo".to_string())
            .target(DeploymentTarget::Docker {
                registry_url: "local".to_string(),
                network: None,
            })
            .service_name(service_name.clone())
            .namespace("default".to_string())
            .port(test_case.port as u32)
            .replicas(1)
            .environment_variables(env_vars)
            .build(container_deployer)
            .map_err(|e| format!("Failed to create deploy job: {}", e))?;

        println!("\n✅ All jobs created successfully");
        println!("   - Download job: {}", download_job.job_id());
        println!("   - Build job: {}", build_job.job_id());
        println!("   - Deploy job: {}", deploy_job.job_id());

        // Create workflow
        println!("\n🔄 Building and executing workflow...");

        let log_writer = Arc::new(CapturingLogWriter::new(1));

        let workflow = WorkflowBuilder::new()
            .with_workflow_run_id(test_id.clone())
            .with_deployment_context(1, 1, 1)
            .with_log_writer(log_writer.clone())
            .with_var("repo_owner", test_case.repo_owner)
            .unwrap()
            .with_var("repo_name", test_case.repo_name)
            .unwrap()
            .with_var("image_tag", &image_tag)
            .unwrap()
            .with_job(Arc::new(download_job))
            .with_job(Arc::new(build_job))
            .with_job(Arc::new(deploy_job))
            .continue_on_failure(false)
            .with_max_parallel_jobs(1)
            .build()
            .map_err(|e| format!("Failed to build workflow: {}", e))?;

        let executor = WorkflowExecutor::new(None);
        let cancellation_provider = Arc::new(NoCancellationProvider);

        println!("\n⏳ Executing workflow (this may take a few minutes)...\n");

        let result = executor
            .execute_workflow(workflow, cancellation_provider)
            .await;

        // Check result
        match result {
            Ok(final_context) => {
                println!("\n✅ Workflow execution completed successfully!");

                // Verify the detected preset matches expected
                let logs = log_writer.get_logs().await;
                let detected_preset = logs
                    .iter()
                    .find(|log| log.contains("Detected project type:"))
                    .and_then(|log| {
                        log.split("Detected project type: ")
                            .nth(1)
                            .map(|s| s.trim().to_string())
                    });

                if let Some(detected) = detected_preset {
                    if detected != test_case.expected_preset {
                        return Err(format!(
                            "Preset mismatch: expected '{}', detected '{}'",
                            test_case.expected_preset, detected
                        ));
                    }
                    println!("  ✓ Detected preset: {} (as expected)", detected);
                } else {
                    println!("  ⚠️  Could not verify detected preset from logs");
                }

                // Verify outputs
                let repo_dir: Option<String> = final_context
                    .get_output("download_public_repo", "repo_dir")
                    .map_err(|e| format!("Failed to get repo_dir: {}", e))?;
                println!("  ✓ Downloaded to: {:?}", repo_dir);

                let built_image: Option<String> = final_context
                    .get_output("build_public_repo", "image_tag")
                    .map_err(|e| format!("Failed to get image_tag: {}", e))?;
                println!("  ✓ Built image: {:?}", built_image);

                let container_id: Option<String> = final_context
                    .get_output("deploy_public_repo", "deployment_id")
                    .map_err(|e| format!("Failed to get deployment_id: {}", e))?;
                println!("  ✓ Deployed container: {:?}", container_id);

                // Cleanup
                println!("\n🧹 Cleaning up test resources...");

                use bollard::query_parameters::{RemoveContainerOptions, StopContainerOptions};

                if let Some(cid) = container_id {
                    let _ = docker
                        .stop_container(&cid, None::<StopContainerOptions>)
                        .await;
                    let _ = docker
                        .remove_container(
                            &cid,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                }

                let _ = docker
                    .remove_image(
                        &image_tag,
                        Some(bollard::query_parameters::RemoveImageOptions {
                            force: true,
                            ..Default::default()
                        }),
                        None,
                    )
                    .await;

                println!("  ✓ Cleanup completed");

                Ok(())
            }
            Err(e) => {
                println!("\n❌ Workflow execution failed: {:?}", e);

                // Print logs for debugging
                let logs = log_writer.get_logs().await;
                println!("\n📋 Error logs ({} entries):", logs.len());
                for (i, log) in logs.iter().enumerate().take(50) {
                    println!("  [{}] {}", i + 1, log.trim());
                }

                Err(format!("Workflow execution failed: {:?}", e))
            }
        }
    }
}
