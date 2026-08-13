//! Docker Compose deployment executor.
//!
//! Manages multi-container deployments using `docker compose` CLI commands.
//! After `compose up`, discovers running containers, applies Temps labels,
//! and returns per-service results that get inserted into `deployment_containers`.

use bollard::query_parameters::LogsOptions;
use bollard::Docker;
use futures::{StreamExt, TryStreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use serde_yaml::{Mapping, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

/// How long `deploy()` waits for every Compose service to report `running`
/// (and `healthy`, if it defines a healthcheck) before failing the
/// deployment. Mirrors the single-container deploy path's
/// `health_check_timeout_secs` default.
const COMPOSE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Interval between `docker compose ps` polls while waiting for readiness.
const COMPOSE_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a single TCP connect attempt against a published port may take
/// before `port_reachable` gives up and reports the port not yet listening.
/// Short by design: this runs once per poll interval, not once total, so a
/// slow/unreachable port degrades to "still pending" rather than stalling
/// the whole readiness loop.
const PORT_PROBE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);

/// Docker label Compose attaches to every network/volume/container it
/// manages, set to the `-p <project_name>` value.
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// Maximum diagnostic text persisted into a deployment error. Full container
/// logs remain available through the authenticated logs endpoint.
const MAX_COMPOSE_DIAGNOSTIC_BYTES: usize = 32 * 1024;
const SAFE_DOCKER_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin";
const DOCKER_BINARY_CANDIDATES: &[&str] = &[
    "/usr/local/bin/docker",
    "/usr/bin/docker",
    "/opt/homebrew/bin/docker",
];

/// Create an isolated Docker CLI process.
///
/// Compose interpolates values from its process environment before reading
/// `.env`/`--env-file`. Inheriting the Temps server environment would let an
/// untrusted repository reference `${TEMPS_ENCRYPTION_KEY}` (or any other
/// control-plane secret) and copy it into a container, label, command, or
/// image build. Resolve Docker only from administrator-owned system paths,
/// clear the inherited environment, and restore the small set of non-secret
/// Docker connection/configuration paths needed by legitimate installations.
fn isolated_docker_command() -> tokio::process::Command {
    let docker_binary = DOCKER_BINARY_CANDIDATES
        .iter()
        .find(|candidate| std::path::Path::new(candidate).is_file())
        .copied()
        .unwrap_or("/usr/bin/docker");
    let mut command = tokio::process::Command::new(docker_binary);
    command.env_clear().env("PATH", SAFE_DOCKER_PATH);
    for key in [
        "HOME",
        "DOCKER_CONFIG",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

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

    #[error("Invalid Compose environment variable '{key}': {reason}")]
    InvalidEnvironmentVariable { key: String, reason: String },

    #[error("Compose stack '{project}' did not become ready within {timeout_secs}s: {reason}")]
    ServicesNotReady {
        project: String,
        timeout_secs: u64,
        reason: String,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Where the contents of a referenced `env_file` come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvFileSource {
    /// The repository ships the file; it is copied verbatim.
    Repository(PathBuf),
    /// The repository does not ship it, so it is synthesized from the
    /// project's Temps environment variables.
    ProjectEnvironment,
}

/// One `env_file:` reference resolved against the repository checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFilePlan {
    /// Path exactly as written in the compose file, relative to the project.
    pub path: String,
    pub source: EnvFileSource,
}

/// Render environment variables as `KEY=value` lines for an env file.
///
/// Keys are emitted in sorted order so redeploying with an unchanged set
/// produces an identical file (`HashMap` iteration order is not stable).
fn render_env_file(vars: &HashMap<String, String>) -> Result<String, ComposeError> {
    let mut entries: Vec<(&String, &String)> = vars.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut rendered = String::new();
    for (key, value) in entries {
        let mut key_chars = key.chars();
        let valid_key = key_chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && key_chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if !valid_key {
            return Err(ComposeError::InvalidEnvironmentVariable {
                key: key.clone(),
                reason: "keys must match [A-Za-z_][A-Za-z0-9_]*".to_string(),
            });
        }

        // Compose applies interpolation to unquoted and double-quoted dotenv
        // values. Encode a literal value in double quotes, escaping `$` as
        // `$$` so tenant data cannot introduce a second interpolation step or
        // inject another dotenv assignment through a newline.
        let mut encoded = String::with_capacity(value.len() + 2);
        encoded.push('"');
        for ch in value.chars() {
            match ch {
                '\\' => encoded.push_str("\\\\"),
                '"' => encoded.push_str("\\\""),
                '\n' => encoded.push_str("\\n"),
                '\r' => encoded.push_str("\\r"),
                '\t' => encoded.push_str("\\t"),
                '$' => encoded.push_str("$$"),
                ch if ch.is_control() => {
                    return Err(ComposeError::InvalidEnvironmentVariable {
                        key: key.clone(),
                        reason: format!(
                            "value contains unsupported control character U+{:04X}",
                            ch as u32
                        ),
                    });
                }
                _ => encoded.push(ch),
            }
        }
        encoded.push('"');
        rendered.push_str(key);
        rendered.push('=');
        rendered.push_str(&encoded);
        rendered.push('\n');
    }
    Ok(rendered)
}

fn sanitize_compose_diagnostic(
    diagnostic: &str,
    environment_vars: &HashMap<String, String>,
    build_args: &HashMap<String, String>,
) -> String {
    let mut sanitized = diagnostic.to_string();
    for value in environment_vars.values().chain(build_args.values()) {
        if !value.is_empty() {
            sanitized = sanitized.replace(value, "<redacted>");
        }
    }

    // Docker and application errors commonly echo credentials as assignments,
    // bearer headers, or URI userinfo. Cover those forms even for literal
    // values originating in a repository Compose document.
    for pattern in [
        r#"(?i)(bearer\s+)[A-Za-z0-9._~+/-]+={0,2}"#,
        r#"(://[^\s:/@]+:)[^\s/@]+@"#,
        r#"(?i)((?:password|passwd|token|api[_-]?key|client[_-]?secret|private[_-]?key|authorization)\s*[:=]\s*)[^\s,;]+"#,
    ] {
        if let Ok(regex) = Regex::new(pattern) {
            sanitized = regex.replace_all(&sanitized, "${1}<redacted>").into_owned();
        }
    }

    if sanitized.len() > MAX_COMPOSE_DIAGNOSTIC_BYTES {
        let mut boundary = MAX_COMPOSE_DIAGNOSTIC_BYTES;
        while !sanitized.is_char_boundary(boundary) {
            boundary -= 1;
        }
        sanitized.truncate(boundary);
        sanitized
            .push_str("\n… diagnostic truncated; inspect authenticated container logs for more");
    }
    sanitized
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
    /// Platform-owned arguments passed only to `docker compose build`.
    /// These are deliberately separate from service runtime environments.
    pub build_args: HashMap<String, String>,
    /// Temps labels to apply to all containers
    pub labels: HashMap<String, String>,
    /// Source repo directory (needed for compose files with build: directives)
    pub repo_dir: Option<PathBuf>,
    /// User-provided docker-compose.temps-override.yml content
    pub compose_override: Option<String>,
    /// Compose service names granted back the minimal Linux capabilities
    /// (see [`ComposeExecutor::RELAXED_CAPABILITIES`]) needed by official
    /// database images to fix ownership on their data/socket directory at
    /// container start. Empty by default — every service keeps the full
    /// `cap_drop: ALL` sandbox unless the user has explicitly opted it in.
    pub relaxed_capability_services: Vec<String>,
    /// Compose services explicitly exempted from Temps' generated runtime
    /// sandbox. Repository security validation still applies.
    pub unsandboxed_services: Vec<String>,
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
    /// wait for every service to become ready, then discover and label them.
    /// Returns one result per service. Fails (rather than reporting a false
    /// success) if a service never reaches `running`/`healthy` within
    /// [`COMPOSE_READY_TIMEOUT`].
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
        Self::validate_security_exemptions(
            &request.relaxed_capability_services,
            &request.unsandboxed_services,
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
                &request.build_args,
            )
            .await?;
        }

        // Ensure the shared Temps network exists before `up` attaches every
        // service to it (docker-compose.temps-network.yml, written above) —
        // this is the same network Temps-managed external services and
        // single-container app deployments join, so a compose service can
        // reach a Temps-managed database by name.
        self.ensure_temps_network_exists().await?;

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

        // 3b. `up -d` returns as soon as containers are created/started, not
        // once they're actually ready. Wait for every service to reach
        // `running` (and `healthy`, for services that define a healthcheck)
        // so a crash-looping or slow-starting service surfaces as a failed
        // deployment instead of a false "success".
        self.wait_for_services_ready(
            &effective_dir,
            &project_name,
            compose_file,
            &request.environment_vars,
            COMPOSE_READY_TIMEOUT,
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
        self.teardown_at(project_name, None, None, &HashMap::new())
            .await
    }

    /// Tear down a Compose stack from the exact directory used for `up`.
    /// Uploaded/Git deployments run Compose inside their checkout rather than
    /// the data-dir fallback, so compensation must retain that location.
    pub async fn teardown_at(
        &self,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
        _environment_vars: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        if let Some(compose_path) = compose_path {
            Self::validate_relative_path(compose_path, "compose_path")?;
        }
        let project_dir = repo_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.project_dir(project_name));

        if !project_dir.exists() {
            debug!(project = %project_name, "Project directory does not exist, nothing to tear down");
            return Ok(());
        }

        let compose_file = compose_path
            .map(ToString::to_string)
            .unwrap_or_else(|| self.find_compose_file(&project_dir));

        // down WITHOUT --volumes: removes containers and networks, keeps volumes
        let mut command = isolated_docker_command();
        command
            .args(["compose", "-p", project_name])
            .args(["-f", &compose_file]);
        for generated in [
            "docker-compose.temps-env.yml",
            "docker-compose.temps-network.yml",
            "docker-compose.temps-override.yml",
            "docker-compose.temps-labels.yml",
            "docker-compose.temps-security.yml",
        ] {
            if project_dir.join(generated).exists() {
                command.args(["-f", generated]);
            }
        }
        for env_file in [".env.temps", ".env"] {
            if project_dir.join(env_file).exists() {
                command.args(["--env-file", env_file]);
            }
        }
        command
            .args(["down", "--remove-orphans", "--timeout", "30"])
            .current_dir(&project_dir)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(35), command.output())
            .await
            .map_err(|_| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: "docker compose down timed out after 35 seconds".to_string(),
            })??;

        if !output.status.success() {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose down failed with status {}", output.status),
            });
        }

        info!(project = %project_name, "Compose stack torn down (volumes preserved)");
        Ok(())
    }

    /// Fully destroy a compose stack including all volumes and data.
    /// Used when deleting a project/environment permanently.
    pub async fn destroy(&self, project_name: &str) -> Result<(), ComposeError> {
        let project_dir = self.project_dir(project_name);

        // `docker compose down` only works from the exact directory/file the
        // stack was `up`'d from. Git-backed deployments run Compose from an
        // ephemeral checkout under a per-deployment temp dir (cleaned up as
        // soon as the deploy job finishes) rather than `project_dir`, so this
        // directory-based teardown is frequently a no-op for them -- it must
        // never be the only cleanup path. Volumes/networks/containers Compose
        // creates always carry the `com.docker.compose.project` label
        // regardless of which directory `up` ran from, so the label-based
        // sweep below is the one step that's reliable independent of
        // compose_path/repo_dir.
        if project_dir.exists() {
            let compose_file = self.find_compose_file(&project_dir);

            // down WITH --volumes: removes everything including persistent data
            let output = isolated_docker_command()
                .args(["compose", "-p", project_name])
                .args(["-f", &compose_file])
                .args(["down", "--remove-orphans", "--volumes"])
                .current_dir(&project_dir)
                .output()
                .await?;

            if !output.status.success() {
                warn!(project = %project_name, status = %output.status, "docker compose down failed (falling back to label-based cleanup)");
            }

            // Clean up work directory
            if let Err(e) = tokio::fs::remove_dir_all(&project_dir).await {
                warn!(project = %project_name, error = %e, "Failed to clean up project directory");
            }
        } else {
            debug!(project = %project_name, "Project directory does not exist, relying on label-based cleanup");
        }

        self.destroy_labeled_resources(project_name).await?;

        info!(project = %project_name, "Compose stack destroyed (volumes removed)");
        Ok(())
    }

    /// Remove every Docker network and volume Compose labeled with
    /// `com.docker.compose.project=<project_name>`. Unlike `docker compose
    /// down`, this needs no compose file or working directory -- Compose
    /// attaches this label to every resource it creates regardless of which
    /// directory `up` ran from, so it is the only cleanup step that reliably
    /// works for git-backed deployments whose checkout directory is long gone
    /// by the time a project/environment is deleted. Containers are expected
    /// to already be removed by the caller (deployment_containers-driven
    /// cleanup); this only sweeps what that leaves behind.
    async fn destroy_labeled_resources(&self, project_name: &str) -> Result<(), ComposeError> {
        let label_filter = format!("{COMPOSE_PROJECT_LABEL}={project_name}");
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![label_filter]);

        let networks = self
            .docker
            .list_networks(Some(
                bollard::query_parameters::ListNetworksOptionsBuilder::new()
                    .filters(&filters)
                    .build(),
            ))
            .await
            .map_err(|e| {
                ComposeError::Docker(format!("list_networks for '{project_name}': {e}"))
            })?;
        for network in networks {
            let Some(id) = network.id.or(network.name) else {
                continue;
            };
            if let Err(e) = self.docker.remove_network(&id).await {
                warn!(project = %project_name, network = %id, error = %e, "Failed to remove Compose-managed network");
            }
        }

        let volumes = self
            .docker
            .list_volumes(Some(
                bollard::query_parameters::ListVolumesOptionsBuilder::new()
                    .filters(&filters)
                    .build(),
            ))
            .await
            .map_err(|e| ComposeError::Docker(format!("list_volumes for '{project_name}': {e}")))?;
        for volume in volumes.volumes.unwrap_or_default() {
            if let Err(e) = self
                .docker
                .remove_volume(
                    &volume.name,
                    Some(
                        bollard::query_parameters::RemoveVolumeOptionsBuilder::new()
                            .force(true)
                            .build(),
                    ),
                )
                .await
            {
                warn!(project = %project_name, volume = %volume.name, error = %e, "Failed to remove Compose-managed volume");
            }
        }

        Ok(())
    }

    /// Stop a compose stack without removing volumes.
    pub async fn stop(&self, project_name: &str) -> Result<(), ComposeError> {
        let project_dir = self.project_dir(project_name);
        let compose_file = self.find_compose_file(&project_dir);

        let output = isolated_docker_command()
            .args(["compose", "-p", project_name])
            .args(["-f", &compose_file])
            .arg("stop")
            .current_dir(&project_dir)
            .output()
            .await?;

        if !output.status.success() {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose stop failed with status {}", output.status),
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
        let compose_to_write = Self::rewrite_missing_relative_bind_mounts(
            &compose_to_write,
            compose_path.parent().unwrap_or(project_dir),
            &self.project_dir(&request.project_name).join("binds"),
        )?;

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

        // Satisfy every `env_file:` the compose file declares. The stack
        // directory is not a repo checkout — only the files Temps writes here
        // exist — so a referenced env file must either be copied out of the
        // repository or synthesized, otherwise `docker compose` aborts with
        // "env file ... not found" before a single container starts. Operators
        // deploying somebody else's repository cannot fix that by editing it.
        let already_written_env = request
            .env_content
            .as_deref()
            .is_some_and(|content| !content.trim().is_empty());
        for plan in Self::plan_env_files(&request.compose_content, request.repo_dir.as_deref()) {
            if already_written_env && plan.path == ".env" {
                continue;
            }
            let destination =
                Self::confined_write_path(project_dir, Path::new(&plan.path), "env_file")?;
            let contents = match &plan.source {
                EnvFileSource::Repository(repo_path) => tokio::fs::read_to_string(repo_path)
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: repo_path.display().to_string(),
                        reason: format!("failed to read referenced env file from repository: {e}"),
                    })?,
                EnvFileSource::ProjectEnvironment => render_env_file(&request.environment_vars)?,
            };
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    ComposeError::FileWriteFailed {
                        path: parent.display().to_string(),
                        reason: e.to_string(),
                    }
                })?;
            }
            tokio::fs::write(&destination, contents)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: destination.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps system env vars to .env.temps
        // These include SENTRY_DSN, TEMPS_API_URL, TEMPS_API_TOKEN, OTEL vars, etc.
        if !request.environment_vars.is_empty() {
            let temps_env = render_env_file(&request.environment_vars)?;
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
            let override_content = self.generate_env_override(
                &request.compose_content,
                ".env.temps",
                &request.environment_vars,
            );
            tokio::fs::write(&temps_override_path, &override_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps network override (attaches every service to the shared
        // temps-app-network, in addition to the project's own default
        // network, so compose services can reach Temps-managed external
        // services). Unconditional — unlike the env override above, this
        // doesn't depend on the project having any configured env vars.
        let network_content = self.generate_network_override(&request.compose_content);
        if !network_content.is_empty() {
            let network_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-network.yml"),
                "docker-compose.temps-network.yml",
            )?;
            tokio::fs::write(&network_override_path, &network_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: network_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps security override for every service that has not
        // explicitly opted out of the runtime sandbox.
        let security_content = self.generate_security_override(
            &request.compose_content,
            &request.relaxed_capability_services,
            &request.unsandboxed_services,
        );
        let security_override_path = Self::confined_write_path(
            project_dir,
            Path::new("docker-compose.temps-security.yml"),
            "docker-compose.temps-security.yml",
        )?;
        if !security_content.is_empty() {
            tokio::fs::write(&security_override_path, &security_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        } else if let Err(error) = tokio::fs::remove_file(&security_override_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: format!("failed to remove stale security override: {error}"),
                });
            }
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

    /// Rewrite absent relative bind sources to stable project-scoped host
    /// directories. Git checkouts are deployment-temporary, so allowing Docker
    /// to create `./data` there would silently move the mount to a fresh empty
    /// directory on every redeploy. Existing repository paths are left alone
    /// because they may intentionally provide checked-in files or build data.
    fn rewrite_missing_relative_bind_mounts(
        compose_content: &str,
        compose_base: &Path,
        persistent_bind_root: &Path,
    ) -> Result<String, ComposeError> {
        let mut root: YamlValue = serde_yaml::from_str(compose_content).map_err(|error| {
            ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: error.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to expand YAML merge keys: {error}"),
            })?;

        let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
            return Ok(compose_content.to_string());
        };
        let mut changed = false;

        for definition in services.values_mut() {
            let Some(entries) = definition
                .as_mapping_mut()
                .and_then(|service| service.get_mut(YamlValue::String("volumes".to_string())))
                .and_then(YamlValue::as_sequence_mut)
            else {
                continue;
            };

            for entry in entries {
                let Some(source) = Self::volume_source(entry) else {
                    continue;
                };
                if Self::is_named_volume_ref(&source) || Self::is_dangerous_host_path(&source) {
                    continue;
                }

                let normalized = Self::lexically_normalize(&source);
                let repository_source = compose_base.join(&normalized);
                let stable_name = Self::stable_bind_name(&normalized);
                let stable_source = persistent_bind_root.join(stable_name);

                // Once a source has been assigned stable storage, keep using it
                // even if a later repository revision happens to add an empty
                // directory at the same relative path.
                if repository_source.exists() && !stable_source.exists() {
                    continue;
                }
                std::fs::create_dir_all(&stable_source).map_err(|error| {
                    ComposeError::FileWriteFailed {
                        path: stable_source.display().to_string(),
                        reason: format!(
                            "failed to create persistent directory for relative bind '{source}': {error}"
                        ),
                    }
                })?;
                let stable_source = stable_source.to_string_lossy().to_string();

                if let Some(short) = entry.as_str() {
                    let mut parts = short.splitn(3, ':');
                    let _ = parts.next();
                    let Some(target) = parts.next() else {
                        continue;
                    };
                    let mode = parts.next();
                    *entry = YamlValue::String(match mode {
                        Some(mode) => format!("{stable_source}:{target}:{mode}"),
                        None => format!("{stable_source}:{target}"),
                    });
                    changed = true;
                } else if let Some(mapping) = entry.as_mapping_mut() {
                    mapping.insert(
                        YamlValue::String("source".to_string()),
                        YamlValue::String(stable_source),
                    );
                    changed = true;
                }
            }
        }

        if changed {
            serde_yaml::to_string(&root).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to render persistent relative bind mounts: {error}"),
            })
        } else {
            Ok(compose_content.to_string())
        }
    }

    fn stable_bind_name(relative_source: &str) -> String {
        // FNV-1a is used only for a compact deterministic directory suffix,
        // not for a security boundary. The readable prefix helps operators
        // identify storage on self-hosted installations.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in relative_source.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let readable = relative_source
            .rsplit('/')
            .next()
            .unwrap_or("bind")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        format!("{readable}-{hash:016x}")
    }

    /// Removes excluded services — and references to them in other services'
    /// `depends_on` — from a compose file's `services:` map. Compose's own
    /// `-f` override-file merging can only add/replace fields on a service,
    /// never unset a whole service (the same constraint the port-stripping
    /// logic above hits at a narrower scope), so exclusion has to rewrite the
    /// base document before it's written to disk or validated.
    ///
    /// Re-serializes the whole file, which normalizes formatting (comments,
    /// quoting, anchor definitions) rather than preserving it byte-for-byte —
    /// an accepted tradeoff, since `depends_on` can be either a list or a map
    /// of `{condition: ...}` entries, and correctly stripping a name out of
    /// either shape needs structural understanding, not line-oriented text
    /// editing.
    pub fn strip_excluded_services(
        &self,
        compose_content: &str,
        excluded: &[String],
    ) -> Result<String, ComposeError> {
        if excluded.is_empty() || compose_content.trim().is_empty() {
            return Ok(compose_content.to_string());
        }

        let mut root: YamlValue = serde_yaml::from_str(compose_content).map_err(|e| {
            ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: e.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|e| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to expand YAML merge keys: {e}"),
            })?;

        let Some(services) = root
            .as_mapping_mut()
            .and_then(|m| m.get_mut("services"))
            .and_then(Value::as_mapping_mut)
        else {
            return Ok(compose_content.to_string());
        };

        for name in excluded {
            services.remove(name.as_str());
        }

        for (_, service_value) in services.iter_mut() {
            let Some(depends_on) = service_value
                .as_mapping_mut()
                .and_then(|m| m.get_mut("depends_on"))
            else {
                continue;
            };
            if let Some(seq) = depends_on.as_sequence_mut() {
                seq.retain(|entry| {
                    entry
                        .as_str()
                        .is_none_or(|name| !excluded.iter().any(|e| e == name))
                });
            } else if let Some(map) = depends_on.as_mapping_mut() {
                for name in excluded {
                    map.remove(name.as_str());
                }
            }
        }

        serde_yaml::to_string(&root).map_err(|e| ComposeError::InvalidComposeYaml {
            compose_source: "compose file".to_string(),
            reason: format!("failed to re-serialize compose file after excluding services: {e}"),
        })
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
                "docker-compose.temps-network.yml",
                "docker-compose.temps-network.yml",
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
        self.validate_top_level_networks(&root)?;

        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Ok(());
        };

        const MAX_SERVICE_SHM_BYTES: u64 = 512 * 1024 * 1024;
        const MAX_COMPOSE_SHM_BYTES: u64 = 1024 * 1024 * 1024;
        let mut total_shm_bytes = 0_u64;

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
            if let Some(shm_size) = service.get(YamlValue::String("shm_size".to_string())) {
                let bytes = Self::parse_compose_byte_size(shm_size).ok_or_else(|| {
                    ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: "shared-memory size must be a positive byte count or a value such as '128m', '256mb', or '512MiB'".to_string(),
                    }
                })?;
                if bytes > MAX_SERVICE_SHM_BYTES {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: format!(
                            "shared-memory size is limited to {} MiB per service",
                            MAX_SERVICE_SHM_BYTES / 1024 / 1024
                        ),
                    });
                }
                total_shm_bytes = total_shm_bytes.checked_add(bytes).ok_or_else(|| {
                    ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: "aggregate shared-memory sizing exceeds the supported range"
                            .to_string(),
                    }
                })?;
                if total_shm_bytes > MAX_COMPOSE_SHM_BYTES {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: format!(
                            "aggregate shared-memory sizing is limited to {} MiB per Compose deployment",
                            MAX_COMPOSE_SHM_BYTES / 1024 / 1024
                        ),
                    });
                }
            }
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

    fn validate_top_level_networks(&self, root: &YamlValue) -> Result<(), ComposeError> {
        let Some(networks) = root.get("networks") else {
            return Ok(());
        };
        let Some(networks) = networks.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: "networks".to_string(),
                reason: "top-level networks must be a mapping".to_string(),
            });
        };

        for (network_name, network) in networks {
            let name = network_name.as_str().unwrap_or("<non-string>");
            if network.is_null() {
                continue;
            }
            let Some(options) = network.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: format!("networks.{name}"),
                    reason: "network configuration must be a mapping".to_string(),
                });
            };

            if options.contains_key(YamlValue::String("external".to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: format!("networks.{name}.external"),
                    reason: "external Compose networks bypass Temps-managed network policy"
                        .to_string(),
                });
            }

            if let Some(driver) = options.get(YamlValue::String("driver".to_string())) {
                let Some(driver) = driver.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.driver"),
                        reason: "network driver must be the literal value 'bridge'".to_string(),
                    });
                };
                if driver != "bridge" {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.driver"),
                        reason: format!(
                            "network driver '{driver}' can bypass routed host filtering; \
                             only the bridge driver is allowed"
                        ),
                    });
                }
            }
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
            if Self::is_remote_build_context(context) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.context".to_string(),
                    reason: "remote build contexts are not allowed because Docker would fetch them from the host network".to_string(),
                });
            }
            if Self::is_dangerous_host_path(context) {
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
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "build.context".to_string(),
                        reason: "remote build contexts are not allowed because Docker would fetch them from the host network".to_string(),
                    });
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

    /// Parse the byte-value syntax accepted by Compose for `shm_size` while
    /// keeping validation deterministic and interpolation-free. Compose's
    /// conventional k/m/g units are binary multiples; explicit `iB` and `B`
    /// suffixes are accepted as well.
    fn parse_compose_byte_size(value: &YamlValue) -> Option<u64> {
        if let Some(bytes) = value.as_u64() {
            return (bytes > 0).then_some(bytes);
        }
        let raw = value.as_str()?.trim().to_ascii_lowercase();
        let digit_count = raw.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 {
            return None;
        }
        let amount = raw[..digit_count].parse::<u64>().ok()?;
        if amount == 0 {
            return None;
        }
        let multiplier = match raw[digit_count..].trim() {
            "" | "b" => 1,
            "k" | "kb" | "kib" => 1024,
            "m" | "mb" | "mib" => 1024 * 1024,
            "g" | "gb" | "gib" => 1024 * 1024 * 1024,
            _ => return None,
        };
        amount.checked_mul(multiplier)
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
                    Self::validate_confined_bind_path(
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

    /// Validate a service bind source against the Compose project without
    /// requiring its final directory to exist yet.
    ///
    /// Docker Compose short volume syntax creates a missing relative host
    /// directory. Rejecting that case breaks common upstream stacks such as
    /// Paperless (`./export` and `./consume`). We still inspect and canonicalize
    /// every existing prefix, including dangling symlinks, so an absent leaf
    /// cannot disguise a path that escapes through a committed symlink.
    fn validate_confined_bind_path(
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

        let normalized = Self::lexically_normalize(raw_path);
        let mut cursor = base_dir.to_path_buf();
        for component in Path::new(&normalized).components() {
            match component {
                Component::CurDir => continue,
                Component::Normal(name) => cursor.push(name),
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service.to_string(),
                        field: field.to_string(),
                        reason: format!(
                            "host path '{raw_path}' must remain inside the compose project directory"
                        ),
                    });
                }
            }

            match std::fs::symlink_metadata(&cursor) {
                Ok(_) => {
                    cursor = std::fs::canonicalize(&cursor).map_err(|error| {
                        ComposeError::SecurityPolicyViolation {
                            service: service.to_string(),
                            field: field.to_string(),
                            reason: format!(
                                "host path '{raw_path}' contains an existing path that cannot be canonicalized: {error}"
                            ),
                        }
                    })?;
                    if !cursor.starts_with(canonical_root) {
                        return Err(ComposeError::SecurityPolicyViolation {
                            service: service.to_string(),
                            field: field.to_string(),
                            reason: format!(
                                "host path '{raw_path}' resolves outside the compose project directory"
                            ),
                        });
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Once a prefix is absent, no deeper path can exist. Compose
                    // may safely create the remaining confined path at `up`.
                    return Ok(base_dir.join(normalized));
                }
                Err(error) => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service.to_string(),
                        field: field.to_string(),
                        reason: format!(
                            "host path '{raw_path}' could not be inspected before deployment: {error}"
                        ),
                    });
                }
            }
        }

        Ok(cursor)
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
            "labels",
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
        build_args: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let mut cmd =
            Self::compose_build_command(project_dir, project_name, compose_file, build_args);

        debug!(project = %project_name, "Running docker compose build");

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = sanitize_compose_diagnostic(&stderr, env_vars, build_args);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose build failed: {}", stderr),
            });
        }

        info!(project = %project_name, "docker compose build completed");
        Ok(())
    }

    fn compose_build_command(
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        build_args: &HashMap<String, String>,
    ) -> tokio::process::Command {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_file_args(&mut cmd, project_dir, compose_file);
        Self::append_compose_env_file_args(&mut cmd, project_dir);
        cmd.args(["build", "--pull"])
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());

        // Sort for deterministic command construction and test output. Build
        // arguments are appended explicitly, so they override any tenant
        // Compose `args:` value without placing tenant-controlled values in the
        // Docker CLI process environment.
        let mut sorted_build_args: Vec<_> = build_args.iter().collect();
        sorted_build_args.sort_by_key(|(key, _)| *key);
        for (key, value) in sorted_build_args {
            cmd.args(["--build-arg", &format!("{key}={value}")]);
        }

        cmd
    }

    fn append_compose_file_args(
        cmd: &mut tokio::process::Command,
        project_dir: &Path,
        compose_file: &str,
    ) {
        cmd.args(["-f", compose_file]);
        for generated in [
            "docker-compose.temps-env.yml",
            "docker-compose.temps-network.yml",
            "docker-compose.temps-override.yml",
            "docker-compose.temps-labels.yml",
            "docker-compose.temps-security.yml",
        ] {
            if project_dir.join(generated).exists() {
                cmd.args(["-f", generated]);
            }
        }
    }

    fn append_compose_env_file_args(cmd: &mut tokio::process::Command, project_dir: &Path) {
        for env_file in [".env.temps", ".env"] {
            if project_dir.join(env_file).exists() {
                cmd.args(["--env-file", env_file]);
            }
        }
    }

    /// Idempotently create the shared Docker network every Temps-managed
    /// external service and single-container app deployment joins
    /// (`temps_core::NETWORK_NAME`). Mirrors
    /// `temps-providers::utils::ensure_network_exists` /
    /// `DockerDeployer::ensure_network_exists` — compose deployments have no
    /// shared code path with either, so this is a small, deliberate
    /// duplication rather than a new cross-crate dependency for one function.
    async fn ensure_temps_network_exists(&self) -> Result<(), ComposeError> {
        let network_name = temps_core::NETWORK_NAME.as_str();
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| ComposeError::Docker(format!("Failed to list networks: {e}")))?;
        if networks
            .iter()
            .any(|n| n.name.as_deref() == Some(network_name))
        {
            return Ok(());
        }
        self.docker
            .create_network(bollard::models::NetworkCreateRequest {
                name: network_name.to_string(),
                driver: Some("bridge".to_string()),
                ..Default::default()
            })
            .await
            .map(|_| ())
            .map_err(|e| {
                ComposeError::Docker(format!("Failed to create network {network_name}: {e}"))
            })
    }

    async fn compose_up(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        env_vars: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_file_args(&mut cmd, project_dir, compose_file);
        Self::append_compose_env_file_args(&mut cmd, project_dir);

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

        // Cancellation drops the command future; terminate the Compose CLI so
        // compensating `compose down` cannot race a still-running `compose up`.
        cmd.kill_on_drop(true);

        debug!(project = %project_name, "Running docker compose up");

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // `up` blocks on `depends_on: condition: service_healthy`, so a
            // dependency that never becomes healthy fails `up` itself — before
            // this fix, that surfaced as just "container X is unhealthy" with
            // no indication of *why* X was unhealthy.
            let container_logs = self
                .describe_unhealthy_containers(project_dir, project_name, compose_file, env_vars)
                .await;
            let diagnostic = sanitize_compose_diagnostic(
                &format!("{}{}", stderr, container_logs),
                env_vars,
                &HashMap::new(),
            );
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose up failed: {diagnostic}"),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!(project = %project_name, stdout = %stdout, "docker compose up completed");

        Ok(())
    }

    /// Run `docker compose ps --format json --all` and parse its output.
    /// Shared by `discover_containers` (final result assembly) and
    /// `wait_for_services_ready` (readiness polling).
    async fn compose_ps(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
    ) -> Result<Vec<ComposePsEntry>, ComposeError> {
        let output = isolated_docker_command()
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
        let mut entries = Vec::new();

        // docker compose ps --format json outputs one JSON object per line
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let entry: ComposePsEntry =
                serde_json::from_str(line).map_err(|e| ComposeError::DiscoveryFailed {
                    project: project_name.to_string(),
                    reason: format!("Failed to parse compose ps output: {} (line: {})", e, line),
                })?;
            entries.push(entry);
        }

        Ok(entries)
    }

    /// Trailing log lines captured per unhealthy/stopped container when a
    /// Compose deploy fails — enough to show the actual startup error (e.g. a
    /// Postgres auth or permission failure) without risking an unbounded dump
    /// into the job's error message.
    const FAILED_CONTAINER_LOG_TAIL: &'static str = "150";

    /// Best-effort debug aid for a failed Compose deploy: finds every
    /// container that isn't `running` + (`healthy` or no healthcheck), and
    /// appends its own log tail. Compose's own failure text only says e.g.
    /// "container X is unhealthy" — it never explains *why*, so without this
    /// the user has to know to go find the container and inspect it manually.
    /// Never itself fails the caller: a container that can't be inspected or
    /// whose logs can't be fetched just gets a short note instead of blocking
    /// the (already-failing) deploy from reporting its real error.
    async fn describe_unhealthy_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        environment_vars: &HashMap<String, String>,
    ) -> String {
        let entries = match self
            .compose_ps(project_dir, project_name, compose_file)
            .await
        {
            Ok(entries) => entries,
            Err(e) => {
                return format!(
                    "\n\n(could not list containers to capture debug logs: {})",
                    e
                )
            }
        };

        let mut sections = Vec::new();
        for entry in &entries {
            let is_unhealthy = !entry.health.is_empty() && entry.health != "healthy";
            let is_not_running = entry.state != "running";
            if !is_unhealthy && !is_not_running {
                continue;
            }

            let logs = sanitize_compose_diagnostic(
                &self.container_log_tail(&entry.id).await,
                environment_vars,
                &HashMap::new(),
            );
            let health = if entry.health.is_empty() {
                "n/a"
            } else {
                &entry.health
            };
            sections.push(format!(
                "--- {} (service '{}', state={}, health={}) ---\n{}",
                entry.name,
                entry.service,
                entry.state,
                health,
                logs.trim()
            ));
        }

        if sections.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nContainer logs for unhealthy/stopped services:\n\n{}",
                sections.join("\n\n")
            )
        }
    }

    /// Fetches the last [`Self::FAILED_CONTAINER_LOG_TAIL`] lines (stdout +
    /// stderr, interleaved in emission order) for one container. Returns a
    /// human-readable placeholder instead of an error so callers can always
    /// embed the result directly in a debug message.
    async fn container_log_tail(&self, container_id: &str) -> String {
        let logs_stream = self.docker.logs(
            container_id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                tail: Self::FAILED_CONTAINER_LOG_TAIL.to_string(),
                ..Default::default()
            }),
        );

        match logs_stream
            .map(|chunk| chunk.map(|c| String::from_utf8_lossy(&c.into_bytes()).into_owned()))
            .try_collect::<Vec<_>>()
            .await
        {
            Ok(chunks) if chunks.iter().all(|c| c.trim().is_empty()) => {
                "(no log output)".to_string()
            }
            Ok(chunks) => chunks.join(""),
            Err(e) => format!("(failed to fetch logs: {})", e),
        }
    }

    /// Poll `docker compose ps` until every service is `running` (and
    /// `healthy`, for services that declare a healthcheck) or `timeout`
    /// elapses. A service stuck `exited`/`dead`/`restarting` fails fast
    /// instead of waiting out the full timeout.
    async fn wait_for_services_ready(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        environment_vars: &HashMap<String, String>,
        timeout: std::time::Duration,
    ) -> Result<(), ComposeError> {
        let start = std::time::Instant::now();
        loop {
            let entries = self
                .compose_ps(project_dir, project_name, compose_file)
                .await?;
            match Self::classify_readiness(&entries) {
                ComposeReadiness::Ready => {
                    // `docker compose ps` reports a container "running" the
                    // instant its process starts — for a service with no
                    // `healthcheck:` block, that can be well before the app
                    // inside has actually bound its published port. Trusting
                    // that state as done meant `deploy_compose` returned
                    // success while the app was still starting, and the
                    // public-readiness gate in MarkDeploymentCompleteJob
                    // (which runs immediately after) would race it and
                    // revert a perfectly good deployment. Close that gap
                    // here instead: for services without a healthcheck,
                    // require their published TCP ports to actually accept
                    // a connection before calling the stack ready.
                    let unreachable = Self::unreachable_published_ports(&entries).await;
                    if unreachable.is_empty() {
                        return Ok(());
                    }
                    if start.elapsed() >= timeout {
                        let container_logs = self
                            .describe_unhealthy_containers(
                                project_dir,
                                project_name,
                                compose_file,
                                environment_vars,
                            )
                            .await;
                        return Err(ComposeError::ServicesNotReady {
                            project: project_name.to_string(),
                            timeout_secs: timeout.as_secs(),
                            reason: format!("{}{}", unreachable.join(", "), container_logs),
                        });
                    }
                    tokio::time::sleep(COMPOSE_READY_POLL_INTERVAL).await;
                }
                ComposeReadiness::Failed(reasons) => {
                    let container_logs = self
                        .describe_unhealthy_containers(
                            project_dir,
                            project_name,
                            compose_file,
                            environment_vars,
                        )
                        .await;
                    return Err(ComposeError::ServicesNotReady {
                        project: project_name.to_string(),
                        timeout_secs: timeout.as_secs(),
                        reason: format!("{}{}", reasons.join(", "), container_logs),
                    });
                }
                ComposeReadiness::Pending(reasons) => {
                    if start.elapsed() >= timeout {
                        let container_logs = self
                            .describe_unhealthy_containers(
                                project_dir,
                                project_name,
                                compose_file,
                                environment_vars,
                            )
                            .await;
                        return Err(ComposeError::ServicesNotReady {
                            project: project_name.to_string(),
                            timeout_secs: timeout.as_secs(),
                            reason: format!("{}{}", reasons.join(", "), container_logs),
                        });
                    }
                    tokio::time::sleep(COMPOSE_READY_POLL_INTERVAL).await;
                }
            }
        }
    }

    /// Classify a `docker compose ps` snapshot into ready/pending/failed.
    /// A service is ready once it's `running` and, if it declares a
    /// healthcheck, `healthy`. `exited`/`dead` fail fast rather than waiting
    /// out the full timeout; any other state (`created`, `restarting`,
    /// `starting` health) is still pending.
    fn classify_readiness(entries: &[ComposePsEntry]) -> ComposeReadiness {
        if entries.is_empty() {
            return ComposeReadiness::Pending(vec![
                "no containers found after 'docker compose up'".to_string(),
            ]);
        }

        let mut failed = Vec::new();
        let mut pending = Vec::new();
        for entry in entries {
            match entry.state.as_str() {
                "running" => match entry.health.as_str() {
                    "" | "healthy" => {}
                    "unhealthy" => failed.push(format!("service '{}' is unhealthy", entry.service)),
                    other => pending.push(format!("service '{}' is {other}", entry.service)),
                },
                "exited" | "dead" => {
                    failed.push(format!("service '{}' {}", entry.service, entry.state))
                }
                other => pending.push(format!("service '{}' is {other}", entry.service)),
            }
        }

        if !failed.is_empty() {
            ComposeReadiness::Failed(failed)
        } else if !pending.is_empty() {
            ComposeReadiness::Pending(pending)
        } else {
            ComposeReadiness::Ready
        }
    }

    /// For every `running` service that declares no Compose `healthcheck`,
    /// verify at least one of its published TCP ports actually accepts a
    /// connection. `docker compose ps` state alone can't tell "container
    /// process started" from "app is listening" — a Node/Python/etc. app
    /// commonly takes a few seconds after process start to bind its port,
    /// and without this check that gap was invisible to the deploy job.
    /// Services with no published ports (workers, internal-only sidecars)
    /// have nothing to probe and are left as-is.
    async fn unreachable_published_ports(entries: &[ComposePsEntry]) -> Vec<String> {
        let mut reasons = Vec::new();
        for entry in entries {
            if !entry.health.is_empty() {
                // Has a healthcheck — `classify_readiness` already required
                // it to be "healthy" before we got here.
                continue;
            }
            let tcp_ports: Vec<u16> = entry
                .publishers
                .iter()
                .filter(|publisher| {
                    publisher.published_port > 0
                        && (publisher.protocol.is_empty() || publisher.protocol == "tcp")
                })
                .map(|publisher| publisher.published_port)
                .collect();
            if tcp_ports.is_empty() {
                continue;
            }
            let mut any_reachable = false;
            for port in &tcp_ports {
                if Self::port_reachable(*port).await {
                    any_reachable = true;
                    break;
                }
            }
            if !any_reachable {
                reasons.push(format!(
                    "service '{}' published port(s) {} not yet accepting connections",
                    entry.service,
                    tcp_ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        reasons
    }

    /// Best-effort TCP connect probe against a published host port.
    async fn port_reachable(published_port: u16) -> bool {
        tokio::time::timeout(
            PORT_PROBE_CONNECT_TIMEOUT,
            TcpStream::connect(("127.0.0.1", published_port)),
        )
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false)
    }

    async fn discover_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
    ) -> Result<Vec<ComposeServiceResult>, ComposeError> {
        let ps_entries = self
            .compose_ps(project_dir, project_name, compose_file)
            .await?;
        let mut results = Vec::new();

        for ps_entry in ps_entries {
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

    /// Generate a docker-compose.temps-override.yml that adds env_file to every
    /// service, plus an `environment:` override for any key a service already
    /// sets inline AND the project has configured. `env_file:` alone can never
    /// beat a service's own inline `environment:` value in Compose's
    /// precedence rules (inline `environment:` always wins over `env_file:`,
    /// regardless of `-f` file order) — an inline override for the *same* key
    /// beats it instead, since Compose merges `environment:` maps across `-f`
    /// files with later files winning. Without this, a compose file that
    /// hardcodes e.g. `DATABASE_URL: postgres://...@postgres:5432/db` inline
    /// makes any Temps-configured `DATABASE_URL` silently ineffective.
    fn generate_env_override(
        &self,
        compose_content: &str,
        env_file: &str,
        project_env_vars: &HashMap<String, String>,
    ) -> String {
        let services = self.parse_service_names_yaml(compose_content);
        if services.is_empty() {
            return String::new();
        }
        let inline_keys = self.service_inline_environment_keys(compose_content);

        let mut services_map = Mapping::new();
        for service in &services {
            let mut service_map = Mapping::new();
            service_map.insert(
                Value::String("env_file".to_string()),
                Value::Sequence(vec![Value::String(env_file.to_string())]),
            );

            if let Some(service_inline_keys) = inline_keys.get(service) {
                let mut keys: Vec<&String> = service_inline_keys
                    .iter()
                    .filter(|key| project_env_vars.contains_key(*key))
                    .collect();
                keys.sort();
                if !keys.is_empty() {
                    let mut overrides = Mapping::new();
                    for key in keys {
                        if let Some(value) = project_env_vars.get(key) {
                            overrides
                                .insert(Value::String(key.clone()), Value::String(value.clone()));
                        }
                    }
                    service_map.insert(
                        Value::String("environment".to_string()),
                        Value::Mapping(overrides),
                    );
                }
            }

            services_map.insert(Value::String(service.clone()), Value::Mapping(service_map));
        }

        let mut root = Mapping::new();
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(services_map),
        );

        serde_yaml::to_string(&Value::Mapping(root)).unwrap_or_default()
    }

    /// For each service, the set of env var keys its own inline `environment:`
    /// block defines (values are ignored — only used to know which keys are
    /// safe to override via a later `-f` file). Handles both Compose shapes:
    /// the sequence form (`- KEY=value` / bare `- KEY`) and the mapping form
    /// (`KEY: value`). Falls back to an empty map if the content isn't valid
    /// YAML — this only feeds a best-effort override, never blocks the deploy.
    fn service_inline_environment_keys(
        &self,
        compose_content: &str,
    ) -> HashMap<String, HashSet<String>> {
        let mut result = HashMap::new();
        let Ok(mut root) = serde_yaml::from_str::<YamlValue>(compose_content) else {
            return result;
        };
        if root.apply_merge().is_err() {
            return result;
        }
        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return result;
        };

        for (name, service_value) in services {
            let Some(service_name) = name.as_str() else {
                continue;
            };
            let Some(environment) = service_value
                .as_mapping()
                .and_then(|m| m.get("environment"))
            else {
                continue;
            };

            let mut keys = HashSet::new();
            if let Some(seq) = environment.as_sequence() {
                for entry in seq {
                    if let Some(s) = entry.as_str() {
                        let key = s.split('=').next().unwrap_or(s).trim();
                        if !key.is_empty() {
                            keys.insert(key.to_string());
                        }
                    }
                }
            } else if let Some(map) = environment.as_mapping() {
                for (k, _) in map {
                    if let Some(k) = k.as_str() {
                        keys.insert(k.to_string());
                    }
                }
            }

            if !keys.is_empty() {
                result.insert(service_name.to_string(), keys);
            }
        }

        result
    }

    /// Generate a docker-compose override that attaches every service to the
    /// shared Temps network (`temps_core::NETWORK_NAME`), *in addition to*
    /// the project's own default network — never instead of, so same-stack
    /// DNS between compose services keeps working. This is what lets a
    /// compose service reach a Temps-managed external service (database,
    /// cache) or a single-container app deployment by name, the same way
    /// those already reach each other. Both `default` and the Temps network
    /// are declared explicitly (top level and per-service) rather than
    /// relying on an unreferenced network still being auto-created once a
    /// top-level `networks:` block exists — Compose's merge behavior for
    /// per-service `networks:` lists across `-f` files is easy to get subtly
    /// wrong, so this stays maximally explicit.
    fn generate_network_override(&self, compose_content: &str) -> String {
        let services = self.parse_service_names_yaml(compose_content);
        if services.is_empty() {
            return String::new();
        }
        let network_name = temps_core::NETWORK_NAME.as_str();

        let mut networks_declared = Mapping::new();
        networks_declared.insert(
            Value::String("default".to_string()),
            Value::Mapping(Mapping::new()),
        );
        let mut external = Mapping::new();
        external.insert(Value::String("external".to_string()), Value::Bool(true));
        networks_declared.insert(
            Value::String(network_name.to_string()),
            Value::Mapping(external),
        );

        let mut services_map = Mapping::new();
        for service in &services {
            let mut service_map = Mapping::new();
            service_map.insert(
                Value::String("networks".to_string()),
                Value::Sequence(vec![
                    Value::String("default".to_string()),
                    Value::String(network_name.to_string()),
                ]),
            );
            services_map.insert(Value::String(service.clone()), Value::Mapping(service_map));
        }

        let mut root = Mapping::new();
        root.insert(
            Value::String("networks".to_string()),
            Value::Mapping(networks_declared),
        );
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(services_map),
        );

        serde_yaml::to_string(&Value::Mapping(root)).unwrap_or_default()
    }

    /// Minimal Linux capabilities the official postgres/mysql/mariadb/mongo
    /// entrypoints need to fix ownership on a data/socket directory at
    /// container start (`chown`/`chmod` as root, then drop to a service user
    /// via `gosu`/`su-exec`). Granted back only for services the user has
    /// explicitly opted in via `relaxed_capability_services` — every other
    /// service keeps the full `cap_drop: ALL` below.
    const RELAXED_CAPABILITIES: [&'static str; 5] =
        ["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETUID", "SETGID"];

    fn validate_security_exemptions(
        relaxed_capability_services: &[String],
        unsandboxed_services: &[String],
    ) -> Result<(), ComposeError> {
        if let Some(service) = unsandboxed_services
            .iter()
            .find(|service| relaxed_capability_services.contains(service))
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.clone(),
                field: "sandbox".to_string(),
                reason:
                    "a service cannot use both elevated capabilities and a disabled Temps sandbox"
                        .to_string(),
            });
        }
        Ok(())
    }

    /// Generate a docker-compose override that applies the same baseline sandboxing
    /// used by the single-container Docker runtime.
    fn generate_security_override(
        &self,
        compose_content: &str,
        relaxed_capability_services: &[String],
        unsandboxed_services: &[String],
    ) -> String {
        // Enumerate service names from the parsed YAML mapping so inline
        // mappings (`web: {image: nginx}`), anchors (`web: &app`), and merge
        // keys are all hardened, not just lines that end in `:`.
        let services = self.parse_service_names_yaml(compose_content);

        let sandboxed_services = services
            .iter()
            .filter(|service| !unsandboxed_services.contains(service))
            .collect::<Vec<_>>();

        if sandboxed_services.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in sandboxed_services {
            override_yaml.push_str(&format!("  {}:\n", service));
            // Applied last in the `-f` order, so `privileged: false` here wins
            // over anything that smuggled `privileged: true` past validation
            // (e.g. via runtime interpolation) as a last line of defense.
            override_yaml.push_str("    privileged: false\n");
            override_yaml.push_str("    cap_drop:\n");
            override_yaml.push_str("      - ALL\n");
            if relaxed_capability_services.iter().any(|s| s == service) {
                override_yaml.push_str("    cap_add:\n");
                for cap in Self::RELAXED_CAPABILITIES {
                    override_yaml.push_str(&format!("      - {}\n", cap));
                }
            }
            override_yaml.push_str("    security_opt:\n");
            // Prevents exec-based privilege re-escalation (SUID binaries,
            // capability gains on exec) after the entrypoint drops to the
            // service user. Does NOT suppress a relaxed service's entrypoint
            // from using capabilities it was already granted above (e.g.
            // `gosu` calling `setuid()` directly) — that's the intended
            // behavior, not a gap.
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
    /// Collect every path referenced by an `env_file:` key across all services.
    ///
    /// Handles all three shapes the Compose spec allows:
    /// `env_file: .env`, `env_file: [a.env, b.env]`, and the long form
    /// `env_file: [{path: .env, required: false}]`. Paths are returned exactly
    /// as written, de-duplicated, in first-seen order.
    ///
    /// Paths that cannot be confined to the project (absolute, `..`) are
    /// dropped here rather than propagated: the compose file may come from a
    /// repository the operator does not control, so an `env_file` entry must
    /// never be able to name a location outside the stack directory.
    pub fn collect_env_file_refs(compose_content: &str) -> Vec<String> {
        let mut root: YamlValue = match serde_yaml::from_str(compose_content) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        let _ = root.apply_merge();
        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Vec::new();
        };

        let mut refs: Vec<String> = Vec::new();
        let push = |candidate: Option<&str>, refs: &mut Vec<String>| {
            let Some(path) = candidate else { return };
            if Self::validate_relative_path(path, "env_file").is_err() {
                return;
            }
            if !refs.iter().any(|existing| existing == path) {
                refs.push(path.to_string());
            }
        };

        for service in services.values() {
            match service.get("env_file") {
                Some(YamlValue::String(single)) => push(Some(single.as_str()), &mut refs),
                Some(YamlValue::Sequence(entries)) => {
                    for entry in entries {
                        match entry {
                            YamlValue::String(path) => push(Some(path.as_str()), &mut refs),
                            // Long form: {path: .env, required: false}. `required`
                            // is Compose's own concern — Temps satisfies the
                            // reference either way, so the file exists whichever
                            // the author chose.
                            YamlValue::Mapping(_) => {
                                push(entry.get("path").and_then(YamlValue::as_str), &mut refs)
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        refs
    }

    /// Resolve each referenced env file against the repository checkout,
    /// deciding whether it is copied from the repo or synthesized.
    ///
    /// Pure and side-effect free so the deployment job can log the same plan it
    /// is about to execute — an operator must be able to see that Temps created
    /// a file the repository never contained.
    pub fn plan_env_files(compose_content: &str, repo_dir: Option<&Path>) -> Vec<EnvFilePlan> {
        Self::collect_env_file_refs(compose_content)
            .into_iter()
            .map(|path| {
                let from_repo = repo_dir.and_then(|dir| {
                    // Confine the read for the same reason as the write: the
                    // path comes from the compose file, not from Temps.
                    let candidate = dir.join(&path);
                    let canonical_dir = std::fs::canonicalize(dir).ok()?;
                    let canonical = std::fs::canonicalize(&candidate).ok()?;
                    (canonical.starts_with(&canonical_dir) && canonical.is_file())
                        .then_some(canonical)
                });
                let source = match from_repo {
                    Some(repo_path) => EnvFileSource::Repository(repo_path),
                    None => EnvFileSource::ProjectEnvironment,
                };
                EnvFilePlan { path, source }
            })
            .collect()
    }

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

/// Result of classifying a `docker compose ps` snapshot for readiness.
#[derive(Debug, PartialEq)]
enum ComposeReadiness {
    Ready,
    /// Still starting; each entry is a human-readable reason, e.g.
    /// `"service 'web' is starting"`.
    Pending(Vec<String>),
    /// Reached a terminal failure state (`exited`, `dead`, `unhealthy`).
    Failed(Vec<String>),
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
    /// "healthy"/"unhealthy"/"starting", or "" when the service defines no
    /// healthcheck (older Compose CLI versions omit the field entirely).
    #[serde(default)]
    health: String,
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
        assert_eq!(entry.health, "");
        assert_eq!(entry.publishers.len(), 1);
        assert_eq!(entry.publishers[0].published_port, 8080);
        assert_eq!(entry.publishers[0].target_port, 80);
    }

    #[test]
    fn test_parse_compose_ps_json_with_health() {
        let json = r#"{"ID":"abc123","Name":"myapp-web-1","Service":"web","Image":"nginx:latest","State":"running","Health":"healthy","Publishers":[]}"#;
        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.health, "healthy");
    }

    fn entry(service: &str, state: &str, health: &str) -> ComposePsEntry {
        ComposePsEntry {
            id: format!("{service}-id"),
            name: format!("{service}-1"),
            service: service.to_string(),
            image: "img:latest".to_string(),
            state: state.to_string(),
            health: health.to_string(),
            publishers: Vec::new(),
        }
    }

    #[test]
    fn classify_readiness_no_containers_is_pending() {
        let result = ComposeExecutor::classify_readiness(&[]);
        assert!(matches!(result, ComposeReadiness::Pending(_)));
    }

    #[test]
    fn classify_readiness_running_no_healthcheck_is_ready() {
        let entries = [entry("web", "running", "")];
        assert_eq!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Ready
        );
    }

    #[test]
    fn classify_readiness_running_healthy_is_ready() {
        let entries = [entry("web", "running", "healthy")];
        assert_eq!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Ready
        );
    }

    #[test]
    fn classify_readiness_running_starting_health_is_pending() {
        let entries = [entry("web", "running", "starting")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Pending(_)
        ));
    }

    #[test]
    fn classify_readiness_created_is_pending() {
        let entries = [entry("web", "created", "")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Pending(_)
        ));
    }

    #[test]
    fn classify_readiness_unhealthy_fails_fast() {
        let entries = [entry("web", "running", "unhealthy")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Failed(_)
        ));
    }

    #[test]
    fn classify_readiness_exited_fails_fast() {
        let entries = [entry("web", "exited", "")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Failed(_)
        ));
    }

    #[test]
    fn classify_readiness_one_failed_service_fails_whole_stack() {
        let entries = [
            entry("web", "running", "healthy"),
            entry("db", "exited", ""),
        ];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Failed(_)
        ));
    }

    #[test]
    fn classify_readiness_all_services_must_be_ready() {
        let entries = [
            entry("web", "running", "healthy"),
            entry("worker", "running", ""),
        ];
        assert_eq!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Ready
        );
    }

    fn entry_with_publisher(service: &str, published_port: u16) -> ComposePsEntry {
        let mut e = entry(service, "running", "");
        e.publishers.push(ComposePsPublisher {
            published_port,
            target_port: published_port,
            protocol: "tcp".to_string(),
        });
        e
    }

    #[tokio::test]
    async fn unreachable_published_ports_no_healthcheck_and_no_listener_is_unreachable() {
        // Nothing is bound to this port, so the connect attempt must fail —
        // reproducing a service whose process started but whose app hasn't
        // opened its port yet.
        let entries = [entry_with_publisher("web", 18291)];
        let reasons = ComposeExecutor::unreachable_published_ports(&entries).await;
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("web"));
        assert!(reasons[0].contains("18291"));
    }

    #[tokio::test]
    async fn unreachable_published_ports_no_healthcheck_and_listening_is_reachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let entries = [entry_with_publisher("web", port)];
        let reasons = ComposeExecutor::unreachable_published_ports(&entries).await;
        assert!(reasons.is_empty());
    }

    #[tokio::test]
    async fn unreachable_published_ports_with_healthcheck_is_skipped() {
        // classify_readiness already required "healthy" for this service to
        // reach the caller, so the port probe must not second-guess it even
        // though nothing is listening on the (fake) published port here.
        let mut e = entry("web", "running", "healthy");
        e.publishers.push(ComposePsPublisher {
            published_port: 18292,
            target_port: 18292,
            protocol: "tcp".to_string(),
        });
        let reasons = ComposeExecutor::unreachable_published_ports(&[e]).await;
        assert!(reasons.is_empty());
    }

    #[tokio::test]
    async fn unreachable_published_ports_with_no_published_ports_is_skipped() {
        // A worker/internal-only service has nothing to probe.
        let entries = [entry("worker", "running", "")];
        let reasons = ComposeExecutor::unreachable_published_ports(&entries).await;
        assert!(reasons.is_empty());
    }

    #[test]
    fn test_parse_compose_ps_no_ports() {
        let json = r#"{"ID":"def456","Name":"myapp-redis-1","Service":"redis","Image":"redis:7","State":"running","Publishers":[]}"#;

        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.service, "redis");
        assert!(entry.publishers.is_empty());
    }

    #[test]
    fn compose_build_forces_platform_build_args_over_tenant_values() {
        let project_dir = Path::new("/tmp/temps-compose-command-test");
        let build_args = HashMap::from([(
            "BUILDKIT_CACHE_MOUNT_NS".to_string(),
            "platform-derived".to_string(),
        )]);

        let command = ComposeExecutor::compose_build_command(
            project_dir,
            "temps-42-7",
            "docker-compose.yml",
            &build_args,
        );
        let command = command.as_std();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(
            args.windows(2).any(|pair| {
                pair == [
                    "--build-arg".to_string(),
                    "BUILDKIT_CACHE_MOUNT_NS=platform-derived".to_string(),
                ]
            }),
            "platform namespace must be an explicit Compose build argument: {args:?}",
        );
        assert!(!args.iter().any(|arg| arg.contains("tenant-controlled")));
        assert!(!args.iter().any(|arg| arg.contains("RUNTIME_ONLY")));

        let namespace_env = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("BUILDKIT_CACHE_MOUNT_NS"))
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(namespace_env, None);
        let allowed = [
            "PATH",
            "PWD",
            "HOME",
            "DOCKER_CONFIG",
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
        ];
        assert!(command.get_envs().all(|(key, _)| {
            allowed
                .iter()
                .any(|allowed_key| key == std::ffi::OsStr::new(allowed_key))
        }));
        assert!(command
            .get_envs()
            .all(|(key, _)| key != std::ffi::OsStr::new("TEMPS_ENCRYPTION_KEY")));
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

        let override_yaml = executor.generate_env_override(compose, ".env.temps", &HashMap::new());
        assert!(override_yaml.contains("web:"));
        assert!(override_yaml.contains("redis:"));
        assert!(override_yaml.contains("postgres:"));
        assert!(override_yaml.contains(".env.temps"));
        // Each service should have env_file
        assert_eq!(override_yaml.matches("env_file:").count(), 3);
    }

    #[test]
    fn test_generate_env_override_beats_inline_environment_for_matching_key() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  postgres:
    image: postgres:17-alpine
  hub:
    image: ghcr.io/getpaseo/hub:latest
    env_file: .env
    environment:
      DATABASE_URL: postgres://paseo:paseo@postgres:5432/paseo_hub
      PORT: 3000
"#;
        let mut project_vars = HashMap::new();
        project_vars.insert(
            "DATABASE_URL".to_string(),
            "postgres://user:pass@managed-db:5432/app".to_string(),
        );
        project_vars.insert("UNRELATED_VAR".to_string(), "value".to_string());

        let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&override_yaml).unwrap();
        let hub_env = parsed
            .get("services")
            .and_then(|s| s.get("hub"))
            .and_then(|h| h.get("environment"))
            .expect("hub should have an environment override");
        assert_eq!(
            hub_env.get("DATABASE_URL").and_then(|v| v.as_str()),
            Some("postgres://user:pass@managed-db:5432/app")
        );
        // PORT is inline in compose.yml but not project-configured — untouched.
        assert!(hub_env.get("PORT").is_none());
        // UNRELATED_VAR is project-configured but not inline in compose.yml —
        // never force-injected into a service that doesn't reference it.
        assert!(hub_env.get("UNRELATED_VAR").is_none());

        // postgres has no inline `environment:` referencing DATABASE_URL, so
        // it gets env_file only, no environment: override block at all.
        let postgres_service = parsed
            .get("services")
            .and_then(|s| s.get("postgres"))
            .unwrap();
        assert!(postgres_service.get("environment").is_none());
        assert!(postgres_service.get("env_file").is_some());
    }

    #[test]
    fn test_generate_env_override_handles_special_characters_in_values() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  app:
    image: app:latest
    environment:
      SECRET: placeholder
"#;
        let mut project_vars = HashMap::new();
        project_vars.insert(
            "SECRET".to_string(),
            "va:lue with \"quotes\" and\nnewline".to_string(),
        );

        let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&override_yaml).unwrap();
        let secret = parsed
            .get("services")
            .and_then(|s| s.get("app"))
            .and_then(|a| a.get("environment"))
            .and_then(|e| e.get("SECRET"))
            .and_then(|v| v.as_str());
        assert_eq!(secret, Some("va:lue with \"quotes\" and\nnewline"));
    }

    #[test]
    fn test_generate_env_override_ignores_sequence_form_environment() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  app:
    image: app:latest
    environment:
      - DATABASE_URL=postgres://local/db
      - PORT
"#;
        let mut project_vars = HashMap::new();
        project_vars.insert(
            "DATABASE_URL".to_string(),
            "postgres://managed/db".to_string(),
        );

        let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&override_yaml).unwrap();
        let app_env = parsed
            .get("services")
            .and_then(|s| s.get("app"))
            .and_then(|a| a.get("environment"))
            .expect("sequence-form environment keys should still be detected");
        assert_eq!(
            app_env.get("DATABASE_URL").and_then(|v| v.as_str()),
            Some("postgres://managed/db")
        );
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
            "labels: {sh.temps.managed: 'false'}",
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
        let override_yaml = executor.generate_env_override(compose, ".env.temps", &HashMap::new());
        assert!(override_yaml.is_empty());
    }

    #[test]
    fn test_generate_network_override_attaches_every_service() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  hub:
    image: ghcr.io/getpaseo/hub:latest
  postgres:
    image: postgres:17-alpine
"#;
        let override_yaml = executor.generate_network_override(compose);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&override_yaml).unwrap();

        let network_name = temps_core::NETWORK_NAME.as_str();
        let top_level_networks = parsed.get("networks").expect("top-level networks: block");
        assert!(top_level_networks.get("default").is_some());
        assert_eq!(
            top_level_networks
                .get(network_name)
                .and_then(|n| n.get("external"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        for service in ["hub", "postgres"] {
            let service_networks = parsed
                .get("services")
                .and_then(|s| s.get(service))
                .and_then(|s| s.get("networks"))
                .and_then(|n| n.as_sequence())
                .unwrap_or_else(|| panic!("{service} should have a networks: list"));
            let names: Vec<&str> = service_networks.iter().filter_map(|v| v.as_str()).collect();
            assert!(names.contains(&"default"));
            assert!(names.contains(&network_name));
        }
    }

    #[test]
    fn test_generate_network_override_empty_for_no_services() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = "version: '3'\n";
        let override_yaml = executor.generate_network_override(compose);
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
    fn test_strip_excluded_services_removes_service_and_sequence_depends_on() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  app:
    image: myapp:latest
    depends_on:
      - db
      - redis
  db:
    image: postgres:16
  redis:
    image: redis:7
"#;
        let result = executor
            .strip_excluded_services(compose, &["db".to_string()])
            .unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        let services = parsed.get("services").unwrap().as_mapping().unwrap();
        assert!(!services.contains_key("db"));
        assert!(services.contains_key("redis"));
        let depends_on = services
            .get("app")
            .unwrap()
            .get("depends_on")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(depends_on.len(), 1);
        assert_eq!(depends_on[0].as_str(), Some("redis"));
    }

    #[test]
    fn test_strip_excluded_services_removes_mapping_form_depends_on() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  app:
    image: myapp:latest
    depends_on:
      db:
        condition: service_healthy
  db:
    image: postgres:16
"#;
        let result = executor
            .strip_excluded_services(compose, &["db".to_string()])
            .unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        let services = parsed.get("services").unwrap().as_mapping().unwrap();
        assert!(!services.contains_key("db"));
        let depends_on = services
            .get("app")
            .unwrap()
            .get("depends_on")
            .unwrap()
            .as_mapping()
            .unwrap();
        assert!(depends_on.is_empty());
    }

    #[test]
    fn test_strip_excluded_services_noop_for_absent_service() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  app:
    image: myapp:latest
"#;
        let result = executor
            .strip_excluded_services(compose, &["nonexistent".to_string()])
            .unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert!(parsed
            .get("services")
            .unwrap()
            .as_mapping()
            .unwrap()
            .contains_key("app"));
    }

    #[test]
    fn test_strip_excluded_services_empty_list_returns_content_unchanged() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = "services:\n  app:\n    image: myapp:latest\n";
        let result = executor.strip_excluded_services(compose, &[]).unwrap();
        assert_eq!(result, compose);
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
    fn test_validate_compose_security_policy_rejects_network_filter_bypasses() {
        let docker = Docker::connect_with_defaults().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), PathBuf::from("/tmp/test"));

        for (field, network) in [
            ("networks.direct.driver", "driver: macvlan"),
            ("networks.direct.driver", "driver: ipvlan"),
            ("networks.direct.external", "external: true"),
        ] {
            let compose = format!(
                "services:\n  web:\n    image: alpine\n    networks: [direct]\nnetworks:\n  direct:\n    {network}\n"
            );
            let error = executor
                .validate_compose_security_policy("compose file", &compose)
                .expect_err("L2 and external networks must not bypass metadata filtering");
            assert!(matches!(
                error,
                ComposeError::SecurityPolicyViolation { field: actual, .. } if actual == field
            ));
        }
    }

    #[test]
    fn test_validate_compose_security_policy_allows_managed_bridge_network() {
        let docker = Docker::connect_with_defaults().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), PathBuf::from("/tmp/test"));
        let compose = r#"
services:
  web:
    image: alpine
    networks: [app]
networks:
  app:
    driver: bridge
"#;

        executor
            .validate_compose_security_policy("compose file", compose)
            .expect("managed bridge networks remain supported");
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
        let override_yaml = executor.generate_security_override(compose, &[], &[]);

        assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
        assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 2);
        assert_eq!(override_yaml.matches("pids_limit: 512").count(), 2);
        assert_eq!(override_yaml.matches("init: true").count(), 2);
        assert!(!override_yaml.contains("cap_add"));
    }

    #[test]
    fn test_unsandboxed_service_keeps_its_image_owned_init_process() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
    restart: unless-stopped
"#;

        let override_yaml =
            executor.generate_security_override(compose, &[], &["webserver".to_string()]);

        assert!(override_yaml.is_empty());
    }

    #[test]
    fn test_security_override_skips_only_explicitly_unsandboxed_services() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
  worker:
    image: alpine:latest
"#;

        let override_yaml =
            executor.generate_security_override(compose, &[], &["webserver".to_string()]);

        assert!(!override_yaml.contains("  webserver:"));
        assert!(override_yaml.contains("  worker:"));
        assert_eq!(override_yaml.matches("cap_drop:").count(), 1);
        assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 1);
        assert_eq!(override_yaml.matches("pids_limit: 512").count(), 1);
        assert_eq!(override_yaml.matches("init: true").count(), 1);
    }

    #[tokio::test]
    async fn test_security_override_runtime_matches_per_service_selection() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose_available = tokio::process::Command::new("docker")
            .args(["compose", "version"])
            .output()
            .await
            .is_ok_and(|output| output.status.success());
        if !compose_available
            || executor
                .docker
                .inspect_image("alpine:latest")
                .await
                .is_err()
        {
            println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
            return;
        }

        let compose = r#"
services:
  image-owned-init:
    image: alpine:latest
    command: ["sleep", "30"]
    network_mode: none
  sandboxed:
    image: alpine:latest
    command: ["sleep", "30"]
    network_mode: none
"#;
        let override_yaml =
            executor.generate_security_override(compose, &[], &["image-owned-init".to_string()]);
        let project_dir = tempfile::tempdir().unwrap();
        let compose_path = project_dir.path().join("compose.yml");
        let override_path = project_dir.path().join("security.yml");
        tokio::fs::write(&compose_path, compose).await.unwrap();
        tokio::fs::write(&override_path, override_yaml)
            .await
            .unwrap();
        let project_name = format!("temps-security-runtime-{}", std::process::id());
        let compose_args = |action: &str| {
            vec![
                "compose".to_string(),
                "-p".to_string(),
                project_name.clone(),
                "-f".to_string(),
                compose_path.to_string_lossy().into_owned(),
                "-f".to_string(),
                override_path.to_string_lossy().into_owned(),
                action.to_string(),
            ]
        };

        let up = tokio::process::Command::new("docker")
            .args(compose_args("up"))
            .args(["-d", "--no-build"])
            .output()
            .await
            .unwrap();
        assert!(
            up.status.success(),
            "Docker Compose runtime setup failed: {}",
            String::from_utf8_lossy(&up.stderr)
        );

        let docker = executor.docker.clone();
        let inspect_service = |service: &'static str| {
            let args = compose_args("ps");
            let docker = docker.clone();
            async move {
                let output = tokio::process::Command::new("docker")
                    .args(args)
                    .args(["-q", service])
                    .output()
                    .await
                    .unwrap();
                let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                docker.inspect_container(&id, None).await.unwrap()
            }
        };
        let image_owned = inspect_service("image-owned-init").await;
        let sandboxed = inspect_service("sandboxed").await;

        let down = tokio::process::Command::new("docker")
            .args(compose_args("down"))
            .args(["--remove-orphans", "--volumes"])
            .output()
            .await
            .unwrap();
        assert!(
            down.status.success(),
            "Docker Compose cleanup failed: {}",
            String::from_utf8_lossy(&down.stderr)
        );

        let image_owned_host = image_owned.host_config.unwrap();
        assert_ne!(image_owned_host.init, Some(true));
        assert!(image_owned_host.cap_drop.unwrap_or_default().is_empty());
        assert!(image_owned_host.security_opt.unwrap_or_default().is_empty());
        assert!(image_owned_host.pids_limit.is_none());

        let sandboxed_host = sandboxed.host_config.unwrap();
        assert_eq!(sandboxed_host.init, Some(true));
        assert_eq!(sandboxed_host.cap_drop.unwrap_or_default(), ["ALL"]);
        assert!(sandboxed_host
            .security_opt
            .unwrap_or_default()
            .iter()
            .any(|option| option == "no-new-privileges:true"));
        assert_eq!(sandboxed_host.pids_limit, Some(512));
    }

    #[test]
    fn test_security_override_is_empty_when_every_service_is_unsandboxed() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = "services:\n  webserver:\n    image: paperless:latest\n";

        let override_yaml =
            executor.generate_security_override(compose, &[], &["webserver".to_string()]);

        assert!(override_yaml.is_empty());
    }

    #[test]
    fn test_security_exemptions_reject_overlapping_modes_at_deploy_time() {
        let error = ComposeExecutor::validate_security_exemptions(
            &["webserver".to_string()],
            &["webserver".to_string()],
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ComposeError::SecurityPolicyViolation {
                ref service,
                ref field,
                ..
            } if service == "webserver" && field == "sandbox"
        ));
    }

    #[tokio::test]
    async fn test_write_compose_files_removes_stale_security_override() {
        let Some(executor) = test_executor() else {
            return;
        };
        let project_dir = tempfile::tempdir().unwrap();
        let stale_override = project_dir.path().join("docker-compose.temps-security.yml");
        tokio::fs::write(
            &stale_override,
            "services:\n  webserver:\n    cap_drop:\n      - ALL\n",
        )
        .await
        .unwrap();
        let request = ComposeDeployRequest {
            project_name: "temps-test".to_string(),
            compose_content:
                "services:\n  webserver:\n    image: paperlessngx/paperless-ngx:latest\n"
                    .to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: vec!["webserver".to_string()],
        };

        executor
            .write_compose_files(project_dir.path(), &request)
            .await
            .unwrap();

        assert!(!stale_override.exists());
    }

    #[test]
    fn test_generate_security_override_grants_relaxed_capabilities() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = r#"
services:
  db:
    image: postgres:18
  web:
    image: nginx
"#;
        let override_yaml = executor.generate_security_override(compose, &["db".to_string()], &[]);

        // Both services still get the strict cap_drop baseline.
        assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
        assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 2);

        // Only the relaxed service gets cap_add, with exactly the minimal set.
        assert_eq!(override_yaml.matches("cap_add:").count(), 1);
        for cap in ["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETUID", "SETGID"] {
            assert_eq!(
                override_yaml.matches(cap).count(),
                1,
                "expected exactly one occurrence of {cap}"
            );
        }

        // cap_add must be nested under `db`, not `web` — check via string
        // position (services are emitted in source order) AND by actually
        // parsing the generated YAML, so a future change to emission order
        // can't silently make the positional check pass while attaching
        // cap_add to the wrong service.
        let db_idx = override_yaml.find("  db:\n").expect("db service present");
        let web_idx = override_yaml.find("  web:\n").expect("web service present");
        let cap_add_idx = override_yaml.find("cap_add:").expect("cap_add present");
        assert!(db_idx < cap_add_idx && cap_add_idx < web_idx);

        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&override_yaml).expect("generated override is valid YAML");
        let db_caps = parsed["services"]["db"]["cap_add"]
            .as_sequence()
            .expect("db has cap_add sequence")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            db_caps,
            vec!["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETUID", "SETGID"]
        );
        assert!(
            parsed["services"]["web"].get("cap_add").is_none(),
            "web must not have cap_add"
        );
    }

    #[test]
    fn test_generate_security_override_empty_relaxed_list_matches_baseline() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let compose = "services:\n  db:\n    image: postgres:18\n";
        let with_empty = executor.generate_security_override(compose, &[], &[]);
        let with_unmatched =
            executor.generate_security_override(compose, &["nonexistent".to_string()], &[]);

        assert_eq!(with_empty, with_unmatched);
        assert!(!with_empty.contains("cap_add"));
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
        let override_yaml = executor.generate_security_override(compose, &[], &[]);
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

        for remote_context in [
            "https://example.test/source.tar.gz",
            "http://127.0.0.1/internal",
            "git://example.test/repository.git",
            "git@example.test:repository.git",
        ] {
            let compose =
                format!("services:\n  app:\n    build:\n      context: {remote_context}\n");
            let err = executor
                .validate_compose_security_policy("compose file", &compose)
                .unwrap_err();
            assert_eq!(violation_field(err), "build.context", "{remote_context}");
        }

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
    fn test_filesystem_confinement_allows_missing_relative_bind_directories() {
        let root = tempfile::tempdir().unwrap();
        let compose_dir = root.path().join("paperless");
        std::fs::create_dir_all(&compose_dir).unwrap();

        let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
    volumes:
      - ./export:/usr/src/paperless/export
      - ./consume:/usr/src/paperless/consume
"#;

        ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "paperless/compose.yml",
            "compose file",
            compose,
        )
        .unwrap();

        // Validation remains read-only. Docker Compose creates short-syntax
        // bind directories when it starts the stack.
        assert!(!compose_dir.join("export").exists());
        assert!(!compose_dir.join("consume").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_confinement_rejects_missing_bind_below_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        symlink("/tmp", root.path().join("escape")).unwrap();
        let compose = r#"
services:
  app:
    image: alpine
    volumes:
      - ./escape/not-created-yet:/data
"#;

        let error = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            compose,
        )
        .unwrap_err();

        assert_eq!(violation_field(error), "volumes");
    }

    #[test]
    fn test_missing_relative_bind_mounts_use_stable_project_storage() {
        let checkout = tempfile::tempdir().unwrap();
        let compose_base = checkout.path().join("paperless");
        let persistent_root = checkout.path().join("temps-data/binds");
        std::fs::create_dir_all(&compose_base).unwrap();
        let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
    volumes:
      - ./export:/usr/src/paperless/export
      - ./consume:/usr/src/paperless/consume:ro
"#;

        let rewritten = ComposeExecutor::rewrite_missing_relative_bind_mounts(
            compose,
            &compose_base,
            &persistent_root,
        )
        .unwrap();
        let export = persistent_root.join(ComposeExecutor::stable_bind_name("export"));
        let consume = persistent_root.join(ComposeExecutor::stable_bind_name("consume"));

        assert!(export.is_dir());
        assert!(consume.is_dir());
        assert!(rewritten.contains(&format!("{}:/usr/src/paperless/export", export.display())));
        assert!(rewritten.contains(&format!(
            "{}:/usr/src/paperless/consume:ro",
            consume.display()
        )));

        // A later checkout adding these directories must not switch the stack
        // away from its already-populated persistent storage.
        std::fs::create_dir_all(compose_base.join("export")).unwrap();
        std::fs::create_dir_all(compose_base.join("consume")).unwrap();
        let redeploy = ComposeExecutor::rewrite_missing_relative_bind_mounts(
            compose,
            &compose_base,
            &persistent_root,
        )
        .unwrap();
        assert!(redeploy.contains(&export.to_string_lossy().to_string()));
        assert!(redeploy.contains(&consume.to_string_lossy().to_string()));
    }

    #[test]
    fn test_existing_repository_bind_is_not_rewritten_without_stable_storage() {
        let checkout = tempfile::tempdir().unwrap();
        let compose_base = checkout.path().join("app");
        let persistent_root = checkout.path().join("temps-data/binds");
        std::fs::create_dir_all(compose_base.join("checked-in-data")).unwrap();
        let compose =
            "services:\n  app:\n    image: alpine\n    volumes:\n      - ./checked-in-data:/data\n";

        let rewritten = ComposeExecutor::rewrite_missing_relative_bind_mounts(
            compose,
            &compose_base,
            &persistent_root,
        )
        .unwrap();

        assert_eq!(rewritten, compose);
        assert!(!persistent_root.exists());
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
    fn test_validate_compose_security_policy_accepts_bounded_shm_sizes() {
        let Some(executor) = test_executor() else {
            return;
        };
        for yaml in [
            "services:\n  immich:\n    image: alpine\n    shm_size: 128mb\n",
            "services:\n  gitlab:\n    image: alpine\n    shm_size: 256m\n",
            "services:\n  app:\n    image: alpine\n    shm_size: 536870912\n",
        ] {
            executor
                .validate_compose_security_policy("compose file", yaml)
                .unwrap_or_else(|error| panic!("expected bounded shm_size to pass: {error}"));
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_oversized_or_invalid_shm_sizes() {
        let Some(executor) = test_executor() else {
            return;
        };
        for yaml in [
            "services:\n  app:\n    image: alpine\n    shm_size: 513m\n",
            "services:\n  app:\n    image: alpine\n    shm_size: unlimited\n",
            "services:\n  app:\n    image: alpine\n    shm_size: 0\n",
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", yaml)
                .unwrap_err();
            assert_eq!(violation_field(err), "shm_size");
        }
    }

    #[test]
    fn test_validate_compose_security_policy_caps_aggregate_shm_size() {
        let Some(executor) = test_executor() else {
            return;
        };
        let yaml = "services:\n  first:\n    image: alpine\n    shm_size: 400m\n  second:\n    image: alpine\n    shm_size: 400m\n  third:\n    image: alpine\n    shm_size: 400m\n";

        let err = executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_err();
        let message = err.to_string();

        assert_eq!(violation_field(err), "shm_size");
        assert!(message.contains("aggregate"));
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
        let override_yaml = executor.generate_security_override(compose, &[], &[]);
        assert_eq!(override_yaml.matches("privileged: false").count(), 2);
    }

    /// Compose allows three shapes for `env_file`; a stack must not fail to
    /// deploy because its author picked one of them.
    #[test]
    fn collects_env_file_refs_in_all_three_compose_shapes() {
        let compose = r#"
services:
  api:
    image: api:latest
    env_file: .env
  worker:
    image: worker:latest
    env_file:
      - .env
      - config/worker.env
  web:
    image: web:latest
    env_file:
      - path: config/web.env
        required: false
"#;
        let refs = ComposeExecutor::collect_env_file_refs(compose);
        // `.env` is referenced by two services but materialized once.
        assert_eq!(refs, vec![".env", "config/worker.env", "config/web.env"]);
    }

    /// The compose file may come from a repository the operator does not
    /// control, so an `env_file` entry must never name a write target outside
    /// the stack directory.
    #[test]
    fn drops_env_file_refs_that_escape_the_project() {
        let compose = r#"
services:
  evil:
    image: evil:latest
    env_file:
      - ../../../root/.ssh/authorized_keys
      - /etc/passwd
      - ok.env
"#;
        assert_eq!(
            ComposeExecutor::collect_env_file_refs(compose),
            vec!["ok.env"]
        );
    }

    #[test]
    fn collects_nothing_from_compose_without_env_files() {
        let compose = "services:\n  api:\n    image: api:latest\n";
        assert!(ComposeExecutor::collect_env_file_refs(compose).is_empty());
        // Unparsable YAML must not panic or invent references.
        assert!(ComposeExecutor::collect_env_file_refs("{{{ not yaml").is_empty());
    }

    /// A file the repository ships is authored config and wins; one it does not
    /// ship is synthesized from the project's environment variables.
    #[test]
    fn plans_repo_copy_when_present_and_synthesis_when_absent() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("config")).unwrap();
        std::fs::write(repo.path().join("config/worker.env"), "FROM_REPO=1\n").unwrap();

        let compose = r#"
services:
  api:
    image: api:latest
    env_file:
      - .env
      - config/worker.env
"#;
        let plans = ComposeExecutor::plan_env_files(compose, Some(repo.path()));
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].path, ".env");
        assert_eq!(plans[0].source, EnvFileSource::ProjectEnvironment);
        assert_eq!(plans[1].path, "config/worker.env");
        assert!(matches!(plans[1].source, EnvFileSource::Repository(_)));
    }

    /// A symlink in the repo must not let a referenced env file read outside it.
    #[cfg(unix)]
    #[test]
    fn plan_rejects_repo_env_file_symlinked_outside_the_checkout() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.env"), "STOLEN=1\n").unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.env"),
            repo.path().join("app.env"),
        )
        .unwrap();

        let compose = "services:\n  api:\n    image: api:latest\n    env_file: app.env\n";
        let plans = ComposeExecutor::plan_env_files(compose, Some(repo.path()));
        assert_eq!(plans[0].source, EnvFileSource::ProjectEnvironment);
    }

    #[test]
    fn renders_env_file_deterministically() {
        let mut vars = HashMap::new();
        vars.insert("B_KEY".to_string(), "2".to_string());
        vars.insert("A_KEY".to_string(), "1".to_string());
        assert_eq!(
            render_env_file(&vars).unwrap(),
            "A_KEY=\"1\"\nB_KEY=\"2\"\n"
        );
        assert_eq!(render_env_file(&HashMap::new()).unwrap(), "");
    }

    #[test]
    fn render_env_file_quotes_newlines_dollars_and_quotes_without_injecting_keys() {
        let vars = HashMap::from([(
            "SAFE_KEY".to_string(),
            "first\nINJECTED=bad $TOKEN \\\"quoted\\\"".to_string(),
        )]);

        let rendered = render_env_file(&vars).unwrap();

        assert_eq!(
            rendered,
            "SAFE_KEY=\"first\\nINJECTED=bad $$TOKEN \\\\\\\"quoted\\\\\\\"\"\n"
        );
        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn render_env_file_rejects_invalid_keys_and_control_characters() {
        let invalid_key = HashMap::from([("DOCKER_HOST\nPATH".to_string(), "x".to_string())]);
        assert!(matches!(
            render_env_file(&invalid_key),
            Err(ComposeError::InvalidEnvironmentVariable { .. })
        ));

        let invalid_value = HashMap::from([("SAFE_KEY".to_string(), "bad\u{0000}".to_string())]);
        assert!(matches!(
            render_env_file(&invalid_value),
            Err(ComposeError::InvalidEnvironmentVariable { .. })
        ));
    }

    #[test]
    fn compose_diagnostics_redact_known_and_structured_credentials() {
        let environment = HashMap::from([(
            "ARBITRARY_NAME".to_string(),
            "known-environment-secret".to_string(),
        )]);
        let build_args =
            HashMap::from([("BUILD_VALUE".to_string(), "known-build-secret".to_string())]);
        let diagnostic = "known-environment-secret known-build-secret \
            password=literal-password Authorization: Bearer abc.def.ghi \
            https://user:literal-uri-password@example.test/path";

        let sanitized = sanitize_compose_diagnostic(diagnostic, &environment, &build_args);

        for secret in [
            "known-environment-secret",
            "known-build-secret",
            "literal-password",
            "abc.def.ghi",
            "literal-uri-password",
        ] {
            assert!(!sanitized.contains(secret), "diagnostic leaked {secret}");
        }
        assert!(sanitized.contains("<redacted>"));
    }

    #[test]
    fn compose_diagnostics_are_bounded_on_utf8_boundaries() {
        let diagnostic = "🔒".repeat(MAX_COMPOSE_DIAGNOSTIC_BYTES);

        let sanitized = sanitize_compose_diagnostic(&diagnostic, &HashMap::new(), &HashMap::new());

        assert!(sanitized.len() < diagnostic.len());
        assert!(sanitized.contains("diagnostic truncated"));
    }
}
