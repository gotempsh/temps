//! Workflow Planner
//!
//! Determines which jobs to create for a deployment based on project configuration

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde_json;
use std::sync::Arc;
use temps_entities::{deployment_jobs, deployments, environments, projects, types::JobStatus};
use tracing::{debug, info, warn};
use temps_logs::LogService;
#[derive(Debug, Clone)]
pub struct JobDefinition {
    pub job_id: String,
    pub job_type: String,
    pub name: String,
    pub description: Option<String>,
    pub dependencies: Vec<String>,
    pub job_config: Option<serde_json::Value>,
    /// If false, this job doesn't need to succeed for deployment to be marked as complete
    pub required_for_completion: bool,
}

/// Plans and creates workflow jobs based on project configuration
pub struct WorkflowPlanner {
    db: Arc<DatabaseConnection>,
    log_service: Arc<LogService>,
    external_service_manager: Option<Arc<temps_providers::ExternalServiceManager>>,
    config_service: Arc<temps_config::ConfigService>,
    dsn_service: Arc<temps_error_tracking::DSNService>,
}

impl WorkflowPlanner {
    pub fn new(
        db: Arc<DatabaseConnection>,
        log_service: Arc<LogService>,
        external_service_manager: Arc<temps_providers::ExternalServiceManager>,
        config_service: Arc<temps_config::ConfigService>,
        dsn_service: Arc<temps_error_tracking::DSNService>,
    ) -> Self {
        Self {
            db,
            log_service,
            external_service_manager: Some(external_service_manager),
            config_service,
            dsn_service,
        }
    }

    /// Gather all environment variables for a deployment
    /// This includes:
    /// 1. Environment variables from the env_vars table for the specific environment (via env_var_environments junction table)
    /// 2. Runtime environment variables from external services linked to the project
    /// 3. Sentry DSN environment variables (SENTRY_DSN and NEXT_PUBLIC_SENTRY_DSN) - auto-generated per project/environment
    async fn gather_environment_variables(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        use temps_entities::{env_vars, env_var_environments, project_services};
        use std::collections::HashMap;

        let mut env_vars_map = HashMap::new();

        // 1. Get environment variables for this project and environment
        // Query through the env_var_environments junction table to get all env vars
        // associated with this environment
        let env_var_ids: Vec<i32> = env_var_environments::Entity::find()
            .filter(env_var_environments::Column::EnvironmentId.eq(environment.id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|eve| eve.env_var_id)
            .collect();

        if !env_var_ids.is_empty() {
            let env_vars_list = env_vars::Entity::find()
                .filter(env_vars::Column::Id.is_in(env_var_ids))
                .filter(env_vars::Column::ProjectId.eq(project.id))
                .all(self.db.as_ref())
                .await?;

            for env_var in env_vars_list {
                env_vars_map.insert(env_var.key, env_var.value);
            }
        }

        debug!("📦 Loaded {} environment variables from env_vars table via env_var_environments", env_vars_map.len());

        // 2. Get runtime environment variables from external services
        // First, get all services linked to this project
        let project_services_list = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await?;

        debug!("🔌 Found {} external services linked to project {}", project_services_list.len(), project.id);

        // Get runtime environment variables from each external service
        if let Some(ref service_manager) = self.external_service_manager {
            for project_service in project_services_list {
                debug!("🔍 Fetching runtime env vars for service ID {}", project_service.service_id);

                match service_manager
                    .get_runtime_env_vars(
                        project_service.service_id,
                        project.slug.clone(),
                        environment.slug.clone(),
                    )
                    .await
                {
                    Ok(service_env_vars) => {
                        debug!(
                            "✅ Got {} env vars from service {}",
                            service_env_vars.len(),
                            project_service.service_id
                        );
                        // Merge service env vars into the main map
                        env_vars_map.extend(service_env_vars);
                    }
                    Err(e) => {
                        warn!(
                            "⚠️  Failed to get runtime env vars for service {}: {:?}",
                            project_service.service_id, e
                        );
                    }
                }
            }
        } else if !project_services_list.is_empty() {
            warn!(
                "⚠️  Project has {} external services but ExternalServiceManager is not available. \
                External service environment variables will NOT be included in deployment.",
                project_services_list.len()
            );
        }

        // 3. Get or create Sentry DSN for error tracking
        // Generate/fetch DSN for this project/environment combination
        // This ensures each environment has its own DSN for proper error isolation
        debug!("🔑 Fetching or generating Sentry DSN for project {} environment {}", project.id, environment.id);

        // Get base URL from config service for DSN generation
        match self.config_service.get_external_url_or_default().await {
            Ok(base_url) => {
                match self.dsn_service
                    .get_or_create_project_dsn(
                        project.id,
                        Some(environment.id),
                        None, // deployment_id is None - DSN is per environment, not per deployment
                        &base_url,
                    )
                    .await
                {
                    Ok(project_dsn) => {
                        debug!(
                            "✅ Got DSN for project {} environment {}: {}",
                            project.id, environment.id, project_dsn.dsn
                        );
                        // Add both SENTRY_DSN and NEXT_PUBLIC_SENTRY_DSN for compatibility with different frameworks
                        env_vars_map.insert("SENTRY_DSN".to_string(), project_dsn.dsn.clone());
                        env_vars_map.insert("NEXT_PUBLIC_SENTRY_DSN".to_string(), project_dsn.dsn);
                    }
                    Err(e) => {
                        warn!(
                            "⚠️  Failed to get or create DSN for project {} environment {}: {:?}. \
                            Sentry DSN environment variables will NOT be included.",
                            project.id, environment.id, e
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "⚠️  Failed to get external URL from config: {:?}. \
                    Sentry DSN environment variables will NOT be included.",
                    e
                );
            }
        }

        info!("✅ Gathered {} total environment variables for deployment", env_vars_map.len());
        Ok(env_vars_map)
    }

    /// Create all jobs for a deployment based on project configuration
    pub async fn create_deployment_jobs(
        &self,
        deployment_id: i32,
    ) -> anyhow::Result<Vec<deployment_jobs::Model>> {
        // Get deployment, project, and environment info
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Deployment not found"))?;

        let project = projects::Entity::find_by_id(deployment.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;

        let environment = environments::Entity::find_by_id(deployment.environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Environment not found"))?;

        info!(
            "📋 Planning workflow for deployment {} (project: {}, env: {})",
            deployment_id, project.name, environment.name
        );

        // Determine jobs based on project configuration and deployment
        let job_definitions = self.plan_jobs_for_project(&project, &environment, &deployment).await?;

        debug!(
            "🔧 Creating {} jobs for deployment {}",
            job_definitions.len(),
            deployment_id
        );

        // Create job records in database
        let mut created_jobs = Vec::new();
        for (order, job_def) in job_definitions.into_iter().enumerate() {
            // Create hierarchical log path: logs/{project_slug}/{env_slug}/{year}/{month}/{day}/{hour}/{minute}/deployment-{id}-job-{job_id}.log
            let now = chrono::Utc::now();
            let log_path = format!(
                "{}/{}/{}/{:02}/{:02}/{:02}/{:02}/deployment-{}-job-{}.log",
                project.slug,
                environment.slug,
                now.format("%Y"),
                now.format("%m"),
                now.format("%d"),
                now.format("%H"),
                now.format("%M"),
                deployment_id,
                job_def.job_id
            );
            let log_id = log_path.clone();
            self.log_service.create_log_path(&log_id).await?;

            // Merge required_for_completion into job_config
            let mut job_config = job_def.job_config.unwrap_or_else(|| serde_json::json!({}));
            if let Some(config_obj) = job_config.as_object_mut() {
                config_obj.insert(
                    "_required_for_completion".to_string(),
                    serde_json::Value::Bool(job_def.required_for_completion)
                );
            }

            let job_record = deployment_jobs::ActiveModel {
                deployment_id: Set(deployment_id),
                job_id: Set(job_def.job_id.clone()),
                job_type: Set(job_def.job_type.clone()),
                name: Set(job_def.name.clone()),
                description: Set(job_def.description.clone()),
                status: Set(JobStatus::Pending),
                log_id: Set(log_id),
                job_config: Set(Some(job_config)),
                dependencies: Set(if job_def.dependencies.is_empty() {
                    None
                } else {
                    Some(serde_json::to_value(job_def.dependencies)?)
                }),
                execution_order: Set(Some(order as i32)),
                ..Default::default()
            };

            let created_job = job_record.insert(self.db.as_ref()).await?;
            debug!(
                "✅ Created job: {} ({})",
                created_job.name, created_job.job_id
            );
            created_jobs.push(created_job);
        }

        info!(
            "🎯 Successfully created {} jobs for deployment {}",
            created_jobs.len(),
            deployment_id
        );
        Ok(created_jobs)
    }

    /// Plan jobs based on project configuration
    /// Uses the 3 generic jobs: DownloadRepoJob -> BuildImageJob -> DeployImageJob
    async fn plan_jobs_for_project(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
    ) -> anyhow::Result<Vec<JobDefinition>> {
        let mut jobs = Vec::new();

        debug!("🔍 Planning jobs for project: {}", project.name);

        // Gather environment variables for the deployment
        let env_vars = self.gather_environment_variables(project, environment).await?;
        debug!("📦 Gathered {} environment variables for deployment", env_vars.len());

        // Check if git info is available
        let has_git_info = project.repo_owner.is_some() && project.repo_name.is_some();

        // Job 1: Download repository (only if git info is available)
        if has_git_info {
            // Determine which branch/commit to use for this deployment
            // Priority: deployment.branch_ref > deployment.commit_sha > project.main_branch
            let branch_or_commit = deployment
                .branch_ref
                .as_ref()
                .or(deployment.commit_sha.as_ref())
                .unwrap_or(&project.main_branch);

            debug!(
                "📌 Using branch/commit for deployment: {}",
                branch_or_commit
            );

            jobs.push(JobDefinition {
                job_id: "download_repo".to_string(),
                job_type: "DownloadRepoJob".to_string(),
                name: "Download Repository".to_string(),
                description: Some("Download source code from git repository".to_string()),
                dependencies: vec![],
                job_config: Some(serde_json::json!({
                    "branch_ref": branch_or_commit,
                    "commit_sha": deployment.commit_sha,
                    "repo_owner": project.repo_owner,
                    "repo_name": project.repo_name,
                    "git_provider_connection_id": project.git_provider_connection_id,
                    "directory": project.directory
                })),
                required_for_completion: true, // Core deployment job
            });
        } else {
            debug!("⚠️  Skipping download_repo job - no git info available");
        }

        // Job 2: Build container image
        // The BuildImageJob will generate Dockerfile from preset if it doesn't exist
        // Depends on download_repo only if git info is available
        let build_dependencies = if has_git_info {
            vec!["download_repo".to_string()]
        } else {
            vec![]
        };

        // Convert environment variables to build args
        // This ensures env vars are available during the Docker build process
        let mut build_args_map = serde_json::Map::new();
        for (key, value) in &env_vars {
            build_args_map.insert(key.clone(), serde_json::Value::String(value.clone()));
        }

        jobs.push(JobDefinition {
            job_id: "build_image".to_string(),
            job_type: "BuildImageJob".to_string(),
            name: "Build Container Image".to_string(),
            description: Some("Build Docker image from source code".to_string()),
            dependencies: build_dependencies,
            job_config: Some(serde_json::json!({
                "dockerfile_path": "Dockerfile",
                "build_args": build_args_map
            })),
            required_for_completion: true, // Core deployment job
        });

        // Job 3: Deploy container
        jobs.push(JobDefinition {
            job_id: "deploy_container".to_string(),
            job_type: "DeployImageJob".to_string(),
            name: "Deploy Container".to_string(),
            description: Some("Deploy the built container image".to_string()),
            dependencies: vec!["build_image".to_string()],
            job_config: Some(serde_json::json!({
                "port": 3000,
                "replicas": 1,
                "environment_variables": env_vars
            })),
            required_for_completion: true, // Core deployment job
        });

        // Job 4: Mark deployment as complete
        // This synthetic job marks the deployment as "Completed" and updates environment routing
        // It acts as a barrier between core deployment jobs and optional post-deployment jobs
        jobs.push(JobDefinition {
            job_id: "mark_deployment_complete".to_string(),
            job_type: "MarkDeploymentCompleteJob".to_string(),
            name: "Mark Deployment Complete".to_string(),
            description: Some("Mark deployment as complete and update environment routing".to_string()),
            dependencies: vec!["deploy_container".to_string()],
            job_config: Some(serde_json::json!({
                "deployment_id": deployment.id
            })),
            required_for_completion: true, // Critical job - ensures deployment is marked complete
        });
        debug!("✅ Added mark_deployment_complete job as barrier between core and optional jobs");

        // Job 5: Configure cron jobs (only if git info is available)
        // This job reads .temps.yaml from the repository and configures cron jobs
        // It runs AFTER deployment is marked complete (via mark_deployment_complete job)
        // NOT required for deployment completion - if it fails, deployment still succeeds
        if has_git_info {
            jobs.push(JobDefinition {
                job_id: "configure_crons".to_string(),
                job_type: "ConfigureCronsJob".to_string(),
                name: "Configure Cron Jobs".to_string(),
                description: Some("Configure scheduled cron jobs from .temps.yaml".to_string()),
                // Depends on mark_deployment_complete - ensures deployment is live before configuring crons
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "project_id": project.id,
                    "environment_id": deployment.environment_id,
                    "download_job_id": "download_repo"
                })),
                required_for_completion: false, // Post-deployment job - not required for deployment success
            });
            debug!("✅ Added configure_crons job to workflow (runs after deployment is marked complete)");
        } else {
            debug!("⚠️  Skipping configure_crons job - no git info available");
        }

        // Job 6: Take screenshot (only if screenshots are enabled in config)
        // This runs in parallel with configure_crons AFTER deployment is marked complete
        // NOT required for deployment completion - if it fails, deployment still succeeds
        let screenshots_enabled = self.config_service.is_screenshots_enabled().await;
        if screenshots_enabled {
            jobs.push(JobDefinition {
                job_id: "take_screenshot".to_string(),
                job_type: "TakeScreenshotJob".to_string(),
                name: "Take Screenshot".to_string(),
                description: Some("Capture screenshot of deployed application".to_string()),
                // Depends on mark_deployment_complete - ensures deployment is LIVE before taking screenshot
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id
                })),
                required_for_completion: false, // Post-deployment job - not required for deployment success
            });
            debug!("✅ Added take_screenshot job to workflow (screenshot service will be injected by plugin system)");
        } else {
            debug!("⚠️  Skipping screenshot job - screenshots are disabled in config");
        }

        info!(
            "📋 Planned {} jobs for project {}",
            jobs.len(),
            project.name
        );
        Ok(jobs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::Set;
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::types::ProjectType;
    use temps_config::{ConfigService, ServerConfig};
    use std::path::PathBuf;

    fn create_test_config_service(db: Arc<DatabaseConnection>) -> Arc<ConfigService> {
        let server_config = Arc::new(ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            Some("127.0.0.1:8000".to_string()),
        ).unwrap());
        Arc::new(ConfigService::new(server_config, db))
    }

    fn create_test_dsn_service(db: Arc<DatabaseConnection>) -> Arc<temps_error_tracking::DSNService> {
        Arc::new(temps_error_tracking::DSNService::new(db))
    }

    fn create_test_external_service_manager(db: Arc<DatabaseConnection>) -> Arc<temps_providers::ExternalServiceManager> {
        let encryption_service = Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().ok().unwrap());
        Arc::new(temps_providers::ExternalServiceManager::new(db, encryption_service, docker))
    }

    async fn create_test_project(
        db: &DatabaseConnection,
        project_type: ProjectType,
        preset: Option<String>,
    ) -> Result<
        (projects::Model, environments::Model, deployments::Model),
        Box<dyn std::error::Error>,
    > {
        // Create project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_owner: Set(Some("test-owner".to_string())),
            repo_name: Set(Some("test-repo".to_string())),
            main_branch: Set("main".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(preset),
            directory: Set("/".to_string()),
            project_type: Set(project_type),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db).await?;

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(serde_json::json!([])),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db).await?;

        // Create deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(serde_json::json!({})),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db).await?;

        Ok((project, environment, deployment))
    }

    #[tokio::test]
    async fn test_generic_job_planning() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(db.clone(), log_service, external_service_manager, config_service, dsn_service);

        let (project, environment, deployment) =
            create_test_project(db.as_ref(), ProjectType::Server, Some("nextjs".to_string()))
                .await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Should create 5 jobs: download_repo, build_image, deploy_container, mark_deployment_complete, configure_crons
        // Screenshots may or may not be included depending on config
        assert!(jobs.len() >= 5, "Expected at least 5 jobs, got {}", jobs.len());

        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert!(job_ids.contains(&"download_repo".to_string()));
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
        assert!(job_ids.contains(&"mark_deployment_complete".to_string()));
        assert!(job_ids.contains(&"configure_crons".to_string()));

        // Check that all jobs are in pending state
        for job in &jobs {
            assert_eq!(job.status, JobStatus::Pending);
            assert!(job.log_id.contains(&format!("deployment-{}", deployment.id)));
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_project_without_git_info() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(db.clone(), log_service, external_service_manager, config_service, dsn_service);

        // Create project without git info
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_owner: Set(None), // No git info
            repo_name: Set(None),
            main_branch: Set("main".to_string()),
            git_provider_connection_id: Set(None),
            preset: Set(Some("nextjs".to_string())),
            directory: Set("/".to_string()),
            project_type: Set(ProjectType::Server),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(serde_json::json!([])),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(serde_json::json!({})),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Should succeed and create only build_image, deploy_container, and mark_deployment_complete jobs
        // (no download_repo or configure_crons since git info is missing)
        let jobs = planner.create_deployment_jobs(deployment.id).await?;
        assert!(jobs.len() >= 3, "Expected at least 3 jobs, got {}", jobs.len());

        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
        assert!(job_ids.contains(&"mark_deployment_complete".to_string()));
        // download_repo and configure_crons should NOT be present
        assert!(!job_ids.contains(&"download_repo".to_string()));
        assert!(!job_ids.contains(&"configure_crons".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_job_execution_order() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(db.clone(), log_service, external_service_manager, config_service, dsn_service);

        let (project, environment, deployment) =
            create_test_project(db.as_ref(), ProjectType::Server, Some("nextjs".to_string()))
                .await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Verify execution order is set correctly
        for (index, job) in jobs.iter().enumerate() {
            assert_eq!(job.execution_order, Some(index as i32));
        }

        // Verify correct dependency order: download_repo -> build_image -> deploy_container -> mark_deployment_complete
        let job_order: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        let download_index = job_order.iter().position(|x| x == "download_repo").unwrap();
        let build_index = job_order.iter().position(|x| x == "build_image").unwrap();
        let deploy_index = job_order.iter().position(|x| x == "deploy_container").unwrap();
        let mark_complete_index = job_order.iter().position(|x| x == "mark_deployment_complete").unwrap();

        assert!(download_index < build_index);
        assert!(build_index < deploy_index);
        assert!(deploy_index < mark_complete_index);

        Ok(())
    }

    #[tokio::test]
    async fn test_job_dependencies() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(db.clone(), log_service, external_service_manager, config_service, dsn_service);

        let (project, environment, deployment) =
            create_test_project(db.as_ref(), ProjectType::Server, Some("nextjs".to_string()))
                .await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Find specific jobs and check their dependencies
        let build_job = jobs.iter().find(|j| j.job_id == "build_image").unwrap();
        let deploy_job = jobs.iter().find(|j| j.job_id == "deploy_container").unwrap();

        // Check dependencies are stored correctly
        if let Some(build_deps) = &build_job.dependencies {
            let deps: Vec<String> = serde_json::from_value(build_deps.clone()).unwrap();
            assert!(deps.contains(&"download_repo".to_string()));
        }

        if let Some(deploy_deps) = &deploy_job.dependencies {
            let deps: Vec<String> = serde_json::from_value(deploy_deps.clone()).unwrap();
            assert!(deps.contains(&"build_image".to_string()));
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_job_configuration() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(db.clone(), log_service, external_service_manager, config_service, dsn_service);

        let (project, environment, deployment) =
            create_test_project(db.as_ref(), ProjectType::Server, Some("nextjs".to_string()))
                .await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Check that jobs have proper configuration
        let build_job = jobs.iter().find(|j| j.job_id == "build_image").unwrap();
        assert!(build_job.job_config.is_some());

        if let Some(config) = &build_job.job_config {
            let config_obj: serde_json::Value = config.clone();
            assert!(config_obj.get("dockerfile_path").is_some());
            assert!(config_obj.get("build_args").is_some());
        }

        let deploy_job = jobs.iter().find(|j| j.job_id == "deploy_container").unwrap();
        assert!(deploy_job.job_config.is_some());

        if let Some(config) = &deploy_job.job_config {
            let config_obj: serde_json::Value = config.clone();
            assert!(config_obj.get("port").is_some());
            assert!(config_obj.get("replicas").is_some());
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_log_id_format() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(db.clone(), log_service, external_service_manager, config_service, dsn_service);

        let (project, environment, deployment) =
            create_test_project(db.as_ref(), ProjectType::Server, Some("nextjs".to_string()))
                .await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Verify log_id format - should be hierarchical: {project_slug}/{env_slug}/{year}/{month}/{day}/{hour}/{minute}/deployment-{id}-job-{job_id}.log
        for job in &jobs {
            assert!(job.log_id.contains(&project.slug), "log_id should contain project slug");
            assert!(job.log_id.contains(&environment.slug), "log_id should contain environment slug");
            assert!(job.log_id.contains(&format!("deployment-{}-job-{}.log", deployment.id, job.job_id)),
                    "log_id should contain deployment-{}-job-{}.log", deployment.id, job.job_id);
        }

        Ok(())
    }
}
