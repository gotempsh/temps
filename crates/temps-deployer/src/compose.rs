//! Docker Compose deployment executor.
//!
//! Manages multi-container deployments using `docker compose` CLI commands.
//! After `compose up`, discovers running containers, applies Temps labels,
//! and returns per-service results that get inserted into `deployment_containers`.

use bollard::Docker;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use serde_yaml::{Mapping, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
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

    #[error("Compose security policy rejected {field} for service '{service}': {reason}")]
    SecurityPolicyViolation {
        service: String,
        field: String,
        reason: String,
    },

    #[error("Failed to parse compose YAML for '{compose_source}': {reason}")]
    InvalidComposeYaml {
        compose_source: String,
        reason: String,
    },

    #[error("Compose path '{path}' rejected for field '{field}': {reason}")]
    InvalidComposePath {
        field: String,
        path: String,
        reason: String,
    },

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
        Self::validate_relative_path(
            request
                .compose_path
                .as_deref()
                .unwrap_or("docker-compose.yml"),
            "compose_path",
        )?;
        self.validate_compose_security_policy("compose file", &request.compose_content)?;
        if let Some(ref compose_override) = request.compose_override {
            self.validate_compose_security_policy("compose override", compose_override)?;
        }
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

        // Lexical path validation cannot see repository symlinks. Resolve every
        // host path from the same base directory Docker Compose uses, after the
        // checkout and compose files exist but before build/up can touch the
        // host. This closes `./data -> /` style escapes for bind mounts,
        // configs/secrets, local-driver binds, and build paths.
        Self::validate_compose_filesystem_confinement(
            &effective_dir,
            compose_file,
            "compose file",
            &request.compose_content,
        )?;
        if let Some(ref compose_override) = request.compose_override {
            Self::validate_compose_filesystem_confinement(
                &effective_dir,
                compose_file,
                "compose override",
                compose_override,
            )?;
        }

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

        // 3. Run docker compose up (pulls pre-built images, starts built + pulled).
        // If a user-provided `container_name` conflicts with an existing
        // container, let Compose report the conflict instead of deleting
        // containers outside this Temps project boundary.
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
        Self::validate_relative_path(compose_file, "compose_path")?;
        let compose_path =
            Self::confined_write_path(project_dir, Path::new(compose_file), "compose_path")?;

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
                let env_path = Self::confined_write_path(project_dir, Path::new(".env"), ".env")?;
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
            let temps_env_path =
                Self::confined_write_path(project_dir, Path::new(".env.temps"), ".env.temps")?;
            tokio::fs::write(&temps_env_path, &temps_env)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_env_path.display().to_string(),
                    reason: e.to_string(),
                })?;

            // Write Temps env override (auto-generated, injects .env.temps into every service)
            let temps_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-env.yml"),
                "docker-compose.temps-env.yml",
            )?;
            let override_content =
                self.generate_env_override(&request.compose_content, ".env.temps");
            tokio::fs::write(&temps_override_path, &override_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps security override (injects sandbox hardening into every service).
        let security_content = self.generate_security_override(&request.compose_content);
        if !security_content.is_empty() {
            let security_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-security.yml"),
                "docker-compose.temps-security.yml",
            )?;
            tokio::fs::write(&security_override_path, &security_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps labels override (injects sh.temps.* labels into every service for log collection)
        if !request.labels.is_empty() {
            let labels_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-labels.yml"),
                "docker-compose.temps-labels.yml",
            )?;
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
        // settings, so validate them (structural allow-list) before handing them to the
        // host Docker daemon — defense-in-depth alongside the value-level policy above.
        if let Some(ref user_override) = request.compose_override {
            if !user_override.trim().is_empty() {
                Self::validate_compose_override(
                    &request.project_name,
                    &request.compose_content,
                    user_override,
                )?;

                let override_path = Self::confined_write_path(
                    project_dir,
                    Path::new("docker-compose.temps-override.yml"),
                    "docker-compose.temps-override.yml",
                )?;
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

    /// Preflight security validation. Run this BEFORE tearing down the existing
    /// stack so a policy rejection does not cause downtime on the running deployment.
    pub fn preflight_validate(
        &self,
        compose_content: &str,
        compose_override: Option<&str>,
    ) -> Result<(), ComposeError> {
        self.validate_compose_security_policy("compose file", compose_content)?;
        if let Some(override_content) = compose_override {
            self.validate_compose_security_policy("compose override", override_content)?;
        }
        Ok(())
    }

    /// Filesystem-aware preflight for repository-backed Compose deployments.
    /// This runs before the old stack is torn down, while `deploy` repeats the
    /// checks immediately before build/up as defense against later changes.
    pub fn preflight_validate_filesystem(
        &self,
        project_dir: &Path,
        compose_file: &str,
        compose_content: &str,
        compose_override: Option<&str>,
    ) -> Result<(), ComposeError> {
        Self::validate_compose_filesystem_confinement(
            project_dir,
            compose_file,
            "compose file",
            compose_content,
        )?;
        if let Some(override_content) = compose_override {
            Self::validate_compose_filesystem_confinement(
                project_dir,
                compose_file,
                "compose override",
                override_content,
            )?;
        }

        // Validate all possible repository write destinations up front. The
        // write path checks are repeated at the actual write to reduce the
        // check/use window.
        for (path, field) in [
            (compose_file, "compose_path"),
            (".env", ".env"),
            (".env.temps", ".env.temps"),
            (
                "docker-compose.temps-env.yml",
                "docker-compose.temps-env.yml",
            ),
            (
                "docker-compose.temps-security.yml",
                "docker-compose.temps-security.yml",
            ),
            (
                "docker-compose.temps-labels.yml",
                "docker-compose.temps-labels.yml",
            ),
            (
                "docker-compose.temps-override.yml",
                "docker-compose.temps-override.yml",
            ),
        ] {
            Self::confined_write_path(project_dir, Path::new(path), field)?;
        }
        Ok(())
    }

    fn validate_compose_security_policy(
        &self,
        source: &str,
        compose_content: &str,
    ) -> Result<(), ComposeError> {
        if compose_content.trim().is_empty() {
            return Ok(());
        }

        let mut root: YamlValue = serde_yaml::from_str(compose_content).map_err(|e| {
            ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: e.to_string(),
            }
        })?;

        // Expand YAML merge keys (`<<`) so settings inherited from an anchor
        // (privileged, devices, volumes, ...) are visible during validation
        // instead of hiding behind the raw `<<` key. Fail closed if expansion
        // errors — otherwise inherited settings could hide from the checks below
        // while `docker compose` still applies them at runtime.
        root.apply_merge()
            .map_err(|e| ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: format!("failed to expand YAML merge keys: {e}"),
            })?;

        // Reject the top-level `include:` directive. Compose merges included
        // files (repo-controlled) into the project at runtime, but only this
        // document's `services:` are validated here — an included file could
        // reintroduce privileged services, host mounts, etc. Inline the
        // referenced services into the reviewed compose file instead.
        if let Some(root_map) = root.as_mapping() {
            if root_map.contains_key(YamlValue::String("include".to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: "include".to_string(),
                    reason: "top-level 'include' pulls in unvalidated compose files; \
                             inline the referenced services into this compose file instead"
                        .to_string(),
                });
            }
        }

        // Top-level named volumes whose driver options bind a forbidden host
        // path (e.g. `driver_opts: {type: none, o: bind, device: /}`). Service
        // mounts that reference these by name are rejected below.
        let forbidden_named_volumes = Self::forbidden_named_volumes(&root);

        // Block host files exposed through top-level configs/secrets `file:` paths.
        self.validate_top_level_files(&root, "configs")?;
        self.validate_top_level_files(&root, "secrets")?;

        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Ok(());
        };

        for (service_key, service_value) in services {
            // Service names must be quoted strings. A bare `true`/`false`/`null`
            // or numeric key parses as a non-string scalar here, so it would be
            // dropped by `parse_service_names_yaml` (which keys off `as_str()`)
            // and silently skip the injected security override, while a compose
            // parser may still treat it as a service. Fail closed instead.
            let Some(service_name) = service_key.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<non-string>".to_string(),
                    field: "services".to_string(),
                    reason: "service names must be quoted strings; non-string scalar keys \
                             (booleans, null, or numbers) are ambiguous across compose parsers \
                             and are not allowed"
                        .to_string(),
                });
            };
            // Service names are interpolated verbatim into the generated
            // `docker-compose.temps-security.yml` override. A name containing a
            // newline or `: ` would corrupt that YAML and could silently drop the
            // sandbox-hardening layer, so constrain names to the Compose spec
            // character set.
            if !Self::is_valid_service_name(service_name) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "services".to_string(),
                    reason: "service names may only contain letters, digits, '.', '_' and '-' \
                             (and must start alphanumeric); other characters can corrupt the \
                             generated security override"
                        .to_string(),
                });
            }
            let Some(service) = service_value.as_mapping() else {
                continue;
            };

            // Reject `${...}`/`$(...)` interpolation in security-guarded fields
            // first. Otherwise `network_mode: ${NET:-host}` or
            // `privileged: ${P:-true}` slip past the literal `host`/`true`
            // checks below because the YAML value is an interpolation string.
            self.reject_interpolation_in_guarded_fields(service, service_name)?;

            self.reject_bool(
                service,
                service_name,
                "privileged",
                true,
                "privileged containers can bypass the host sandbox",
            )?;
            self.reject_bool(
                service,
                service_name,
                "use_api_socket",
                true,
                "use_api_socket exposes the docker engine API socket to the container",
            )?;
            self.reject_present(
                service,
                service_name,
                "cap_add",
                "adding Linux capabilities is not allowed for compose deployments",
            )?;
            self.reject_present(
                service,
                service_name,
                "devices",
                "host device passthrough is not allowed for compose deployments",
            )?;
            self.reject_present(
                service,
                service_name,
                "device_cgroup_rules",
                "device cgroup rules can grant host device access",
            )?;
            self.reject_present(
                service,
                service_name,
                "security_opt",
                "custom security options can disable no-new-privileges or confinement",
            )?;
            self.reject_present(
                service,
                service_name,
                "gpus",
                "GPU device requests expose host accelerators and are not allowed",
            )?;
            self.reject_present(
                service,
                service_name,
                "extends",
                "extends can import privileged settings from another compose file; \
                 inline the service definition instead",
            )?;
            self.reject_present(
                service,
                service_name,
                "volumes_from",
                "volumes_from can inherit volumes from arbitrary host containers \
                 outside this deployment (e.g. other tenants' or Temps infrastructure \
                 containers)",
            )?;
            self.reject_present(
                service,
                service_name,
                "sysctls",
                "setting kernel parameters (sysctls) is not allowed for compose deployments",
            )?;
            self.reject_present(
                service,
                service_name,
                "group_add",
                "adding supplementary groups (e.g. the docker group) can escalate host access",
            )?;
            self.reject_present(
                service,
                service_name,
                "cgroup_parent",
                "cgroup_parent can place the container in an arbitrary host cgroup",
            )?;
            self.reject_present(
                service,
                service_name,
                "runtime",
                "selecting an alternate OCI runtime can bypass the enforced container sandbox",
            )?;
            self.reject_present(
                service,
                service_name,
                "oom_kill_disable",
                "disabling the OOM killer can turn container memory pressure into a host-wide denial of service",
            )?;
            self.reject_present(
                service,
                service_name,
                "shm_size",
                "custom shared-memory sizing is not allowed because aggregate tmpfs-backed memory is not host-bounded",
            )?;
            self.reject_present(
                service,
                service_name,
                "tmpfs",
                "user-defined tmpfs mounts are not allowed because their aggregate host-memory use is not bounded",
            )?;
            self.reject_present(
                service,
                service_name,
                "ulimits",
                "overriding container ulimits is not allowed for compose deployments",
            )?;
            self.reject_host_namespace(service, service_name, "network_mode")?;
            self.reject_host_namespace(service, service_name, "pid")?;
            self.reject_host_namespace(service, service_name, "ipc")?;
            self.reject_host_namespace(service, service_name, "cgroup")?;
            self.reject_host_namespace(service, service_name, "uts")?;
            self.reject_host_namespace(service, service_name, "userns_mode")?;
            self.reject_deploy_devices(service, service_name)?;
            self.validate_build_options(service, service_name)?;
            self.validate_service_volumes(service, service_name, &forbidden_named_volumes)?;
        }

        Ok(())
    }

    /// Collect names of top-level named volumes whose `driver_opts.device`
    /// binds a forbidden host path. These are local-bind volumes that smuggle a
    /// host path past the service-source check.
    fn forbidden_named_volumes(root: &YamlValue) -> HashSet<String> {
        let mut forbidden = HashSet::new();
        let Some(volumes) = root.get("volumes").and_then(YamlValue::as_mapping) else {
            return forbidden;
        };
        for (name, def) in volumes {
            let Some(name) = name.as_str() else {
                continue;
            };
            let Some(def_map) = def.as_mapping() else {
                continue;
            };

            // A non-`local` volume driver invokes an external volume plugin
            // (NFS/CIFS clients, cloud plugins) that can mount attacker-controlled
            // remote or host filesystems into the container.
            if let Some(driver) = def_map
                .get(YamlValue::String("driver".to_string()))
                .and_then(YamlValue::as_str)
            {
                if driver != "local" {
                    forbidden.insert(name.to_string());
                    continue;
                }
            }

            let Some(driver_opts) = def_map
                .get(YamlValue::String("driver_opts".to_string()))
                .and_then(YamlValue::as_mapping)
            else {
                continue;
            };

            // A dangerous bind `device` (local-driver bind mount of a host path).
            if let Some(device) = driver_opts
                .get(YamlValue::String("device".to_string()))
                .and_then(YamlValue::as_str)
            {
                if Self::is_dangerous_host_path(device) {
                    forbidden.insert(name.to_string());
                    continue;
                }
            }

            // A remote/network filesystem `type` (e.g. `type: nfs` with an
            // `addr=` option) mounts an off-host filesystem even under the
            // `local` driver.
            if let Some(fs_type) = driver_opts
                .get(YamlValue::String("type".to_string()))
                .and_then(YamlValue::as_str)
            {
                const NETWORK_FS: &[&str] =
                    &["nfs", "nfs4", "cifs", "smb", "smbfs", "glusterfs", "ceph"];
                if NETWORK_FS.contains(&fs_type.to_ascii_lowercase().as_str()) {
                    forbidden.insert(name.to_string());
                }
            }
        }
        forbidden
    }

    /// Reject top-level `configs.*.file` / `secrets.*.file` entries that point at
    /// forbidden or project-escaping host paths (e.g. `/etc/passwd`).
    fn validate_top_level_files(&self, root: &YamlValue, key: &str) -> Result<(), ComposeError> {
        let Some(map) = root.get(key).and_then(YamlValue::as_mapping) else {
            return Ok(());
        };
        for (name, def) in map {
            let name = name.as_str().unwrap_or("<unknown>");
            let Some(def_map) = def.as_mapping() else {
                continue;
            };
            if let Some(file) = def_map
                .get(YamlValue::String("file".to_string()))
                .and_then(YamlValue::as_str)
            {
                if Self::is_dangerous_host_path(file) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("{key}.{name}"),
                        field: format!("{key}.file"),
                        reason: format!("host file '{file}' exposed through {key} is not allowed"),
                    });
                }
            }
        }
        Ok(())
    }

    /// Reject privileged build options before `docker compose build` runs them.
    fn validate_build_options(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let Some(build) = service.get(YamlValue::String("build".to_string())) else {
            return Ok(());
        };
        // Short form (`build: .`) is itself a context path. It needs the same
        // lexical and canonical checks as long-form `build.context`.
        if let Some(context) = build.as_str() {
            if !Self::is_remote_build_context(context) && Self::is_dangerous_host_path(context) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.context".to_string(),
                    reason: "build context must be a confined relative path; absolute, project-escaping, or interpolated values are not allowed".to_string(),
                });
            }
            return Ok(());
        }
        let Some(build_map) = build.as_mapping() else {
            return Ok(());
        };

        if build_map
            .get(YamlValue::String("privileged".to_string()))
            .and_then(YamlValue::as_bool)
            == Some(true)
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.privileged".to_string(),
                reason: "privileged build steps can escape the build sandbox".to_string(),
            });
        }
        if build_map.contains_key(YamlValue::String("entitlements".to_string())) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.entitlements".to_string(),
                reason: "build entitlements (e.g. security.insecure) grant host access".to_string(),
            });
        }
        if build_map
            .get(YamlValue::String("network".to_string()))
            .and_then(YamlValue::as_str)
            == Some("host")
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.network".to_string(),
                reason: "host network during build is not allowed".to_string(),
            });
        }
        if build_map.contains_key(YamlValue::String("ssh".to_string())) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.ssh".to_string(),
                reason: "build.ssh forwards the host SSH agent into the build and can leak \
                         host keys"
                    .to_string(),
            });
        }
        for field in ["shm_size", "ulimits"] {
            if build_map.contains_key(YamlValue::String(field.to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: format!("build.{field}"),
                    reason: format!(
                        "build.{field} is not allowed because build resource overrides are not host-bounded"
                    ),
                });
            }
        }
        // `context` and `dockerfile` become the Docker build context / Dockerfile
        // path. An absolute or project-escaping host path (or an interpolated one)
        // would send arbitrary host directories into the build (`COPY . /`), so
        // confine them the same way as bind sources.
        for field in ["context", "dockerfile"] {
            if let Some(value) = build_map
                .get(YamlValue::String(field.to_string()))
                .and_then(YamlValue::as_str)
            {
                if field == "context" && Self::is_remote_build_context(value) {
                    continue;
                }
                if Self::is_dangerous_host_path(value) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: format!("build.{field}"),
                        reason: format!(
                            "build.{field} must be a confined relative path; absolute, \
                             project-escaping, or interpolated values are not allowed"
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// Reject `deploy.resources.reservations.devices` — the long-form equivalent
    /// of the already-blocked `gpus:` short form. Docker Compose translates
    /// `gpus:` into exactly this structure, so leaving it unchecked allows the
    /// same host-accelerator passthrough the `gpus` guard is meant to prevent.
    fn reject_deploy_devices(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let has_devices = service
            .get(YamlValue::String("deploy".to_string()))
            .and_then(YamlValue::as_mapping)
            .and_then(|d| d.get(YamlValue::String("resources".to_string())))
            .and_then(YamlValue::as_mapping)
            .and_then(|r| r.get(YamlValue::String("reservations".to_string())))
            .and_then(YamlValue::as_mapping)
            .is_some_and(|res| res.contains_key(YamlValue::String("devices".to_string())));
        if has_devices {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "deploy.resources.reservations.devices".to_string(),
                reason: "device reservations expose host accelerators/devices (equivalent to \
                         the blocked 'gpus') and are not allowed"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Whether a Compose service name is safe to interpolate into generated YAML.
    /// Restricts to the Compose spec character set (alphanumeric start, then
    /// letters/digits/`.`/`_`/`-`), rejecting names with newlines, colons, or
    /// spaces that could corrupt the generated security override.
    fn is_valid_service_name(name: &str) -> bool {
        let mut chars = name.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphanumeric() => {}
            _ => return false,
        }
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    }

    /// Service fields whose value (or any nested sequence/mapping value) must
    /// never contain `${...}` / `$(...)` interpolation. An attacker could
    /// otherwise smuggle host/privileged access past the static checks via env
    /// defaults like `network_mode: ${NET:-host}` or `privileged: ${P:-true}`,
    /// because the literal YAML value is an interpolation string rather than
    /// `host`/`true`.
    const INTERPOLATION_GUARDED_FIELDS: &'static [&'static str] = &[
        "privileged",
        "use_api_socket",
        "network_mode",
        "pid",
        "ipc",
        "userns_mode",
        "uts",
        "cgroup",
        "cap_add",
        "devices",
        "volumes",
        "security_opt",
        "group_add",
        "device_cgroup_rules",
        "volumes_from",
        "runtime",
        "oom_kill_disable",
        "shm_size",
        "tmpfs",
        "ulimits",
    ];

    /// Reject `${...}` / `$(...)` interpolation appearing anywhere within a
    /// security-guarded field's value (recursing into sequences and mappings).
    fn reject_interpolation_in_guarded_fields(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        for field in Self::INTERPOLATION_GUARDED_FIELDS {
            let Some(value) = service.get(YamlValue::String((*field).to_string())) else {
                continue;
            };
            if Self::value_contains_interpolation(value) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: (*field).to_string(),
                    reason: format!(
                        "'${{...}}' interpolation in guarded field '{field}' is not allowed; \
                         it can smuggle host/privileged access past static validation"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Recursively check whether a YAML value (string, sequence, or mapping)
    /// contains shell/compose variable interpolation.
    fn value_contains_interpolation(value: &YamlValue) -> bool {
        match value {
            YamlValue::String(s) => Self::contains_interpolation(s),
            YamlValue::Sequence(seq) => seq.iter().any(Self::value_contains_interpolation),
            YamlValue::Mapping(map) => map.values().any(Self::value_contains_interpolation),
            _ => false,
        }
    }

    fn reject_bool(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
        field: &str,
        rejected: bool,
        reason: &str,
    ) -> Result<(), ComposeError> {
        if service
            .get(YamlValue::String(field.to_string()))
            .and_then(YamlValue::as_bool)
            == Some(rejected)
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: reason.to_string(),
            });
        }
        Ok(())
    }

    fn reject_present(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
        field: &str,
        reason: &str,
    ) -> Result<(), ComposeError> {
        if service.contains_key(YamlValue::String(field.to_string())) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: reason.to_string(),
            });
        }
        Ok(())
    }

    fn reject_host_namespace(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
        field: &str,
    ) -> Result<(), ComposeError> {
        let Some(value) = service.get(YamlValue::String(field.to_string())) else {
            return Ok(());
        };
        let Some(mode) = value.as_str() else {
            return Ok(());
        };
        if mode == "host" {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: "host namespace sharing is not allowed for compose deployments".to_string(),
            });
        }
        // `container:<name|id>` joins the namespace of an arbitrary container on
        // the host — including other tenants' and Temps' own infrastructure
        // containers. Only intra-project `service:<name>` sharing is acceptable.
        if mode.starts_with("container:") {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: "joining another container's namespace via 'container:' is not allowed; \
                         it can target containers outside this deployment"
                    .to_string(),
            });
        }
        Ok(())
    }

    fn validate_service_volumes(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
        forbidden_named_volumes: &HashSet<String>,
    ) -> Result<(), ComposeError> {
        let Some(volumes) = service.get(YamlValue::String("volumes".to_string())) else {
            return Ok(());
        };
        let Some(entries) = volumes.as_sequence() else {
            return Ok(());
        };

        for entry in entries {
            if entry
                .as_mapping()
                .and_then(|mapping| mapping.get(YamlValue::String("type".to_string())))
                .and_then(YamlValue::as_str)
                == Some("tmpfs")
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "volumes".to_string(),
                    reason: "user-defined tmpfs mounts are not allowed because their aggregate host-memory use is not bounded".to_string(),
                });
            }
            let Some(source) = Self::volume_source(entry) else {
                continue;
            };

            // Reject interpolation in bind sources. `${HOST_ROOT:-/}` cannot be
            // statically validated, so a `/`-style check is trivially bypassed.
            if Self::contains_interpolation(&source) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "volumes".to_string(),
                    reason: format!(
                        "interpolation in bind mount source '{source}' is not allowed; \
                         it cannot be statically validated"
                    ),
                });
            }

            // A bare name (no path separators, not relative) is a named volume
            // reference, not a host bind. It is only dangerous if the named
            // volume binds a forbidden host path via driver_opts.
            if Self::is_named_volume_ref(&source) {
                if forbidden_named_volumes.contains(&source) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes".to_string(),
                        reason: format!(
                            "named volume '{source}' binds a forbidden host path via driver_opts"
                        ),
                    });
                }
                continue;
            }

            // Host bind mount: normalize `..`/`.` and reject absolute host paths
            // outside the sandbox or relative paths that escape the project dir.
            if Self::is_dangerous_host_path(&source) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "volumes".to_string(),
                    reason: format!("host bind mount source '{source}' is not allowed"),
                });
            }
        }

        Ok(())
    }

    fn volume_source(entry: &YamlValue) -> Option<String> {
        if let Some(value) = entry.as_str() {
            return value.split(':').next().map(str::to_string);
        }

        let mapping = entry.as_mapping()?;
        mapping
            .get(YamlValue::String("source".to_string()))
            .and_then(YamlValue::as_str)
            .map(str::to_string)
    }

    /// Whether a string contains compose/shell variable interpolation.
    ///
    /// Docker Compose interpolates `${VAR}`, `$(cmd)`, AND the braceless `$VAR`
    /// form; `$$` is an escaped literal dollar. Matching only `${`/`$(` let
    /// `network_mode: $NET` or `volumes: [$SRC:/host]` slip past the guard and
    /// resolve to attacker-controlled values from the repo `.env` at runtime,
    /// so treat any real `$` sigil as interpolation.
    fn contains_interpolation(value: &str) -> bool {
        let bytes = value.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                match bytes.get(i + 1).copied() {
                    // `$$` escapes a literal dollar — not interpolation.
                    Some(b'$') => {
                        i += 2;
                        continue;
                    }
                    // `${VAR}` / `$(cmd)` / `$VAR` are all interpolation.
                    Some(b'{') | Some(b'(') => return true,
                    Some(c) if c.is_ascii_alphabetic() || c == b'_' => return true,
                    _ => {}
                }
            }
            i += 1;
        }
        false
    }

    /// A bare volume name (no path separators and not relative) references a
    /// named volume rather than a host bind path.
    fn is_named_volume_ref(source: &str) -> bool {
        !source.contains('/') && !source.starts_with('.') && !source.is_empty()
    }

    /// Whether a host path is dangerous: it interpolates, is any absolute host
    /// path, or escapes the compose project directory via `..`. Paths are
    /// normalized lexically first so `../../etc` and `/tmp/../etc` cannot bypass
    /// the block.
    ///
    /// Bind sources in user compose must be relative to the per-project working
    /// directory (compose runs with `current_dir(project_dir)`). Absolute host
    /// paths are rejected unconditionally — there is no allowed absolute prefix,
    /// because a world-writable location like `/tmp` can hold other tenants'
    /// project artifacts (`.env.temps`, encryption keys) when the data dir lives
    /// under it, and shared host paths are exactly the escape this guard exists
    /// to prevent.
    fn is_dangerous_host_path(source: &str) -> bool {
        if Self::contains_interpolation(source) {
            return true;
        }
        let normalized = Self::lexically_normalize(source);
        // Relative path that climbs above the project directory.
        if normalized == ".." || normalized.starts_with("../") {
            return true;
        }
        // Any absolute host path.
        if normalized.starts_with('/') {
            return true;
        }
        false
    }

    /// Lexically normalize a path: collapse `.` and resolve `..` without
    /// touching the filesystem. Relative `..` that escapes the base is kept as
    /// a leading `..` so callers can detect project-directory escape.
    fn lexically_normalize(source: &str) -> String {
        let is_absolute = source.starts_with('/');
        let mut stack: Vec<&str> = Vec::new();
        for comp in source.split('/') {
            match comp {
                "" | "." => {}
                ".." => match stack.last() {
                    Some(&last) if last != ".." => {
                        stack.pop();
                    }
                    _ => {
                        // For absolute paths, `..` at the root is a no-op.
                        if !is_absolute {
                            stack.push("..");
                        }
                    }
                },
                other => stack.push(other),
            }
        }
        let joined = stack.join("/");
        if is_absolute {
            format!("/{joined}")
        } else if joined.is_empty() {
            ".".to_string()
        } else {
            joined
        }
    }

    /// Resolve every repository-backed path used by Compose and verify the
    /// canonical target remains inside the checked-out project. Relative-path
    /// string checks alone cannot detect a committed symlink such as
    /// `data -> /`, which Docker follows when it opens a bind source.
    fn validate_compose_filesystem_confinement(
        project_dir: &Path,
        compose_file: &str,
        source: &str,
        compose_content: &str,
    ) -> Result<(), ComposeError> {
        if compose_content.trim().is_empty() {
            return Ok(());
        }
        Self::validate_relative_path(compose_file, "compose_path")?;

        let canonical_root =
            std::fs::canonicalize(project_dir).map_err(|e| ComposeError::InvalidComposePath {
                field: "compose project directory".to_string(),
                path: project_dir.display().to_string(),
                reason: format!("failed to canonicalize project directory: {e}"),
            })?;
        let compose_path = canonical_root.join(compose_file);
        let compose_base =
            compose_path
                .parent()
                .ok_or_else(|| ComposeError::InvalidComposePath {
                    field: "compose_path".to_string(),
                    path: compose_file.to_string(),
                    reason: "compose path has no parent directory".to_string(),
                })?;
        let compose_base =
            std::fs::canonicalize(compose_base).map_err(|e| ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_file.to_string(),
                reason: format!("failed to canonicalize compose base directory: {e}"),
            })?;
        if !compose_base.starts_with(&canonical_root) {
            return Err(ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_file.to_string(),
                reason: "compose base directory resolves outside the project directory".to_string(),
            });
        }

        let mut root: YamlValue = serde_yaml::from_str(compose_content).map_err(|e| {
            ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: e.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|e| ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: format!("failed to expand YAML merge keys: {e}"),
            })?;

        for key in ["configs", "secrets"] {
            let Some(files) = root.get(key).and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, definition) in files {
                let name = name.as_str().unwrap_or("<unknown>");
                let Some(file) = definition
                    .as_mapping()
                    .and_then(|mapping| mapping.get(YamlValue::String("file".to_string())))
                    .and_then(YamlValue::as_str)
                else {
                    continue;
                };
                Self::canonicalize_confined_existing_path(
                    &canonical_root,
                    &compose_base,
                    file,
                    &format!("{key}.{name}"),
                    &format!("{key}.file"),
                )?;
            }
        }

        // Local-driver bind volumes can hide a repository symlink in
        // `driver_opts.device` and then expose it through an apparently named
        // volume. Validate relative bind devices against the same compose base.
        if let Some(volumes) = root.get("volumes").and_then(YamlValue::as_mapping) {
            for (name, definition) in volumes {
                let name = name.as_str().unwrap_or("<unknown>");
                let Some(options) = definition
                    .as_mapping()
                    .and_then(|mapping| mapping.get(YamlValue::String("driver_opts".to_string())))
                    .and_then(YamlValue::as_mapping)
                else {
                    continue;
                };
                let is_bind = options
                    .get(YamlValue::String("type".to_string()))
                    .and_then(YamlValue::as_str)
                    == Some("none")
                    || options
                        .get(YamlValue::String("o".to_string()))
                        .and_then(YamlValue::as_str)
                        .is_some_and(|value| value.split(',').any(|option| option == "bind"));
                if !is_bind {
                    continue;
                }
                if let Some(device) = options
                    .get(YamlValue::String("device".to_string()))
                    .and_then(YamlValue::as_str)
                {
                    Self::canonicalize_confined_existing_path(
                        &canonical_root,
                        &compose_base,
                        device,
                        &format!("volumes.{name}"),
                        "volumes.driver_opts.device",
                    )?;
                }
            }
        }

        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Ok(());
        };
        for (service_name, definition) in services {
            let service_name = service_name.as_str().unwrap_or("<unknown>");
            let Some(service) = definition.as_mapping() else {
                continue;
            };

            if let Some(entries) = service
                .get(YamlValue::String("volumes".to_string()))
                .and_then(YamlValue::as_sequence)
            {
                for entry in entries {
                    let Some(bind_source) = Self::volume_source(entry) else {
                        continue;
                    };
                    if Self::is_named_volume_ref(&bind_source) {
                        continue;
                    }
                    Self::canonicalize_confined_existing_path(
                        &canonical_root,
                        &compose_base,
                        &bind_source,
                        service_name,
                        "volumes",
                    )?;
                }
            }

            Self::validate_build_filesystem_paths(
                &canonical_root,
                &compose_base,
                service,
                service_name,
            )?;
        }

        Ok(())
    }

    fn canonicalize_confined_existing_path(
        canonical_root: &Path,
        base_dir: &Path,
        raw_path: &str,
        service: &str,
        field: &str,
    ) -> Result<PathBuf, ComposeError> {
        if Self::is_dangerous_host_path(raw_path) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' must be a confined, non-interpolated relative path"
                ),
            });
        }

        let candidate = base_dir.join(raw_path);
        let canonical = std::fs::canonicalize(&candidate).map_err(|e| {
            ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' must already exist and be canonicalizable before deployment: {e}"
                ),
            }
        })?;
        if !canonical.starts_with(canonical_root) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' resolves outside the compose project directory"
                ),
            });
        }
        Ok(canonical)
    }

    fn validate_build_filesystem_paths(
        canonical_root: &Path,
        compose_base: &Path,
        service: &Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let Some(build) = service.get(YamlValue::String("build".to_string())) else {
            return Ok(());
        };

        let (context, dockerfile, has_inline_dockerfile) = if let Some(context) = build.as_str() {
            (context, None, false)
        } else if let Some(build_map) = build.as_mapping() {
            let context = build_map
                .get(YamlValue::String("context".to_string()))
                .and_then(YamlValue::as_str)
                .unwrap_or(".");
            let dockerfile = build_map
                .get(YamlValue::String("dockerfile".to_string()))
                .and_then(YamlValue::as_str);
            let has_inline =
                build_map.contains_key(YamlValue::String("dockerfile_inline".to_string()));
            (context, dockerfile, has_inline)
        } else {
            return Ok(());
        };

        // Remote Git/HTTP build contexts do not resolve against the host
        // checkout. Local/file schemes are intentionally not exempted.
        if Self::is_remote_build_context(context) {
            return Ok(());
        }

        let canonical_context = Self::canonicalize_confined_existing_path(
            canonical_root,
            compose_base,
            context,
            service_name,
            "build.context",
        )?;
        if let Some(dockerfile) = dockerfile {
            Self::canonicalize_confined_existing_path(
                canonical_root,
                &canonical_context,
                dockerfile,
                service_name,
                "build.dockerfile",
            )?;
        } else if !has_inline_dockerfile {
            let default_dockerfile = canonical_context.join("Dockerfile");
            if default_dockerfile.exists() {
                Self::canonicalize_confined_existing_path(
                    canonical_root,
                    &canonical_context,
                    "Dockerfile",
                    service_name,
                    "build.dockerfile",
                )?;
            }
        }

        Ok(())
    }

    fn is_remote_build_context(context: &str) -> bool {
        ["https://", "http://", "git://", "ssh://"]
            .iter()
            .any(|prefix| context.starts_with(prefix))
            || context.starts_with("git@")
    }

    /// Confine a user-supplied path (e.g. `compose_path`) to the project
    /// checkout directory: reject empty values, absolute paths, and any `..`
    /// / root / prefix component that would escape the project tree.
    fn validate_relative_path(path: &str, field: &str) -> Result<(), ComposeError> {
        let candidate = Path::new(path);
        if candidate.as_os_str().is_empty() || candidate.is_absolute() {
            return Err(ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: path.to_string(),
                reason: "must be a non-empty relative path".to_string(),
            });
        }
        if candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: path.to_string(),
                reason: "must not contain '..' or absolute/root path components".to_string(),
            });
        }
        Ok(())
    }

    /// Return a canonical destination for a file Temps writes into the checked
    /// out repository, rejecting committed symlinks in either the destination
    /// or any parent directory. Git preserves symlinks, so a repository could
    /// otherwise make `.env.temps -> /host/file` and turn a normal deployment
    /// into an arbitrary host-file overwrite.
    fn confined_write_path(
        project_dir: &Path,
        relative_path: &Path,
        field: &str,
    ) -> Result<PathBuf, ComposeError> {
        let relative = relative_path
            .to_str()
            .ok_or_else(|| ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: relative_path.display().to_string(),
                reason: "path must be valid UTF-8".to_string(),
            })?;
        Self::validate_relative_path(relative, field)?;

        let canonical_root =
            std::fs::canonicalize(project_dir).map_err(|e| ComposeError::FileWriteFailed {
                path: project_dir.display().to_string(),
                reason: format!("failed to canonicalize compose project directory: {e}"),
            })?;

        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        let mut canonical_parent = canonical_root.clone();
        for component in parent.components() {
            match component {
                Component::CurDir => continue,
                Component::Normal(name) => canonical_parent.push(name),
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ComposeError::InvalidComposePath {
                        field: field.to_string(),
                        path: relative_path.display().to_string(),
                        reason: "write destination must remain inside the compose project"
                            .to_string(),
                    });
                }
            }

            match std::fs::symlink_metadata(&canonical_parent) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<compose-files>".to_string(),
                        field: field.to_string(),
                        reason: format!(
                            "refusing to write through repository symlink '{}'",
                            canonical_parent.display()
                        ),
                    });
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(ComposeError::FileWriteFailed {
                        path: canonical_parent.display().to_string(),
                        reason: "compose write parent exists but is not a directory".to_string(),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&canonical_parent).map_err(|e| {
                        ComposeError::FileWriteFailed {
                            path: canonical_parent.display().to_string(),
                            reason: format!("failed to create confined compose directory: {e}"),
                        }
                    })?;
                }
                Err(error) => {
                    return Err(ComposeError::FileWriteFailed {
                        path: canonical_parent.display().to_string(),
                        reason: format!("failed to inspect compose write parent: {error}"),
                    });
                }
            }
        }

        let canonical_parent = std::fs::canonicalize(&canonical_parent).map_err(|e| {
            ComposeError::FileWriteFailed {
                path: canonical_parent.display().to_string(),
                reason: format!("failed to canonicalize compose write parent: {e}"),
            }
        })?;
        if !canonical_parent.starts_with(&canonical_root) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<compose-files>".to_string(),
                field: field.to_string(),
                reason: "compose write destination resolves outside the project directory"
                    .to_string(),
            });
        }

        let file_name =
            relative_path
                .file_name()
                .ok_or_else(|| ComposeError::InvalidComposePath {
                    field: field.to_string(),
                    path: relative_path.display().to_string(),
                    reason: "write destination must name a file".to_string(),
                })?;
        let destination = canonical_parent.join(file_name);
        match std::fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<compose-files>".to_string(),
                    field: field.to_string(),
                    reason: format!(
                        "refusing to overwrite repository symlink '{}'",
                        destination.display()
                    ),
                });
            }
            Ok(metadata) if metadata.is_dir() => {
                return Err(ComposeError::FileWriteFailed {
                    path: destination.display().to_string(),
                    reason: "compose write destination is a directory".to_string(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ComposeError::FileWriteFailed {
                    path: destination.display().to_string(),
                    reason: format!("failed to inspect compose write destination: {error}"),
                });
            }
        }

        Ok(destination)
    }

    /// Structural allow-list for inline compose overrides. Complements the
    /// value-level `validate_compose_security_policy`: an inline override may
    /// only modify services that already exist in the base compose file, may not
    /// introduce top-level keys other than `services`, and may not use
    /// host-affecting service keys (privileged, network_mode, volumes, ...).
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
        let mut compose =
            serde_yaml::from_str::<Value>(content).map_err(|e| ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("failed to parse {label} YAML: {e}"),
            })?;
        compose
            .apply_merge()
            .map_err(|e| ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("failed to expand YAML merge keys in {label}: {e}"),
            })?;
        Ok(compose)
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
            "volumes_from",
            "group_add",
            "runtime",
            "oom_kill_disable",
            "shm_size",
            "tmpfs",
            "ulimits",
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

        // Include Temps security override LAST so its sandbox hardening
        // (cap_drop, no-new-privileges, pids_limit, init) wins over anything a
        // user/preset override tried to weaken. Compose applies `-f` files in
        // order, with later files overriding earlier ones.
        let security_override = project_dir.join("docker-compose.temps-security.yml");
        if security_override.exists() {
            cmd.args(["-f", "docker-compose.temps-security.yml"]);
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

    /// Generate a docker-compose override that applies the same baseline sandboxing
    /// used by the single-container Docker runtime.
    fn generate_security_override(&self, compose_content: &str) -> String {
        // Enumerate service names from the parsed YAML mapping so inline
        // mappings (`web: {image: nginx}`), anchors (`web: &app`), and merge
        // keys are all hardened, not just lines that end in `:`.
        let services = self.parse_service_names_yaml(compose_content);

        if services.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in &services {
            override_yaml.push_str(&format!("  {}:\n", service));
            // Applied last in the `-f` order, so `privileged: false` here wins
            // over anything that smuggled `privileged: true` past validation
            // (e.g. via runtime interpolation) as a last line of defense.
            override_yaml.push_str("    privileged: false\n");
            override_yaml.push_str("    cap_drop:\n");
            override_yaml.push_str("      - ALL\n");
            override_yaml.push_str("    security_opt:\n");
            override_yaml.push_str("      - no-new-privileges:true\n");
            override_yaml.push_str("    pids_limit: 512\n");
            override_yaml.push_str("    init: true\n");
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

    /// Enumerate service names from parsed compose YAML (with merge keys
    /// expanded). Falls back to the line-based parser if the content is not
    /// valid YAML or has no `services:` mapping.
    fn parse_service_names_yaml(&self, compose_content: &str) -> Vec<String> {
        let mut root: YamlValue = match serde_yaml::from_str(compose_content) {
            Ok(value) => value,
            Err(_) => return self.parse_service_names(compose_content),
        };
        let _ = root.apply_merge();
        match root.get("services").and_then(YamlValue::as_mapping) {
            Some(services) => {
                let names: Vec<String> = services
                    .keys()
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect();
                if names.is_empty() {
                    self.parse_service_names(compose_content)
                } else {
                    names
                }
            }
            None => self.parse_service_names(compose_content),
        }
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
            "volumes_from: ['container:temps-db']",
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
    fn test_validate_compose_security_policy_rejects_privileged_host_escape() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  pwn:
    image: alpine
    privileged: true
    network_mode: host
    pid: host
    cap_add:
      - SYS_ADMIN
    devices:
      - /dev/kmsg:/dev/kmsg
    volumes:
      - /:/host:rw
"#;

        let error = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert!(matches!(
            error,
            ComposeError::SecurityPolicyViolation { field, .. } if field == "privileged"
        ));
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_docker_socket_mount() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  worker:
    image: alpine
    volumes:
      - type: bind
        source: /var/run/docker.sock
        target: /var/run/docker.sock
"#;

        let error = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert!(matches!(
            error,
            ComposeError::SecurityPolicyViolation { field, .. } if field == "volumes"
        ));
    }

    #[test]
    fn test_generate_security_override() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  web:
    image: nginx
  worker:
    image: alpine
"#;
        let override_yaml = executor.generate_security_override(compose);

        assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
        assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 2);
        assert_eq!(override_yaml.matches("pids_limit: 512").count(), 2);
        assert_eq!(override_yaml.matches("init: true").count(), 2);
    }

    /// Build an executor for tests, skipping when Docker is unavailable.
    fn test_executor() -> Option<ComposeExecutor> {
        let docker = Docker::connect_with_defaults().ok()?;
        Some(ComposeExecutor::new(
            Arc::new(docker),
            PathBuf::from("/tmp/test"),
        ))
    }

    fn violation_field(err: ComposeError) -> String {
        match err {
            ComposeError::SecurityPolicyViolation { field, .. } => field,
            other => panic!("expected SecurityPolicyViolation, got {other:?}"),
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_interpolated_bind_source() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  pwn:
    image: alpine
    volumes:
      - "${HOST_ROOT:-/}:/host:rw"
"#;
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes");
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_extends() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  app:
    image: alpine
    extends:
      file: malicious.yml
      service: privileged_base
"#;
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "extends");
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_use_api_socket() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  app:
    image: alpine
    use_api_socket: true
"#;
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "use_api_socket");
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_relative_escape_bind() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  pwn:
    image: alpine
    volumes:
      - ../../../../etc:/host:rw
"#;
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes");

        // A relative path that stays inside the project dir is allowed.
        let ok = r#"
services:
  app:
    image: alpine
    volumes:
      - ./data:/data:rw
"#;
        assert!(executor
            .validate_compose_security_policy("compose file", ok)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_privileged_build_options() {
        let Some(executor) = test_executor() else {
            return;
        };
        for compose in [
            "services:\n  app:\n    build:\n      context: .\n      privileged: true\n",
            "services:\n  app:\n    build:\n      context: .\n      network: host\n",
            "services:\n  app:\n    build:\n      context: .\n      entitlements:\n        - security.insecure\n",
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert!(violation_field(err).starts_with("build."));
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_named_volume_driver_device() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  pwn:
    image: alpine
    volumes:
      - hostroot:/host
volumes:
  hostroot:
    driver_opts:
      type: none
      o: bind
      device: /
"#;
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes");
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_configs_and_secrets_files() {
        let Some(executor) = test_executor() else {
            return;
        };
        let configs = r#"
services:
  app:
    image: alpine
configs:
  hostfile:
    file: /etc/passwd
"#;
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", configs)
                    .unwrap_err()
            ),
            "configs.file"
        );

        let secrets = r#"
services:
  app:
    image: alpine
secrets:
  hostsecret:
    file: ../../../../etc/shadow
"#;
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", secrets)
                    .unwrap_err()
            ),
            "secrets.file"
        );
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_remaining_host_namespaces() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (field, compose) in [
            (
                "cgroup",
                "services:\n  app:\n    image: alpine\n    cgroup: host\n",
            ),
            (
                "userns_mode",
                "services:\n  app:\n    image: alpine\n    userns_mode: \"host\"\n",
            ),
            (
                "uts",
                "services:\n  app:\n    image: alpine\n    uts: \"host\"\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), field);
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_gpus() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = "services:\n  app:\n    image: alpine\n    gpus: all\n";
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "gpus");
    }

    #[test]
    fn test_validate_compose_security_policy_resolves_merge_keys() {
        let Some(executor) = test_executor() else {
            return;
        };
        // The privileged setting is inherited via a `<<` merge key from an anchor.
        let compose = r#"
x-base: &base
  privileged: true
services:
  app:
    image: alpine
    <<: *base
"#;
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "privileged");
    }

    #[test]
    fn test_generate_security_override_inline_and_anchor_services() {
        let Some(executor) = test_executor() else {
            return;
        };
        // Inline mapping and anchor service definitions that the old
        // line-based parser missed.
        let compose = r#"
services:
  web: { image: nginx }
  worker: &app
    image: alpine
"#;
        let override_yaml = executor.generate_security_override(compose);
        assert!(override_yaml.contains("web:"));
        assert!(override_yaml.contains("worker:"));
        assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
        assert_eq!(override_yaml.matches("init: true").count(), 2);
    }

    #[test]
    fn test_lexically_normalize() {
        assert_eq!(
            ComposeExecutor::lexically_normalize("../../../../etc"),
            "../../../../etc"
        );
        assert_eq!(ComposeExecutor::lexically_normalize("/tmp/../etc"), "/etc");
        assert_eq!(ComposeExecutor::lexically_normalize("./data"), "data");
        assert_eq!(ComposeExecutor::lexically_normalize("/"), "/");
        assert!(ComposeExecutor::is_dangerous_host_path("/tmp/../etc"));
        assert!(ComposeExecutor::is_dangerous_host_path("../escape"));
        assert!(!ComposeExecutor::is_dangerous_host_path("./data"));
        // All absolute host paths are rejected — including /tmp, which is
        // world-writable and can hold other tenants' project artifacts.
        assert!(ComposeExecutor::is_dangerous_host_path("/tmp/ok"));
        assert!(ComposeExecutor::is_dangerous_host_path(
            "/tmp/test/compose/victim"
        ));
        assert!(ComposeExecutor::is_dangerous_host_path("/etc/passwd"));
        assert!(ComposeExecutor::is_dangerous_host_path("/"));
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_interpolation_bypass() {
        let Some(executor) = test_executor() else {
            return;
        };

        // network_mode via env default would resolve to `host` at runtime but
        // the literal value is `${NET_MODE:-host}`, bypassing the `host` check.
        let net = "services:\n  web:\n    image: alpine\n    network_mode: ${NET_MODE:-host}\n";
        let err = executor
            .validate_compose_security_policy("compose file", net)
            .unwrap_err();
        assert_eq!(violation_field(err), "network_mode");

        // privileged via env default bypasses the `as_bool()` check.
        let priv_compose = "services:\n  web:\n    image: alpine\n    privileged: ${P:-true}\n";
        let err = executor
            .validate_compose_security_policy("compose file", priv_compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "privileged");

        // $(...) command-substitution form inside a guarded sequence field.
        let grp = "services:\n  web:\n    image: alpine\n    group_add:\n      - $(id -g docker)\n";
        let err = executor
            .validate_compose_security_policy("compose file", grp)
            .unwrap_err();
        assert_eq!(violation_field(err), "group_add");

        // userns_mode via interpolation.
        let userns = "services:\n  web:\n    image: alpine\n    userns_mode: ${U:-host}\n";
        let err = executor
            .validate_compose_security_policy("compose file", userns)
            .unwrap_err();
        assert_eq!(violation_field(err), "userns_mode");
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_volumes_from() {
        let Some(executor) = test_executor() else {
            return;
        };

        // `volumes_from: container:X` inherits every volume of an arbitrary host
        // container (other tenants', Temps infra) — a full host-escape vector.
        let container_form =
            "services:\n  pwn:\n    image: alpine\n    volumes_from:\n      - container:temps-db-1a2b3c\n";
        let err = executor
            .validate_compose_security_policy("compose file", container_form)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes_from");

        // The `service:X` intra-project form is blocked too — the field is
        // rejected outright rather than trying to distinguish safe targets.
        let service_form =
            "services:\n  pwn:\n    image: alpine\n    volumes_from:\n      - service:web\n";
        let err = executor
            .validate_compose_security_policy("compose file", service_form)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes_from");

        // A benign service with no volumes_from still validates.
        let clean = "services:\n  web:\n    image: alpine\n    volumes:\n      - ./data:/data\n";
        assert!(executor
            .validate_compose_security_policy("compose file", clean)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_absolute_tmp_bind() {
        let Some(executor) = test_executor() else {
            return;
        };

        // Absolute host bind sources are rejected even under /tmp, which is
        // world-writable and can hold another project's data-dir artifacts.
        let tmp_bind =
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - /tmp/test/compose/victim:/stolen:ro\n";
        let err = executor
            .validate_compose_security_policy("compose file", tmp_bind)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes");
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_host_control_keys() {
        let Some(executor) = test_executor() else {
            return;
        };

        // Each host-affecting key must be rejected in isolation (not only when
        // it appears alongside another violation that short-circuits first).
        let cases = [
            ("sysctls", "services:\n  a:\n    image: alpine\n    sysctls:\n      net.ipv4.ip_forward: \"1\"\n"),
            ("group_add", "services:\n  a:\n    image: alpine\n    group_add:\n      - docker\n"),
            ("cgroup_parent", "services:\n  a:\n    image: alpine\n    cgroup_parent: /custom.slice\n"),
        ];
        for (field, yaml) in cases {
            let err = executor
                .validate_compose_security_policy("compose file", yaml)
                .unwrap_err();
            assert_eq!(
                violation_field(err),
                field,
                "expected {field} to be rejected"
            );
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_deploy_devices() {
        let Some(executor) = test_executor() else {
            return;
        };

        // Long-form device reservation — the equivalent of the blocked `gpus:`.
        let yaml = "services:\n  gpu:\n    image: alpine\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - driver: nvidia\n              count: all\n              capabilities: [gpu]\n";
        let err = executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_err();
        assert_eq!(
            violation_field(err),
            "deploy.resources.reservations.devices"
        );

        // A benign deploy block (replicas only) is still allowed.
        let benign = "services:\n  web:\n    image: alpine\n    deploy:\n      replicas: 2\n";
        assert!(executor
            .validate_compose_security_policy("compose file", benign)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_build_context_and_ssh() {
        let Some(executor) = test_executor() else {
            return;
        };

        // Absolute build context would send an arbitrary host dir into the build.
        let ctx = "services:\n  app:\n    build:\n      context: /etc\n";
        let err = executor
            .validate_compose_security_policy("compose file", ctx)
            .unwrap_err();
        assert_eq!(violation_field(err), "build.context");

        // Project-escaping dockerfile path.
        let dockerfile =
            "services:\n  app:\n    build:\n      context: .\n      dockerfile: ../../../etc/x\n";
        let err = executor
            .validate_compose_security_policy("compose file", dockerfile)
            .unwrap_err();
        assert_eq!(violation_field(err), "build.dockerfile");

        // SSH agent forwarding during build.
        let ssh =
            "services:\n  app:\n    build:\n      context: .\n      ssh:\n        - default\n";
        let err = executor
            .validate_compose_security_policy("compose file", ssh)
            .unwrap_err();
        assert_eq!(violation_field(err), "build.ssh");

        // A confined relative build context is accepted.
        let ok =
            "services:\n  app:\n    build:\n      context: ./app\n      dockerfile: Dockerfile\n";
        assert!(executor
            .validate_compose_security_policy("compose file", ok)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_network_volume_driver() {
        let Some(executor) = test_executor() else {
            return;
        };

        // Non-local driver invokes an external volume plugin.
        let plugin = "services:\n  app:\n    image: alpine\n    volumes:\n      - vol:/data\nvolumes:\n  vol:\n    driver: some-plugin\n";
        let err = executor
            .validate_compose_security_policy("compose file", plugin)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes");

        // Local driver with an NFS type mounts an off-host filesystem.
        let nfs = "services:\n  app:\n    image: alpine\n    volumes:\n      - vol:/data\nvolumes:\n  vol:\n    driver: local\n    driver_opts:\n      type: nfs\n      o: addr=attacker.example.com,rw\n      device: \":/exports/root\"\n";
        let err = executor
            .validate_compose_security_policy("compose file", nfs)
            .unwrap_err();
        assert_eq!(violation_field(err), "volumes");

        // A plain local named volume is fine.
        let ok = "services:\n  app:\n    image: alpine\n    volumes:\n      - vol:/data\nvolumes:\n  vol: {}\n";
        assert!(executor
            .validate_compose_security_policy("compose file", ok)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_unsafe_service_names() {
        let Some(executor) = test_executor() else {
            return;
        };

        // A service name carrying newlines/YAML structure would corrupt the
        // generated security override; reject it up front.
        let yaml =
            "services:\n  ? \"evil:\\n  cap_drop:\\n  - ALL\\n  legit\"\n  :\n    image: alpine\n";
        let err = executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_err();
        assert_eq!(violation_field(err), "services");

        // Normal names pass the character-set check.
        assert!(ComposeExecutor::is_valid_service_name("web-1.api_v2"));
        assert!(!ComposeExecutor::is_valid_service_name("evil:\n  x"));
        assert!(!ComposeExecutor::is_valid_service_name("-leading-dash"));
        assert!(!ComposeExecutor::is_valid_service_name(""));
    }

    #[test]
    fn test_validate_relative_path_confines_to_project_dir() {
        // Valid relative paths are accepted.
        assert!(
            ComposeExecutor::validate_relative_path("docker-compose.yml", "compose_path").is_ok()
        );
        assert!(
            ComposeExecutor::validate_relative_path("apps/web/compose.yml", "compose_path").is_ok()
        );
        assert!(ComposeExecutor::validate_relative_path("./compose.yml", "compose_path").is_ok());

        // Empty, absolute, and traversing paths are rejected.
        for bad in [
            "",
            "/tmp/compose.yml",
            "/etc/passwd",
            "../compose.yml",
            "apps/../../compose.yml",
        ] {
            let err = ComposeExecutor::validate_relative_path(bad, "compose_path").unwrap_err();
            assert!(matches!(
                err,
                ComposeError::InvalidComposePath { ref field, .. } if field == "compose_path"
            ));
        }
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

    #[test]
    fn test_contains_interpolation_covers_braceless_and_escapes() {
        // Braced, command-substitution, and braceless forms are all caught.
        assert!(ComposeExecutor::contains_interpolation("${VAR}"));
        assert!(ComposeExecutor::contains_interpolation("$(id -g docker)"));
        assert!(ComposeExecutor::contains_interpolation("$VAR"));
        assert!(ComposeExecutor::contains_interpolation(
            "prefix-$HOST_ROOT/x"
        ));
        assert!(ComposeExecutor::contains_interpolation("${NET:-host}"));
        // `$$` escapes a literal dollar, and a bare/trailing `$` is not a var.
        assert!(!ComposeExecutor::contains_interpolation("$$HOME"));
        assert!(!ComposeExecutor::contains_interpolation("no dollars here"));
        assert!(!ComposeExecutor::contains_interpolation("trailing$"));
        assert!(!ComposeExecutor::contains_interpolation("cost is $ 5"));
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_braceless_interpolation() {
        let Some(executor) = test_executor() else {
            return;
        };
        // Braceless $VAR in network_mode would resolve to `host` from a
        // repo-controlled .env at runtime, bypassing the literal `host` check.
        let net = "services:\n  web:\n    image: alpine\n    network_mode: $NET\n";
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", net)
                    .unwrap_err()
            ),
            "network_mode"
        );

        // Braceless $SRC in a bind mount source.
        let vol = "services:\n  web:\n    image: alpine\n    volumes:\n      - $SRC:/host:rw\n";
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", vol)
                    .unwrap_err()
            ),
            "volumes"
        );
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_top_level_include() {
        let Some(executor) = test_executor() else {
            return;
        };
        // `include` merges repo-controlled compose files that never flow through
        // this validator.
        let compose = "include:\n  - ./evil.yml\nservices:\n  web:\n    image: nginx\n";
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", compose)
                    .unwrap_err()
            ),
            "include"
        );
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_container_namespace() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (field, compose) in [
            (
                "network_mode",
                "services:\n  web:\n    image: alpine\n    network_mode: \"container:other\"\n",
            ),
            (
                "pid",
                "services:\n  web:\n    image: alpine\n    pid: \"container:other\"\n",
            ),
        ] {
            assert_eq!(
                violation_field(
                    executor
                        .validate_compose_security_policy("compose file", compose)
                        .unwrap_err()
                ),
                field
            );
        }

        // Intra-project `service:` sharing stays within the deployment and is
        // allowed.
        let ok = "services:\n  web:\n    image: alpine\n    network_mode: \"service:db\"\n  db:\n    image: postgres\n";
        assert!(executor
            .validate_compose_security_policy("compose file", ok)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_non_string_service_names() {
        let Some(executor) = test_executor() else {
            return;
        };
        // A bare boolean/null/numeric key is a non-string scalar that would be
        // dropped by the service-name enumerator and skip the security override.
        for compose in [
            "services:\n  true:\n    image: alpine\n",
            "services:\n  null:\n    image: alpine\n",
            "services:\n  8080:\n    image: alpine\n",
        ] {
            assert_eq!(
                violation_field(
                    executor
                        .validate_compose_security_policy("compose file", compose)
                        .unwrap_err()
                ),
                "services"
            );
        }

        // A normal quoted/bareword string service name is still accepted.
        let ok = "services:\n  web:\n    image: nginx\n";
        assert!(executor
            .validate_compose_security_policy("compose file", ok)
            .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_confinement_rejects_symlink_host_paths() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        symlink("/", root.path().join("escape")).unwrap();

        let bind =
            "services:\n  app:\n    image: alpine\n    volumes:\n      - ./escape:/host:ro\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            bind,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "volumes");

        let config = "services:\n  app:\n    image: alpine\nconfigs:\n  host:\n    file: ./escape/etc/passwd\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            config,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "configs.file");

        let build = "services:\n  app:\n    build: ./escape\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            build,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "build.context");
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_confinement_rejects_symlink_dockerfile_and_write_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("app")).unwrap();
        symlink("/etc/passwd", root.path().join("app/Dockerfile")).unwrap();

        let compose = "services:\n  app:\n    build: ./app\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            compose,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "build.dockerfile");

        symlink(
            "/tmp/temps-compose-write-target",
            root.path().join(".env.temps"),
        )
        .unwrap();
        let err = ComposeExecutor::confined_write_path(
            root.path(),
            Path::new(".env.temps"),
            ".env.temps",
        )
        .unwrap_err();
        assert_eq!(violation_field(err), ".env.temps");
    }

    #[test]
    fn test_filesystem_confinement_allows_existing_paths_inside_nested_compose_base() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("apps/app");
        std::fs::create_dir_all(app.join("data")).unwrap();
        std::fs::write(app.join("Dockerfile"), "FROM scratch\n").unwrap();
        std::fs::write(root.path().join("apps/config.txt"), "safe\n").unwrap();

        let compose = r#"
services:
  app:
    build: ./app
    volumes:
      - ./app/data:/data
configs:
  app-config:
    file: ./config.txt
"#;
        ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "apps/compose.yml",
            "compose file",
            compose,
        )
        .unwrap();
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_runtime_and_oom_disable() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (field, yaml) in [
            (
                "runtime",
                "services:\n  app:\n    image: alpine\n    runtime: kata-runtime\n",
            ),
            (
                "oom_kill_disable",
                "services:\n  app:\n    image: alpine\n    oom_kill_disable: true\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", yaml)
                .unwrap_err();
            assert_eq!(violation_field(err), field);
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_unbounded_resource_controls() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (field, yaml) in [
            (
                "shm_size",
                "services:\n  app:\n    image: alpine\n    shm_size: 64m\n",
            ),
            (
                "tmpfs",
                "services:\n  app:\n    image: alpine\n    tmpfs:\n      - /run\n",
            ),
            (
                "volumes",
                "services:\n  app:\n    image: alpine\n    volumes:\n      - type: tmpfs\n        target: /run\n",
            ),
            (
                "ulimits",
                "services:\n  app:\n    image: alpine\n    ulimits:\n      nofile: 1024\n",
            ),
            (
                "build.shm_size",
                "services:\n  app:\n    build:\n      context: .\n      shm_size: 64m\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", yaml)
                .unwrap_err();
            assert_eq!(violation_field(err), field, "expected rejection for {yaml}");
        }
    }

    #[test]
    fn test_validate_compose_override_expands_merge_keys() {
        let compose = "services:\n  web:\n    image: nginx\n";
        let override_content = r#"
services:
  web:
    x-runtime: &runtime
      runtime: kata-runtime
    <<: *runtime
"#;
        let error =
            ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("forbidden inline override key 'runtime'"));
    }

    #[test]
    fn test_generate_security_override_sets_privileged_false() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = "services:\n  web:\n    image: nginx\n  worker:\n    image: alpine\n";
        let override_yaml = executor.generate_security_override(compose);
        assert_eq!(override_yaml.matches("privileged: false").count(), 2);
    }
}
