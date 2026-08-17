//! Deploy Compose Job
//!
//! Deploys a Docker Compose stack using the ComposeExecutor.
//! Outputs container IDs, names, ports, and service names for
//! MarkDeploymentCompleteJob to register in deployment_containers.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use temps_core::{
    JobResult, WorkflowCancellationProvider, WorkflowContext, WorkflowError, WorkflowTask,
};
use temps_deployer::compose::{ComposeDeployRequest, ComposeExecutor, EnvFileSource};
use temps_entities::preset::ComposePublicPort;
use temps_logs::LogService;
use tracing::debug;

fn route_binding_for_service<'a>(
    service_name: &str,
    bindings: &'a [temps_deployer::compose::ComposePortBinding],
    public_ports: &[ComposePublicPort],
) -> Option<&'a temps_deployer::compose::ComposePortBinding> {
    let configured_target = public_ports
        .iter()
        .find(|port| port.service == service_name)
        .map(|port| port.port);

    configured_target
        .and_then(|target| {
            bindings
                .iter()
                .find(|binding| binding.container_port == target)
        })
        .or_else(|| bindings.first())
}

impl std::fmt::Debug for DeployComposeJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployComposeJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .finish()
    }
}

/// Job that deploys a Docker Compose stack.
///
/// Reads the compose file from the repo checkout directory (set by DownloadRepoJob)
/// during execution, not at construction time.
pub struct DeployComposeJob {
    job_id: String,
    deployment_id: i32,
    project_id: i32,
    environment_id: i32,
    compose_executor: Arc<ComposeExecutor>,
    /// Compose file path relative to project directory (e.g. "docker-compose.yml")
    compose_path: Option<String>,
    /// Project directory relative to repo root (e.g. "./", "apps/web")
    directory: String,
    /// Inline compose content (used when no git repo, e.g. manual project)
    compose_content: Option<String>,
    environment_vars: HashMap<String, String>,
    /// Decrypted project secrets, mounted as files under `/run/secrets/<KEY>`
    /// in every compose service. Never injected as environment variables, so
    /// they stay out of `docker inspect` and out of the generated env files.
    secrets: HashMap<String, String>,
    /// Which compose services may read each secret. A key that is absent goes
    /// to every service.
    secret_compose_services: HashMap<String, Vec<String>>,
    /// Platform-owned build arguments. These are passed only to
    /// `docker compose build`, never to service runtime environments.
    build_args: HashMap<String, String>,
    /// User-provided docker-compose override YAML
    compose_override: Option<String>,
    /// Compose service names to exclude from deployment entirely (e.g. an
    /// unmanaged `postgres`/`redis` service the user wants to skip in favor
    /// of a Temps-managed database with backup/restore support).
    excluded_services: Vec<String>,
    /// Compose service names granted back the minimal Linux capabilities
    /// official database images need to fix ownership on their data/socket
    /// directory at container start (see `ComposeExecutor::RELAXED_CAPABILITIES`).
    relaxed_capability_services: Vec<String>,
    /// Compose services explicitly exempted from Temps' runtime sandbox.
    unsandboxed_services: Vec<String>,
    /// Explicit public service/target selections. These disambiguate services
    /// that publish multiple ports (for example GitLab HTTP and SSH).
    public_ports: Vec<ComposePublicPort>,
    /// Job ID of the download_repo job (to read repo_dir from context)
    download_job_id: String,
    log_id: Option<String>,
    log_service: Arc<LogService>,
}

pub struct DeployComposeJobBuilder {
    job_id: Option<String>,
    deployment_id: Option<i32>,
    project_id: Option<i32>,
    environment_id: Option<i32>,
    compose_executor: Option<Arc<ComposeExecutor>>,
    compose_path: Option<String>,
    directory: Option<String>,
    compose_content: Option<String>,
    compose_override: Option<String>,
    excluded_services: Vec<String>,
    relaxed_capability_services: Vec<String>,
    unsandboxed_services: Vec<String>,
    public_ports: Vec<ComposePublicPort>,
    environment_vars: HashMap<String, String>,
    secrets: HashMap<String, String>,
    secret_compose_services: HashMap<String, Vec<String>>,
    build_args: HashMap<String, String>,
    download_job_id: Option<String>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl Default for DeployComposeJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DeployComposeJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            deployment_id: None,
            project_id: None,
            environment_id: None,
            compose_executor: None,
            compose_path: None,
            directory: None,
            compose_content: None,
            compose_override: None,
            excluded_services: Vec::new(),
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
            public_ports: Vec::new(),
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            download_job_id: None,
            log_id: None,
            log_service: None,
        }
    }

    pub fn job_id(mut self, id: String) -> Self {
        self.job_id = Some(id);
        self
    }
    pub fn deployment_id(mut self, id: i32) -> Self {
        self.deployment_id = Some(id);
        self
    }
    pub fn project_id(mut self, id: i32) -> Self {
        self.project_id = Some(id);
        self
    }
    pub fn environment_id(mut self, id: i32) -> Self {
        self.environment_id = Some(id);
        self
    }
    pub fn compose_executor(mut self, executor: Arc<ComposeExecutor>) -> Self {
        self.compose_executor = Some(executor);
        self
    }
    pub fn compose_path(mut self, path: Option<String>) -> Self {
        self.compose_path = path;
        self
    }
    pub fn directory(mut self, dir: String) -> Self {
        self.directory = Some(dir);
        self
    }
    pub fn compose_content(mut self, content: Option<String>) -> Self {
        self.compose_content = content;
        self
    }
    pub fn compose_override(mut self, content: Option<String>) -> Self {
        self.compose_override = content;
        self
    }
    pub fn excluded_services(mut self, services: Vec<String>) -> Self {
        self.excluded_services = services;
        self
    }
    pub fn relaxed_capability_services(mut self, services: Vec<String>) -> Self {
        self.relaxed_capability_services = services;
        self
    }
    pub fn unsandboxed_services(mut self, services: Vec<String>) -> Self {
        self.unsandboxed_services = services;
        self
    }
    pub fn public_ports(mut self, ports: Vec<ComposePublicPort>) -> Self {
        self.public_ports = ports;
        self
    }
    pub fn download_job_id(mut self, id: String) -> Self {
        self.download_job_id = Some(id);
        self
    }
    pub fn environment_vars(mut self, vars: HashMap<String, String>) -> Self {
        self.environment_vars = vars;
        self
    }
    pub fn build_args(mut self, args: HashMap<String, String>) -> Self {
        self.build_args = args;
        self
    }
    /// Decrypted project secrets. Materialized as files under
    /// `/run/secrets/<KEY>` by the compose executor.
    pub fn secrets(mut self, secrets: HashMap<String, String>) -> Self {
        self.secrets = secrets;
        self
    }
    /// Per-secret compose-service scope. Absent key = every service.
    pub fn secret_compose_services(mut self, scopes: HashMap<String, Vec<String>>) -> Self {
        self.secret_compose_services = scopes;
        self
    }
    pub fn log_id(mut self, id: Option<String>) -> Self {
        self.log_id = id;
        self
    }
    pub fn log_service(mut self, service: Arc<LogService>) -> Self {
        self.log_service = Some(service);
        self
    }

    pub fn build(self) -> Result<DeployComposeJob, WorkflowError> {
        Ok(DeployComposeJob {
            job_id: self
                .job_id
                .ok_or_else(|| WorkflowError::JobValidationFailed("job_id required".into()))?,
            deployment_id: self.deployment_id.ok_or_else(|| {
                WorkflowError::JobValidationFailed("deployment_id required".into())
            })?,
            project_id: self
                .project_id
                .ok_or_else(|| WorkflowError::JobValidationFailed("project_id required".into()))?,
            environment_id: self.environment_id.ok_or_else(|| {
                WorkflowError::JobValidationFailed("environment_id required".into())
            })?,
            compose_executor: self.compose_executor.ok_or_else(|| {
                WorkflowError::JobValidationFailed("compose_executor required".into())
            })?,
            compose_path: self.compose_path,
            directory: self.directory.unwrap_or_else(|| ".".to_string()),
            compose_content: self.compose_content,
            compose_override: self.compose_override,
            excluded_services: self.excluded_services,
            relaxed_capability_services: self.relaxed_capability_services,
            unsandboxed_services: self.unsandboxed_services,
            public_ports: self.public_ports,
            environment_vars: self.environment_vars,
            secrets: self.secrets,
            secret_compose_services: self.secret_compose_services,
            build_args: self.build_args,
            download_job_id: self
                .download_job_id
                .unwrap_or_else(|| "download_repo".to_string()),
            log_id: self.log_id,
            log_service: self
                .log_service
                .ok_or_else(|| WorkflowError::JobValidationFailed("log_service required".into()))?,
        })
    }
}

#[async_trait]
impl WorkflowTask for DeployComposeJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Deploy Compose Stack"
    }

    fn description(&self) -> &str {
        "Deploy a multi-container Docker Compose stack"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let project_name = format!("temps-{}-{}", self.project_id, self.environment_id);

        // Log start
        if let Some(ref log_id) = self.log_id {
            let _ = self
                .log_service
                .log_info(
                    log_id,
                    &format!("Deploying Docker Compose stack (project: {})", project_name),
                )
                .await;
        }

        // Build Temps labels for container discovery
        let mut labels = HashMap::new();
        labels.insert(
            "sh.temps.project_id".to_string(),
            self.project_id.to_string(),
        );
        labels.insert(
            "sh.temps.environment".to_string(),
            self.environment_id.to_string(),
        );
        labels.insert(
            "sh.temps.deploy_id".to_string(),
            self.deployment_id.to_string(),
        );
        labels.insert("sh.temps.managed".to_string(), "true".to_string());

        // Read compose file from repo checkout or inline content
        let compose_file_name = self.compose_path.as_deref().unwrap_or("docker-compose.yml");
        // Confine user-supplied paths to the repo checkout / project directory
        // so a project writer cannot escape the intended work directory.
        validate_relative_path(compose_file_name, "compose_path")?;
        validate_relative_path(&self.directory, "directory")?;

        // Resolve the selected repository subdirectory once and carry the
        // canonical path through reads and execution. A lexical `directory`
        // like `./app` can still be a committed symlink to `/`; accepting that
        // would move the entire Compose trust boundary outside the checkout.
        let repo_path = if self.compose_content.is_none() {
            let repo_dir: String = context
                .get_output(&self.download_job_id, "repo_dir")
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to get repo_dir from download job: {}",
                        e
                    ))
                })?
                .ok_or_else(|| {
                    WorkflowError::JobExecutionFailed(
                        "No repo_dir output from download job — is the repository configured?"
                            .to_string(),
                    )
                })?;
            Some(canonicalize_confined_repo_path(
                Path::new(&repo_dir),
                Path::new(&self.directory),
                "directory",
            )?)
        } else {
            None
        };

        let compose_content = if let Some(ref inline) = self.compose_content {
            // Inline compose content (manual project, no git repo)
            inline.clone()
        } else {
            let selected_repo_dir = repo_path.as_deref().ok_or_else(|| {
                WorkflowError::JobExecutionFailed(
                    "Canonical repository directory was not available for compose read".to_string(),
                )
            })?;
            let compose_file_path = canonicalize_confined_repo_path(
                selected_repo_dir,
                Path::new(compose_file_name),
                "compose_path",
            )?;

            if let Some(ref log_id) = self.log_id {
                let _ = self
                    .log_service
                    .log_info(
                        log_id,
                        &format!("Reading compose file from {}", compose_file_path.display()),
                    )
                    .await;
            }

            match std::fs::read_to_string(&compose_file_path) {
                Ok(content) => content,
                Err(e) => {
                    let error_msg = format!(
                        "Failed to read compose file at {}: {}",
                        compose_file_path.display(),
                        e
                    );
                    if let Some(ref log_id) = self.log_id {
                        let _ = self.log_service.log_error(log_id, &error_msg).await;
                    }
                    return Err(WorkflowError::JobExecutionFailed(error_msg));
                }
            }
        };

        // Full service list from the raw compose file, *before* exclusion
        // stripping — so excluded services still show up as re-includable
        // options in the settings-page checklist. Best-effort: a parse
        // failure degrades to an empty snapshot rather than failing the
        // deploy, since this only feeds a UI convenience, not the deploy
        // itself.
        let compose_services: Vec<temps_entities::preset::ComposeServiceSnapshot> =
            temps_presets::list_compose_services_with_override(
                &compose_content,
                self.compose_override.as_deref(),
            )
            .map(|services| {
                services
                    .into_iter()
                    .map(|s| temps_entities::preset::ComposeServiceSnapshot {
                        name: s.name,
                        image: s.image,
                        looks_like_database: s.looks_like_database,
                        detected_service_type: s.detected_service_type,
                        ports: s.ports,
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Services whose image matches a well-known database/storage engine
        // are running unmanaged inside this compose stack — they won't get
        // Temps backup/restore, monitoring, or one-click upgrades the way a
        // Temps-managed external_services row would. Recommend the specific
        // managed equivalent up front, and — for the subset that also runs
        // with all Linux capabilities dropped by default (defense in depth)
        // and isn't in `relaxed_capability_services` — warn about that too,
        // rather than let the operator discover it only via a raw
        // "Operation not permitted" crash-loop.
        for service in &compose_services {
            let Some(family) = service.detected_service_type else {
                continue;
            };
            let (display_name, type_param) = match family {
                temps_entities::preset::ComposeServiceFamily::Postgres => {
                    ("PostgreSQL", "postgres")
                }
                temps_entities::preset::ComposeServiceFamily::Mariadb => ("MariaDB", "mariadb"),
                temps_entities::preset::ComposeServiceFamily::Mongodb => ("MongoDB", "mongodb"),
                temps_entities::preset::ComposeServiceFamily::Redis => ("Redis", "redis"),
                temps_entities::preset::ComposeServiceFamily::S3 => ("S3 / RustFS", "s3"),
            };
            let needs_relaxed_capabilities = service.looks_like_database
                && !self
                    .relaxed_capability_services
                    .iter()
                    .any(|s| s == &service.name);
            if let Some(ref log_id) = self.log_id {
                let image_suffix = service
                    .image
                    .as_deref()
                    .map(|image| format!(" ({image})"))
                    .unwrap_or_default();
                let mut message = format!(
                    "Service '{}'{} looks like a {} container running unmanaged in this \
                     compose stack — it won't have Temps backup/restore, monitoring, or \
                     one-click upgrades. Consider deploying it as a Temps-managed {} \
                     service instead: Databases → Create → {} (type={}).",
                    service.name,
                    image_suffix,
                    display_name,
                    display_name,
                    display_name,
                    type_param
                );
                if needs_relaxed_capabilities {
                    message.push_str(
                        " It also runs with all Linux capabilities dropped by default; if \
                         it fails to start with \"Operation not permitted\" errors, enable \
                         \"Elevated permissions\" for it in Project Settings → Git → \
                         Compose services, then redeploy.",
                    );
                }
                let _ = self.log_service.log_info(log_id, &message).await;
            }
        }

        // Strip any user-excluded services (and references to them in other
        // services' `depends_on`) before anything downstream — env-file
        // planning, security preflight, and the actual `docker compose up` —
        // ever sees them. Compose's own `-f` override merging can only
        // add/replace fields on a service, never unset one, so this has to
        // rewrite the base document rather than layer another override file.
        let compose_content = if self.excluded_services.is_empty() {
            compose_content
        } else {
            if let Some(ref log_id) = self.log_id {
                let _ = self
                    .log_service
                    .log_info(
                        log_id,
                        &format!(
                            "Excluding service(s) from deployment: {}",
                            self.excluded_services.join(", ")
                        ),
                    )
                    .await;
            }
            match self
                .compose_executor
                .strip_excluded_services(&compose_content, &self.excluded_services)
            {
                Ok(stripped) => stripped,
                Err(e) => {
                    let error_msg = format!("Failed to apply excluded services: {}", e);
                    if let Some(ref log_id) = self.log_id {
                        let _ = self.log_service.log_error(log_id, &error_msg).await;
                    }
                    return Err(WorkflowError::JobExecutionFailed(error_msg));
                }
            }
        };

        // Read .env from repo if it exists
        let env_content = if let Some(selected_repo_dir) = repo_path.as_deref() {
            let env_candidate = selected_repo_dir.join(".env");
            if std::fs::symlink_metadata(&env_candidate).is_ok() {
                let env_path =
                    canonicalize_confined_repo_path(selected_repo_dir, Path::new(".env"), ".env")?;
                Some(std::fs::read_to_string(&env_path).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to read repository env file at {}: {}",
                        env_path.display(),
                        e
                    ))
                })?)
            } else {
                None
            }
        } else {
            None
        };

        // Report every `env_file:` Temps is about to satisfy on the operator's
        // behalf. Synthesizing configuration silently is how a stack boots with
        // an empty DATABASE_URL and fails later with something unrelated — the
        // person debugging this has no support channel, so the deployment log
        // has to say what was created and where it came from.
        for plan in ComposeExecutor::plan_env_files(&compose_content, repo_path.as_deref()) {
            let message = match plan.source {
                EnvFileSource::Repository(_) => format!(
                    "Compose references env file '{}' — copied from the repository",
                    plan.path
                ),
                EnvFileSource::ProjectEnvironment if self.environment_vars.is_empty() => format!(
                    "Compose references env file '{}', which is not in the repository — created it empty. \
                     If the stack needs values, set them in the environment's variables and redeploy.",
                    plan.path
                ),
                EnvFileSource::ProjectEnvironment => format!(
                    "Compose references env file '{}', which is not in the repository — created it from \
                     this environment's {} variable(s)",
                    plan.path,
                    self.environment_vars.len()
                ),
            };
            if let Some(ref log_id) = self.log_id {
                let _ = self.log_service.log_info(log_id, &message).await;
            }
        }

        // Report secret delivery per service. A secret that exists in the
        // dashboard but never reaches the container is the worst failure mode
        // here -- silent, and indistinguishable from a wrong value -- so the
        // log states the delivery matrix rather than asserting success.
        if let Some(ref log_id) = self.log_id {
            if !self.secrets.is_empty() {
                let documents = [
                    compose_content.as_str(),
                    self.compose_override.as_deref().unwrap_or_default(),
                ];
                let services = self.compose_executor.all_service_names(&documents);
                let skipped = ComposeExecutor::services_managing_own_secrets(&documents);

                for service in &services {
                    if skipped.contains(service) {
                        continue;
                    }
                    let mut keys = ComposeExecutor::secret_names_for_service(
                        &self.secrets,
                        &self.secret_compose_services,
                        service,
                    );
                    keys.sort();
                    let message = if keys.is_empty() {
                        format!(
                            "Service '{service}' receives no secrets                              (every secret is scoped to other services)"
                        )
                    } else {
                        format!(
                            "Service '{}' receives {} secret(s) at /run/secrets: {}",
                            service,
                            keys.len(),
                            keys.join(", ")
                        )
                    };
                    let _ = self.log_service.log_info(log_id, &message).await;
                }

                let mut skipped: Vec<String> = skipped.into_iter().collect();
                skipped.sort();
                if !skipped.is_empty() {
                    let _ = self
                        .log_service
                        .log_warning(
                            log_id,
                            &format!(
                                "Service(s) {} already define their own /run/secrets mount or                                  compose `secrets:` entry — Temps secrets are NOT mounted there.                                  Remove that mount if you want Temps to deliver them.",
                                skipped.join(", ")
                            ),
                        )
                        .await;
                }

                // A compose service rename strands every secret scoped to the
                // old name. Without this the secret simply stops arriving and
                // nothing anywhere says why.
                for (key, missing) in ComposeExecutor::unmatched_secret_scopes(
                    &self.secret_compose_services,
                    &services,
                ) {
                    let _ = self
                        .log_service
                        .log_warning(
                            log_id,
                            &format!(
                                "Secret '{}' is scoped to compose service '{}', which does not                                  exist in this stack (services: {}). It was not delivered to that                                  service. Update the secret's scope or the compose file.",
                                key,
                                missing,
                                if services.is_empty() {
                                    "none".to_string()
                                } else {
                                    services.join(", ")
                                }
                            ),
                        )
                        .await;
                }
            }
        }

        // Validate the compose security policy BEFORE tearing down the existing
        // stack. Rejecting after teardown would cause downtime on the running
        // deployment for what is purely a configuration problem.
        if let Err(e) = self
            .compose_executor
            .preflight_validate(&compose_content, self.compose_override.as_deref())
        {
            let error_msg = format!("Compose security policy rejected deployment: {}", e);
            tracing::error!(error = %error_msg, "Docker Compose preflight validation failed");
            if let Some(ref log_id) = self.log_id {
                let _ = self.log_service.log_error(log_id, &error_msg).await;
            }
            return Err(WorkflowError::JobExecutionFailed(error_msg));
        }
        if let Some(selected_repo_dir) = repo_path.as_deref() {
            if let Err(e) = self.compose_executor.preflight_validate_filesystem(
                selected_repo_dir,
                compose_file_name,
                &compose_content,
                self.compose_override.as_deref(),
            ) {
                let error_msg =
                    format!("Compose filesystem security policy rejected deployment: {e}");
                tracing::error!(error = %error_msg, "Docker Compose filesystem preflight failed");
                if let Some(ref log_id) = self.log_id {
                    let _ = self.log_service.log_error(log_id, &error_msg).await;
                }
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        }

        // Tear down previous containers (preserve volumes for data persistence)
        if let Some(ref log_id) = self.log_id {
            let _ = self
                .log_service
                .log_info(
                    log_id,
                    "Stopping previous compose stack (preserving volumes)",
                )
                .await;
        }
        if let Err(e) = self
            .compose_executor
            .teardown_at(
                &project_name,
                repo_path.as_deref(),
                Some(compose_file_name),
                &self.environment_vars,
            )
            .await
        {
            debug!(
                project = %project_name,
                error = %e,
                "Previous compose stack teardown failed (may not exist)"
            );
        }

        let request = ComposeDeployRequest {
            project_name: project_name.clone(),
            compose_content,
            env_content,
            work_dir: PathBuf::from("/tmp"),
            compose_path: self.compose_path.clone(),
            environment_vars: self.environment_vars.clone(),
            secrets: self.secrets.clone(),
            secret_compose_services: self.secret_compose_services.clone(),
            build_args: self.build_args.clone(),
            labels,
            repo_dir: repo_path.clone(),
            compose_override: self.compose_override.clone(),
            relaxed_capability_services: self.relaxed_capability_services.clone(),
            unsandboxed_services: self.unsandboxed_services.clone(),
        };

        // Deploy
        let services = match self.compose_executor.deploy(request).await {
            Ok(s) => s,
            Err(e) => {
                let cleanup_error = self
                    .compose_executor
                    .teardown_at(
                        &project_name,
                        repo_path.as_deref(),
                        Some(compose_file_name),
                        &self.environment_vars,
                    )
                    .await
                    .err();
                let error_msg = format!("Compose deploy failed: {}", e);
                tracing::error!(error = %error_msg, "Docker Compose deployment failed");
                if let Some(ref log_id) = self.log_id {
                    if let Err(log_err) = self.log_service.log_error(log_id, &error_msg).await {
                        tracing::error!("Failed to write error to log stream: {}", log_err);
                    }
                    // The embedded container-log tail already carries the raw
                    // entrypoint failure (e.g. "chmod: /var/run/postgresql:
                    // Operation not permitted") — an operator with no support
                    // channel shouldn't have to reverse-engineer that this is
                    // Temps's own cap_drop: ALL sandbox, not a broken image.
                    let lower = error_msg.to_lowercase();
                    let looks_like_capability_denial = lower.contains("operation not permitted")
                        && ["chown", "chmod", "setuid", "setgid"]
                            .iter()
                            .any(|needle| lower.contains(needle));
                    if looks_like_capability_denial {
                        let _ = self
                            .log_service
                            .log_error(
                                log_id,
                                "This looks like a Linux capability denial: Temps drops all \
                                 container capabilities by default, which can block a database \
                                 image's entrypoint from fixing ownership on its data/socket \
                                 directory. If the failing service is a well-known database or \
                                 storage image (PostgreSQL, MariaDB/MySQL, MongoDB, Redis, or \
                                 S3-compatible/MinIO), enable \"Elevated permissions\" for it in \
                                 Project Settings → Git → Compose services, then redeploy — or \
                                 better, deploy it as a Temps-managed service instead via \
                                 Databases → Create.",
                            )
                            .await;
                    }
                }
                if let Some(cleanup_error) = cleanup_error {
                    tracing::error!(project = %project_name, error = %cleanup_error, "Compose compensation cleanup failed");
                }
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        };

        if services.is_empty() {
            let _ = self
                .compose_executor
                .teardown_at(
                    &project_name,
                    repo_path.as_deref(),
                    Some(compose_file_name),
                    &self.environment_vars,
                )
                .await;
            let error_msg = "No containers found after docker compose up".to_string();
            tracing::error!(error = %error_msg, "Docker Compose deployment produced no containers");
            if let Some(ref log_id) = self.log_id {
                if let Err(log_err) = self.log_service.log_error(log_id, &error_msg).await {
                    tracing::error!("Failed to write error to log stream: {}", log_err);
                }
            }
            return Err(WorkflowError::JobExecutionFailed(error_msg));
        }

        // Log discovered services
        for svc in &services {
            let ports_str = svc
                .ports
                .iter()
                .map(|p| format!("{}:{}", p.host_port, p.container_port))
                .collect::<Vec<_>>()
                .join(", ");

            if let Some(ref log_id) = self.log_id {
                let _ = self
                    .log_service
                    .log_info(
                        log_id,
                        &format!(
                            "Service '{}': container={}, image={}, ports=[{}], status={}",
                            svc.service_name,
                            &svc.container_id[..12.min(svc.container_id.len())],
                            svc.image_name,
                            ports_str,
                            svc.status
                        ),
                    )
                    .await;
            }
        }

        // Set outputs compatible with MarkDeploymentCompleteJob
        // Uses job_id "deploy_container" for backward compatibility
        let container_ids: Vec<String> = services.iter().map(|s| s.container_id.clone()).collect();
        let container_names: Vec<String> =
            services.iter().map(|s| s.container_name.clone()).collect();
        let service_names: Vec<String> = services.iter().map(|s| s.service_name.clone()).collect();
        let route_bindings: Vec<_> = services
            .iter()
            .map(|service| {
                route_binding_for_service(&service.service_name, &service.ports, &self.public_ports)
            })
            .collect();
        let host_ports: Vec<u16> = route_bindings
            .iter()
            .map(|binding| binding.map(|port| port.host_port).unwrap_or(0))
            .collect();
        let container_ports: Vec<i32> = route_bindings
            .iter()
            .map(|binding| {
                binding
                    .map(|port| i32::from(port.container_port))
                    .unwrap_or(0)
            })
            .collect();

        // First service's port as the "main" container_port
        let main_port = container_ports.first().copied().unwrap_or(0);

        context.set_output("deploy_container", "container_ids", &container_ids)?;
        context.set_output("deploy_container", "container_id", &container_ids[0])?;
        context.set_output("deploy_container", "container_name", &container_names[0])?;
        context.set_output("deploy_container", "host_ports", &host_ports)?;
        context.set_output("deploy_container", "host_port", host_ports[0])?;
        context.set_output("deploy_container", "container_port", main_port)?;
        context.set_output(
            "deploy_container",
            "node_ids",
            vec![None::<i32>; services.len()],
        )?;

        // Compose-specific outputs for MarkDeploymentCompleteJob
        context.set_output("deploy_container", "service_names", &service_names)?;
        context.set_output("deploy_container", "container_names", &container_names)?;
        context.set_output("deploy_container", "container_ports", &container_ports)?;
        context.set_output(
            "deploy_container",
            "image_names",
            services
                .iter()
                .map(|s| s.image_name.clone())
                .collect::<Vec<_>>(),
        )?;
        context.set_output("deploy_container", "compose_services", &compose_services)?;

        debug!(
            project = %project_name,
            services = services.len(),
            "Compose deployment complete"
        );

        if let Some(ref log_id) = self.log_id {
            let _ = self
                .log_service
                .log_success(
                    log_id,
                    &format!(
                        "Docker Compose stack deployed: {} services running",
                        services.len()
                    ),
                )
                .await;
        }

        Ok(JobResult::success(context))
    }

    async fn execute_with_cancellation(
        &self,
        context: WorkflowContext,
        cancellation_provider: &dyn WorkflowCancellationProvider,
    ) -> Result<JobResult, WorkflowError> {
        let workflow_run_id = context.workflow_run_id.clone();
        let project_name = format!("temps-{}-{}", self.project_id, self.environment_id);
        let compose_file_name = self.compose_path.as_deref().unwrap_or("docker-compose.yml");
        validate_relative_path(compose_file_name, "compose_path")?;
        validate_relative_path(&self.directory, "directory")?;
        let cleanup_repo_path = if self.compose_content.is_none() {
            let repo_dir: Option<String> = context
                .get_output(&self.download_job_id, "repo_dir")
                .map_err(|error| WorkflowError::JobExecutionFailed(error.to_string()))?;
            repo_dir
                .map(|repo_dir| {
                    canonicalize_confined_repo_path(
                        Path::new(&repo_dir),
                        Path::new(&self.directory),
                        "directory",
                    )
                })
                .transpose()?
        } else {
            None
        };
        let cancellation_check = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                match cancellation_provider.is_cancelled(&workflow_run_id).await {
                    Ok(false) => continue,
                    Ok(true) | Err(_) => return,
                }
            }
        };

        tokio::select! {
            result = self.execute(context) => result,
            _ = cancellation_check => {
                self.compose_executor
                    .teardown_at(
                        &project_name,
                        cleanup_repo_path.as_deref(),
                        Some(compose_file_name),
                        &self.environment_vars,
                    )
                    .await
                    .map_err(|error| WorkflowError::JobExecutionFailed(format!(
                        "Compose deployment was cancelled, but stack cleanup failed for {project_name}: {error}"
                    )))?;
                Err(WorkflowError::WorkflowCancelled)
            }
        }
    }
}

/// Confine a user-supplied path (`compose_path`, `directory`) to the repo
/// checkout / project directory: reject empty values, absolute paths, and any
/// `..` / root / prefix component that would escape the project tree.
fn validate_relative_path(path: &str, field: &str) -> Result<(), WorkflowError> {
    let candidate = Path::new(path);
    if candidate.as_os_str().is_empty() || candidate.is_absolute() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "{field} must be a non-empty relative path (got '{path}')"
        )));
    }

    if candidate.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(WorkflowError::JobValidationFailed(format!(
            "{field} must not contain '..' or absolute/root path components (got '{path}')"
        )));
    }

    Ok(())
}

/// Canonicalize an existing repository path and prove it remains beneath the
/// supplied base directory. This is the filesystem half of
/// `validate_relative_path`: Git preserves symlinks, so lexical confinement is
/// not sufficient by itself.
pub(crate) fn canonicalize_confined_repo_path(
    base_dir: &Path,
    relative_path: &Path,
    field: &str,
) -> Result<PathBuf, WorkflowError> {
    let relative = relative_path.to_str().ok_or_else(|| {
        WorkflowError::JobValidationFailed(format!("{field} must be valid UTF-8"))
    })?;
    validate_relative_path(relative, field)?;

    let canonical_base = std::fs::canonicalize(base_dir).map_err(|e| {
        WorkflowError::JobValidationFailed(format!(
            "Failed to canonicalize repository base '{}' for {field}: {e}",
            base_dir.display()
        ))
    })?;
    let canonical_path =
        std::fs::canonicalize(canonical_base.join(relative_path)).map_err(|e| {
            WorkflowError::JobValidationFailed(format!(
                "Failed to canonicalize {field} '{}' beneath repository '{}': {e}",
                relative_path.display(),
                canonical_base.display()
            ))
        })?;
    if !canonical_path.starts_with(&canonical_base) {
        return Err(WorkflowError::JobValidationFailed(format!(
            "{field} '{}' resolves outside repository directory '{}'",
            relative_path.display(),
            canonical_base.display()
        )));
    }
    Ok(canonical_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_preserves_compose_runtime_and_route_configuration() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .expect("constructing the Docker client should not contact the daemon"),
        );
        let job = DeployComposeJobBuilder::new()
            .job_id("deploy_compose".to_string())
            .deployment_id(1)
            .project_id(2)
            .environment_id(3)
            .compose_executor(Arc::new(ComposeExecutor::new(docker, std::env::temp_dir())))
            .unsandboxed_services(vec!["webserver".to_string()])
            .public_ports(vec![ComposePublicPort {
                service: "webserver".to_string(),
                port: 8000,
                published: Some(18005),
            }])
            .log_service(Arc::new(LogService::new(std::env::temp_dir())))
            .build()
            .expect("complete builder should produce a deployment job");

        assert_eq!(job.unsandboxed_services, ["webserver"]);
        assert_eq!(job.public_ports.len(), 1);
        assert_eq!(job.public_ports[0].service, "webserver");
        assert_eq!(job.public_ports[0].port, 8000);
        assert_eq!(job.public_ports[0].published, Some(18005));
    }

    #[test]
    fn route_binding_prefers_configured_target_for_multi_port_service() {
        let bindings = vec![
            temps_deployer::compose::ComposePortBinding {
                host_port: 2224,
                container_port: 22,
                protocol: "tcp".to_string(),
            },
            temps_deployer::compose::ComposePortBinding {
                host_port: 8929,
                container_port: 8929,
                protocol: "tcp".to_string(),
            },
        ];
        let public_ports = vec![ComposePublicPort {
            service: "gitlab".to_string(),
            port: 8929,
            published: Some(8929),
        }];

        let selected = route_binding_for_service("gitlab", &bindings, &public_ports).unwrap();

        assert_eq!(selected.container_port, 8929);
        assert_eq!(selected.host_port, 8929);
    }

    #[test]
    fn route_binding_keeps_first_binding_without_public_selection() {
        let bindings = vec![temps_deployer::compose::ComposePortBinding {
            host_port: 2224,
            container_port: 22,
            protocol: "tcp".to_string(),
        }];

        let selected = route_binding_for_service("gitlab", &bindings, &[]).unwrap();

        assert_eq!(selected.container_port, 22);
    }

    #[test]
    fn test_validate_relative_path_accepts_confined_paths() {
        for ok in [".", "docker-compose.yml", "apps/web", "./compose.yml"] {
            assert!(
                validate_relative_path(ok, "compose_path").is_ok(),
                "expected '{ok}' to be accepted"
            );
        }
    }

    #[test]
    fn test_validate_relative_path_rejects_escape_paths() {
        for bad in [
            "",
            "/tmp/compose.yml",
            "/etc/passwd",
            "../compose.yml",
            "apps/../../compose.yml",
        ] {
            let err = validate_relative_path(bad, "compose_path").unwrap_err();
            assert!(
                matches!(err, WorkflowError::JobValidationFailed(_)),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_canonicalize_confined_repo_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir().unwrap();
        symlink("/", repo.path().join("app")).unwrap();
        let err = canonicalize_confined_repo_path(repo.path(), Path::new("app"), "directory")
            .unwrap_err();
        assert!(matches!(&err, WorkflowError::JobValidationFailed(_)));
        assert!(err.to_string().contains("resolves outside"));
    }

    #[test]
    fn test_canonicalize_confined_repo_path_allows_nested_file() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("apps/web")).unwrap();
        std::fs::write(repo.path().join("apps/web/compose.yml"), "services: {}\n").unwrap();

        let project =
            canonicalize_confined_repo_path(repo.path(), Path::new("apps/web"), "directory")
                .unwrap();
        let compose =
            canonicalize_confined_repo_path(&project, Path::new("compose.yml"), "compose_path")
                .unwrap();
        let canonical_repo = repo.path().canonicalize().unwrap();
        assert!(compose.starts_with(canonical_repo));
    }

    /// Regression coverage for the settings editor's “effective deployment”
    /// promise. The deployment writer removes base ports for services whose
    /// override supplies ports before Docker Compose performs its normal merge;
    /// model that test-only file preparation, then compare the important
    /// semantics from `docker compose config` with the redacted preview.
    #[tokio::test]
    async fn effective_preview_matches_prepared_compose_deployment_semantics() {
        let probe = tokio::process::Command::new("docker")
            .args(["compose", "version"])
            .output()
            .await;
        if !probe.is_ok_and(|output| output.status.success()) {
            println!("Docker Compose is unavailable; skipping preview parity test");
            return;
        }

        let base = r#"
x-service-defaults: &service-defaults
  restart: unless-stopped
services:
  web:
    <<: *service-defaults
    image: example/web:latest
    command: ["serve", "--port", "3000"]
    entrypoint: ["/old-entrypoint"]
    depends_on:
      - db
    ports:
      - "3000:3000"
  worker:
    <<: *service-defaults
    image: example/worker:latest
    depends_on:
      db:
        condition: service_healthy
      web:
        condition: service_started
  db:
    image: postgres:17
"#;
        let override_yaml = r#"
services:
  web:
    command: ["serve", "--port", "80"]
    entrypoint: ["/new-entrypoint"]
    ports:
      - "15455:80"
  worker:
    depends_on:
      web:
        condition: service_completed_successfully
"#;
        let excluded = vec!["db".to_string()];

        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .expect("constructing the Docker client should not contact the daemon"),
        );
        let executor = temps_deployer::compose::ComposeExecutor::new(
            docker,
            tempfile::tempdir().unwrap().path().to_path_buf(),
        );
        let stripped = executor
            .strip_excluded_services(base, &excluded)
            .expect("deployment exclusion rewrite should succeed");

        // `ComposeExecutor::write_compose_files` performs this same rewrite
        // before layering an override that defines `ports`, preventing Compose
        // from retaining both the base and override port mappings.
        let mut prepared: serde_yaml::Value = serde_yaml::from_str(&stripped).unwrap();
        prepared["services"]["web"]
            .as_mapping_mut()
            .expect("web service should be a mapping")
            .remove("ports");
        let prepared = serde_yaml::to_string(&prepared).unwrap();

        let preview =
            temps_presets::render_effective_compose_preview(base, Some(override_yaml), &excluded)
                .expect("preview should render");
        let preview_value: serde_yaml::Value = serde_yaml::from_str(&preview.yaml).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let base_path = temp.path().join("compose.yml");
        let override_path = temp.path().join("compose.override.yml");
        std::fs::write(&base_path, prepared).unwrap();
        std::fs::write(&override_path, override_yaml).unwrap();
        let output = tokio::process::Command::new("docker")
            .args(["compose", "-f"])
            .arg(&base_path)
            .args(["-f"])
            .arg(&override_path)
            .args(["config", "--format", "json"])
            .output()
            .await
            .expect("Docker Compose config command should start");
        assert!(
            output.status.success(),
            "Docker Compose config failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deployed: serde_json::Value = serde_json::from_slice(&output.stdout)
            .expect("Docker Compose config should return JSON");

        let preview_services = preview_value["services"].as_mapping().unwrap();
        let deployed_services = deployed["services"].as_object().unwrap();
        assert_eq!(preview.enabled_services, ["web", "worker"]);
        assert_eq!(preview.disabled_services, ["db"]);
        assert_eq!(preview_services.len(), deployed_services.len());
        assert!(!deployed_services.contains_key("db"));

        let preview_web = &preview_value["services"]["web"];
        let deployed_web = &deployed["services"]["web"];
        assert_eq!(preview_web["restart"].as_str(), Some("unless-stopped"));
        assert_eq!(deployed_web["restart"].as_str(), Some("unless-stopped"));
        // Commands are deliberately hidden in the browser preview because
        // repositories frequently embed credentials in them. Docker's final
        // config must still prove that the override replaced the base command.
        assert_eq!(preview_web["command"].as_str(), Some("<redacted>"));
        assert_eq!(deployed_web["command"][2].as_str(), Some("80"));
        assert_eq!(preview_web["entrypoint"].as_str(), Some("<redacted>"));
        assert_eq!(
            deployed_web["entrypoint"][0].as_str(),
            Some("/new-entrypoint")
        );
        assert_eq!(preview_web["ports"].as_sequence().unwrap().len(), 1);
        assert_eq!(deployed_web["ports"].as_array().unwrap().len(), 1);
        assert_eq!(deployed_web["ports"][0]["target"].as_u64(), Some(80));
        assert_eq!(
            deployed_web["ports"][0]["published"].as_str(),
            Some("15455")
        );

        let preview_dependencies = preview_value["services"]["worker"]["depends_on"]
            .as_mapping()
            .unwrap();
        let deployed_dependencies = deployed["services"]["worker"]["depends_on"]
            .as_object()
            .unwrap();
        assert_eq!(preview_dependencies.len(), 1);
        assert_eq!(deployed_dependencies.len(), 1);
        assert!(preview_dependencies.contains_key("web"));
        assert!(deployed_dependencies.contains_key("web"));
    }
}
