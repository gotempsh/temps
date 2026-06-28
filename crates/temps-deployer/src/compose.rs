//! Docker Compose deployment executor.
//!
//! Manages multi-container deployments using `docker compose` CLI commands.
//! After `compose up`, discovers running containers, applies Temps labels,
//! and returns per-service results that get inserted into `deployment_containers`.

use bollard::Docker;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, error, info, warn};

#[derive(Error, Debug)]
pub enum ComposeError {
    #[error("Compose command failed for project '{project}': {reason}")]
    CommandFailed { project: String, reason: String },

    #[error("Failed to write compose files to '{path}': {reason}")]
    FileWriteFailed { path: String, reason: String },

    #[error("Failed to discover containers for project '{project}': {reason}")]
    DiscoveryFailed { project: String, reason: String },

    #[error("Invalid compose override for project '{project}': {reason}")]
    InvalidOverride { project: String, reason: String },

    #[error("Docker API error: {0}")]
    Docker(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Request to deploy a Docker Compose stack.
#[derive(Debug, Clone)]
pub struct ComposeDeployRequest {
    /// Compose project name (e.g., "temps-{project_id}-{env_id}")
    pub project_name: String,
    /// Compose file content (the YAML)
    pub compose_content: String,
    /// Optional .env file content
    pub env_content: Option<String>,
    /// Working directory where compose files are written
    pub work_dir: PathBuf,
    /// Path to compose file relative to work_dir (default: "docker-compose.yml")
    pub compose_path: Option<String>,
    /// Environment variables to inject (merged with .env)
    pub environment_vars: HashMap<String, String>,
    /// Temps labels to apply to all containers
    pub labels: HashMap<String, String>,
    /// Source repo directory (needed for compose files with build: directives)
    pub repo_dir: Option<PathBuf>,
    /// User-provided docker-compose.temps-override.yml content
    pub compose_override: Option<String>,
}

/// Result for a single compose service after deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeServiceResult {
    pub container_id: String,
    pub container_name: String,
    pub service_name: String,
    pub image_name: String,
    /// Ports published to the host (may be empty for internal services)
    pub ports: Vec<ComposePortBinding>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposePortBinding {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

/// Docker Compose deployment executor.
#[derive(Debug)]
pub struct ComposeExecutor {
    docker: Arc<Docker>,
    /// Base directory for compose work dirs
    data_dir: PathBuf,
}

impl ComposeExecutor {
    pub fn new(docker: Arc<Docker>, data_dir: PathBuf) -> Self {
        Self { docker, data_dir }
    }

    /// Get the work directory for a compose project.
    fn project_dir(&self, project_name: &str) -> PathBuf {
        self.data_dir.join("compose").join(project_name)
    }

    /// Deploy a compose stack: write files, pull images, start containers,
    /// discover and label them. Returns one result per service.
    pub async fn deploy(
        &self,
        request: ComposeDeployRequest,
    ) -> Result<Vec<ComposeServiceResult>, ComposeError> {
        let project_dir = self.project_dir(&request.project_name);
        let project_name = request.project_name.clone();
        let has_build = self.has_build_directives(&request.compose_content);

        // Always use the repo checkout directory when available.
        // Compose files often reference local paths (bind mounts, configs,
        // build contexts) that only exist in the repo, not in the temps data dir.
        let effective_dir = request
            .repo_dir
            .clone()
            .unwrap_or_else(|| project_dir.clone());

        // 1. Write compose files + env overrides to disk
        self.write_compose_files(&effective_dir, &request).await?;

        let compose_file = request
            .compose_path
            .as_deref()
            .unwrap_or("docker-compose.yml");

        // 2. Build images if compose file has build: directives
        if has_build {
            self.compose_build(
                &effective_dir,
                &project_name,
                compose_file,
                &request.environment_vars,
            )
            .await?;
        }

        // 3. Remove any containers with hardcoded names that would conflict
        self.remove_conflicting_containers(&request.compose_content)
            .await;

        // 4. Run docker compose up (pulls pre-built images, starts built + pulled)
        self.compose_up(
            &effective_dir,
            &project_name,
            compose_file,
            &request.environment_vars,
        )
        .await?;

        // 4. Discover running containers
        let containers = self
            .discover_containers(&effective_dir, &project_name, compose_file)
            .await?;

        // 4. Apply Temps labels to each container
        for container in &containers {
            if let Err(e) = self
                .apply_labels(
                    &container.container_id,
                    &request.labels,
                    &container.service_name,
                )
                .await
            {
                warn!(
                    container_id = %container.container_id,
                    service = %container.service_name,
                    error = %e,
                    "Failed to apply Temps labels to container"
                );
            }
        }

        info!(
            project = %project_name,
            services = containers.len(),
            "Compose stack deployed"
        );

        Ok(containers)
    }

    /// Tear down containers before a redeploy. Preserves volumes (database data,
    /// uploads, etc.) so they survive between deployments.
    pub async fn teardown_for_redeploy(&self, project_name: &str) -> Result<(), ComposeError> {
        let project_dir = self.project_dir(project_name);

        if !project_dir.exists() {
            debug!(project = %project_name, "Project directory does not exist, nothing to tear down");
            return Ok(());
        }

        let compose_file = self.find_compose_file(&project_dir);

        // down WITHOUT --volumes: removes containers and networks, keeps volumes
        let output = tokio::process::Command::new("docker")
            .args(["compose", "-p", project_name])
            .args(["-f", &compose_file])
            .args(["down", "--remove-orphans"])
            .current_dir(&project_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(project = %project_name, stderr = %stderr, "docker compose down failed (best-effort)");
        }

        info!(project = %project_name, "Compose stack torn down (volumes preserved)");
        Ok(())
    }

    /// Fully destroy a compose stack including all volumes and data.
    /// Used when deleting a project/environment permanently.
    pub async fn destroy(&self, project_name: &str) -> Result<(), ComposeError> {
        let project_dir = self.project_dir(project_name);

        if !project_dir.exists() {
            debug!(project = %project_name, "Project directory does not exist, nothing to destroy");
            return Ok(());
        }

        let compose_file = self.find_compose_file(&project_dir);

        // down WITH --volumes: removes everything including persistent data
        let output = tokio::process::Command::new("docker")
            .args(["compose", "-p", project_name])
            .args(["-f", &compose_file])
            .args(["down", "--remove-orphans", "--volumes"])
            .current_dir(&project_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(project = %project_name, stderr = %stderr, "docker compose down failed");
        }

        // Clean up work directory
        if let Err(e) = tokio::fs::remove_dir_all(&project_dir).await {
            warn!(project = %project_name, error = %e, "Failed to clean up project directory");
        }

        info!(project = %project_name, "Compose stack destroyed (volumes removed)");
        Ok(())
    }

    /// Stop a compose stack without removing volumes.
    pub async fn stop(&self, project_name: &str) -> Result<(), ComposeError> {
        let project_dir = self.project_dir(project_name);
        let compose_file = self.find_compose_file(&project_dir);

        let output = tokio::process::Command::new("docker")
            .args(["compose", "-p", project_name])
            .args(["-f", &compose_file])
            .arg("stop")
            .current_dir(&project_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose stop failed: {}", stderr),
            });
        }

        Ok(())
    }

    // --- Internal methods ---

    async fn write_compose_files(
        &self,
        project_dir: &Path,
        request: &ComposeDeployRequest,
    ) -> Result<(), ComposeError> {
        tokio::fs::create_dir_all(project_dir).await.map_err(|e| {
            ComposeError::FileWriteFailed {
                path: project_dir.display().to_string(),
                reason: e.to_string(),
            }
        })?;

        let compose_file = request
            .compose_path
            .as_deref()
            .unwrap_or("docker-compose.yml");
        let compose_path = project_dir.join(compose_file);

        // Ensure parent directories exist (for nested paths like "subdir/docker-compose.yml")
        if let Some(parent) = compose_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: parent.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // If the user override defines ports for specific services, strip those
        // ports from the base compose file. Docker Compose merges (appends) port
        // arrays from override files, so without stripping, the original ports
        // remain alongside the override ports, causing conflicts.
        let compose_to_write = if let Some(ref user_override) = request.compose_override {
            let services_with_port_overrides = self.services_with_ports_in_override(user_override);
            if services_with_port_overrides.is_empty() {
                request.compose_content.clone()
            } else {
                self.strip_ports_for_services(
                    &request.compose_content,
                    &services_with_port_overrides,
                )
            }
        } else {
            request.compose_content.clone()
        };

        tokio::fs::write(&compose_path, &compose_to_write)
            .await
            .map_err(|e| ComposeError::FileWriteFailed {
                path: compose_path.display().to_string(),
                reason: e.to_string(),
            })?;

        // Write .env file (repo's original .env content if any)
        if let Some(ref env_content) = request.env_content {
            if !env_content.trim().is_empty() {
                let env_path = project_dir.join(".env");
                tokio::fs::write(&env_path, env_content.trim())
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: env_path.display().to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        // Write Temps system env vars to .env.temps
        // These include SENTRY_DSN, TEMPS_API_URL, TEMPS_API_TOKEN, OTEL vars, etc.
        if !request.environment_vars.is_empty() {
            let temps_env: String = request
                .environment_vars
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            let temps_env_path = project_dir.join(".env.temps");
            tokio::fs::write(&temps_env_path, &temps_env)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_env_path.display().to_string(),
                    reason: e.to_string(),
                })?;

            // Write Temps env override (auto-generated, injects .env.temps into every service)
            let temps_override_path = project_dir.join("docker-compose.temps-env.yml");
            let override_content =
                self.generate_env_override(&request.compose_content, ".env.temps");
            tokio::fs::write(&temps_override_path, &override_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps labels override (injects sh.temps.* labels into every service for log collection)
        if !request.labels.is_empty() {
            let labels_override_path = project_dir.join("docker-compose.temps-labels.yml");
            let labels_content =
                self.generate_labels_override(&request.compose_content, &request.labels);
            if !labels_content.is_empty() {
                tokio::fs::write(&labels_override_path, &labels_content)
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: labels_override_path.display().to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        // Write user-provided override if present. Inline overrides come from project
        // settings, so validate them before handing them to the host Docker daemon.
        if let Some(ref user_override) = request.compose_override {
            if !user_override.trim().is_empty() {
                Self::validate_compose_override(
                    &request.project_name,
                    &request.compose_content,
                    user_override,
                )?;

                let override_path = project_dir.join("docker-compose.temps-override.yml");
                tokio::fs::write(&override_path, user_override)
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: override_path.display().to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        debug!(
            path = %compose_path.display(),
            "Wrote compose files"
        );

        Ok(())
    }

    fn validate_compose_override(
        project_name: &str,
        compose_content: &str,
        override_content: &str,
    ) -> Result<(), ComposeError> {
        let base = Self::parse_compose_yaml(project_name, compose_content, "compose file")?;
        let override_yaml =
            Self::parse_compose_yaml(project_name, override_content, "compose override")?;

        let base_services = Self::compose_services(&base).ok_or_else(|| ComposeError::InvalidOverride {
            project: project_name.to_string(),
            reason: "base compose file must define a services mapping before an inline override can be applied".to_string(),
        })?;

        let Some(override_root) = override_yaml.as_mapping() else {
            return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: "inline compose override must be a mapping".to_string(),
            });
        };
        for key in override_root.keys().filter_map(Self::yaml_key) {
            if key != "services" {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "inline compose override cannot set top-level key '{key}'; only service-level changes are allowed"
                    ),
                });
            }
        }

        let Some(override_services) = Self::compose_services(&override_yaml) else {
            return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason:
                    "inline compose override must define only service-level changes under services"
                        .to_string(),
            });
        };

        let base_service_names: HashSet<String> =
            base_services.keys().filter_map(Self::yaml_key).collect();
        for (service_name_value, service_config) in override_services {
            let service_name = Self::yaml_key(service_name_value).ok_or_else(|| {
                ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: "service names in inline compose override must be strings".to_string(),
                }
            })?;

            if !base_service_names.contains(&service_name) {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "inline compose override cannot add service '{service_name}'; add new services to the repository compose file for review"
                    ),
                });
            }

            Self::validate_override_service(project_name, &service_name, service_config)?;
        }

        Ok(())
    }

    fn parse_compose_yaml(
        project_name: &str,
        content: &str,
        label: &str,
    ) -> Result<Value, ComposeError> {
        serde_yaml::from_str::<Value>(content).map_err(|e| ComposeError::InvalidOverride {
            project: project_name.to_string(),
            reason: format!("failed to parse {label} YAML: {e}"),
        })
    }

    fn compose_services(compose: &Value) -> Option<&Mapping> {
        compose
            .as_mapping()?
            .get(Value::String("services".to_string()))?
            .as_mapping()
    }

    fn yaml_key(value: &Value) -> Option<String> {
        value.as_str().map(ToString::to_string)
    }

    fn validate_override_service(
        project_name: &str,
        service_name: &str,
        service_config: &Value,
    ) -> Result<(), ComposeError> {
        let Some(service) = service_config.as_mapping() else {
            return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("service '{service_name}' override must be a mapping"),
            });
        };

        const FORBIDDEN_SERVICE_KEYS: &[&str] = &[
            "privileged",
            "network_mode",
            "pid",
            "ipc",
            "uts",
            "cgroup",
            "cgroup_parent",
            "cap_add",
            "cap_drop",
            "devices",
            "device_cgroup_rules",
            "security_opt",
            "sysctls",
            "userns_mode",
            "volumes",
        ];

        for key in service.keys().filter_map(Self::yaml_key) {
            if FORBIDDEN_SERVICE_KEYS.contains(&key.as_str()) {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "service '{service_name}' uses forbidden inline override key '{key}'; put host-affecting Compose settings in the repository compose file for review"
                    ),
                });
            }
        }

        Ok(())
    }

    /// Check if a compose file contains build: directives (services that need building)
    fn has_build_directives(&self, compose_content: &str) -> bool {
        for line in compose_content.lines() {
            let trimmed = line.trim();
            if trimmed == "build:" || trimmed.starts_with("build:") {
                return true;
            }
        }
        false
    }

    /// Run docker compose build for services with build: directives
    async fn compose_build(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        env_vars: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let mut cmd = tokio::process::Command::new("docker");
        cmd.args(["compose", "-p", project_name])
            .args(["-f", compose_file])
            .args(["build", "--pull"])
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        debug!(project = %project_name, "Running docker compose build");

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose build failed: {}", stderr),
            });
        }

        info!(project = %project_name, "docker compose build completed");
        Ok(())
    }

    /// Remove containers that would conflict with compose services.
    /// This handles the case where a container with a hardcoded `container_name:`
    /// already exists from a previous deployment (e.g., old stacks system).
    async fn remove_conflicting_containers(&self, compose_content: &str) {
        // Parse container_name: values from compose YAML
        let mut next_is_container_name = false;
        for line in compose_content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("container_name:") {
                let name = trimmed
                    .trim_start_matches("container_name:")
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'');
                if !name.is_empty() {
                    // Try to stop and remove this container if it exists
                    debug!(container = %name, "Removing conflicting container");
                    let _ = self.docker.stop_container(name, None).await;
                    let _ = self
                        .docker
                        .remove_container(
                            name,
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                }
            }
            // Handle multi-line container_name (unlikely but safe)
            if next_is_container_name {
                next_is_container_name = false;
            }
            if trimmed == "container_name:" {
                next_is_container_name = true;
            }
        }
    }

    async fn compose_up(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        env_vars: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let mut cmd = tokio::process::Command::new("docker");
        cmd.args(["compose", "-p", project_name])
            .args(["-f", compose_file]);

        // Include Temps env override (auto-generated)
        let temps_override = project_dir.join("docker-compose.temps-env.yml");
        if temps_override.exists() {
            cmd.args(["-f", "docker-compose.temps-env.yml"]);
        }

        // Include Temps labels override (injects sh.temps.* labels for log collection)
        let labels_override = project_dir.join("docker-compose.temps-labels.yml");
        if labels_override.exists() {
            cmd.args(["-f", "docker-compose.temps-labels.yml"]);
        }

        // Include user-provided override (ports, volumes, etc.)
        let user_override = project_dir.join("docker-compose.temps-override.yml");
        if user_override.exists() {
            cmd.args(["-f", "docker-compose.temps-override.yml"]);
        }

        // Load .env.temps for YAML variable substitution (${VAR} in compose file)
        let temps_env_path = project_dir.join(".env.temps");
        if temps_env_path.exists() {
            cmd.args(["--env-file", ".env.temps"]);
        }

        // Also load repo .env if it exists
        let repo_env_path = project_dir.join(".env");
        if repo_env_path.exists() {
            cmd.args(["--env-file", ".env"]);
        }

        cmd.args([
            "up",
            "-d",
            "--pull",
            "always",
            "--remove-orphans",
            "--force-recreate",
        ])
        .current_dir(project_dir);

        // Set PWD so compose files using ${PWD} resolve correctly
        cmd.env("PWD", project_dir.to_string_lossy().to_string());

        // Pass environment variables for compose YAML substitution (process env)
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        debug!(project = %project_name, "Running docker compose up");

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose up failed: {}", stderr),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!(project = %project_name, stdout = %stdout, "docker compose up completed");

        Ok(())
    }

    async fn discover_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
    ) -> Result<Vec<ComposeServiceResult>, ComposeError> {
        // Use docker compose ps to list containers
        let output = tokio::process::Command::new("docker")
            .args(["compose", "-p", project_name])
            .args(["-f", compose_file])
            .args(["ps", "--format", "json", "--all"])
            .current_dir(project_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComposeError::DiscoveryFailed {
                project: project_name.to_string(),
                reason: format!("docker compose ps failed: {}", stderr),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut results = Vec::new();

        // docker compose ps --format json outputs one JSON object per line
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let ps_entry: ComposePsEntry =
                serde_json::from_str(line).map_err(|e| ComposeError::DiscoveryFailed {
                    project: project_name.to_string(),
                    reason: format!("Failed to parse compose ps output: {} (line: {})", e, line),
                })?;

            // Parse published ports
            let ports = self.parse_publishers(&ps_entry.publishers);

            // Resolve full container ID via Docker inspect (compose ps returns short IDs)
            let full_id = match self.docker.inspect_container(&ps_entry.id, None).await {
                Ok(info) => info.id.unwrap_or(ps_entry.id.clone()),
                Err(_) => ps_entry.id.clone(),
            };

            results.push(ComposeServiceResult {
                container_id: full_id,
                container_name: ps_entry.name,
                service_name: ps_entry.service,
                image_name: ps_entry.image,
                ports,
                status: ps_entry.state,
            });
        }

        debug!(
            project = %project_name,
            services = results.len(),
            "Discovered compose containers"
        );

        Ok(results)
    }

    fn parse_publishers(&self, publishers: &[ComposePsPublisher]) -> Vec<ComposePortBinding> {
        publishers
            .iter()
            .filter(|p| p.published_port > 0)
            .map(|p| ComposePortBinding {
                host_port: p.published_port,
                container_port: p.target_port,
                protocol: p.protocol.clone(),
            })
            .collect()
    }

    async fn apply_labels(
        &self,
        container_id: &str,
        base_labels: &HashMap<String, String>,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        // Bollard doesn't support updating labels on a running container directly.
        // We need to use `docker container update` is also limited.
        // Instead, we verify the container exists and log the labels.
        // The labels were already set via compose labels or we use docker inspect
        // to verify the container is running.
        //
        // For Temps integration, we rely on:
        // 1. The compose project name (temps-{project_id}-{env_id}) for discovery
        // 2. The container IDs stored in deployment_containers table
        // 3. Container names for log aggregation
        //
        // The deployment pipeline inserts these containers into deployment_containers
        // with the correct project_id, environment_id, deployment_id, and service_name.
        // The proxy and monitoring systems use deployment_containers for lookup,
        // not Docker labels.

        let inspect = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| ComposeError::Docker(format!("inspect failed: {}", e)))?;

        let state = inspect
            .state
            .as_ref()
            .and_then(|s| s.status.as_ref())
            .map(|s| format!("{:?}", s))
            .unwrap_or_else(|| "unknown".to_string());

        debug!(
            container_id = %container_id,
            service = %service_name,
            state = %state,
            labels = ?base_labels.keys().collect::<Vec<_>>(),
            "Verified compose container"
        );

        Ok(())
    }

    /// Generate a docker-compose.temps-override.yml that adds env_file to every service.
    /// This injects Temps system env vars into all containers without modifying
    /// the original compose file.
    fn generate_env_override(&self, compose_content: &str, env_file: &str) -> String {
        // Parse service names from compose content (simple YAML parsing)
        let mut services = Vec::new();
        let mut in_services = false;
        let mut services_indent: usize = 0;
        let mut service_indent: Option<usize> = None; // indent of first service found

        for line in compose_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - line.trim_start().len();

            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services = true;
                services_indent = indent;
                service_indent = None;
                continue;
            }

            if in_services {
                // If we go back to root level, stop
                if indent <= services_indent {
                    in_services = false;
                    continue;
                }

                // Service names are keys at the first level after services:
                if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                    match service_indent {
                        None => {
                            // First service found — set the indent level
                            service_indent = Some(indent);
                            services.push(trimmed.trim_end_matches(':').to_string());
                        }
                        Some(si) if indent == si => {
                            // Same indent as first service — it's a service
                            services.push(trimmed.trim_end_matches(':').to_string());
                        }
                        _ => {
                            // Deeper indent — it's a property (image:, ports:, etc.), skip
                        }
                    }
                }
            }
        }

        if services.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in &services {
            override_yaml.push_str(&format!("  {}:\n", service));
            override_yaml.push_str("    env_file:\n");
            override_yaml.push_str(&format!("      - {}\n", env_file));
        }

        override_yaml
    }

    /// Generate a docker-compose override that adds Temps labels to every service.
    /// These labels are required for log collection, monitoring, and container discovery.
    fn generate_labels_override(
        &self,
        compose_content: &str,
        labels: &HashMap<String, String>,
    ) -> String {
        // Reuse the same service parsing logic
        let services = self.parse_service_names(compose_content);

        if services.is_empty() || labels.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in &services {
            override_yaml.push_str(&format!("  {}:\n", service));
            override_yaml.push_str("    labels:\n");
            for (key, value) in labels {
                override_yaml.push_str(&format!("      {}: \"{}\"\n", key, value));
            }
            // Per-service label: the compose service name
            override_yaml.push_str(&format!("      sh.temps.service: \"{}\"\n", service));
        }

        override_yaml
    }

    /// Parse service names from compose YAML content.
    fn parse_service_names(&self, compose_content: &str) -> Vec<String> {
        let mut services = Vec::new();
        let mut in_services = false;
        let mut services_indent: usize = 0;
        let mut service_indent: Option<usize> = None;

        for line in compose_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - line.trim_start().len();

            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services = true;
                services_indent = indent;
                service_indent = None;
                continue;
            }

            if in_services {
                if indent <= services_indent {
                    in_services = false;
                    continue;
                }

                if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                    match service_indent {
                        None => {
                            service_indent = Some(indent);
                            services.push(trimmed.trim_end_matches(':').to_string());
                        }
                        Some(si) if indent == si => {
                            services.push(trimmed.trim_end_matches(':').to_string());
                        }
                        _ => {}
                    }
                }
            }
        }

        services
    }

    /// Parse a user override YAML and return the names of services that define `ports:`.
    fn services_with_ports_in_override(&self, override_content: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut in_services = false;
        let mut services_indent: usize = 0;
        let mut current_service: Option<(String, usize)> = None; // (name, indent)

        for line in override_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - line.trim_start().len();

            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services = true;
                services_indent = indent;
                current_service = None;
                continue;
            }

            if !in_services {
                continue;
            }

            // Left of services block
            if indent <= services_indent && !trimmed.is_empty() {
                in_services = false;
                continue;
            }

            // Inside a service — check for ports: before checking service names
            if let Some((ref svc_name, svc_indent)) = current_service {
                if indent > svc_indent && (trimmed == "ports:" || trimmed.starts_with("ports:")) {
                    if !result.contains(svc_name) {
                        result.push(svc_name.clone());
                    }
                    continue;
                }
            }

            // Service-level key (direct child of services:)
            if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                let svc_name = trimmed.trim_end_matches(':').to_string();
                match &current_service {
                    None => {
                        current_service = Some((svc_name, indent));
                    }
                    Some((_, si)) if indent == *si => {
                        current_service = Some((svc_name, indent));
                    }
                    _ => {}
                }
            }
        }

        result
    }

    /// Strip `ports:` sections from the base compose content for the given services only.
    /// Other services keep their ports untouched.
    fn strip_ports_for_services(&self, compose_content: &str, services: &[String]) -> String {
        let mut output = String::new();
        let mut in_services_block = false;
        let mut services_indent: usize = 0;
        let mut current_service: Option<(String, usize)> = None;
        let mut service_indent: Option<usize> = None;
        let mut skipping_ports = false;
        let mut ports_indent: usize = 0;

        for line in compose_content.lines() {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();

            // Track services: block
            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services_block = true;
                services_indent = indent;
                service_indent = None;
                current_service = None;
                skipping_ports = false;
                output.push_str(line);
                output.push('\n');
                continue;
            }

            // If currently skipping a ports block, check if we've exited it
            if skipping_ports {
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    // Skip blank lines and comments inside ports block
                    continue;
                }
                if indent > ports_indent {
                    // Still inside ports block (port entries are indented further)
                    continue;
                }
                // We've exited the ports block
                skipping_ports = false;
            }

            if in_services_block && !trimmed.is_empty() && indent <= services_indent {
                in_services_block = false;
                current_service = None;
                service_indent = None;
            }

            if in_services_block && !trimmed.is_empty() && !trimmed.starts_with('#') {
                // Detect service names
                if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                    match service_indent {
                        None => {
                            service_indent = Some(indent);
                            let name = trimmed.trim_end_matches(':').to_string();
                            current_service = Some((name, indent));
                        }
                        Some(si) if indent == si => {
                            let name = trimmed.trim_end_matches(':').to_string();
                            current_service = Some((name, indent));
                        }
                        _ => {}
                    }
                }

                // Check if this line is `ports:` inside a service we need to strip
                if let Some((ref svc_name, svc_indent)) = current_service {
                    if indent > svc_indent
                        && (trimmed == "ports:" || trimmed.starts_with("ports:"))
                        && services.contains(svc_name)
                    {
                        // If it's `ports:` with inline value like `ports: ["80:80"]`
                        if trimmed.starts_with("ports:") && trimmed != "ports:" {
                            // Single-line ports — just skip this line
                            continue;
                        }
                        // Block-style ports: — skip this line and subsequent indented lines
                        skipping_ports = true;
                        ports_indent = indent;
                        continue;
                    }
                }
            }

            output.push_str(line);
            output.push('\n');
        }

        output
    }

    fn find_compose_file(&self, project_dir: &Path) -> String {
        for name in &[
            "docker-compose.yml",
            "docker-compose.yaml",
            "compose.yml",
            "compose.yaml",
        ] {
            if project_dir.join(name).exists() {
                return name.to_string();
            }
        }
        "docker-compose.yml".to_string()
    }
}

/// JSON output from `docker compose ps --format json`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ComposePsEntry {
    #[serde(alias = "ID")]
    id: String,
    name: String,
    service: String,
    image: String,
    state: String,
    #[serde(default)]
    publishers: Vec<ComposePsPublisher>,
}

/// Port publisher from `docker compose ps --format json`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ComposePsPublisher {
    #[serde(default)]
    published_port: u16,
    #[serde(default)]
    target_port: u16,
    #[serde(default)]
    protocol: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_compose_ps_json() {
        let json = r#"{"ID":"abc123","Name":"myapp-web-1","Service":"web","Image":"nginx:latest","State":"running","Publishers":[{"URL":"0.0.0.0","TargetPort":80,"PublishedPort":8080,"Protocol":"tcp"}]}"#;

        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "abc123");
        assert_eq!(entry.service, "web");
        assert_eq!(entry.state, "running");
        assert_eq!(entry.publishers.len(), 1);
        assert_eq!(entry.publishers[0].published_port, 8080);
        assert_eq!(entry.publishers[0].target_port, 80);
    }

    #[test]
    fn test_parse_compose_ps_no_ports() {
        let json = r#"{"ID":"def456","Name":"myapp-redis-1","Service":"redis","Image":"redis:7","State":"running","Publishers":[]}"#;

        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.service, "redis");
        assert!(entry.publishers.is_empty());
    }

    #[test]
    fn test_parse_publishers() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            // Can still test parse_publishers without Docker
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let publishers = vec![
            ComposePsPublisher {
                published_port: 8080,
                target_port: 80,
                protocol: "tcp".to_string(),
            },
            ComposePsPublisher {
                published_port: 0, // Not published
                target_port: 6379,
                protocol: "tcp".to_string(),
            },
        ];

        let ports = executor.parse_publishers(&publishers);
        assert_eq!(ports.len(), 1); // Only the published port
        assert_eq!(ports[0].host_port, 8080);
        assert_eq!(ports[0].container_port, 80);
    }

    #[test]
    fn test_generate_env_override() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  web:
    image: nginx
    ports:
      - "8080:80"
  redis:
    image: redis:7
  postgres:
    image: postgres:17
"#;

        let override_yaml = executor.generate_env_override(compose, ".env.temps");
        assert!(override_yaml.contains("web:"));
        assert!(override_yaml.contains("redis:"));
        assert!(override_yaml.contains("postgres:"));
        assert!(override_yaml.contains(".env.temps"));
        // Each service should have env_file
        assert_eq!(override_yaml.matches("env_file:").count(), 3);
    }

    #[test]
    fn test_validate_compose_override_allows_safe_service_changes() {
        let compose = r#"
services:
  web:
    image: nginx
"#;
        let override_content = r#"
services:
  web:
    ports:
      - "127.0.0.1:8080:80"
    environment:
      RUST_LOG: info
    command: ["nginx", "-g", "daemon off;"]
"#;

        ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
            .unwrap();
    }

    #[test]
    fn test_validate_compose_override_rejects_new_services() {
        let compose = r#"
services:
  web:
    image: nginx
"#;
        let override_content = r#"
services:
  attacker:
    image: alpine
"#;

        let error =
            ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
                .unwrap_err();
        assert!(error.to_string().contains("cannot add service 'attacker'"));
    }

    #[test]
    fn test_validate_compose_override_rejects_host_escape_keys() {
        let compose = r#"
services:
  web:
    image: nginx
"#;
        let dangerous_overrides = [
            "privileged: true",
            "network_mode: host",
            "pid: host",
            "cap_add: [SYS_ADMIN]",
            "devices: ['/dev/kvm:/dev/kvm']",
            "security_opt: ['apparmor:unconfined']",
            "sysctls: {net.ipv4.ip_forward: '1'}",
            "volumes: ['/:/host:rw']",
        ];

        for dangerous_override in dangerous_overrides {
            let override_content = format!(
                "services:
  web:
    {dangerous_override}
"
            );
            let error = ComposeExecutor::validate_compose_override(
                "temps-test",
                compose,
                &override_content,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("forbidden inline override key"),
                "expected {dangerous_override} to be rejected, got {error}"
            );
        }
    }

    #[test]
    fn test_validate_compose_override_rejects_top_level_escape_keys() {
        let compose = r#"
services:
  web:
    image: nginx
"#;
        let override_content = r#"
services:
  web:
    ports:
      - "8080:80"
networks:
  hostnet:
    external: true
"#;

        let error =
            ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
                .unwrap_err();
        assert!(error.to_string().contains("top-level key 'networks'"));
    }

    #[test]
    fn test_has_build_directives() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        // No build
        assert!(!executor.has_build_directives("services:\n  web:\n    image: nginx\n"));

        // build: with context
        assert!(executor.has_build_directives("services:\n  web:\n    build: .\n"));

        // build: block
        assert!(executor.has_build_directives(
            "services:\n  web:\n    build:\n      context: .\n      dockerfile: Dockerfile\n"
        ));
    }

    #[test]
    fn test_generate_env_override_empty() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = "version: '3'\n";
        let override_yaml = executor.generate_env_override(compose, ".env.temps");
        assert!(override_yaml.is_empty());
    }

    #[test]
    fn test_services_with_ports_in_override() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let override_content = r#"
services:
  clickhouse:
    ports:
      - '127.0.0.1:28123:8123'
      - '127.0.0.1:29001:9000'
"#;
        let result = executor.services_with_ports_in_override(override_content);
        assert_eq!(result, vec!["clickhouse"]);

        // No ports override
        let override_no_ports = r#"
services:
  clickhouse:
    environment:
      - FOO=bar
"#;
        let result = executor.services_with_ports_in_override(override_no_ports);
        assert!(result.is_empty());

        // Multiple services, only one with ports
        let override_mixed = r#"
services:
  web:
    ports:
      - '8080:80'
  redis:
    environment:
      - REDIS_PASSWORD=secret
"#;
        let result = executor.services_with_ports_in_override(override_mixed);
        assert_eq!(result, vec!["web"]);
    }

    #[test]
    fn test_strip_ports_for_services() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"version: '3.8'
services:
  clickhouse:
    image: clickhouse/clickhouse-server:23.4
    ports:
      - '8123:8123'
      - '9000:9000'
    volumes:
      - ./data:/var/lib/clickhouse
  keeper:
    image: clickhouse/clickhouse-keeper:23.4-alpine
    ports:
      - '9181:9181'
"#;

        // Strip ports only for clickhouse, keep keeper's ports
        let result = executor.strip_ports_for_services(compose, &["clickhouse".to_string()]);
        assert!(!result.contains("8123:8123"));
        assert!(!result.contains("9000:9000"));
        assert!(result.contains("9181:9181")); // keeper untouched
        assert!(result.contains("volumes:")); // other sections preserved
        assert!(result.contains("./data:/var/lib/clickhouse"));

        // Strip ports for both
        let result = executor
            .strip_ports_for_services(compose, &["clickhouse".to_string(), "keeper".to_string()]);
        assert!(!result.contains("8123:8123"));
        assert!(!result.contains("9000:9000"));
        assert!(!result.contains("9181:9181"));
    }

    #[test]
    fn test_strip_ports_no_services_matched() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"services:
  web:
    image: nginx
    ports:
      - '80:80'
"#;

        // No services to strip — output should be identical
        let result = executor.strip_ports_for_services(compose, &[]);
        assert!(result.contains("80:80"));
    }
}
