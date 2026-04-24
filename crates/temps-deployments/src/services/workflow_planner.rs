//! Workflow Planner
//!
//! Determines which jobs to create for a deployment based on project configuration

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde_json;
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::{deployment_jobs, deployments, environments, projects, types::JobStatus};
use temps_logs::LogService;
use tracing::{debug, info};
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

use super::deployment_token_service::DeploymentTokenService;

/// Plans and creates workflow jobs based on project configuration
pub struct WorkflowPlanner {
    db: Arc<DatabaseConnection>,
    log_service: Arc<LogService>,
    external_service_manager: Arc<temps_providers::ExternalServiceManager>,
    config_service: Arc<temps_config::ConfigService>,
    dsn_service: Arc<temps_error_tracking::DSNService>,
    deployment_token_service: Arc<DeploymentTokenService>,
    encryption_service: Arc<EncryptionService>,
}

impl WorkflowPlanner {
    pub fn new(
        db: Arc<DatabaseConnection>,
        log_service: Arc<LogService>,
        external_service_manager: Arc<temps_providers::ExternalServiceManager>,
        config_service: Arc<temps_config::ConfigService>,
        dsn_service: Arc<temps_error_tracking::DSNService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        let deployment_token_service = Arc::new(DeploymentTokenService::new(
            db.clone(),
            encryption_service.clone(),
        ));
        Self {
            db,
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            deployment_token_service,
            encryption_service,
        }
    }

    /// Gather all environment variables for a deployment
    /// This includes:
    /// 1. Environment variables from the env_vars table for the specific environment (via env_var_environments junction table)
    /// 2. Runtime environment variables from external services linked to the project
    /// 3. Sentry DSN environment variables - auto-generated per project/environment:
    ///    - `SENTRY_DSN` is always added
    ///    - `NEXT_PUBLIC_SENTRY_DSN` is added when preset is Next.js
    ///    - `VITE_PUBLIC_SENTRY_DSN` is added when preset is Vite
    /// 4. Deployment token environment variables (TEMPS_API_URL and TEMPS_API_TOKEN) - for API access from deployed apps
    /// 5. Cron secret (`CRON_SECRET`) - derived from the deployment token, used to authenticate
    ///    cron job HTTP requests via `Authorization: Bearer <CRON_SECRET>` header
    /// 6. OpenTelemetry environment variables for automatic instrumentation:
    ///    - `OTEL_EXPORTER_OTLP_ENDPOINT` - OTLP endpoint URL
    ///    - `OTEL_EXPORTER_OTLP_PROTOCOL` - always `http/protobuf`
    ///    - `OTEL_EXPORTER_OTLP_HEADERS` - auth header with deployment token
    ///    - `OTEL_SERVICE_NAME` - project name
    ///    - `OTEL_SERVICE_VERSION` - commit SHA (when available)
    /// 7. `CRON_SECRET` - the deployment token value, so deployed apps can verify
    ///    that incoming cron requests are authentic
    ///
    /// IMPORTANT: If any external service fails to provide env vars, the entire deployment will fail
    /// with a meaningful error message. This prevents silent failures where containers would be
    /// missing critical configuration (e.g., database connection strings).
    async fn gather_environment_variables(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        use std::collections::HashMap;
        use temps_entities::{env_var_environments, env_vars, project_services};

        let mut env_vars_map = HashMap::new();

        // Add default HOST environment variable
        // This ensures containers bind to all network interfaces (0.0.0.0)
        // which is required for external access via port mapping
        // Can be overridden by user-defined environment variables
        env_vars_map.insert("HOST".to_string(), "0.0.0.0".to_string());

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
                let value = if env_var.is_encrypted {
                    self.encryption_service
                        .decrypt_string(&env_var.value)
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to decrypt environment variable '{}' (id={}): {}",
                                env_var.key,
                                env_var.id,
                                e
                            )
                        })?
                } else {
                    env_var.value
                };
                env_vars_map.insert(env_var.key, value);
            }
        }

        debug!(
            "📦 Loaded {} environment variables from env_vars table via env_var_environments",
            env_vars_map.len()
        );

        // 2. Get runtime environment variables from external services
        // First, get all services linked to this project
        let project_services_list = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await?;

        debug!(
            "🔌 Found {} external services linked to project {}",
            project_services_list.len(),
            project.id
        );

        // Track failed services to provide detailed error messages
        let mut failed_services: Vec<(i32, String)> = Vec::new();

        // Get runtime environment variables from each external service
        for project_service in project_services_list {
            debug!(
                "Fetching runtime env vars for service ID {} (project: {}, environment: {})",
                project_service.service_id, project.id, environment.id
            );

            match self
                .external_service_manager
                .get_runtime_env_vars(project_service.service_id, project.id, environment.id)
                .await
            {
                Ok(service_env_vars) => {
                    debug!(
                        "Got {} env vars from service {}: {}",
                        service_env_vars.len(),
                        project_service.service_id,
                        service_env_vars
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    // Merge service env vars into the main map
                    env_vars_map.extend(service_env_vars);
                }
                Err(e) => {
                    // Collect the error - we'll fail the entire deployment if any service fails
                    let error_msg = format!("{}", e);
                    failed_services.push((project_service.service_id, error_msg));
                    tracing::error!(
                        "Failed to get runtime env vars for service {}: {}",
                        project_service.service_id,
                        e
                    );
                }
            }
        }

        // CRITICAL: If any external service failed, fail the entire deployment
        // This prevents silent failures where containers would be missing critical environment variables
        if !failed_services.is_empty() {
            let failure_details = failed_services
                .iter()
                .map(|(service_id, error)| format!("  • Service ID {}: {}", service_id, error))
                .collect::<Vec<_>>()
                .join("\n");

            let error_message = format!(
                "Failed to gather environment variables from {} external service(s). \
                The deployment cannot proceed without all required external services configured:\n{}",
                failed_services.len(),
                failure_details
            );

            return Err(anyhow::anyhow!(error_message));
        }

        // 3. Get or create Sentry DSN for error tracking
        // Generate/fetch DSN for this project/environment combination
        // This ensures each environment has its own DSN for proper error isolation
        debug!(
            "🔑 Fetching or generating Sentry DSN for project {} environment {}",
            project.id, environment.id
        );

        // Get base URL from config service for DSN generation
        match self.config_service.get_external_url_or_default().await {
            Ok(base_url) => {
                match self
                    .dsn_service
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
                            "Got DSN for project {} environment {}: {}",
                            project.id, environment.id, project_dsn.dsn
                        );
                        // Always add SENTRY_DSN for server-side usage
                        env_vars_map.insert("SENTRY_DSN".to_string(), project_dsn.dsn.clone());

                        // Add framework-specific public DSN env var based on preset
                        match project.preset {
                            temps_entities::preset::Preset::NextJs => {
                                env_vars_map
                                    .insert("NEXT_PUBLIC_SENTRY_DSN".to_string(), project_dsn.dsn);
                            }
                            temps_entities::preset::Preset::Vite => {
                                env_vars_map
                                    .insert("VITE_PUBLIC_SENTRY_DSN".to_string(), project_dsn.dsn);
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        // Warn about Sentry DSN failure but don't fail the deployment
                        // Sentry is optional for monitoring, not required for app functionality
                        tracing::error!(
                            "Failed to get or create DSN for project {} environment {}: {}. \
                            Sentry DSN environment variables will NOT be included.",
                            project.id,
                            environment.id,
                            e
                        );
                    }
                }
            }
            Err(e) => {
                // Warn about external URL failure but don't fail the deployment
                // Sentry is optional for monitoring, not required for app functionality
                tracing::error!(
                    "Failed to get external URL from config: {}. \
                    Sentry DSN environment variables will NOT be included.",
                    e
                );
            }
        }

        // 4. Get or create deployment token for API access
        // This provides TEMPS_API_URL and TEMPS_API_TOKEN environment variables
        // allowing deployed applications to access Temps APIs for:
        // - Enriching visitor data
        // - Sending emails
        // - Other platform features
        debug!(
            "🔑 Getting or creating deployment token for project {} environment {}",
            project.id, environment.id
        );

        match self.config_service.get_external_url_or_default().await {
            Ok(base_url) => {
                // Set the API URL - this is always available
                env_vars_map.insert("TEMPS_API_URL".to_string(), format!("{}/api", base_url));

                // Get or create the deployment token
                match self
                    .deployment_token_service
                    .get_or_create_deployment_token(
                        project.id,
                        Some(environment.id),
                        Some(deployment.id),
                    )
                    .await
                {
                    Ok(token) => {
                        debug!(
                            "Got deployment token for project {} environment {} (prefix: {}...)",
                            project.id,
                            environment.id,
                            &token[..8.min(token.len())]
                        );
                        env_vars_map.insert("TEMPS_API_TOKEN".to_string(), token.clone());

                        // 5. CRON_SECRET - same token so the cron scheduler can send
                        // Authorization: Bearer <CRON_SECRET> and the deployed app can verify it
                        env_vars_map.insert("CRON_SECRET".to_string(), token);
                    }
                    Err(e) => {
                        // Warn about deployment token failure but don't fail the deployment
                        // Deployment tokens are optional for API access
                        tracing::warn!(
                            "Failed to get or create deployment token for project {} environment {}: {}. \
                            TEMPS_API_TOKEN environment variable will NOT be included.",
                            project.id,
                            environment.id,
                            e
                        );
                    }
                }
            }
            Err(e) => {
                // Warn about external URL failure but don't fail the deployment
                tracing::warn!(
                    "Failed to get external URL from config: {}. \
                    TEMPS_API_URL and TEMPS_API_TOKEN environment variables will NOT be included.",
                    e
                );
            }
        }

        // 5. OpenTelemetry environment variables for automatic instrumentation
        // Standard OTel SDK env vars so deployed apps can send traces/metrics/logs
        // without any manual configuration. Uses the same deployment token for auth.
        // See: https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/
        if let Some(api_url) = env_vars_map.get("TEMPS_API_URL").cloned() {
            // TEMPS_API_URL is "{base}/api", OTLP endpoint is "{base}/api/otel"
            env_vars_map.insert(
                "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                format!("{}/otel", api_url),
            );
            env_vars_map.insert(
                "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
                "http/protobuf".to_string(),
            );

            // Auth header using the deployment token (already in TEMPS_API_TOKEN)
            if let Some(token) = env_vars_map.get("TEMPS_API_TOKEN").cloned() {
                env_vars_map.insert(
                    "OTEL_EXPORTER_OTLP_HEADERS".to_string(),
                    format!("Authorization=Bearer {}", token),
                );
            }

            env_vars_map.insert("OTEL_SERVICE_NAME".to_string(), project.name.clone());

            // Use commit SHA as service version when available
            if let Some(ref commit_sha) = deployment.commit_sha {
                env_vars_map.insert("OTEL_SERVICE_VERSION".to_string(), commit_sha.clone());
            }

            debug!(
                "Set OTEL_EXPORTER_OTLP_ENDPOINT for project {} environment {}",
                project.id, environment.id
            );
        }

        info!(
            "Gathered {} total environment variables for deployment: {}",
            env_vars_map.len(),
            env_vars_map.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        Ok(env_vars_map)
    }

    /// Gathers secrets visible to this deployment, decrypted and keyed by name.
    /// Returns a HashMap ready to be serialized into the job config under the
    /// `secrets` field; the deployer mounts each entry as a file under
    /// `/run/secrets/<KEY>`.
    ///
    /// Scoping matches the `gather_environment_variables` model:
    ///   - Secrets with junction rows to this environment are included.
    ///   - Secrets with no junction rows are treated as project-wide.
    async fn gather_secrets(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        use std::collections::{HashMap, HashSet};
        use temps_entities::{secret_environments, secrets};

        let mut out: HashMap<String, String> = HashMap::new();

        // 1. All secrets for this project.
        let all_secrets = secrets::Entity::find()
            .filter(secrets::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await?;

        if all_secrets.is_empty() {
            return Ok(out);
        }

        // 2. Junction rows — which secrets are environment-scoped.
        let secret_ids: Vec<i32> = all_secrets.iter().map(|s| s.id).collect();
        let junctions = secret_environments::Entity::find()
            .filter(secret_environments::Column::SecretId.is_in(secret_ids))
            .all(self.db.as_ref())
            .await?;

        let mut bindings: HashMap<i32, HashSet<i32>> = HashMap::new();
        for j in junctions {
            bindings
                .entry(j.secret_id)
                .or_default()
                .insert(j.environment_id);
        }

        for secret in all_secrets {
            let applies = match bindings.get(&secret.id) {
                // No explicit bindings -> project-wide secret.
                None => true,
                // Environment-bound -> only if this environment is listed.
                Some(envs) => envs.contains(&environment.id),
            };
            if !applies {
                continue;
            }

            let plaintext = self
                .encryption_service
                .decrypt_string(&secret.value)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decrypt secret '{}' (id={}): {}",
                        secret.key,
                        secret.id,
                        e
                    )
                })?;
            out.insert(secret.key, plaintext);
        }

        info!(
            "Gathered {} secret file(s) for deployment to env {}",
            out.len(),
            environment.id
        );
        Ok(out)
    }

    /// Build remote environment variables by rewriting connection strings for cross-node access.
    ///
    /// When `private_address` is set in multi-node settings, this method:
    /// 1. Copies the local environment variables
    /// 2. For each linked external service, replaces Docker container names and internal ports
    ///    with the control plane's private address and host port
    /// 3. Rewrites TEMPS_API_URL if it references localhost/127.0.0.1
    ///
    /// Returns `None` if `private_address` is not configured (single-node mode).
    async fn build_remote_environment_variables(
        &self,
        project: &projects::Model,
        local_env_vars: &std::collections::HashMap<String, String>,
    ) -> Option<std::collections::HashMap<String, String>> {
        use temps_entities::project_services;

        // Get the private address for cross-node service connectivity.
        // Priority: multi_node.private_address > host from external_url
        let private_address = match self.config_service.get_settings().await {
            Ok(settings) => {
                if let Some(addr) = settings.multi_node.private_address {
                    addr
                } else {
                    // Fall back to extracting host from external URL
                    match self.config_service.get_external_url_or_default().await {
                        Ok(url) => {
                            if let Ok(parsed) = url::Url::parse(&url) {
                                match parsed.host_str() {
                                    Some(host)
                                        if host != "localhost"
                                            && host != "127.0.0.1"
                                            && host != "localho.st" =>
                                    {
                                        info!(
                                            "No private_address configured, falling back to external URL host: {}",
                                            host
                                        );
                                        host.to_string()
                                    }
                                    _ => return None,
                                }
                            } else {
                                return None;
                            }
                        }
                        Err(_) => return None,
                    }
                }
            }
            Err(_) => return None,
        };

        info!(
            "build_remote_environment_variables: resolved private_address={}",
            private_address
        );

        // Only build remote env vars if there are active worker nodes
        use temps_entities::nodes;
        let has_active_nodes = matches!(
            nodes::Entity::find()
                .filter(nodes::Column::Status.eq("active"))
                .one(self.db.as_ref())
                .await,
            Ok(Some(_))
        );
        if !has_active_nodes {
            return None;
        }

        let mut remote_vars = local_env_vars.clone();

        // Get all services linked to this project
        let project_services_list = match project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await
        {
            Ok(services) => services,
            Err(e) => {
                tracing::warn!(
                    "Failed to query project services for remote env var rewriting: {}",
                    e
                );
                return None;
            }
        };

        info!(
            "Building remote env vars for project {}: {} linked services, private_address={}",
            project.id,
            project_services_list.len(),
            private_address
        );

        // For each service, get its address mapping and do replacements
        for project_service in &project_services_list {
            match self
                .external_service_manager
                .get_service_effective_address(project_service.service_id)
                .await
            {
                Ok((container_name, internal_port, host_port)) => {
                    info!(
                        "Service {}: container_name={}, internal_port={}, host_port={} — rewriting to {}:{}",
                        project_service.service_id, container_name, internal_port, host_port,
                        private_address, host_port
                    );
                    let mut rewritten_count = 0;
                    for (key, value) in remote_vars.iter_mut() {
                        // Replace container_name:internal_port → private_address:host_port
                        if value.contains(&container_name) {
                            let old_value = value.clone();
                            *value = value
                                .replace(
                                    &format!("{}:{}", container_name, internal_port),
                                    &format!("{}:{}", private_address, host_port),
                                )
                                .replace(&container_name, &private_address);
                            info!("Rewrote {}={} -> {}", key, old_value, value);
                            rewritten_count += 1;
                        }
                    }
                    if rewritten_count == 0 {
                        tracing::warn!(
                            "Service {} container_name='{}' not found in any env var value",
                            project_service.service_id,
                            container_name
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to get effective address for service {} (skipping rewrite): {}",
                        project_service.service_id,
                        e
                    );
                }
            }
        }

        // Rewrite TEMPS_API_URL if it references localhost/127.0.0.1
        if let Some(api_url) = remote_vars.get("TEMPS_API_URL").cloned() {
            if api_url.contains("localhost") || api_url.contains("127.0.0.1") {
                let rewritten = api_url
                    .replace("localhost", &private_address)
                    .replace("127.0.0.1", &private_address);
                remote_vars.insert("TEMPS_API_URL".to_string(), rewritten.clone());

                // Also rewrite OTEL endpoint which is derived from TEMPS_API_URL
                if let Some(otel_url) = remote_vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").cloned() {
                    let rewritten_otel = otel_url
                        .replace("localhost", &private_address)
                        .replace("127.0.0.1", &private_address);
                    remote_vars.insert("OTEL_EXPORTER_OTLP_ENDPOINT".to_string(), rewritten_otel);
                }
            }
        }

        debug!(
            "Built remote environment variables with private_address={}",
            private_address
        );
        Some(remote_vars)
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
            "Planning workflow for deployment {} (project: {}, env: {})",
            deployment_id, project.name, environment.name
        );

        // Determine jobs based on project configuration and deployment
        let job_definitions = self
            .plan_jobs_for_project(&project, &environment, &deployment)
            .await?;

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
                    serde_json::Value::Bool(job_def.required_for_completion),
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
            debug!("Created job: {} ({})", created_job.name, created_job.job_id);
            created_jobs.push(created_job);
        }

        info!(
            "Successfully created {} jobs for deployment {}",
            created_jobs.len(),
            deployment_id
        );
        Ok(created_jobs)
    }

    /// Determine the fallback port configuration for the container
    ///
    /// This method resolves manual port overrides during job planning.
    /// The actual port used at deployment time is determined by inspecting the built image
    /// in DeployImageJob.resolve_container_port() with this priority:
    ///
    /// 1. Image EXPOSE directive (inspected after build - highest priority)
    /// 2. Environment-level exposed_port
    /// 3. Project-level exposed_port
    /// 4. Default: 3000
    ///
    /// Note: Image inspection happens in the deploy job (after build completes),
    /// not during planning, since the image doesn't exist yet at planning time.
    ///
    /// # Arguments
    /// * `environment` - Environment model with optional exposed_port
    /// * `project` - Project model with optional exposed_port
    /// * `image_name` - Unused (kept for API compatibility, inspection happens in deploy job)
    async fn resolve_exposed_port(
        &self,
        environment: &environments::Model,
        project: &projects::Model,
        _image_name: Option<&str>, // Unused - inspection happens in deploy job after build
    ) -> u16 {
        // 1. Check environment-level port override (from deployment_config)
        if let Some(ref deployment_config) = environment.deployment_config {
            if let Some(port) = deployment_config.exposed_port {
                debug!(
                    "Using environment-level port override: {} (environment: {})",
                    port, environment.name
                );
                return port as u16;
            }
        }

        // 2. Check project-level port override (from deployment_config)
        if let Some(ref deployment_config) = project.deployment_config {
            if let Some(port) = deployment_config.exposed_port {
                debug!(
                    "Using project-level port override: {} (project: {})",
                    port, project.name
                );
                return port as u16;
            }
        }

        // 3. Default to 3000
        // Note: Image EXPOSE directive will be checked in DeployImageJob after build completes
        debug!("Using default port: 3000 (will be overridden by image EXPOSE if present)");
        3000
    }

    /// Determine the effective deployment source type for this deployment
    ///
    /// Checks deployment metadata first (for all project types), then falls back
    /// to the project's configured source type:
    /// 1. Explicit `deployment_source_type` in metadata (set by remote deployment handlers)
    /// 2. Presence of `external_image_ref` in metadata -> DockerImage
    /// 3. Presence of `static_bundle_path` in metadata -> StaticFiles
    /// 4. Project's own source type (for non-flexible projects)
    /// 5. Has git info (repo_owner, repo_name) -> Git (for flexible/Manual projects)
    /// 6. Fallback to Manual (will fail at job planning time)
    fn determine_deployment_source_type(
        &self,
        project: &projects::Model,
        deployment: &deployments::Model,
    ) -> temps_entities::source_type::SourceType {
        use temps_entities::source_type::SourceType;

        // Check deployment metadata first (applies to ALL project types)
        // This allows any project to deploy via Docker image or static bundle
        // through the remote deployment API, regardless of its configured source type
        if let Some(metadata) = &deployment.metadata {
            // 1. Explicit deployment_source_type (set by remote deployment handlers)
            if let Some(dtype) = &metadata.deployment_source_type {
                debug!(
                    "Using explicit deployment_source_type from metadata: {:?}",
                    dtype
                );
                return *dtype;
            }

            // 2. Infer from external image reference
            if metadata.external_image_ref.is_some() {
                debug!("Inferred DockerImage deployment from external_image_ref in metadata");
                return SourceType::DockerImage;
            }

            // 3. Infer from static bundle path
            if metadata.static_bundle_path.is_some() {
                debug!("Inferred StaticFiles deployment from static_bundle_path in metadata");
                return SourceType::StaticFiles;
            }
        }

        // 4. Non-flexible projects: use project source type
        if !project.source_type.is_flexible() {
            return project.source_type;
        }

        // 5. Flexible/Manual projects: check if project has git info
        if !project.repo_owner.is_empty() && !project.repo_name.is_empty() {
            debug!("Using Git deployment for Manual project with git info");
            return SourceType::Git;
        }

        // 6. No deployment method could be determined
        // Keep as Manual - will cause a meaningful error in plan_jobs_for_project
        debug!("Could not determine deployment method for Manual project");
        SourceType::Manual
    }

    /// Plan jobs based on project configuration and source type
    ///
    /// Job workflows by source type:
    /// - Git: DownloadRepoJob -> BuildImageJob -> DeployImageJob (or DeployStaticJob)
    /// - DockerImage: PullExternalImageJob -> DeployImageJob
    /// - StaticFiles: DeployStaticBundleJob
    /// - Manual: Determined at runtime based on deployment metadata
    async fn plan_jobs_for_project(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
    ) -> anyhow::Result<Vec<JobDefinition>> {
        use temps_entities::source_type::SourceType;

        // Determine the effective source type for this deployment
        // For Manual projects, this inspects the deployment metadata
        let effective_source_type = self.determine_deployment_source_type(project, deployment);

        debug!(
            "Planning jobs for project: {} (project_source_type: {:?}, effective_source_type: {:?})",
            project.name, project.source_type, effective_source_type
        );

        // Gather environment variables for the deployment
        let mut env_vars = self
            .gather_environment_variables(project, environment, deployment)
            .await?;

        // Inject TEMPS_ASSET_PREFIX for stale-chunk prevention.
        // Frameworks can use this to namespace static assets per deployment:
        //   Next.js: assetPrefix: process.env.NEXT_PUBLIC_TEMPS_ASSET_PREFIX || ''
        //   Vite:    base: process.env.TEMPS_ASSET_PREFIX || '/'
        // The value is the deployment slug, which is unique and URL-safe.
        let asset_prefix = format!("/_temps/assets/{}", deployment.slug);
        env_vars.insert("TEMPS_ASSET_PREFIX".to_string(), asset_prefix.clone());
        // NEXT_PUBLIC_ prefix makes it available at build time in Next.js client bundles
        if project.preset == temps_entities::preset::Preset::NextJs {
            env_vars.insert("NEXT_PUBLIC_TEMPS_ASSET_PREFIX".to_string(), asset_prefix);
        }

        debug!(
            "📦 Gathered {} environment variables for deployment",
            env_vars.len()
        );

        // Build remote environment variables (connection strings rewritten for worker nodes)
        let remote_env_vars = self
            .build_remote_environment_variables(project, &env_vars)
            .await;
        if remote_env_vars.is_some() {
            debug!("📦 Built remote environment variables for cross-node deployments");
        }

        // Gather secrets — decrypted plaintext values, mounted as files at
        // /run/secrets/<KEY> by the deployer. Intentionally NOT merged into
        // env_vars so they don't appear in `docker inspect` or build args.
        let secrets = self.gather_secrets(project, environment).await?;

        // Docker Compose preset uses its own deployment path
        if project.preset == temps_entities::preset::Preset::DockerCompose {
            return self
                .plan_compose_deployment(project, environment, deployment, env_vars)
                .await;
        }

        // Route to appropriate job planning based on effective source type
        match effective_source_type {
            SourceType::DockerImage => {
                self.plan_docker_image_deployment(
                    project,
                    environment,
                    deployment,
                    env_vars,
                    remote_env_vars,
                    secrets,
                )
                .await
            }
            SourceType::StaticFiles => {
                self.plan_static_bundle_deployment(project, environment, deployment, env_vars)
                    .await
            }
            SourceType::Git => {
                self.plan_git_deployment(
                    project,
                    environment,
                    deployment,
                    env_vars,
                    remote_env_vars,
                    secrets,
                )
                .await
            }
            SourceType::Manual => {
                // Manual projects without explicit deployment method
                // This happens when someone tries to deploy a Manual project without specifying
                // how to deploy (no image, no bundle, no git info)
                Err(anyhow::anyhow!(
                    "Cannot determine deployment method for Manual project '{}'. \
                    Please deploy using one of the following methods:\n\
                    - Docker image: POST /projects/{}/environments/{}/deploy/image\n\
                    - Static files: POST /projects/{}/environments/{}/deploy/static\n\
                    - Configure git repository in project settings for git-based deployments",
                    project.name,
                    project.id,
                    deployment.environment_id,
                    project.id,
                    deployment.environment_id
                ))
            }
        }
    }

    /// Plan jobs for Git-based deployment (traditional workflow)
    /// DownloadRepoJob -> BuildImageJob -> DeployImageJob (or DeployStaticJob)
    async fn plan_git_deployment(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        mut env_vars: std::collections::HashMap<String, String>,
        remote_env_vars: Option<std::collections::HashMap<String, String>>,
        secrets: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Vec<JobDefinition>> {
        let mut jobs = Vec::new();

        // Inject SENTRY_RELEASE so the SDK tags events with the correct release version.
        // This must match the release used for source map uploads.
        if let Some(ref commit_sha) = deployment.commit_sha {
            env_vars
                .entry("SENTRY_RELEASE".to_string())
                .or_insert_with(|| commit_sha.clone());
        }

        // Inject Sentry source map upload env vars so `@sentry/nextjs` (and other Sentry
        // build plugins) automatically upload source maps to Temps during the build.
        // This makes migration from Sentry zero-config — users don't need to change anything.
        //
        // Docker builds use networkmode "host" (BuildKit) so the container can reach the
        // Temps server. Auth uses deployment tokens (dt_*) validated via the Sentry-compat API.
        if let Some(api_url) = env_vars.get("TEMPS_API_URL").cloned() {
            let sentry_url = api_url.trim_end_matches("/api").to_string();
            env_vars
                .entry("SENTRY_URL".to_string())
                .or_insert(sentry_url);
        }
        if let Some(api_token) = env_vars.get("TEMPS_API_TOKEN").cloned() {
            env_vars
                .entry("SENTRY_AUTH_TOKEN".to_string())
                .or_insert(api_token);
        }
        env_vars
            .entry("SENTRY_ORG".to_string())
            .or_insert_with(|| "default".to_string());
        env_vars
            .entry("SENTRY_PROJECT".to_string())
            .or_insert_with(|| project.slug.clone());

        // Check if git info is available
        let has_git_info = !project.repo_owner.is_empty() && !project.repo_name.is_empty();

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
                    "tag_ref": deployment.tag_ref,
                    "commit_sha": deployment.commit_sha,
                    "repo_owner": project.repo_owner,
                    "repo_name": project.repo_name,
                    "git_provider_connection_id": project.git_provider_connection_id,
                    "git_url": project.git_url,
                    "is_public_repo": project.is_public_repo,
                    "directory": project.directory
                })),
                required_for_completion: true, // Core deployment job
            });
        } else {
            debug!("Skipping download_repo job - no git info available");
        }

        // Check if this preset supports static deployment using temps-presets
        // Get the preset instance and check if it has a static output directory
        let preset_instance = temps_presets::get_preset_by_slug(project.preset.as_str());
        let static_output_dir = preset_instance.as_ref().and_then(|p| p.static_output_dir());

        debug!(
            "Preset {} static output directory: {:?}",
            project.preset, static_output_dir
        );

        // Job 2: Build container image (skip for static deployments)
        // The BuildImageJob will generate Dockerfile from preset if it doesn't exist
        // Depends on download_repo only if git info is available
        let build_dependencies = if has_git_info {
            vec!["download_repo".to_string()]
        } else {
            vec![]
        };

        // Determine deployment strategy: Static or Container
        let deploy_job_id = if let Some(output_dir) = static_output_dir {
            // Static deployment path: BuildImageJob + DeployStaticJob
            debug!("📦 Using static deployment for preset {}", project.preset);
            debug!("📂 Static output directory: {}", output_dir);

            // Convert environment variables to build args
            let mut build_args_map = serde_json::Map::new();
            for (key, value) in &env_vars {
                build_args_map.insert(key.clone(), serde_json::Value::String(value.clone()));
            }

            // Parse preset_config if present (for Dockerfile preset)
            let mut dockerfile_path = "Dockerfile".to_string();
            let mut build_context = project.directory.clone();

            if let Some(temps_entities::preset::PresetConfig::Dockerfile(dockerfile_config)) =
                &project.preset_config
            {
                if let Some(custom_dockerfile) = &dockerfile_config.dockerfile_path {
                    dockerfile_path = custom_dockerfile.clone();
                }
                if let Some(custom_context) = &dockerfile_config.build_context {
                    build_context = custom_context.clone();
                }
            }

            // Job 2: Build image (for static deployments, this builds the static files inside container)
            jobs.push(JobDefinition {
                job_id: "build_image".to_string(),
                job_type: "BuildImageJob".to_string(),
                name: "Build Container Image".to_string(),
                description: Some("Build Docker image and compile static files".to_string()),
                dependencies: build_dependencies.clone(),
                job_config: Some(serde_json::json!({
                    "dockerfile_path": dockerfile_path,
                    "build_args": build_args_map,
                    "build_context": build_context
                })),
                required_for_completion: true,
            });

            // Job 3: Deploy static files (extracts from built image and deploys to filesystem)
            jobs.push(JobDefinition {
                job_id: "deploy_static".to_string(),
                job_type: "DeployStaticJob".to_string(),
                name: "Deploy Static Files".to_string(),
                description: Some("Extract and deploy static files from container".to_string()),
                dependencies: vec!["build_image".to_string()],
                job_config: Some(serde_json::json!({
                    "static_output_dir": output_dir,  // Path inside container (e.g., "/app/dist")
                    "project_slug": project.slug,
                    "environment_slug": environment.slug,
                    "deployment_slug": deployment.slug
                })),
                required_for_completion: true,
            });

            // Persist static assets to CAS for stale-chunk fallback
            let search_paths_for_static_chunks: Vec<String> = match project.preset {
                temps_entities::preset::Preset::Vite => vec!["dist/assets".to_string()],
                _ => vec!["dist".to_string(), "build/static".to_string()],
            };

            let path_rewrites_for_static_chunks: Vec<(String, String)> = match project.preset {
                temps_entities::preset::Preset::Vite => {
                    vec![("dist/".to_string(), String::new())]
                }
                _ => vec![
                    ("dist/".to_string(), String::new()),
                    ("build/".to_string(), String::new()),
                ],
            };

            jobs.push(JobDefinition {
                job_id: "persist_static_assets".to_string(),
                job_type: "PersistStaticAssetsJob".to_string(),
                name: "Persist Static Assets".to_string(),
                description: Some(
                    "Extract and deduplicate static assets for stale-chunk fallback".to_string(),
                ),
                dependencies: vec!["build_image".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id,
                    "deployment_slug": deployment.slug,
                    "project_id": project.id,
                    "environment_id": deployment.environment_id,
                    "search_paths": search_paths_for_static_chunks,
                    "path_rewrites": path_rewrites_for_static_chunks,
                })),
                required_for_completion: false,
            });

            "deploy_static".to_string()
        } else {
            // Container deployment path: BuildImageJob + DeployImageJob
            debug!(
                "🐳 Using container deployment for preset {}",
                project.preset
            );

            // Convert environment variables to build args
            let mut build_args_map = serde_json::Map::new();
            for (key, value) in &env_vars {
                build_args_map.insert(key.clone(), serde_json::Value::String(value.clone()));
            }

            // Parse preset_config if present (for Dockerfile preset)
            let mut dockerfile_path = "Dockerfile".to_string();
            let mut build_context = project.directory.clone();

            if let Some(temps_entities::preset::PresetConfig::Dockerfile(dockerfile_config)) =
                &project.preset_config
            {
                if let Some(custom_dockerfile) = &dockerfile_config.dockerfile_path {
                    dockerfile_path = custom_dockerfile.clone();
                }
                if let Some(custom_context) = &dockerfile_config.build_context {
                    build_context = custom_context.clone();
                }
            }

            jobs.push(JobDefinition {
                job_id: "build_image".to_string(),
                job_type: "BuildImageJob".to_string(),
                name: "Build Container Image".to_string(),
                description: Some("Build Docker image from source code".to_string()),
                dependencies: build_dependencies.clone(),
                job_config: Some(serde_json::json!({
                    "dockerfile_path": dockerfile_path,
                    "build_args": build_args_map,
                    "build_context": build_context
                })),
                required_for_completion: true,
            });

            // Deploy container
            let image_name = format!("temps-{}:{}", project.slug, deployment.id);
            let exposed_port = self
                .resolve_exposed_port(environment, project, Some(&image_name))
                .await;

            debug!(
                "📡 Container will expose port {} (image: {})",
                exposed_port, image_name
            );

            let mut deploy_env_vars = env_vars.clone();
            deploy_env_vars.insert("PORT".to_string(), exposed_port.to_string());

            let remote_deploy_env_vars = remote_env_vars.as_ref().map(|rv| {
                let mut remote = rv.clone();
                remote.insert("PORT".to_string(), exposed_port.to_string());
                remote
            });

            let replicas = environment
                .deployment_config
                .as_ref()
                .map(|c| c.replicas)
                .or_else(|| project.deployment_config.as_ref().map(|c| c.replicas))
                .unwrap_or(1);

            debug!("🔢 Planning deployment with {} replicas", replicas);

            let mut job_config = serde_json::json!({
                "port": exposed_port,
                "replicas": replicas,
                "environment_variables": deploy_env_vars,
                "image_name": image_name
            });
            if let Some(ref remote_vars) = remote_deploy_env_vars {
                info!(
                    "Storing remote_environment_variables in job config: POSTGRES_HOST={:?}, POSTGRES_URL={:?}",
                    remote_vars.get("POSTGRES_HOST"),
                    remote_vars.get("POSTGRES_URL").map(|u| if u.len() > 60 { format!("{}...", &u[..60]) } else { u.clone() })
                );
                job_config["remote_environment_variables"] =
                    serde_json::to_value(remote_vars).unwrap_or_default();
            } else {
                info!("No remote_environment_variables to store (single-node mode or no active nodes)");
            }
            if !secrets.is_empty() {
                info!(
                    "Storing {} secret file(s) in deploy job config",
                    secrets.len()
                );
                job_config["secrets"] = serde_json::to_value(&secrets).unwrap_or_default();
            }

            jobs.push(JobDefinition {
                job_id: "deploy_container".to_string(),
                job_type: "DeployImageJob".to_string(),
                name: "Deploy Container".to_string(),
                description: Some("Deploy the built container image".to_string()),
                dependencies: vec!["build_image".to_string()],
                job_config: Some(job_config),
                required_for_completion: true,
            });

            // Persist static assets to CAS for stale-chunk fallback.
            // Only for frontend presets that produce hashed static assets.
            #[allow(clippy::type_complexity)]
            let static_asset_config: Option<(Vec<String>, Vec<(String, String)>)> =
                match project.preset {
                    temps_entities::preset::Preset::NextJs => Some((
                        vec![".next/static".to_string()],
                        vec![(".next".to_string(), "_next".to_string())],
                    )),
                    temps_entities::preset::Preset::Vite
                    | temps_entities::preset::Preset::Rsbuild => Some((
                        vec!["dist/assets".to_string()],
                        vec![("dist/".to_string(), String::new())],
                    )),
                    temps_entities::preset::Preset::Nuxt => Some((
                        vec![".output/public/_nuxt".to_string()],
                        vec![(".output/public/".to_string(), String::new())],
                    )),
                    temps_entities::preset::Preset::Remix
                    | temps_entities::preset::Preset::Astro
                    | temps_entities::preset::Preset::SvelteKit
                    | temps_entities::preset::Preset::SolidStart => Some((
                        vec!["build/static".to_string(), "dist/assets".to_string()],
                        vec![
                            ("build/".to_string(), String::new()),
                            ("dist/".to_string(), String::new()),
                        ],
                    )),
                    // Dockerfile and backend presets don't produce predictable static assets.
                    // Users who want stale-chunk fallback should pick a frontend preset.
                    _ => None,
                };

            if let Some((search_paths, path_rewrites)) = static_asset_config {
                jobs.push(JobDefinition {
                    job_id: "persist_static_assets".to_string(),
                    job_type: "PersistStaticAssetsJob".to_string(),
                    name: "Persist Static Assets".to_string(),
                    description: Some(
                        "Extract and deduplicate static assets for stale-chunk fallback"
                            .to_string(),
                    ),
                    dependencies: vec!["build_image".to_string()],
                    job_config: Some(serde_json::json!({
                        "deployment_id": deployment.id,
                        "deployment_slug": deployment.slug,
                        "project_id": project.id,
                        "environment_id": deployment.environment_id,
                        "search_paths": search_paths,
                        "path_rewrites": path_rewrites,
                    })),
                    required_for_completion: false,
                });
            }

            "deploy_container".to_string()
        };

        // mark_deployment_complete depends only on the deploy job
        let complete_dependencies = vec![deploy_job_id];

        // Mark deployment as complete
        jobs.push(JobDefinition {
            job_id: "mark_deployment_complete".to_string(),
            job_type: "MarkDeploymentCompleteJob".to_string(),
            name: "Mark Deployment Complete".to_string(),
            description: Some(
                "Mark deployment as complete and update environment routing".to_string(),
            ),
            dependencies: complete_dependencies,
            job_config: Some(serde_json::json!({
                "deployment_id": deployment.id
            })),
            required_for_completion: true, // Critical job - ensures deployment is marked complete
        });
        debug!("Added mark_deployment_complete job as barrier between core and optional jobs");

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
            debug!(
                "Added configure_crons job to workflow (runs after deployment is marked complete)"
            );

            // Job: Sync agent definitions from .temps/agents/*.yaml
            // Runs in parallel with configure_crons, after deployment is complete
            jobs.push(JobDefinition {
                job_id: "configure_agents".to_string(),
                job_type: "ConfigureAgentsJob".to_string(),
                name: "Configure Agents".to_string(),
                description: Some("Sync agent definitions from .temps/agents/*.yaml".to_string()),
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "project_id": project.id,
                    "download_job_id": "download_repo"
                })),
                required_for_completion: false,
            });
            debug!(
                "Added configure_agents job to workflow (runs after deployment is marked complete)"
            );
        } else {
            debug!("Skipping configure_crons job - no git info available");
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
            debug!("Added take_screenshot job to workflow (screenshot service will be injected by plugin system)");
        } else {
            debug!("Skipping screenshot job - screenshots are disabled in config");
        }

        // Job 7: Scan for vulnerabilities (only if git info is available)
        // This runs in parallel with other post-deployment jobs AFTER deployment is marked complete
        // NOT required for deployment completion - if it fails, deployment still succeeds
        if has_git_info {
            jobs.push(JobDefinition {
                job_id: "scan_vulnerabilities".to_string(),
                job_type: "ScanVulnerabilitiesJob".to_string(),
                name: "Scan Vulnerabilities".to_string(),
                description: Some("Scan Docker image for security vulnerabilities".to_string()),
                // Depends on mark_deployment_complete - ensures deployment is LIVE before scanning
                // The image is available from build_image (stored in registry) but we scan after deployment
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id,
                    "project_id": project.id,
                    "environment_id": deployment.environment_id,
                    "branch": deployment.branch_ref,
                    "commit_hash": deployment.commit_sha,
                    "download_job_id": "download_repo",
                    "build_job_id": "build_image"
                })),
                required_for_completion: false, // Post-deployment job - not required for deployment success
            });
            debug!(
                "Added scan_vulnerabilities job to workflow (runs after deployment is marked complete)"
            );
        } else {
            debug!("Skipping vulnerability scan job - no git info available");
        }

        // Job 8: Capture source maps (only for JS-based presets with git info)
        // Extracts .map files from the built image for error symbolication
        if has_git_info {
            // Search paths are relative to the image's WORKDIR (detected at runtime).
            // The CaptureSourceMapsJob inspects the image to find the WORKDIR and
            // prepends it to these relative paths.
            let search_paths = match project.preset {
                temps_entities::preset::Preset::NextJs => {
                    vec![".next/static".to_string(), ".next/server".to_string()]
                }
                temps_entities::preset::Preset::Vite => vec!["dist/assets".to_string()],
                _ => vec!["dist".to_string(), "build/static".to_string()],
            };

            // Path rewrites: map container paths to browser-visible paths.
            // Next.js serves .next/static as /_next/static in the browser,
            // so we rewrite .next -> _next to match stack trace filenames.
            let path_rewrites: Vec<(String, String)> = match project.preset {
                temps_entities::preset::Preset::NextJs => {
                    vec![(".next".to_string(), "_next".to_string())]
                }
                _ => vec![],
            };

            // Use commit SHA as release version (matches SENTRY_RELEASE env var)
            let release = deployment
                .commit_sha
                .clone()
                .unwrap_or_else(|| format!("deploy-{}", deployment.id));

            jobs.push(JobDefinition {
                job_id: "capture_source_maps".to_string(),
                job_type: "CaptureSourceMapsJob".to_string(),
                name: "Capture Source Maps".to_string(),
                description: Some(
                    "Extract source maps from build output for error symbolication".to_string(),
                ),
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id,
                    "project_id": project.id,
                    "release": release,
                    "build_job_id": "build_image",
                    "search_paths": search_paths,
                    "path_rewrites": path_rewrites,
                })),
                required_for_completion: false,
            });
            debug!(
                "Added capture_source_maps job to workflow (release: {})",
                release
            );
        }

        info!(
            "Planned {} jobs for Git-based project {}",
            jobs.len(),
            project.name
        );
        Ok(jobs)
    }

    /// Plan jobs for Docker Compose deployment
    /// DownloadRepoJob (if git) -> DeployComposeJob -> MarkDeploymentCompleteJob
    async fn plan_compose_deployment(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        env_vars: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Vec<JobDefinition>> {
        let mut jobs = Vec::new();

        // Check if git info is available
        let has_git_info = !project.repo_owner.is_empty() && !project.repo_name.is_empty();

        // Job 1: Download repository (only if git-backed)
        if has_git_info {
            let branch_or_commit = deployment
                .branch_ref
                .as_ref()
                .or(deployment.commit_sha.as_ref())
                .unwrap_or(&project.main_branch);

            jobs.push(JobDefinition {
                job_id: "download_repo".to_string(),
                job_type: "DownloadRepoJob".to_string(),
                name: "Download Repository".to_string(),
                description: Some("Download source code from git repository".to_string()),
                dependencies: vec![],
                job_config: Some(serde_json::json!({
                    "branch_ref": branch_or_commit,
                    "tag_ref": deployment.tag_ref,
                    "commit_sha": deployment.commit_sha,
                    "repo_owner": project.repo_owner,
                    "repo_name": project.repo_name,
                    "git_provider_connection_id": project.git_provider_connection_id,
                    "git_url": project.git_url,
                    "is_public_repo": project.is_public_repo,
                    "directory": project.directory
                })),
                required_for_completion: true,
            });
        }

        // Get compose path from preset config
        let compose_path = project
            .preset_config
            .as_ref()
            .and_then(|pc| {
                if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc {
                    cfg.compose_path.clone()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "docker-compose.yml".to_string());

        // Job 2: Deploy Compose Stack (no build step)
        let deploy_dependencies = if has_git_info {
            vec!["download_repo".to_string()]
        } else {
            vec![]
        };

        jobs.push(JobDefinition {
            job_id: "deploy_compose".to_string(),
            job_type: "DeployComposeJob".to_string(),
            name: "Deploy Compose Stack".to_string(),
            description: Some("Pull images and start Docker Compose services".to_string()),
            dependencies: deploy_dependencies,
            job_config: Some(serde_json::json!({
                "compose_path": compose_path,
                "environment_vars": env_vars,
                "project_id": project.id,
                "environment_id": environment.id,
                "directory": project.directory,
            })),
            required_for_completion: true,
        });

        // Job 3: Mark deployment complete
        jobs.push(JobDefinition {
            job_id: "mark_complete".to_string(),
            job_type: "MarkDeploymentCompleteJob".to_string(),
            name: "Finalize Deployment".to_string(),
            description: Some("Register containers and update routes".to_string()),
            dependencies: vec!["deploy_compose".to_string()],
            job_config: Some(serde_json::json!({
                "deployment_id": deployment.id
            })),
            required_for_completion: true,
        });

        // Job 4: Take screenshot (optional, runs after deployment is live)
        let screenshots_enabled = self.config_service.is_screenshots_enabled().await;
        if screenshots_enabled {
            jobs.push(JobDefinition {
                job_id: "take_screenshot".to_string(),
                job_type: "TakeScreenshotJob".to_string(),
                name: "Take Screenshot".to_string(),
                description: Some("Capture screenshot of deployed application".to_string()),
                dependencies: vec!["mark_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id
                })),
                required_for_completion: false,
            });
        }

        debug!(
            "Planned {} jobs for compose deployment of project '{}'",
            jobs.len(),
            project.name
        );

        Ok(jobs)
    }

    /// Plan jobs for external Docker image deployment
    /// For registry images: PullExternalImageJob -> DeployImageJob -> MarkDeploymentCompleteJob
    /// For uploaded images: VerifyLocalImageJob -> DeployImageJob -> MarkDeploymentCompleteJob
    async fn plan_docker_image_deployment(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        env_vars: std::collections::HashMap<String, String>,
        remote_env_vars: Option<std::collections::HashMap<String, String>>,
        secrets: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Vec<JobDefinition>> {
        let mut jobs = Vec::new();

        // Get external image reference from deployment metadata
        let external_image_ref = deployment
            .metadata
            .as_ref()
            .and_then(|m| m.external_image_ref.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Docker image deployment requires external_image_ref in deployment metadata"
                )
            })?;

        // Check if this is a locally uploaded image (via docker save/load)
        let is_uploaded_locally = deployment
            .metadata
            .as_ref()
            .map(|m| m.image_uploaded_locally)
            .unwrap_or(false);

        let uploaded_image_id = deployment
            .metadata
            .as_ref()
            .and_then(|m| m.uploaded_image_id.clone());

        info!(
            "📦 Planning Docker image deployment for project {} with image: {} (uploaded_locally: {})",
            project.name, external_image_ref, is_uploaded_locally
        );

        // Job 1: Either verify local image or pull from registry
        if is_uploaded_locally {
            // Image was uploaded via docker save/load - just verify it exists locally
            debug!(
                "Image was uploaded locally, using VerifyLocalImageJob instead of PullExternalImageJob"
            );
            jobs.push(JobDefinition {
                job_id: "verify_local_image".to_string(),
                job_type: "VerifyLocalImageJob".to_string(),
                name: "Verify Local Image".to_string(),
                description: Some(format!(
                    "Verify uploaded image exists locally: {}",
                    external_image_ref
                )),
                dependencies: vec![],
                job_config: Some(serde_json::json!({
                    "image_ref": external_image_ref,
                    "expected_image_id": uploaded_image_id,
                })),
                required_for_completion: true,
            });
        } else {
            // Image needs to be pulled from registry
            jobs.push(JobDefinition {
                job_id: "pull_external_image".to_string(),
                job_type: "PullExternalImageJob".to_string(),
                name: "Pull External Image".to_string(),
                description: Some(format!(
                    "Pull and verify external image: {}",
                    external_image_ref
                )),
                dependencies: vec![],
                job_config: Some(serde_json::json!({
                    "image_ref": external_image_ref,
                    "external_image_id": deployment.metadata.as_ref().and_then(|m| m.external_image_id),
                })),
                required_for_completion: true,
            });
        }

        // Job 2: Deploy container
        // Dependency is based on whether image was uploaded locally or needs to be pulled
        let image_job_dependency = if is_uploaded_locally {
            "verify_local_image".to_string()
        } else {
            "pull_external_image".to_string()
        };

        let exposed_port = self
            .resolve_exposed_port(environment, project, Some(&external_image_ref))
            .await;

        let mut deploy_env_vars = env_vars.clone();
        deploy_env_vars.insert("PORT".to_string(), exposed_port.to_string());

        let remote_deploy_env_vars = remote_env_vars.as_ref().map(|rv| {
            let mut remote = rv.clone();
            remote.insert("PORT".to_string(), exposed_port.to_string());
            remote
        });

        let replicas = environment
            .deployment_config
            .as_ref()
            .map(|c| c.replicas)
            .or_else(|| project.deployment_config.as_ref().map(|c| c.replicas))
            .unwrap_or(1);

        let mut job_config = serde_json::json!({
            "port": exposed_port,
            "replicas": replicas,
            "environment_variables": deploy_env_vars,
            "image_name": external_image_ref,
            "use_external_image": true,
        });
        if let Some(ref remote_vars) = remote_deploy_env_vars {
            info!(
                "Storing remote_environment_variables in docker image job config: POSTGRES_HOST={:?}",
                remote_vars.get("POSTGRES_HOST"),
            );
            job_config["remote_environment_variables"] =
                serde_json::to_value(remote_vars).unwrap_or_default();
        } else {
            info!("No remote_environment_variables for docker image deployment");
        }
        if !secrets.is_empty() {
            info!(
                "Storing {} secret file(s) in docker image deploy job config",
                secrets.len()
            );
            job_config["secrets"] = serde_json::to_value(&secrets).unwrap_or_default();
        }

        jobs.push(JobDefinition {
            job_id: "deploy_container".to_string(),
            job_type: "DeployImageJob".to_string(),
            name: "Deploy Container".to_string(),
            description: Some("Deploy the external Docker image".to_string()),
            dependencies: vec![image_job_dependency],
            job_config: Some(job_config),
            required_for_completion: true,
        });

        // Job 3: Mark deployment complete
        jobs.push(JobDefinition {
            job_id: "mark_deployment_complete".to_string(),
            job_type: "MarkDeploymentCompleteJob".to_string(),
            name: "Mark Deployment Complete".to_string(),
            description: Some(
                "Mark deployment as complete and update environment routing".to_string(),
            ),
            dependencies: vec!["deploy_container".to_string()],
            job_config: Some(serde_json::json!({
                "deployment_id": deployment.id
            })),
            required_for_completion: true,
        });

        // Job 4: Take screenshot (optional)
        let screenshots_enabled = self.config_service.is_screenshots_enabled().await;
        if screenshots_enabled {
            jobs.push(JobDefinition {
                job_id: "take_screenshot".to_string(),
                job_type: "TakeScreenshotJob".to_string(),
                name: "Take Screenshot".to_string(),
                description: Some("Capture screenshot of deployed application".to_string()),
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id
                })),
                required_for_completion: false,
            });
        }

        info!(
            "Planned {} jobs for Docker image deployment of project {}",
            jobs.len(),
            project.name
        );
        Ok(jobs)
    }

    /// Plan jobs for static bundle deployment
    /// DeployStaticBundleJob -> MarkDeploymentCompleteJob
    async fn plan_static_bundle_deployment(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        _env_vars: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Vec<JobDefinition>> {
        let mut jobs = Vec::new();

        // Get static bundle path from deployment metadata
        let static_bundle_path = deployment
            .metadata
            .as_ref()
            .and_then(|m| m.static_bundle_path.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Static files deployment requires static_bundle_path in deployment metadata"
                )
            })?;

        info!(
            "📦 Planning static bundle deployment for project {} with bundle: {}",
            project.name, static_bundle_path
        );

        // Get content type from deployment metadata (for proper extraction)
        let content_type = deployment
            .metadata
            .as_ref()
            .and_then(|m| m.static_bundle_content_type.clone())
            .unwrap_or_default();

        // Job 1: Deploy static bundle (extract and deploy to filesystem)
        jobs.push(JobDefinition {
            job_id: "deploy_static_bundle".to_string(),
            job_type: "DeployStaticBundleJob".to_string(),
            name: "Deploy Static Bundle".to_string(),
            description: Some(format!(
                "Extract and deploy static files from bundle: {}",
                static_bundle_path
            )),
            dependencies: vec![],
            job_config: Some(serde_json::json!({
                "bundle_path": static_bundle_path,
                "content_type": content_type,
                "static_bundle_id": deployment.metadata.as_ref().and_then(|m| m.static_bundle_id),
                "project_slug": project.slug,
                "environment_slug": environment.slug,
                "deployment_slug": deployment.slug,
            })),
            required_for_completion: true,
        });

        // Job 2: Mark deployment complete
        jobs.push(JobDefinition {
            job_id: "mark_deployment_complete".to_string(),
            job_type: "MarkDeploymentCompleteJob".to_string(),
            name: "Mark Deployment Complete".to_string(),
            description: Some(
                "Mark deployment as complete and update environment routing".to_string(),
            ),
            dependencies: vec!["deploy_static_bundle".to_string()],
            job_config: Some(serde_json::json!({
                "deployment_id": deployment.id
            })),
            required_for_completion: true,
        });

        // Job 3: Take screenshot (optional)
        let screenshots_enabled = self.config_service.is_screenshots_enabled().await;
        if screenshots_enabled {
            jobs.push(JobDefinition {
                job_id: "take_screenshot".to_string(),
                job_type: "TakeScreenshotJob".to_string(),
                name: "Take Screenshot".to_string(),
                description: Some("Capture screenshot of deployed application".to_string()),
                dependencies: vec!["mark_deployment_complete".to_string()],
                job_config: Some(serde_json::json!({
                    "deployment_id": deployment.id
                })),
                required_for_completion: false,
            });
        }

        info!(
            "Planned {} jobs for static bundle deployment of project {}",
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

    use temps_config::{ConfigService, ServerConfig};
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{preset::Preset, upstream_config::UpstreamList};

    fn create_test_config_service(db: Arc<DatabaseConnection>) -> Arc<ConfigService> {
        let server_config = Arc::new(
            ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .unwrap(),
        );
        Arc::new(ConfigService::new(server_config, db))
    }

    fn create_test_dsn_service(
        db: Arc<DatabaseConnection>,
    ) -> Arc<temps_error_tracking::DSNService> {
        Arc::new(temps_error_tracking::DSNService::new(db))
    }

    fn create_test_external_service_manager(
        db: Arc<DatabaseConnection>,
    ) -> Arc<temps_providers::ExternalServiceManager> {
        let encryption_service = create_test_encryption_service();
        let docker = Arc::new(bollard::Docker::connect_with_local_defaults().ok().unwrap());
        Arc::new(temps_providers::ExternalServiceManager::new(
            db,
            encryption_service,
            docker,
        ))
    }

    fn create_test_encryption_service() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    async fn create_test_project(
        db: &DatabaseConnection,
        preset: Preset,
    ) -> Result<
        (projects::Model, environments::Model, deployments::Model),
        Box<dyn std::error::Error>,
    > {
        // Create project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            main_branch: Set("main".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(preset),
            directory: Set("/".to_string()),
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
            upstreams: Set(UpstreamList::default()),
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
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
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
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        let (_project, _environment, deployment) =
            create_test_project(db.as_ref(), Preset::NextJs).await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Should create 5 jobs: download_repo, build_image, deploy_container, mark_deployment_complete, configure_crons
        // Screenshots may or may not be included depending on config
        assert!(
            jobs.len() >= 5,
            "Expected at least 5 jobs, got {}",
            jobs.len()
        );

        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert!(job_ids.contains(&"download_repo".to_string()));
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
        assert!(job_ids.contains(&"mark_deployment_complete".to_string()));
        assert!(job_ids.contains(&"configure_crons".to_string()));

        // Check that all jobs are in pending state
        for job in &jobs {
            assert_eq!(job.status, JobStatus::Pending);
            assert!(job
                .log_id
                .contains(&format!("deployment-{}", deployment.id)));
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
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        // Create project without git info
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_owner: Set("".to_string()), // No git info
            repo_name: Set("".to_string()),
            main_branch: Set("main".to_string()),
            git_provider_connection_id: Set(None),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
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
            upstreams: Set(UpstreamList::default()),
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
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Should succeed and create only build_image, deploy_container, and mark_deployment_complete jobs
        // (no download_repo or configure_crons since git info is missing)
        let jobs = planner.create_deployment_jobs(deployment.id).await?;
        assert!(
            jobs.len() >= 3,
            "Expected at least 3 jobs, got {}",
            jobs.len()
        );

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
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        let (_project, _environment, deployment) =
            create_test_project(db.as_ref(), Preset::NextJs).await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Verify execution order is set correctly
        for (index, job) in jobs.iter().enumerate() {
            assert_eq!(job.execution_order, Some(index as i32));
        }

        // Verify correct dependency order: download_repo -> build_image -> deploy_container -> mark_deployment_complete
        let job_order: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        let download_index = job_order.iter().position(|x| x == "download_repo").unwrap();
        let build_index = job_order.iter().position(|x| x == "build_image").unwrap();
        let deploy_index = job_order
            .iter()
            .position(|x| x == "deploy_container")
            .unwrap();
        let mark_complete_index = job_order
            .iter()
            .position(|x| x == "mark_deployment_complete")
            .unwrap();

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
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        let (_project, _environment, deployment) =
            create_test_project(db.as_ref(), Preset::NextJs).await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Find specific jobs and check their dependencies
        let build_job = jobs.iter().find(|j| j.job_id == "build_image").unwrap();
        let deploy_job = jobs
            .iter()
            .find(|j| j.job_id == "deploy_container")
            .unwrap();

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
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        let (_project, _environment, deployment) =
            create_test_project(db.as_ref(), Preset::NextJs).await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Check that jobs have proper configuration
        let build_job = jobs.iter().find(|j| j.job_id == "build_image").unwrap();
        assert!(build_job.job_config.is_some());

        if let Some(config) = &build_job.job_config {
            let config_obj: serde_json::Value = config.clone();
            assert!(config_obj.get("dockerfile_path").is_some());
            assert!(config_obj.get("build_args").is_some());
        }

        let deploy_job = jobs
            .iter()
            .find(|j| j.job_id == "deploy_container")
            .unwrap();
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
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        let (project, environment, deployment) =
            create_test_project(db.as_ref(), Preset::NextJs).await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        // Verify log_id format - should be hierarchical: {project_slug}/{env_slug}/{year}/{month}/{day}/{hour}/{minute}/deployment-{id}-job-{job_id}.log
        for job in &jobs {
            assert!(
                job.log_id.contains(&project.slug),
                "log_id should contain project slug"
            );
            assert!(
                job.log_id.contains(&environment.slug),
                "log_id should contain environment slug"
            );
            assert!(
                job.log_id.contains(&format!(
                    "deployment-{}-job-{}.log",
                    deployment.id, job.job_id
                )),
                "log_id should contain deployment-{}-job-{}.log",
                deployment.id,
                job.job_id
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_git_project_with_docker_image_deployment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());
        let external_service_manager = create_test_external_service_manager(db.clone());
        let planner = WorkflowPlanner::new(
            db.clone(),
            log_service,
            external_service_manager,
            config_service,
            dsn_service,
            create_test_encryption_service(),
        );

        // Create a Git project (default source_type)
        let project = projects::ActiveModel {
            name: Set("Git Project".to_string()),
            slug: Set("git-project".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            main_branch: Set("main".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Verify it's a Git project
        assert_eq!(
            project.source_type,
            temps_entities::source_type::SourceType::Git
        );

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create deployment with Docker image metadata (simulating deploy_from_image handler)
        let deployment_metadata = temps_entities::deployments::DeploymentMetadata {
            external_image_ref: Some("ghcr.io/org/app:v1.0".to_string()),
            deployment_source_type: Some(temps_entities::source_type::SourceType::DockerImage),
            ..Default::default()
        };

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-docker-deploy".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(Some(deployment_metadata)),
            image_name: Set(Some("ghcr.io/org/app:v1.0".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        let jobs = planner.create_deployment_jobs(deployment.id).await?;

        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();

        // Should use Docker image pipeline, NOT Git pipeline
        assert!(
            job_ids.contains(&"pull_external_image".to_string()),
            "Expected pull_external_image job for Docker image deployment, got: {:?}",
            job_ids
        );
        assert!(
            job_ids.contains(&"deploy_container".to_string()),
            "Expected deploy_container job"
        );
        assert!(
            job_ids.contains(&"mark_deployment_complete".to_string()),
            "Expected mark_deployment_complete job"
        );

        // Should NOT contain Git pipeline jobs
        assert!(
            !job_ids.contains(&"download_repo".to_string()),
            "Should NOT contain download_repo for Docker image deployment"
        );
        assert!(
            !job_ids.contains(&"build_image".to_string()),
            "Should NOT contain build_image for Docker image deployment"
        );

        Ok(())
    }
}
