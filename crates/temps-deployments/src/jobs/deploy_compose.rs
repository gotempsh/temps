//! Deploy Compose Job
//!
//! Deploys a Docker Compose stack using the ComposeExecutor.
//! Outputs container IDs, names, ports, and service names for
//! MarkDeploymentCompleteJob to register in deployment_containers.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::compose::{ComposeDeployRequest, ComposeExecutor};
use temps_logs::LogService;
use tracing::debug;

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
    /// User-provided docker-compose override YAML
    compose_override: Option<String>,
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
    environment_vars: HashMap<String, String>,
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
            environment_vars: HashMap::new(),
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
    pub fn download_job_id(mut self, id: String) -> Self {
        self.download_job_id = Some(id);
        self
    }
    pub fn environment_vars(mut self, vars: HashMap<String, String>) -> Self {
        self.environment_vars = vars;
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
            environment_vars: self.environment_vars,
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
            .teardown_for_redeploy(&project_name)
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
            labels,
            repo_dir: repo_path,
            compose_override: self.compose_override.clone(),
        };

        // Deploy
        let services = match self.compose_executor.deploy(request).await {
            Ok(s) => s,
            Err(e) => {
                let error_msg = format!("Compose deploy failed: {}", e);
                tracing::error!(error = %error_msg, "Docker Compose deployment failed");
                if let Some(ref log_id) = self.log_id {
                    if let Err(log_err) = self.log_service.log_error(log_id, &error_msg).await {
                        tracing::error!("Failed to write error to log stream: {}", log_err);
                    }
                }
                return Err(WorkflowError::JobExecutionFailed(error_msg));
            }
        };

        if services.is_empty() {
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
        let host_ports: Vec<u16> = services
            .iter()
            .map(|s| s.ports.first().map(|p| p.host_port).unwrap_or(0))
            .collect();
        let container_ports: Vec<i32> = services
            .iter()
            .map(|s| {
                s.ports
                    .first()
                    .map(|p| p.container_port as i32)
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
fn canonicalize_confined_repo_path(
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
}
