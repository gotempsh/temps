// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker Compose deployment executor.
//!
//! Manages multi-container deployments using `docker compose` CLI commands.
//! After `compose up`, discovers running containers, applies Temps labels,
//! and returns per-service results that get inserted into `deployment_containers`.

use bollard::query_parameters::{LogsOptions, RemoveContainerOptions};
use bollard::Docker;
use futures::{StreamExt, TryStreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use serde_yaml::{Mapping, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Weak};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

/// How long `deploy()` waits for every Compose service to report `running`
/// (and `healthy`, if it defines a healthcheck) before failing the
/// deployment. Mirrors the single-container deploy path's
/// `health_check_timeout_secs` default.
const COMPOSE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Interval between `docker compose ps` polls while waiting for readiness.
const COMPOSE_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Maximum time allowed for `docker compose pull` to complete. Pulls can be
/// slow on large images or slow registries, but an unbounded pull would stall a
/// deployment forever. 300 s matches [`COMPOSE_READY_TIMEOUT`] — both bound a
/// single phase of the deployment that should complete within minutes, not
/// hours, on any reasonable network.
const COMPOSE_PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const COMPOSE_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);
const COMPOSE_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Resolving the fully merged Compose model is local work and should complete
/// quickly. Keep it bounded independently from image pulls so a wedged Compose
/// plugin cannot stall a deployment after all images have already downloaded.
const COMPOSE_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A merged Compose model expands anchors and interpolation, so its output can
/// be much larger than the source. Bound both memory and subsequent Docker API
/// fan-out before parsing tenant-controlled output.
const MAX_RESOLVED_COMPOSE_CONFIG_BYTES: usize = 4 * 1024 * 1024;
const MAX_COMPOSE_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RESOLVED_COMPOSE_SERVICES: usize = 256;
// Platform-owned safety ceilings, not reservations. These remain high enough
// for database and application services while preventing a single container
// from consuming the entire host. A future per-service resource editor can
// safely choose lower values without weakening these maxima.
const COMPOSE_SERVICE_CPU_LIMIT: &str = "4.0";
const COMPOSE_SERVICE_MEMORY_LIMIT: &str = "4g";
const DOCKER_IMAGE_INSPECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_CONCURRENT_IMAGE_INSPECTIONS: usize = 8;

/// How long a single TCP connect attempt against a published port may take
/// before `port_reachable` gives up and reports the port not yet listening.
/// Short by design: this runs once per poll interval, not once total, so a
/// slow/unreachable port degrades to "still pending" rather than stalling
/// the whole readiness loop.
const PORT_PROBE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);

/// Docker label Compose attaches to every network/volume/container it
/// manages, set to the `-p <project_name>` value.
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// Temps-generated override that bind-mounts the stack's materialized secret
/// files into every service. Written last in the `-f` order so a repository
/// or user override cannot redirect the mount.
const TEMPS_SECRETS_OVERRIDE: &str = "docker-compose.temps-secrets.yml";

static COMPOSE_LIFECYCLE_LOCKS: LazyLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Mount point for project secrets inside every container. Identical to the
/// single-container deploy path (`DockerRuntime`), so an application reads its
/// secrets the same way regardless of which preset deployed it.
const CONTAINER_SECRETS_DIR: &str = "/run/secrets";

/// Maximum diagnostic text persisted into a deployment error. Full container
/// logs remain available through the authenticated logs endpoint.
const MAX_COMPOSE_DIAGNOSTIC_BYTES: usize = 32 * 1024;

/// Where the console exposes the per-service "Elevated permissions" toggle.
/// Named once here because two places quote it — the pre-deploy advisory in
/// `temps-deployments::jobs::deploy_compose` and the post-failure remediation
/// in [`ComposeExecutor::capability_denial_remediation`]. A UI reshuffle that
/// updated only one of them would send half of users to a page that no longer
/// exists.
pub const ELEVATED_PERMISSIONS_SETTINGS_PATH: &str = "Project Settings → Git → Compose services";

/// Keys an inline override may not set because [`ComposeExecutor`]'s
/// deploy-time security policy rejects them outright, wherever they appear.
/// Telling someone to move one of these into the repository compose file would
/// be sending them to do work that fails later, at deploy, with a different
/// error. Mirrors `temps-presets::docker_compose::NEVER_ALLOWED_SERVICE_KEYS`,
/// which guards the same override on the preview path.
const NEVER_ALLOWED_OVERRIDE_KEYS: &[&str] = &[
    "privileged",
    "cgroup_parent",
    "cap_add",
    "devices",
    "device_cgroup_rules",
    "security_opt",
    "sysctls",
    "volumes_from",
    "external_links",
    "label_file",
    "post_start",
    "pre_stop",
    "provider",
    "container_name",
    "blkio_config",
    "storage_opt",
    "memswap_limit",
    "group_add",
    "runtime",
    "oom_kill_disable",
    "tmpfs",
    "ulimits",
];

/// Keys an inline override may not set, but which the repository compose file
/// legitimately can — [`ComposeExecutor::validate_compose_security_policy`]
/// admits them subject to its own checks (bind-mount path confinement for
/// `volumes`, byte caps for `shm_size`, host-namespace rejection for
/// `pid`/`ipc`/`network_mode` and friends). Kept separate from
/// [`NEVER_ALLOWED_OVERRIDE_KEYS`] so the error can point somewhere that
/// actually works instead of issuing the same blanket "move it to the
/// repository" advice for every key.
const REPO_ONLY_OVERRIDE_KEYS: &[&str] = &[
    "network_mode",
    "pid",
    "ipc",
    "uts",
    "cgroup",
    "userns_mode",
    "cap_drop",
    "volumes",
    "shm_size",
    "labels",
    "build",
    "image",
    "env_file",
];
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

/// A failed Compose startup together with every container Docker managed to
/// create for the candidate stack.
///
/// Readiness failures are operationally different from preparation failures:
/// the containers often contain the only useful diagnostics. Carrying them
/// alongside the typed error lets the deployment layer register them for
/// authenticated log access instead of immediately destroying the evidence.
#[derive(Debug)]
pub struct ComposeDeployFailure {
    pub error: ComposeError,
    pub containers: Vec<ComposeServiceResult>,
    /// Ownership-verified containers that are unsafe to retain but can be
    /// removed directly if `docker compose down` fails.
    pub cleanup_containers: Vec<ComposeServiceResult>,
    /// Why a partially-created stack could not be retained safely. The
    /// primary deployment error remains in `error`; this secondary reason is
    /// for operator diagnostics and forces the caller to tear the stack down.
    pub retention_error: Option<ComposeError>,
}

impl std::fmt::Display for ComposeDeployFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ComposeDeployFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl ComposeDeployFailure {
    fn without_containers(error: ComposeError) -> Self {
        Self {
            error,
            containers: Vec::new(),
            cleanup_containers: Vec::new(),
            retention_error: None,
        }
    }
}

/// Compose filenames Temps generates itself inside the stack directory.
///
/// `compose_up` passes every one of these to `docker compose -f` purely
/// because it exists on disk, and only the *user-supplied* override is run
/// through `validate_compose_override`. A tenant compose file that names one
/// of them as an `env_file:` target would therefore have Temps write
/// attacker-influenced bytes to a path the daemon later parses as trusted
/// Compose YAML — dotenv syntax and YAML syntax overlap enough (`#` comments,
/// bare `key: value` lines) to smuggle a whole `services:` mapping past the
/// dotenv reader. That is a sandbox escape, so referencing these names is
/// rejected outright rather than silently skipped.
///
/// This is the *single* list. `validate_compose_path_not_generated` consumed a
/// second, longer copy of it, and the two drifted: `TEMPS_SECRETS_OVERRIDE` was
/// on that one but not here, even though it is passed to `docker compose -f` on
/// sight exactly like the rest. Only write ordering kept that from being
/// exploitable — the secrets override is written or deleted unconditionally
/// after the `env_file` loop, unlike the labels override, which is written only
/// when labels exist and was therefore reachable. One refactor making the
/// secrets override conditional would have reopened the escape, so the lists
/// are now the same list.
pub(crate) const RESERVED_GENERATED_COMPOSE_FILES: &[&str] = &[
    "docker-compose.temps-env.yml",
    "docker-compose.temps-network.yml",
    "docker-compose.temps-override.yml",
    "docker-compose.temps-labels.yml",
    "docker-compose.temps-security.yml",
    TEMPS_SECRETS_OVERRIDE,
];

/// Whether `path` names one of the Compose files Temps generates.
///
/// Compares the final component only: `./docker-compose.temps-override.yml`
/// and `docker-compose.temps-override.yml` resolve to the same file in the
/// stack root, and a same-named file in a subdirectory is not passed to
/// `docker compose -f`, so it is harmless.
pub(crate) fn is_reserved_generated_compose_file(path: &Path) -> bool {
    // Only the stack root is dangerous: `append_compose_file_args` looks for
    // these names directly under the project directory.
    if path.parent().is_some_and(|parent| {
        !parent.as_os_str().is_empty() && parent != Path::new(".") && parent != Path::new("")
    }) {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            RESERVED_GENERATED_COMPOSE_FILES
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
        })
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

/// True when a container's log tail shows its entrypoint being denied a
/// privileged startup operation — the signature of Temps' own `cap_drop: ALL`
/// sandbox rather than a fault in the image or the user's configuration.
///
/// The official postgres/mysql/mariadb/mongo entrypoints (and others, e.g.
/// Gitea) start as root, `chown`/`chmod` their data directory, then drop to a
/// service user via `gosu`/`su-exec`. All three steps need capabilities the
/// sandbox removes, so they fail with `EPERM` unless the service is listed in
/// `relaxed_capability_services`.
///
/// Deliberately a two-factor match: "operation not permitted" on its own
/// appears in plenty of ordinary application errors, and pointing those at a
/// capability toggle that cannot help would be worse than saying nothing.
/// Requiring a privileged-operation verb alongside it keeps the hint tied to
/// the failure mode it actually explains.
fn looks_like_capability_denial(logs: &str) -> bool {
    const PRIVILEGED_OPERATIONS: [&str; 8] = [
        "chmod",
        "chown",
        "setgroups",
        "setuid",
        "setgid",
        "failed switching to",
        "su-exec",
        "gosu",
    ];

    let lower = logs.to_ascii_lowercase();
    lower.contains("operation not permitted")
        && PRIVILEGED_OPERATIONS
            .iter()
            .any(|operation| lower.contains(operation))
}

/// Render service names as a quoted, comma-separated list so a remediation
/// message reads the same for one service as for several.
fn quote_service_list(services: &[&String]) -> String {
    services
        .iter()
        .map(|service| format!("'{service}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every literal value a Compose deployment knows to be sensitive, gathered
/// once so it can be scrubbed out of any diagnostic the deploy produces.
///
/// Takes values rather than a map because the sources overlap by key —
/// a secret and an environment variable may share a name, and merging them
/// into one map would silently drop one of the two values from redaction.
fn collect_redactable_values(request: &ComposeDeployRequest) -> Vec<String> {
    let mut values = request
        .environment_vars
        .values()
        .chain(request.build_args.values())
        .chain(request.secrets.values())
        .filter(|value| !value.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(env_content) = request.env_content.as_deref() {
        values.extend(env_content.lines().filter_map(|line| {
            let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (_, value) = line.split_once('=')?;
            let value = value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            (!value.is_empty()).then(|| value.to_string())
        }));
    }
    values
}

fn sanitize_compose_diagnostic(diagnostic: &str, redact_values: &[String]) -> String {
    let mut sanitized = diagnostic.to_string();
    for value in redact_values {
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
    /// Decrypted project secrets, keyed by name. Materialized as files under
    /// `/run/secrets/<KEY>` via a generated override.
    ///
    /// Deliberately separate from [`Self::environment_vars`] and
    /// [`Self::build_args`]: values here must never reach a container
    /// environment, a build argument, an env file, or `docker inspect`.
    pub secrets: HashMap<String, String>,
    /// Which Compose services may read each secret, keyed by secret name.
    ///
    /// A key absent from this map (or mapped to an empty list) goes to every
    /// service -- that is the pre-scoping behaviour, and it has to stay the
    /// default so an unconfigured secret is never silently withheld from the
    /// service that needs it.
    pub secret_compose_services: HashMap<String, Vec<String>>,
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

/// State produced by [`ComposeExecutor::prepare_and_pull`] and consumed by
/// [`ComposeExecutor::deploy_prepared`].
///
/// Carrying these fields out of `prepare_and_pull` avoids re-computing them in
/// `deploy_prepared` and keeps the split-phase API type-safe: a caller cannot
/// pass an arbitrary directory to `deploy_prepared` — it must come from the
/// same preparation step that wrote the files to disk.
pub struct PreparedComposeDeploy {
    /// Resolved working directory (repo checkout when available, otherwise the
    /// Temps data-dir project directory).
    pub effective_dir: PathBuf,
    /// Compose project name (e.g., `"temps-{project_id}-{env_id}"`).
    pub project_name: String,
    /// Compose file name relative to `effective_dir`.
    pub compose_file: String,
    /// Values to strip from diagnostic messages (secrets, env values, etc.).
    pub redact_values: Vec<String>,
    /// Deployment-scoped directory containing only this candidate's secrets.
    /// The previous generation remains mounted by the live stack until this
    /// candidate has started successfully.
    pub secret_generation: String,
    pub has_materialized_secrets: bool,
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

    /// Serialize prepare → teardown → start → compensation for one Compose
    /// project. Superseding workflows overlap cooperatively, so without this
    /// lock an older cancellation could delete a newer workflow's candidate
    /// files or containers.
    pub async fn acquire_project_lifecycle_lock(&self, project_name: &str) -> OwnedMutexGuard<()> {
        let data_dir =
            std::fs::canonicalize(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
        let key = format!("{}\0{project_name}", data_dir.display());
        let lock = {
            let mut locks = COMPOSE_LIFECYCLE_LOCKS.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(&key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }

    /// Get the work directory for a compose project.
    fn project_dir(&self, project_name: &str) -> PathBuf {
        self.data_dir.join("compose").join(project_name)
    }

    /// Root directory holding every stack's materialized secret files.
    ///
    /// Created `0700` so no other local user can traverse into it — that
    /// directory bit is the entire host-side protection for these files, since
    /// the per-stack directory below it has to stay traversable by whatever
    /// uid the container's image happens to run as.
    ///
    /// Deliberately *not* under [`Self::project_dir`]: the per-stack work
    /// directory is created `0755` by `write_compose_files` and shares its
    /// parent with every other stack, so it cannot be the confidentiality
    /// boundary. Mirrors `DockerRuntime`'s `$TEMPS_DATA_DIR/secrets` root,
    /// kept separate so a container name and a Compose project name can never
    /// collide on the same directory.
    fn secrets_root(&self) -> PathBuf {
        self.data_dir.join("compose-secrets")
    }

    /// Host directory bind-mounted at `/run/secrets` for every service in a
    /// stack. Lives under the Temps data dir rather than the repository
    /// checkout because git-backed deployments run Compose from an ephemeral
    /// checkout that is deleted as soon as the deploy job finishes — a mount
    /// source inside it would be gone by the first container restart.
    fn secrets_dir(&self, project_name: &str) -> PathBuf {
        self.secrets_root().join(project_name)
    }

    fn secret_generation_dir(&self, project_name: &str, generation: &str) -> PathBuf {
        self.secrets_dir(project_name).join(generation)
    }

    /// Which secrets a given Compose service is entitled to read.
    ///
    /// A secret with no scope entry (or an empty one) is readable by every
    /// service. Scoping is opt-in: "not configured" must never mean "withheld",
    /// or enabling this feature would break stacks on upgrade.
    fn secrets_for_service<'a>(
        secrets: &'a HashMap<String, String>,
        scopes: &HashMap<String, Vec<String>>,
        service: &str,
    ) -> Vec<(&'a String, &'a String)> {
        secrets
            .iter()
            .filter(|(key, _)| match scopes.get(*key) {
                None => true,
                Some(services) if services.is_empty() => true,
                Some(services) => services.iter().any(|s| s == service),
            })
            .collect()
    }

    /// Secret names a given service is entitled to read. Public so the deploy
    /// job can report the delivery matrix using exactly the same rule the
    /// executor applies when writing the files -- a second, drifting copy of
    /// this predicate would make the log lie.
    pub fn secret_names_for_service(
        secrets: &HashMap<String, String>,
        scopes: &HashMap<String, Vec<String>>,
        service: &str,
    ) -> Vec<String> {
        Self::secrets_for_service(secrets, scopes, service)
            .into_iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Scoped service names that do not exist in the deployed stack.
    ///
    /// A Compose service name lives in the user's repository, so a rename
    /// silently strands any secret scoped to the old name. Callers surface
    /// this in the deployment log rather than letting delivery quietly stop.
    pub fn unmatched_secret_scopes(
        scopes: &HashMap<String, Vec<String>>,
        known_services: &[String],
    ) -> Vec<(String, String)> {
        let mut unmatched: Vec<(String, String)> = scopes
            .iter()
            .flat_map(|(key, services)| {
                services
                    .iter()
                    .filter(|service| !known_services.contains(service))
                    .map(move |service| (key.clone(), service.clone()))
            })
            .collect();
        unmatched.sort();
        unmatched
    }

    /// Materialize `secrets` as one directory per Compose service and return
    /// the per-service host directories to bind-mount.
    ///
    /// Per-service directories rather than one shared directory are what make
    /// scoping real: a service's mount can only expose files that were written
    /// into its own directory, so a service outside a secret's scope has no
    /// path to the value at all. Duplicating a value across the services
    /// entitled to it costs at most `SECRET_VALUE_MAX_BYTES` per copy.
    ///
    /// Only the candidate generation is removed and recreated. The generation
    /// mounted by the active stack remains untouched until replacement
    /// containers are healthy, so a slow or failed pull cannot rotate secrets
    /// underneath live containers.
    ///
    /// ### Permissions
    /// `0700` on the root, `0755` on each service directory, `0444` on each
    /// file. The single-container path chowns `0400` files to the uid resolved
    /// from the image's `USER`; that is not available here, because Compose
    /// pulls several of a stack's images during `up` -- after these files have
    /// to exist. World-readable *inside* the container is not a boundary worth
    /// defending (any process there already runs as the app), and on the host
    /// the `0700` root denies every other local user.
    async fn materialize_secrets(
        &self,
        project_name: &str,
        generation: &str,
        secrets: &HashMap<String, String>,
        scopes: &HashMap<String, Vec<String>>,
        services: &[String],
    ) -> Result<HashMap<String, PathBuf>, ComposeError> {
        Self::validate_service_dir_name(generation)?;
        let root_for_project = self.secret_generation_dir(project_name, generation);

        // Clear unconditionally, even with no secrets: a project whose last
        // secret was just deleted must not keep serving the old file.
        if let Err(error) = tokio::fs::remove_dir_all(&root_for_project).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(ComposeError::FileWriteFailed {
                    path: root_for_project.display().to_string(),
                    reason: format!("failed to clear previous secrets directory: {error}"),
                });
            }
        }

        if secrets.is_empty() {
            return Ok(HashMap::new());
        }

        // Docker splits a short-form bind spec on ':', so a data directory
        // containing one would silently mount the wrong host path (or nothing)
        // rather than fail. Refuse instead of delivering no secrets quietly.
        if root_for_project.to_string_lossy().contains(':') {
            return Err(ComposeError::FileWriteFailed {
                path: root_for_project.display().to_string(),
                reason: "the Temps data directory path contains ':', which Docker treats as a \
                         bind-mount field separator; secrets cannot be mounted from it. \
                         Move TEMPS_DATA_DIR to a path without a colon."
                    .to_string(),
            });
        }

        let root = self.secrets_root();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| ComposeError::FileWriteFailed {
                path: root.display().to_string(),
                reason: format!("failed to create secrets root: {e}"),
            })?;
        Self::set_mode(&root, 0o700).await?;

        let mut mounts = HashMap::new();
        for service in services {
            let entitled = Self::secrets_for_service(secrets, scopes, service);
            if entitled.is_empty() {
                continue;
            }

            // The service name becomes a directory component. Compose service
            // names come from a repository file, so this is validated here and
            // not merely trusted from the API layer.
            Self::validate_service_dir_name(service)?;
            let dir = root_for_project.join(service);
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: dir.display().to_string(),
                    reason: format!("failed to create secrets directory: {e}"),
                })?;
            Self::set_mode(&dir, 0o755).await?;

            for (key, value) in entitled {
                // `SecretService::validate_secret_key` already enforces this at
                // the API. Re-checked because this value becomes a path.
                Self::validate_secret_file_name(key)?;
                let path = dir.join(key);
                tokio::fs::write(&path, value).await.map_err(|e| {
                    ComposeError::FileWriteFailed {
                        path: path.display().to_string(),
                        // Never interpolate `value` -- this reaches the log.
                        reason: format!("failed to write secret '{key}': {e}"),
                    }
                })?;
                Self::set_mode(&path, 0o444).await?;
            }

            mounts.insert(service.clone(), dir);
        }

        // `0700` on the per-project root too, so an unscoped service cannot
        // even enumerate the sibling directories it has no mount for.
        Self::set_mode(&root_for_project, 0o700).await?;

        Ok(mounts)
    }

    /// Reject a Compose service name that cannot be used as a single path
    /// component.
    fn validate_service_dir_name(service: &str) -> Result<(), ComposeError> {
        let invalid = service.is_empty()
            || service == "."
            || service == ".."
            || service.contains('/')
            || service.contains('\\')
            || service.contains('\0');
        if invalid {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: "service name".to_string(),
                reason: "compose service name cannot be used as a directory name; \
                         it must not be empty, '.', '..', or contain path separators"
                    .to_string(),
            });
        }
        Ok(())
    }

    #[cfg_attr(not(unix), allow(unused_variables))]
    async fn set_mode(path: &Path, mode: u32) -> Result<(), ComposeError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: path.display().to_string(),
                    reason: format!("failed to set mode {mode:o}: {e}"),
                })?;
        }
        Ok(())
    }

    /// Every override filename Temps writes into the stack directory itself.
    /// Reject a `compose_path` that names one of Temps' own generated
    /// overrides. Those files are written unconditionally, so pointing the
    /// project at one would have Temps overwrite the user's compose document
    /// with generated content and then pass the same file to `-f` twice.
    fn validate_compose_path_not_generated(compose_path: &str) -> Result<(), ComposeError> {
        let name = Path::new(compose_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(compose_path);
        if RESERVED_GENERATED_COMPOSE_FILES.contains(&name) {
            return Err(ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_path.to_string(),
                reason: format!(
                    "'{name}' is reserved for a Temps-generated override; \
                     rename the compose file in your repository"
                ),
            });
        }
        Ok(())
    }

    /// Reject a secret key that cannot be used as a single filename.
    ///
    /// Mirrors `SecretService::validate_secret_key` rather than merely
    /// excluding path separators: this runs on a value read back from the
    /// database, so it must not assume the API layer was the only writer.
    /// Anything outside `[A-Za-z0-9_]` (leading digit included) is rejected,
    /// which subsumes `.`, `..`, and every path separator.
    fn validate_secret_file_name(key: &str) -> Result<(), ComposeError> {
        let valid = !key.is_empty()
            && key.len() <= 255
            && key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<secrets>".to_string(),
                field: "secret key".to_string(),
                reason: format!(
                    "secret key '{key}' is not a valid filename; keys must start with a \
                     letter or underscore and contain only letters, digits and underscores"
                ),
            });
        }
        Ok(())
    }

    /// Services that already put something at `/run/secrets` themselves —
    /// either an explicit volume mount or Compose's own `secrets:` key, which
    /// mounts each entry at `/run/secrets/<name>`.
    ///
    /// Temps skips injection for these rather than mounting on top: Compose
    /// *appends* volume lists across `-f` files instead of replacing them, so
    /// two mounts at the same target abort `up` for the whole stack. Losing
    /// secret delivery for one service is recoverable and reported; failing
    /// the deployment of an otherwise-valid stack is not.
    pub fn services_managing_own_secrets(compose_documents: &[&str]) -> HashSet<String> {
        let mut conflicting = HashSet::new();
        for document in compose_documents {
            if document.trim().is_empty() {
                continue;
            }
            let Ok(mut root) = serde_yaml::from_str::<YamlValue>(document) else {
                continue;
            };
            let _ = root.apply_merge();
            let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, service) in services {
                let Some(name) = name.as_str() else { continue };
                if service.get("secrets").is_some() {
                    conflicting.insert(name.to_string());
                    continue;
                }
                let Some(volumes) = service.get("volumes").and_then(YamlValue::as_sequence) else {
                    continue;
                };
                if volumes.iter().any(Self::mounts_container_secrets_dir) {
                    conflicting.insert(name.to_string());
                }
            }
        }
        conflicting
    }

    /// Whether a single `volumes:` entry targets `/run/secrets` (or a path
    /// inside it), in either the short `src:dst:opts` form or the long
    /// `{target: ...}` form.
    fn mounts_container_secrets_dir(entry: &YamlValue) -> bool {
        let target = match entry {
            // Short form. The target is the second colon-separated field;
            // a single-field entry (`- /run/secrets`) is an anonymous volume
            // whose target is the whole string.
            YamlValue::String(spec) => {
                let mut parts = spec.split(':');
                let first = parts.next().unwrap_or_default();
                parts.next().unwrap_or(first).to_string()
            }
            YamlValue::Mapping(_) => match entry.get("target").and_then(YamlValue::as_str) {
                Some(target) => target.to_string(),
                None => return false,
            },
            _ => return false,
        };
        let target = target.trim_end_matches('/');
        target == CONTAINER_SECRETS_DIR || target.starts_with(&format!("{CONTAINER_SECRETS_DIR}/"))
    }

    /// Every service name across the base compose document and the user
    /// override, de-duplicated in first-seen order.
    ///
    /// The override is a full compose document, so it can introduce services
    /// the base file never mentions. Enumerating only the base file would
    /// leave those with no secrets while the deployment log claimed every
    /// service got them.
    pub fn all_service_names(&self, compose_documents: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for document in compose_documents {
            if document.trim().is_empty() {
                continue;
            }
            for name in self.parse_service_names_yaml(document) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// Generate the override that mounts each service's own secrets directory
    /// read-only at `/run/secrets`. Returns the YAML and the services covered.
    ///
    /// Every service gets a *different* mount source, so a service outside a
    /// secret's scope has no filesystem path to that value at all -- the
    /// scoping is enforced by what was written, not by the container.
    fn generate_secrets_override(
        &self,
        mounts: &HashMap<String, PathBuf>,
        skip_services: &HashSet<String>,
    ) -> (String, Vec<String>) {
        let mut mounted: Vec<String> = mounts
            .keys()
            .filter(|service| !skip_services.contains(*service))
            .cloned()
            .collect();
        // Deterministic output so a redeploy with unchanged inputs produces a
        // byte-identical override.
        mounted.sort();

        let mut services_map = Mapping::new();
        for service in &mounted {
            let Some(host_dir) = mounts.get(service) else {
                continue;
            };
            let mount = format!(
                "{}:{}:ro",
                host_dir.to_string_lossy(),
                CONTAINER_SECRETS_DIR
            );
            let mut service_map = Mapping::new();
            service_map.insert(
                Value::String("volumes".to_string()),
                Value::Sequence(vec![Value::String(mount)]),
            );
            services_map.insert(Value::String(service.clone()), Value::Mapping(service_map));
        }

        if services_map.is_empty() {
            return (String::new(), mounted);
        }

        let mut root = Mapping::new();
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(services_map),
        );
        // Built through serde_yaml rather than string formatting so a service
        // name or host path containing YAML metacharacters is quoted, not
        // injected as structure.
        (
            serde_yaml::to_string(&Value::Mapping(root)).unwrap_or_default(),
            mounted,
        )
    }

    /// Remove a stack's materialized secret files from the host.
    async fn remove_secrets_dir(&self, project_name: &str) {
        let dir = self.secrets_dir(project_name);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    project = %project_name,
                    path = %dir.display(),
                    error = %error,
                    "Failed to remove materialized secrets directory"
                );
            }
        }
    }

    /// Remove only one not-yet-promoted secret generation. This is used when
    /// preparation fails or is cancelled while the previous stack is still
    /// serving; deleting the whole project directory would invalidate the
    /// bind mounts of that live stack.
    pub async fn cleanup_secret_generation(
        &self,
        project_name: &str,
        generation: &str,
    ) -> Result<(), ComposeError> {
        Self::validate_service_dir_name(generation)?;
        let dir = self.secret_generation_dir(project_name, generation);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ComposeError::FileWriteFailed {
                path: dir.display().to_string(),
                reason: format!("failed to remove candidate secret generation: {error}"),
            }),
        }
    }

    /// Keep one candidate's secret generation and remove every older one.
    ///
    /// Callers may use this either after the replacement containers become
    /// healthy or after the previous stack has been stopped. In both cases no
    /// live container may still depend on the generations being removed.
    pub async fn prune_secret_generations(
        &self,
        project_name: &str,
        generation: Option<&str>,
    ) -> Result<(), ComposeError> {
        if let Some(generation) = generation {
            Self::validate_service_dir_name(generation)?;
        }
        let project_dir = self.secrets_dir(project_name);
        let mut entries = match tokio::fs::read_dir(&project_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(ComposeError::FileWriteFailed {
                    path: project_dir.display().to_string(),
                    reason: format!("failed to list secret generations: {error}"),
                });
            }
        };
        while let Some(entry) =
            entries
                .next_entry()
                .await
                .map_err(|error| ComposeError::FileWriteFailed {
                    path: project_dir.display().to_string(),
                    reason: format!("failed to inspect secret generations: {error}"),
                })?
        {
            if generation.is_some_and(|current| entry.file_name() == current) {
                continue;
            }
            let path = entry.path();
            tokio::fs::remove_dir_all(&path).await.map_err(|error| {
                ComposeError::FileWriteFailed {
                    path: path.display().to_string(),
                    reason: format!("failed to remove obsolete secret generation: {error}"),
                }
            })?;
        }
        if generation.is_none() {
            match tokio::fs::remove_dir(&project_dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ComposeError::FileWriteFailed {
                        path: project_dir.display().to_string(),
                        reason: format!("failed to remove empty secret project directory: {error}"),
                    });
                }
            }
        }
        Ok(())
    }

    /// Prepare compose files on disk, build images (if required), and pull
    /// images for `image:`-based services.
    ///
    /// This is the first half of a two-phase deploy. It MUST be called while
    /// the previously-running stack is **still alive** so that image-fetch
    /// latency falls outside the downtime window. If this step fails, the
    /// caller MUST NOT call [`Self::teardown_at`] — the old stack is still
    /// serving traffic and should keep doing so.
    ///
    /// On success, returns a [`PreparedComposeDeploy`] that must be passed to
    /// [`Self::deploy_prepared`] after [`Self::teardown_at`].
    pub async fn prepare_and_pull(
        &self,
        request: &ComposeDeployRequest,
    ) -> Result<PreparedComposeDeploy, ComposeError> {
        let generation = uuid::Uuid::new_v4().simple().to_string();
        self.prepare_and_pull_for_generation(request, &generation)
            .await
    }

    /// Variant used by workflow jobs that need deterministic cancellation
    /// cleanup. The caller supplies an internal, path-safe generation name so
    /// it can remove the candidate without touching the active generation.
    pub async fn prepare_and_pull_for_generation(
        &self,
        request: &ComposeDeployRequest,
        generation: &str,
    ) -> Result<PreparedComposeDeploy, ComposeError> {
        let result = self
            .prepare_and_pull_generation_inner(request, generation)
            .await;
        if result.is_err() {
            if let Err(cleanup_error) = self
                .cleanup_secret_generation(&request.project_name, generation)
                .await
            {
                warn!(
                    project = %request.project_name,
                    generation,
                    error = %cleanup_error,
                    "Failed to clean candidate secrets after Compose preparation failure"
                );
            }
        }
        result
    }

    async fn prepare_and_pull_generation_inner(
        &self,
        request: &ComposeDeployRequest,
        generation: &str,
    ) -> Result<PreparedComposeDeploy, ComposeError> {
        Self::validate_service_dir_name(generation)?;
        let project_dir = self.project_dir(&request.project_name);
        let project_name = request.project_name.clone();
        Self::validate_relative_path(
            request
                .compose_path
                .as_deref()
                .unwrap_or("docker-compose.yml"),
            "compose_path",
        )?;
        Self::validate_compose_path_not_generated(
            request
                .compose_path
                .as_deref()
                .unwrap_or("docker-compose.yml"),
        )?;
        Self::validate_security_exemptions(
            &request.relaxed_capability_services,
            &request.unsandboxed_services,
        )?;
        self.validate_compose_security_policy("compose file", &request.compose_content)?;
        if let Some(ref compose_override) = request.compose_override {
            self.validate_compose_security_policy("compose override", compose_override)?;
            Self::validate_compose_override(
                &request.project_name,
                &request.compose_content,
                compose_override,
            )?;
        }

        // Build/image selection is repository-owned. Inline overrides cannot
        // alter either field, preventing a merge from assigning a daemon-global
        // image tag to a build or changing the trusted pull/build decision.
        let has_build = self.has_build_directives(&request.compose_content);

        // Every value that must never appear in a deployment error, including
        // the secrets this deploy is about to mount: a container that echoes
        // its own secret while crash-looping would otherwise have it captured
        // into the failure diagnostic and persisted on the deployment.
        let redact_values = collect_redactable_values(request);

        // Always use the repo checkout directory when available.
        // Compose files often reference local paths (bind mounts, configs,
        // build contexts) that only exist in the repo, not in the temps data dir.
        let effective_dir = request
            .repo_dir
            .clone()
            .unwrap_or_else(|| project_dir.clone());

        // 1. Write compose files + env overrides to disk
        self.write_compose_files(&effective_dir, request, generation)
            .await?;

        let compose_file = request
            .compose_path
            .as_deref()
            .unwrap_or("docker-compose.yml")
            .to_string();

        // Lexical path validation cannot see repository symlinks. Resolve every
        // host path from the same base directory Docker Compose uses, after the
        // checkout and compose files exist but before build/up can touch the
        // host. This closes `./data -> /` style escapes for bind mounts,
        // configs/secrets, local-driver binds, and build paths.
        Self::validate_compose_filesystem_confinement(
            &effective_dir,
            &compose_file,
            "compose file",
            &request.compose_content,
        )?;
        if let Some(ref compose_override) = request.compose_override {
            Self::validate_compose_filesystem_confinement(
                &effective_dir,
                &compose_file,
                "compose override",
                compose_override,
            )?;
        }

        // 2. Build images if compose file has build: directives
        if has_build {
            self.compose_build(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
                &request.build_args,
            )
            .await?;
        }

        // 3. Pull images for plain `image:` services BEFORE tearing down old
        // containers. Image-fetch latency (seconds to minutes on slow registries
        // or fat images) would otherwise occur inside the downtime window between
        // old-container-stop and new-container-healthy. By pulling here, while
        // the old stack is still serving traffic, we minimise the actual gap.
        // `--ignore-buildable` skips services that only have a `build:` directive
        // (those were already handled above by `compose_build`), so this call is
        // safe regardless of whether the project mixes built and pulled services.
        self.compose_pull(&effective_dir, &project_name, &compose_file, &redact_values)
            .await?;

        // Docker's injected init process is useful for ordinary application
        // images, but it must not sit in front of an init system already owned
        // by the image. Some init systems (notably s6-overlay) require PID 1;
        // others (Tini/dumb-init) lose their reaping role when wrapped. Inspect
        // the images we just built/pulled instead of coupling this behavior to
        // registry names or catalog templates, then rewrite only the generated
        // security override. Explicit `init: false` remains supported as a
        // user-controlled fallback when an image hides its init behind a shell
        // entrypoint that metadata cannot identify safely.
        let image_owned_init_services = self
            .detect_image_owned_init_services(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
            )
            .await?;
        self.write_security_override(&effective_dir, request, &image_owned_init_services)
            .await?;

        Ok(PreparedComposeDeploy {
            effective_dir,
            project_name,
            compose_file,
            redact_values,
            secret_generation: generation.to_string(),
            has_materialized_secrets: !request.secrets.is_empty(),
        })
    }

    /// Complete a deployment after the old stack has been torn down.
    ///
    /// This is the second half of a two-phase deploy. It expects a
    /// [`PreparedComposeDeploy`] produced by a prior call to
    /// [`Self::prepare_and_pull`] (images are already local at this point),
    /// creates the shared network if needed, starts the new containers, waits
    /// for readiness, discovers containers, and applies Temps labels.
    pub async fn deploy_prepared(
        &self,
        prepared: PreparedComposeDeploy,
        request: &ComposeDeployRequest,
    ) -> Result<Vec<ComposeServiceResult>, Box<ComposeDeployFailure>> {
        let PreparedComposeDeploy {
            effective_dir,
            project_name,
            compose_file,
            redact_values,
            secret_generation,
            has_materialized_secrets,
        } = prepared;

        // Ensure the shared Temps network exists before `up` attaches every
        // service to it (docker-compose.temps-network.yml, written above) —
        // this is the same network Temps-managed external services and
        // single-container app deployments join, so a compose service can
        // reach a Temps-managed database by name.
        self.ensure_temps_network_exists()
            .await
            .map_err(|error| Box::new(ComposeDeployFailure::without_containers(error)))?;

        // 4. Run docker compose up. Images are already pulled (step 3) or built
        // (step 2), so `--pull never` would also work, but omitting `--pull`
        // entirely lets Compose fall back to its default (only pull if absent
        // locally), which is safe and avoids a redundant network round-trip.
        // If a user-provided `container_name` conflicts with an existing
        // container, let Compose report the conflict instead of deleting
        // containers outside this Temps project boundary.
        if let Err(error) = self
            .compose_up(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
                &request.relaxed_capability_services,
            )
            .await
        {
            return Err(Box::new(
                self.failure_with_discovered_containers(
                    &effective_dir,
                    &project_name,
                    &compose_file,
                    request,
                    error,
                )
                .await,
            ));
        }

        // 4b. `up -d` returns as soon as containers are created/started, not
        // once they're actually ready. Wait for every service to reach
        // `running` (and `healthy`, for services that define a healthcheck)
        // so a crash-looping or slow-starting service surfaces as a failed
        // deployment instead of a false "success".
        if let Err(error) = self
            .wait_for_services_ready(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
                &request.relaxed_capability_services,
                COMPOSE_READY_TIMEOUT,
            )
            .await
        {
            return Err(Box::new(
                self.failure_with_discovered_containers(
                    &effective_dir,
                    &project_name,
                    &compose_file,
                    request,
                    error,
                )
                .await,
            ));
        }

        // 5. Discover running containers
        let containers = self
            .discover_containers(&effective_dir, &project_name, &compose_file)
            .await
            .map_err(|error| Box::new(ComposeDeployFailure::without_containers(error)))?;

        // 5b. Verify the ownership labels written by the generated override.
        // Never register containers cleanup cannot prove belong to this stack.
        for container in &containers {
            self.verify_labels(
                &container.container_id,
                &request.labels,
                &container.service_name,
            )
            .await
            .map_err(|error| Box::new(ComposeDeployFailure::without_containers(error)))?;
        }

        if let Err(error) = self
            .prune_secret_generations(
                &project_name,
                has_materialized_secrets.then_some(secret_generation.as_str()),
            )
            .await
        {
            // The candidate is already mounted by healthy replacement
            // containers. Turning an obsolete-generation janitor failure into
            // a deploy error would make the caller tear those containers down
            // after the old stack is gone. Keep serving and retry removal on a
            // later successful deployment.
            warn!(
                project = %project_name,
                generation = %secret_generation,
                error = %error,
                "Compose deployed successfully, but obsolete secret generation cleanup was deferred"
            );
        }

        info!(
            project = %project_name,
            services = containers.len(),
            "Compose stack deployed"
        );

        Ok(containers)
    }

    /// Deploy a compose stack: write files, pull images, start containers,
    /// wait for every service to become ready, then discover and label them.
    /// Returns one result per service. Fails (rather than reporting a false
    /// success) if a service never reaches `running`/`healthy` within
    /// [`COMPOSE_READY_TIMEOUT`].
    ///
    /// This is a thin wrapper over [`Self::prepare_and_pull`] followed by
    /// [`Self::deploy_prepared`]. Callers that need to tear down an existing
    /// stack between the two phases (the common redeploy path) should call
    /// `prepare_and_pull` and `deploy_prepared` directly so that image-fetch
    /// latency falls outside the downtime window.
    pub async fn deploy(
        &self,
        request: ComposeDeployRequest,
    ) -> Result<Vec<ComposeServiceResult>, ComposeError> {
        let prepared = self.prepare_and_pull(&request).await?;
        self.deploy_prepared(prepared, &request)
            .await
            .map_err(|failure| failure.error)
    }

    /// Tear down containers before a redeploy. Preserves volumes (database data,
    /// uploads, etc.) so they survive between deployments.
    ///
    /// Passes `remove_secrets: false` because an upcoming `deploy_prepared()`
    /// call will consume the secrets that `prepare_and_pull()` just materialized.
    /// Deleting them here would cause the new containers to start with missing
    /// or empty secret files.
    pub async fn teardown_for_redeploy(&self, project_name: &str) -> Result<(), ComposeError> {
        self.teardown_at(project_name, None, None, &HashMap::new(), false)
            .await
    }

    /// Tear down a Compose stack from the exact directory used for `up`.
    /// Uploaded/Git deployments run Compose inside their checkout rather than
    /// the data-dir fallback, so compensation must retain that location.
    ///
    /// `remove_secrets` controls whether the on-disk directory of materialized
    /// secret files (`data_dir/compose-secrets/<project_name>/`) is deleted.
    ///
    /// **HAZARD**: In a `prepare_and_pull()` → `teardown_at()` → `deploy_prepared()`
    /// sequence the secrets have just been materialized and `deploy_prepared()`
    /// is about to bind-mount them into new containers. Passing `remove_secrets:
    /// true` in that sequence deletes the files that the new containers need,
    /// causing them to start with missing or empty secrets — a silent credential
    /// loss that only surfaces at container runtime. Always pass `false` when
    /// an immediate `deploy_prepared()` follows.
    ///
    /// Pass `true` only when the deploy attempt is definitively over: compensating
    /// cleanup on failure, zero-services edge-case teardown, cancellation cleanup,
    /// or final destruction of a project/environment.
    pub async fn teardown_at(
        &self,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
        _environment_vars: &HashMap<String, String>,
        remove_secrets: bool,
    ) -> Result<(), ComposeError> {
        if let Some(compose_path) = compose_path {
            Self::validate_relative_path(compose_path, "compose_path")?;
        }
        // Only delete the plaintext secret files when this teardown is the last
        // step — i.e. the deploy attempt is over and nothing downstream will
        // consume the files that were just materialized. When `remove_secrets`
        // is false, the caller is about to call `deploy_prepared()` and those
        // files must remain on disk so the new containers can bind-mount them.
        // See the hazard note on `teardown_at`'s doc comment.
        if remove_secrets {
            self.remove_secrets_dir(project_name).await;
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
            TEMPS_SECRETS_OVERRIDE,
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
        let output = Self::bounded_command_output(
            command,
            std::time::Duration::from_secs(35),
            project_name,
            "docker compose down",
        )
        .await?;

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

        // Secrets live outside `project_dir` (see `secrets_root`), so removing
        // the work directory below does not cover them.
        self.remove_secrets_dir(project_name).await;

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
            let mut command = isolated_docker_command();
            command
                .args(["compose", "-p", project_name])
                .args(["-f", &compose_file])
                .args(["down", "--remove-orphans", "--volumes", "--timeout", "30"])
                .current_dir(&project_dir);
            let output = Self::bounded_command_output(
                command,
                std::time::Duration::from_secs(35),
                project_name,
                "docker compose destroy",
            )
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
        self.stop_at(project_name, None, None).await
    }

    pub async fn stop_at(
        &self,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
    ) -> Result<(), ComposeError> {
        let project_dir = repo_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.project_dir(project_name));
        let compose_file = compose_path
            .map(ToString::to_string)
            .unwrap_or_else(|| self.find_compose_file(&project_dir));
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
            TEMPS_SECRETS_OVERRIDE,
        ] {
            if project_dir.join(generated).exists() {
                command.args(["-f", generated]);
            }
        }
        Self::append_compose_env_file_args(&mut command, &project_dir);
        command
            .args(["stop", "--timeout", "30"])
            .current_dir(&project_dir);
        let output = Self::bounded_command_output(
            command,
            std::time::Duration::from_secs(35),
            project_name,
            "docker compose stop",
        )
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
        secret_generation: &str,
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
        // Same reasoning as the `env_file` guard below: these names are handed
        // to `docker compose -f` on sight, so nothing user-selected may land on
        // them. Here it would also mean the base compose file and a generated
        // override silently clobber each other depending on write order.
        if is_reserved_generated_compose_file(Path::new(compose_file)) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<compose-files>".to_string(),
                field: "compose_path".to_string(),
                reason: format!(
                    "'{compose_file}' is a Compose file name Temps generates; \
                     choose a different compose file path."
                ),
            });
        }
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
            // Fail closed rather than skipping: a stack that only works because
            // Temps quietly declined to satisfy one of its `env_file:` entries
            // is worse to debug than an explicit "rename this file" error.
            if is_reserved_generated_compose_file(Path::new(&plan.path)) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<compose-files>".to_string(),
                    field: "env_file".to_string(),
                    reason: format!(
                        "'{}' is a Compose file name Temps generates and passes to the Docker \
                         daemon; an env_file may not target it. Rename the env file in your \
                         compose configuration.",
                        plan.path
                    ),
                });
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

        // Write Temps' runtime policy/compatibility override. Sandboxing applies
        // only to opted-in services; safe health/PID-1 corrections also apply
        // to unsandboxed services without adding any isolation controls.
        self.write_security_override(project_dir, request, &HashSet::new())
            .await?;

        // Materialize project secrets and mount them at /run/secrets in every
        // service. Values never enter the compose documents, the env files or
        // the build args — only the host path of the directory does.
        let documents = [
            request.compose_content.as_str(),
            request.compose_override.as_deref().unwrap_or_default(),
        ];
        let services = self.all_service_names(&documents);
        let secret_mounts = self
            .materialize_secrets(
                &request.project_name,
                secret_generation,
                &request.secrets,
                &request.secret_compose_services,
                &services,
            )
            .await?;
        let secrets_override_path = Self::confined_write_path(
            project_dir,
            Path::new(TEMPS_SECRETS_OVERRIDE),
            TEMPS_SECRETS_OVERRIDE,
        )?;
        let secrets_content = if secret_mounts.is_empty() {
            String::new()
        } else {
            let skip = Self::services_managing_own_secrets(&documents);
            for service in &skip {
                warn!(
                    project = %request.project_name,
                    service = %service,
                    "Service already mounts {CONTAINER_SECRETS_DIR}; \
                     skipping Temps secret injection for it"
                );
            }
            let (content, mounted) = self.generate_secrets_override(&secret_mounts, &skip);
            debug!(
                project = %request.project_name,
                services = %mounted.join(", "),
                "Mounted project secrets into compose services"
            );
            content
        };
        if secrets_content.is_empty() {
            // A stale override from a previous deploy would keep mounting a
            // directory this deploy just emptied.
            if let Err(error) = tokio::fs::remove_file(&secrets_override_path).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(ComposeError::FileWriteFailed {
                        path: secrets_override_path.display().to_string(),
                        reason: format!("failed to remove stale secrets override: {error}"),
                    });
                }
            }
        } else {
            tokio::fs::write(&secrets_override_path, &secrets_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: secrets_override_path.display().to_string(),
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

    async fn write_security_override(
        &self,
        project_dir: &Path,
        request: &ComposeDeployRequest,
        detected_image_owned_init_services: &HashSet<String>,
    ) -> Result<(), ComposeError> {
        let healthcheck_loopback_overrides = Self::healthcheck_loopback_overrides(
            &request.compose_content,
            request.compose_override.as_deref(),
        );
        let security_content = self.generate_security_override_with_image_init(
            &request.compose_content,
            &request.relaxed_capability_services,
            &request.unsandboxed_services,
            detected_image_owned_init_services,
            &healthcheck_loopback_overrides,
        );
        let security_override_path = Self::confined_write_path(
            project_dir,
            Path::new("docker-compose.temps-security.yml"),
            "docker-compose.temps-security.yml",
        )?;
        if !security_content.is_empty() {
            tokio::fs::write(&security_override_path, &security_content)
                .await
                .map_err(|error| ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: error.to_string(),
                })?;
        } else if let Err(error) = tokio::fs::remove_file(&security_override_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: format!("failed to remove stale security override: {error}"),
                });
            }
        }
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
                if (Self::is_named_volume_ref(&source) && !Self::is_explicit_bind_mount(entry))
                    || Self::is_dangerous_host_path(&source)
                {
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
            Self::validate_compose_override(
                "compose-preflight",
                compose_content,
                override_content,
            )?;
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
            (TEMPS_SECRETS_OVERRIDE, TEMPS_SECRETS_OVERRIDE),
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
        let forbidden_named_volumes = self.validate_top_level_volumes(&root)?;

        // Block host files exposed through top-level configs/secrets `file:` paths.
        self.validate_top_level_files(&root, "configs")?;
        self.validate_top_level_files(&root, "secrets")?;
        self.validate_top_level_networks(&root)?;

        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Ok(());
        };
        if services.len() > MAX_RESOLVED_COMPOSE_SERVICES {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: "services".to_string(),
                reason: format!(
                    "Compose deployments may declare at most {} services; found {}",
                    MAX_RESOLVED_COMPOSE_SERVICES,
                    services.len()
                ),
            });
        }

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
                "external_links",
                "external_links can connect to arbitrary host containers outside this deployment",
            )?;
            self.reject_present(
                service,
                service_name,
                "label_file",
                "label_file can read arbitrary files from the deployment host",
            )?;
            for (field, reason) in [
                (
                    "post_start",
                    "post_start hooks can run privileged commands after container startup",
                ),
                (
                    "pre_stop",
                    "pre_stop hooks can run privileged commands during container shutdown",
                ),
                (
                    "provider",
                    "Compose providers invoke daemon-host plugins and are not allowed",
                ),
                (
                    "container_name",
                    "container_name is daemon-global and can collide with another deployment",
                ),
                (
                    "blkio_config",
                    "blkio_config can alter host device I/O scheduling",
                ),
                (
                    "storage_opt",
                    "storage_opt can allocate unbounded daemon-host storage",
                ),
                (
                    "memswap_limit",
                    "custom swap limits can bypass the platform memory-pressure policy",
                ),
            ] {
                self.reject_present(service, service_name, field, reason)?;
            }
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
            self.validate_network_mode(service, service_name, services)?;
            self.reject_host_namespace(service, service_name, "pid")?;
            self.reject_host_namespace(service, service_name, "ipc")?;
            self.reject_host_namespace(service, service_name, "cgroup")?;
            self.reject_host_namespace(service, service_name, "uts")?;
            self.reject_host_namespace(service, service_name, "userns_mode")?;
            self.reject_deploy_devices(service, service_name)?;
            self.validate_replica_count(service, service_name)?;
            self.validate_service_image(service, service_name)?;
            self.validate_build_options(service, service_name)?;
            self.validate_env_files(service, service_name)?;
            self.validate_published_ports(service, service_name)?;
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
            let Some(name) = network_name.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: "networks".to_string(),
                    reason: "network names must be strings".to_string(),
                });
            };
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

            if options.contains_key(YamlValue::String("name".to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: format!("networks.{name}.name"),
                    reason: "custom Docker network names can attach a service to a daemon-global network outside this deployment"
                        .to_string(),
                });
            }
            if let Some(external) = options.get(YamlValue::String("external".to_string())) {
                if !external.is_null() && external.as_bool() != Some(false) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.external"),
                        reason: "external Compose networks bypass Temps-managed network policy"
                            .to_string(),
                    });
                }
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
            for field in ["driver_opts", "ipam"] {
                if options.contains_key(YamlValue::String(field.to_string())) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.{field}"),
                        reason: format!(
                            "custom network {field} can alter daemon-host routing and is not allowed"
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    /// Validate top-level named volumes and collect definitions whose driver
    /// options would escape the project. Docker volume names are daemon-global,
    /// so `external` and custom `name` values can otherwise mount another
    /// project's database volume into this deployment.
    fn validate_top_level_volumes(
        &self,
        root: &YamlValue,
    ) -> Result<HashSet<String>, ComposeError> {
        let mut forbidden = HashSet::new();
        let Some(volumes_value) = root.get("volumes") else {
            return Ok(forbidden);
        };
        let Some(volumes) = volumes_value.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: "volumes".to_string(),
                reason: "top-level volumes must be a mapping".to_string(),
            });
        };
        for (name, def) in volumes {
            let Some(name) = name.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: "volumes".to_string(),
                    reason: "volume names must be strings".to_string(),
                });
            };
            if def.is_null() {
                continue;
            }
            let Some(def_map) = def.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("volumes.{name}"),
                    field: format!("volumes.{name}"),
                    reason: "volume configuration must be a mapping".to_string(),
                });
            };

            if def_map.contains_key(YamlValue::String("name".to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("volumes.{name}"),
                    field: format!("volumes.{name}.name"),
                    reason: "custom Docker volume names are daemon-global and can reference another deployment's data"
                        .to_string(),
                });
            }
            if let Some(external) = def_map.get(YamlValue::String("external".to_string())) {
                if !external.is_null() && external.as_bool() != Some(false) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("volumes.{name}"),
                        field: format!("volumes.{name}.external"),
                        reason:
                            "external Docker volumes can expose data owned by another deployment"
                                .to_string(),
                    });
                }
            }

            // A non-`local` volume driver invokes an external volume plugin
            // (NFS/CIFS clients, cloud plugins) that can mount attacker-controlled
            // remote or host filesystems into the container.
            if let Some(driver) = def_map.get(YamlValue::String("driver".to_string())) {
                let Some(driver) = driver.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("volumes.{name}"),
                        field: format!("volumes.{name}.driver"),
                        reason: "volume driver must be the literal value 'local'".to_string(),
                    });
                };
                if driver != "local" {
                    forbidden.insert(name.to_string());
                    continue;
                }
            }

            let Some(driver_opts_value) = def_map.get(YamlValue::String("driver_opts".to_string()))
            else {
                continue;
            };
            let _ = driver_opts_value;
            return Err(ComposeError::SecurityPolicyViolation {
                service: format!("volumes.{name}"),
                field: format!("volumes.{name}.driver_opts"),
                reason: "volume driver_opts can make the Docker daemon mount host or network filesystems and are not allowed"
                    .to_string(),
            });
        }
        Ok(forbidden)
    }

    /// Reject top-level `configs.*.file` / `secrets.*.file` entries that point at
    /// forbidden or project-escaping host paths (e.g. `/etc/passwd`).
    fn validate_top_level_files(&self, root: &YamlValue, key: &str) -> Result<(), ComposeError> {
        let Some(value) = root.get(key) else {
            return Ok(());
        };
        let Some(map) = value.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: key.to_string(),
                reason: format!("top-level {key} must be a mapping"),
            });
        };
        for (name, def) in map {
            let name = name.as_str().unwrap_or("<unknown>");
            if def.is_null() {
                continue;
            }
            let Some(def_map) = def.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("{key}.{name}"),
                    field: format!("{key}.{name}"),
                    reason: format!("{key} configuration must be a mapping"),
                });
            };
            if def_map.contains_key(YamlValue::String("name".to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("{key}.{name}"),
                    field: format!("{key}.{name}.name"),
                    reason: format!(
                        "custom Docker {key} names can reference daemon-global objects outside this deployment"
                    ),
                });
            }
            if let Some(external) = def_map.get(YamlValue::String("external".to_string())) {
                if !external.is_null() && external.as_bool() != Some(false) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("{key}.{name}"),
                        field: format!("{key}.{name}.external"),
                        reason: format!(
                            "external Docker {key} can reference objects outside this deployment"
                        ),
                    });
                }
            }
            if let Some(file) = def_map.get(YamlValue::String("file".to_string())) {
                let Some(file) = file.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("{key}.{name}"),
                        field: format!("{key}.file"),
                        reason: format!("{key} file must be a confined literal path"),
                    });
                };
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
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build".to_string(),
                reason:
                    "build must be a relative context path or a mapping of validated build options"
                        .to_string(),
            });
        };

        if let Some(privileged) = build_map.get(YamlValue::String("privileged".to_string())) {
            match privileged.as_bool() {
                Some(false) => {}
                Some(true) => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "build.privileged".to_string(),
                        reason: "privileged build steps can escape the build sandbox".to_string(),
                    });
                }
                None => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "build.privileged".to_string(),
                        reason: "build.privileged must be the literal boolean false; strings, interpolation, and other value types are not allowed"
                            .to_string(),
                    });
                }
            }
        }
        if build_map.contains_key(YamlValue::String("entitlements".to_string())) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.entitlements".to_string(),
                reason: "build entitlements (e.g. security.insecure) grant host access".to_string(),
            });
        }
        for field in ["additional_contexts", "cache_from", "cache_to", "tags"] {
            if build_map.contains_key(YamlValue::String(field.to_string())) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: format!("build.{field}"),
                    reason: format!(
                        "build.{field} is not allowed because it can read from or write to daemon-host and external locations"
                    ),
                });
            }
        }
        if let Some(network) = build_map.get(YamlValue::String("network".to_string())) {
            let Some(network) = network.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.network".to_string(),
                    reason: "build.network must be a literal network name; interpolation and other value types are not allowed"
                        .to_string(),
                });
            };
            if Self::contains_interpolation(network) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.network".to_string(),
                    reason: "interpolation in build.network is not allowed because it can resolve to the host network after validation"
                        .to_string(),
                });
            }
            if !matches!(network.trim(), "default" | "none") {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.network".to_string(),
                    reason: "build.network must be 'default' or 'none'; named and host networks can expose other deployments to build steps"
                        .to_string(),
                });
            }
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
            if let Some(raw_value) = build_map.get(YamlValue::String(field.to_string())) {
                let Some(value) = raw_value.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: format!("build.{field}"),
                        reason: format!("build.{field} must be a literal relative path"),
                    });
                };
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

    fn validate_replica_count(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        if let Some(scale) = service.get(YamlValue::String("scale".to_string())) {
            if scale.as_u64() != Some(1) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "scale".to_string(),
                    reason: "Compose services are limited to one replica; scale must be the literal integer 1"
                        .to_string(),
                });
            }
        }

        if let Some(deploy) = service.get(YamlValue::String("deploy".to_string())) {
            let Some(deploy) = deploy.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "deploy".to_string(),
                    reason: "deploy must be a literal mapping".to_string(),
                });
            };
            if let Some(replicas) = deploy.get(YamlValue::String("replicas".to_string())) {
                if replicas.as_u64() != Some(1) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "deploy.replicas".to_string(),
                        reason: "Compose services are limited to one replica; deploy.replicas must be the literal integer 1"
                            .to_string(),
                    });
                }
            }
            if let Some(mode) = deploy.get(YamlValue::String("mode".to_string())) {
                if mode.as_str() != Some("replicated") {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "deploy.mode".to_string(),
                        reason: "only deploy.mode 'replicated' is allowed".to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_service_image(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let has_build = service.contains_key(YamlValue::String("build".to_string()));
        if let Some(image) = service.get(YamlValue::String("image".to_string())) {
            let Some(image) = image.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "image".to_string(),
                    reason: "image must be a literal registry reference".to_string(),
                });
            };
            let normalized = image.trim().to_ascii_lowercase();
            let is_raw_image_id = normalized.starts_with("sha256:")
                || (normalized.len() == 64
                    && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()));
            if Self::contains_interpolation(image)
                || normalized == "temps.internal"
                || normalized.starts_with("temps.internal/")
                || is_raw_image_id
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "image".to_string(),
                    reason: "interpolated, raw-ID, and Temps-internal image references are not allowed because they can select another deployment's local image"
                        .to_string(),
                });
            }
            if has_build {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "image".to_string(),
                    reason: "a Compose build may not set image; Temps assigns a deployment-scoped image name to prevent daemon-global tag collisions"
                        .to_string(),
                });
            }
        }

        if let Some(pull_policy) = service.get(YamlValue::String("pull_policy".to_string())) {
            if has_build || pull_policy.as_str() != Some("always") {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "pull_policy".to_string(),
                    reason: "image services always pull from their registry and build services use a Temps-scoped local image; custom pull_policy values are not allowed"
                        .to_string(),
                });
            }
        }

        Ok(())
    }

    fn validate_env_files(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let Some(env_file) = service.get(YamlValue::String("env_file".to_string())) else {
            return Ok(());
        };
        let validate_path = |path: &str| -> Result<(), ComposeError> {
            if Self::contains_interpolation(path)
                || Self::validate_relative_path(path, "env_file").is_err()
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "env_file".to_string(),
                    reason: "env_file paths must be literal relative paths confined to the project"
                        .to_string(),
                });
            }
            Ok(())
        };
        match env_file {
            YamlValue::String(path) => validate_path(path),
            YamlValue::Sequence(entries) => {
                for entry in entries {
                    match entry {
                        YamlValue::String(path) => validate_path(path)?,
                        YamlValue::Mapping(options) => {
                            let Some(path) = options
                                .get(YamlValue::String("path".to_string()))
                                .and_then(YamlValue::as_str)
                            else {
                                return Err(ComposeError::SecurityPolicyViolation {
                                    service: service_name.to_string(),
                                    field: "env_file".to_string(),
                                    reason:
                                        "long-form env_file entries must contain a literal path"
                                            .to_string(),
                                });
                            };
                            validate_path(path)?;
                        }
                        _ => {
                            return Err(ComposeError::SecurityPolicyViolation {
                                service: service_name.to_string(),
                                field: "env_file".to_string(),
                                reason: "env_file must be a path or a sequence of path entries"
                                    .to_string(),
                            });
                        }
                    }
                }
                Ok(())
            }
            _ => Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "env_file".to_string(),
                reason: "env_file must be a literal path or sequence".to_string(),
            }),
        }
    }

    fn validate_published_ports(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let Some(ports) = service.get(YamlValue::String("ports".to_string())) else {
            return Ok(());
        };
        let Some(ports) = ports.as_sequence() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "ports".to_string(),
                reason: "ports must be a sequence of loopback-only bindings".to_string(),
            });
        };
        for port in ports {
            let loopback_only = match port {
                YamlValue::String(binding) => {
                    !Self::contains_interpolation(binding)
                        && binding.starts_with("127.0.0.1:")
                        && binding.split(':').count() >= 3
                }
                YamlValue::Mapping(binding) => {
                    binding
                        .get(YamlValue::String("host_ip".to_string()))
                        .and_then(YamlValue::as_str)
                        == Some("127.0.0.1")
                }
                _ => false,
            };
            if !loopback_only {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "ports".to_string(),
                    reason: "published ports must explicitly bind to 127.0.0.1 so traffic cannot bypass the Temps proxy"
                        .to_string(),
                });
            }
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
        let Some(value) = service.get(YamlValue::String(field.to_string())) else {
            return Ok(());
        };
        match value.as_bool() {
            Some(value) if value == rejected => {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: field.to_string(),
                    reason: reason.to_string(),
                });
            }
            Some(_) => {}
            None => {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: field.to_string(),
                    reason: format!(
                        "{field} must be a literal boolean; quoted values, interpolation, and other value types are not allowed"
                    ),
                });
            }
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

    fn validate_network_mode(
        &self,
        service: &serde_yaml::Mapping,
        service_name: &str,
        declared_services: &serde_yaml::Mapping,
    ) -> Result<(), ComposeError> {
        let Some(value) = service.get(YamlValue::String("network_mode".to_string())) else {
            return Ok(());
        };
        let Some(mode) = value.as_str() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "network_mode".to_string(),
                reason: "network_mode must be omitted, 'none', or 'service:<declared-service>'"
                    .to_string(),
            });
        };
        if mode == "none" {
            return Ok(());
        }
        if let Some(target) = mode.strip_prefix("service:") {
            if !target.is_empty()
                && declared_services.contains_key(YamlValue::String(target.to_string()))
            {
                return Ok(());
            }
        }

        Err(ComposeError::SecurityPolicyViolation {
            service: service_name.to_string(),
            field: "network_mode".to_string(),
            reason: "network_mode may not join host, default bridge, arbitrary container, or external namespaces; omit it for the project network, use 'none', or share a declared service namespace"
                .to_string(),
        })
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
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: format!("{field} must be a literal namespace mode"),
            });
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
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "volumes".to_string(),
                reason: "volumes must be a sequence of validated mount entries".to_string(),
            });
        };

        for entry in entries {
            let long_form_type = if let Some(mapping) = entry.as_mapping() {
                let Some(mount_type) = mapping
                    .get(YamlValue::String("type".to_string()))
                    .and_then(YamlValue::as_str)
                else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.type".to_string(),
                        reason: "long-form volume mounts must declare a literal type of 'bind' or 'volume'"
                            .to_string(),
                    });
                };
                if Self::contains_interpolation(mount_type)
                    || !matches!(mount_type, "bind" | "volume")
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.type".to_string(),
                        reason: format!(
                            "long-form volume mount type '{mount_type}' is not allowed; only literal 'bind' and 'volume' mounts are supported"
                        ),
                    });
                }
                Some(mount_type)
            } else {
                None
            };
            let Some(source) = Self::volume_source(entry) else {
                if long_form_type == Some("bind") {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.source".to_string(),
                        reason: "long-form bind mounts must declare a literal source path"
                            .to_string(),
                    });
                }
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
            if long_form_type == Some("volume") {
                if !Self::is_named_volume_ref(&source) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.source".to_string(),
                        reason: format!(
                            "long-form volume source '{source}' must be a project-scoped named volume"
                        ),
                    });
                }
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

            if long_form_type.is_none() && Self::is_named_volume_ref(&source) {
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

    fn is_explicit_bind_mount(entry: &YamlValue) -> bool {
        entry
            .as_mapping()
            .and_then(|mapping| mapping.get(YamlValue::String("type".to_string())))
            .and_then(YamlValue::as_str)
            == Some("bind")
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
        !source.contains('/')
            && !source.contains('\\')
            && !source.starts_with('.')
            && !source.starts_with('~')
            && !source.is_empty()
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
        // Docker Compose expands home-relative paths before invoking Docker.
        // Treat both Unix and Windows separators as host-path escapes instead
        // of validating them as a literal directory beneath the checkout.
        if source.starts_with('~') {
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
                    if Self::is_named_volume_ref(&bind_source)
                        && !Self::is_explicit_bind_mount(entry)
                    {
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
        if candidate.as_os_str().is_empty() || candidate.is_absolute() || path.starts_with('~') {
            return Err(ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: path.to_string(),
                reason: "must be a non-empty relative path without home-directory expansion"
                    .to_string(),
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

        for key in service.keys().filter_map(Self::yaml_key) {
            if NEVER_ALLOWED_OVERRIDE_KEYS.contains(&key.as_str()) {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "service '{service_name}' uses forbidden key '{key}', which Compose \
                         deployments do not permit anywhere — the deploy-time security policy \
                         rejects it in the repository compose file too, so moving it there \
                         will not help"
                    ),
                });
            }
            if REPO_ONLY_OVERRIDE_KEYS.contains(&key.as_str()) {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "service '{service_name}' cannot set '{key}' as an inline override; \
                         declare it in the repository compose file instead, where it is \
                         checked against the deployment security policy (host paths, host \
                         namespaces and resource limits are still rejected there)"
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
        redact_values: &[String],
        build_args: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let cmd = Self::compose_build_command(project_dir, project_name, compose_file, build_args);

        debug!(project = %project_name, "Running docker compose build");

        let output = Self::bounded_command_output(
            cmd,
            COMPOSE_BUILD_TIMEOUT,
            project_name,
            "docker compose build",
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = sanitize_compose_diagnostic(&stderr, redact_values);
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

    async fn bounded_command_output(
        mut command: tokio::process::Command,
        timeout: std::time::Duration,
        project_name: &str,
        operation: &str,
    ) -> Result<std::process::Output, ComposeError> {
        command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("{operation} did not expose stdout"),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("{operation} did not expose stderr"),
            })?;
        let run = async {
            let (stdout, stderr, status) = tokio::try_join!(
                Self::read_bounded_stream(stdout),
                Self::read_bounded_stream(stderr),
                child.wait()
            )?;
            Ok::<_, std::io::Error>(std::process::Output {
                status,
                stdout,
                stderr,
            })
        };
        tokio::time::timeout(timeout, run)
            .await
            .map_err(|_| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("{operation} timed out after {} seconds", timeout.as_secs()),
            })?
            .map_err(ComposeError::Io)
    }

    async fn read_bounded_stream<R>(mut reader: R) -> Result<Vec<u8>, std::io::Error>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            let remaining = MAX_COMPOSE_COMMAND_OUTPUT_BYTES.saturating_sub(captured.len());
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        Ok(captured)
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
            // Last: applied after the user override so a repository cannot
            // redirect where secrets land.
            TEMPS_SECRETS_OVERRIDE,
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

    async fn detect_image_owned_init_services(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
    ) -> Result<HashSet<String>, ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_file_args(&mut cmd, project_dir, compose_file);
        Self::append_compose_env_file_args(&mut cmd, project_dir);
        cmd.arg("config")
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());
        cmd.kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(ComposeError::Io)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: "docker compose config stdout was unavailable".to_string(),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: "docker compose config stderr was unavailable".to_string(),
            })?;
        let stdout_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout
                .take((MAX_RESOLVED_COMPOSE_CONFIG_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr
                .take((MAX_COMPOSE_DIAGNOSTIC_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let collect_output = async {
            let status = child.wait().await?;
            let stdout = stdout_task.await.map_err(|error| {
                std::io::Error::other(format!("stdout reader failed: {error}"))
            })??;
            let stderr = stderr_task.await.map_err(|error| {
                std::io::Error::other(format!("stderr reader failed: {error}"))
            })??;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };
        let (status, stdout, stderr) = tokio::time::timeout(COMPOSE_CONFIG_TIMEOUT, collect_output)
            .await
            .map_err(|_| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "docker compose config timed out after {} seconds while resolving image entrypoints",
                    COMPOSE_CONFIG_TIMEOUT.as_secs()
                ),
            })??;
        if stdout.len() > MAX_RESOLVED_COMPOSE_CONFIG_BYTES {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "resolved Compose config exceeds the {} MiB safety limit",
                    MAX_RESOLVED_COMPOSE_CONFIG_BYTES / 1024 / 1024
                ),
            });
        }
        if !status.success() {
            let stderr =
                sanitize_compose_diagnostic(&String::from_utf8_lossy(&stderr), redact_values);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "docker compose config failed while resolving image entrypoints: {stderr}"
                ),
            });
        }

        let resolved: YamlValue =
            serde_yaml::from_slice(&stdout).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: format!("resolved Compose config for project '{project_name}'"),
                reason: error.to_string(),
            })?;
        let services = resolved
            .get("services")
            .and_then(YamlValue::as_mapping)
            .ok_or_else(|| ComposeError::InvalidComposeYaml {
                compose_source: format!("resolved Compose config for project '{project_name}'"),
                reason: "missing top-level services mapping".to_string(),
            })?;
        if services.len() > MAX_RESOLVED_COMPOSE_SERVICES {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "resolved Compose config declares {} services; the safety limit is {}",
                    services.len(),
                    MAX_RESOLVED_COMPOSE_SERVICES
                ),
            });
        }

        let mut services_by_image = HashMap::<String, Vec<String>>::new();
        for (name, definition) in services {
            let (Some(service), Some(image)) = (
                name.as_str(),
                definition.get("image").and_then(YamlValue::as_str),
            ) else {
                continue;
            };
            services_by_image
                .entry(image.to_string())
                .or_default()
                .push(service.to_string());
        }
        let docker = self.docker.clone();
        let inspections = futures::stream::iter(services_by_image)
            .map(|(image, services)| {
                let docker = docker.clone();
                async move {
                    let inspection = tokio::time::timeout(
                        DOCKER_IMAGE_INSPECT_TIMEOUT,
                        docker.inspect_image(&image),
                    )
                    .await;
                    (services, inspection)
                }
            })
            .buffer_unordered(MAX_CONCURRENT_IMAGE_INSPECTIONS)
            .collect::<Vec<_>>()
            .await;
        let mut detected = HashSet::new();
        for (services, inspection) in inspections {
            match inspection {
                Ok(inspect) => match inspect {
                    Ok(inspect) => {
                        let entrypoint = inspect
                            .config
                            .and_then(|config| config.entrypoint)
                            .unwrap_or_default();
                        if Self::entrypoint_owns_pid_one(&entrypoint) {
                            for service in services {
                                info!(
                                    project = %project_name,
                                    service = %service,
                                    "Detected image-owned init process; preserving it as PID 1"
                                );
                                detected.insert(service);
                            }
                        }
                    }
                    Err(error) => {
                        let error = sanitize_compose_diagnostic(&error.to_string(), redact_values);
                        for service in services {
                            warn!(
                                project = %project_name,
                                service = %service,
                                error = %error,
                                "Could not inspect image entrypoint; keeping Docker's init wrapper"
                            );
                        }
                    }
                },
                Err(_) => {
                    for service in services {
                        warn!(
                            project = %project_name,
                            service = %service,
                            timeout_secs = DOCKER_IMAGE_INSPECT_TIMEOUT.as_secs(),
                            "Image entrypoint inspection timed out; keeping Docker's init wrapper"
                        );
                    }
                }
            }
        }
        Ok(detected)
    }

    /// True only for entrypoints that are themselves well-known process
    /// supervisors. Do not inspect arbitrary command arguments or image names:
    /// a shell script may or may not eventually exec an init process, and
    /// guessing there would silently remove zombie reaping from ordinary apps.
    fn entrypoint_owns_pid_one(entrypoint: &[String]) -> bool {
        let Some(executable) = entrypoint.first().map(|value| value.trim()) else {
            return false;
        };
        let normalized = executable.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "/init" | "/sbin/init" | "/usr/sbin/init"
        ) {
            return true;
        }
        Path::new(&normalized)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                matches!(
                    name,
                    "tini"
                        | "tini-static"
                        | "dumb-init"
                        | "docker-init"
                        | "catatonit"
                        | "s6-svscan"
                        | "s6-overlay-suexec"
                        | "runsvdir"
                        | "runsvdir-start"
                        | "my_init"
                )
            })
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

    /// Pull images for every `image:`-based service in the compose stack.
    ///
    /// This is intentionally a separate step from [`compose_up`] so that
    /// image-fetch latency — which can range from seconds to minutes depending
    /// on registry speed and image size — occurs while the *old* containers are
    /// still running and serving traffic, not during the downtime window between
    /// old-container-stop and new-container-healthy. By the time `compose_up`
    /// tears down and recreates containers the images are already local, which
    /// keeps the actual downtime window as short as possible.
    ///
    /// `--ignore-buildable` skips services that only declare a `build:` stanza
    /// and have no pullable `image:` tag; those services are handled by
    /// `compose_build`. This makes the call safe regardless of whether the
    /// project mixes built and pulled services.
    async fn compose_pull(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
    ) -> Result<(), ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_file_args(&mut cmd, project_dir, compose_file);
        Self::append_compose_env_file_args(&mut cmd, project_dir);

        cmd.args(["pull", "--ignore-buildable"])
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());

        // Cancellation drops the command future; terminate the Compose CLI so
        // compensating `compose down` cannot race a still-running `compose pull`.
        cmd.kill_on_drop(true);

        debug!(project = %project_name, "Running docker compose pull");

        let output = Self::bounded_command_output(
            cmd,
            COMPOSE_PULL_TIMEOUT,
            project_name,
            "docker compose pull",
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = sanitize_compose_diagnostic(&stderr, redact_values);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose pull failed: {stderr}"),
            });
        }

        info!(project = %project_name, "docker compose pull completed");
        Ok(())
    }

    async fn compose_up(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
        relaxed_capability_services: &[String],
    ) -> Result<(), ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_file_args(&mut cmd, project_dir, compose_file);
        Self::append_compose_env_file_args(&mut cmd, project_dir);

        cmd.args(["up", "-d", "--remove-orphans", "--force-recreate"])
            .current_dir(project_dir);

        // Set PWD so compose files using ${PWD} resolve correctly
        cmd.env("PWD", project_dir.to_string_lossy().to_string());

        // Cancellation drops the command future; terminate the Compose CLI so
        // compensating `compose down` cannot race a still-running `compose up`.
        cmd.kill_on_drop(true);

        debug!(project = %project_name, "Running docker compose up");

        let output = Self::bounded_command_output(
            cmd,
            COMPOSE_UP_TIMEOUT,
            project_name,
            "docker compose up",
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // `up` blocks on `depends_on: condition: service_healthy`, so a
            // dependency that never becomes healthy fails `up` itself — before
            // this fix, that surfaced as just "container X is unhealthy" with
            // no indication of *why* X was unhealthy.
            let container_logs = self
                .describe_unhealthy_containers(
                    project_dir,
                    project_name,
                    compose_file,
                    redact_values,
                    relaxed_capability_services,
                )
                .await;
            let diagnostic = sanitize_compose_diagnostic(
                &format!("{}{}", stderr, container_logs),
                redact_values,
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
        let mut command = isolated_docker_command();
        command
            .args(["compose", "-p", project_name])
            .args(["-f", compose_file])
            .args(["ps", "--format", "json", "--all"])
            .current_dir(project_dir);
        let output = Self::bounded_command_output(
            command,
            COMPOSE_CONFIG_TIMEOUT,
            project_name,
            "docker compose ps",
        )
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
        redact_values: &[String],
        relaxed_capability_services: &[String],
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
        let mut capability_denied = Vec::new();
        for entry in &entries {
            let is_unhealthy = !entry.health.is_empty() && entry.health != "healthy";
            let is_not_running = entry.state != "running";
            if !is_unhealthy && !is_not_running {
                continue;
            }

            let logs = sanitize_compose_diagnostic(
                &self.container_log_tail(&entry.id).await,
                redact_values,
            );
            if looks_like_capability_denial(&logs) {
                capability_denied.push(entry.service.clone());
            }
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
            return String::new();
        }

        format!(
            "\n\nContainer logs for unhealthy/stopped services:\n\n{}{}",
            sections.join("\n\n"),
            Self::capability_denial_remediation(&capability_denied, relaxed_capability_services)
        )
    }

    /// Turn a detected capability denial into the one instruction that fixes
    /// it. Appended to the failure text rather than only logged, because the
    /// advisory `deploy_compose` emits before the deploy is easy to miss under
    /// hundreds of lines of image-pull progress — whereas the error is the one
    /// thing the operator cannot avoid reading.
    ///
    /// A service that is *already* relaxed and still denied gets the opposite
    /// message: the toggle is not the answer, so say so instead of sending the
    /// operator to flip a switch that is on.
    fn capability_denial_remediation(
        denied_services: &[String],
        relaxed_capability_services: &[String],
    ) -> String {
        if denied_services.is_empty() {
            return String::new();
        }

        let (already_relaxed, needs_relaxing): (Vec<_>, Vec<_>) = denied_services
            .iter()
            .partition(|service| relaxed_capability_services.contains(service));

        let mut hints = Vec::new();
        if !needs_relaxing.is_empty() {
            hints.push(format!(
                "Service(s) {} failed with \"Operation not permitted\". Temps runs every \
                 Compose service with all Linux capabilities dropped, and image entrypoints \
                 that prepare a data directory and then drop from root to a service user \
                 need {} to start. Enable \"Elevated permissions\" for them in {}, then \
                 redeploy.",
                quote_service_list(&needs_relaxing),
                Self::RELAXED_CAPABILITIES.join(", "),
                ELEVATED_PERMISSIONS_SETTINGS_PATH,
            ));
        }
        if !already_relaxed.is_empty() {
            hints.push(format!(
                "Service(s) {} already have \"Elevated permissions\" enabled, so this denial \
                 is not the Temps sandbox's capability drop — check whether the image needs a \
                 capability outside the granted set ({}), or runs as a user that cannot write \
                 its mounted data directory.",
                quote_service_list(&already_relaxed),
                Self::RELAXED_CAPABILITIES.join(", "),
            ));
        }

        format!("\n\n{}", hints.join("\n\n"))
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
        redact_values: &[String],
        relaxed_capability_services: &[String],
        timeout: std::time::Duration,
    ) -> Result<(), ComposeError> {
        let start = std::time::Instant::now();
        let mut observed_ready_once = false;
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
                        // Docker may briefly report a just-started container as
                        // running before its health state is populated (or
                        // before a short-lived process exits). Require one
                        // stable poll interval before accepting readiness.
                        if observed_ready_once {
                            return Ok(());
                        }
                        observed_ready_once = true;
                        tokio::time::sleep(COMPOSE_READY_POLL_INTERVAL).await;
                        continue;
                    }
                    observed_ready_once = false;
                    if start.elapsed() >= timeout {
                        let container_logs = self
                            .describe_unhealthy_containers(
                                project_dir,
                                project_name,
                                compose_file,
                                redact_values,
                                relaxed_capability_services,
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
                            redact_values,
                            relaxed_capability_services,
                        )
                        .await;
                    return Err(ComposeError::ServicesNotReady {
                        project: project_name.to_string(),
                        timeout_secs: timeout.as_secs(),
                        reason: format!("{}{}", reasons.join(", "), container_logs),
                    });
                }
                ComposeReadiness::Pending(reasons) => {
                    observed_ready_once = false;
                    if start.elapsed() >= timeout {
                        let container_logs = self
                            .describe_unhealthy_containers(
                                project_dir,
                                project_name,
                                compose_file,
                                redact_values,
                                relaxed_capability_services,
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

    /// Preserve the primary deployment error while best-effort discovering
    /// the candidate containers it left behind. Discovery must never replace
    /// the startup error: an empty list simply tells the caller that there is
    /// no safe live diagnostic surface to retain.
    async fn failure_with_discovered_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        request: &ComposeDeployRequest,
        error: ComposeError,
    ) -> ComposeDeployFailure {
        let containers = match self
            .discover_containers(project_dir, project_name, compose_file)
            .await
        {
            Ok(containers) => containers,
            Err(discovery_error) => {
                warn!(
                    project = %project_name,
                    error = %discovery_error,
                    "Compose startup failed and candidate containers could not be discovered"
                );
                return ComposeDeployFailure {
                    error,
                    containers: Vec::new(),
                    cleanup_containers: Vec::new(),
                    retention_error: Some(discovery_error),
                };
            }
        };

        for container in &containers {
            if let Err(retention_error) = self
                .verify_labels(
                    &container.container_id,
                    &request.labels,
                    &container.service_name,
                )
                .await
            {
                warn!(
                    project = %project_name,
                    container_id = %container.container_id,
                    service = %container.service_name,
                    error = %retention_error,
                    "Failed to verify retained Compose container labels"
                );
                return ComposeDeployFailure {
                    error,
                    containers: Vec::new(),
                    cleanup_containers: Vec::new(),
                    retention_error: Some(retention_error),
                };
            }
        }

        // Only expose candidates for direct cleanup after ownership has been
        // verified for the entire stack. Port safety is checked separately so
        // an unsafe first container cannot short-circuit sibling verification.
        for container in &containers {
            if let Err(retention_error) = self
                .verify_retention_port_bindings(&container.container_id, &container.service_name)
                .await
            {
                warn!(
                    project = %project_name,
                    container_id = %container.container_id,
                    service = %container.service_name,
                    error = %retention_error,
                    "Failed Compose container has a host port that is unsafe to retain"
                );
                return ComposeDeployFailure {
                    error,
                    containers: Vec::new(),
                    cleanup_containers: containers.clone(),
                    retention_error: Some(retention_error),
                };
            }
        }

        ComposeDeployFailure {
            error,
            containers,
            cleanup_containers: Vec::new(),
            retention_error: None,
        }
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

    async fn verify_labels(
        &self,
        container_id: &str,
        base_labels: &HashMap<String, String>,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let inspect = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| ComposeError::Docker(format!("inspect failed: {}", e)))?;

        let actual_labels = inspect
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or_else(|| ComposeError::Docker(format!(
                "container {container_id} for service '{service_name}' has no labels; refusing to register an unowned Compose container"
            )))?;
        for (key, expected) in base_labels {
            let actual = actual_labels.get(key).map(String::as_str);
            if actual != Some(expected.as_str()) {
                return Err(ComposeError::Docker(format!(
                    "container {container_id} for service '{service_name}' has ownership label '{key}'={actual:?}, expected '{expected}'"
                )));
            }
        }
        if actual_labels.get("sh.temps.service").map(String::as_str) != Some(service_name) {
            return Err(ComposeError::Docker(format!(
                "container {container_id} has a mismatched sh.temps.service ownership label for Compose service '{service_name}'"
            )));
        }

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

    /// A retained failure must not bypass the Temps proxy through a Docker
    /// host port. Inspect Docker's configured bindings (not `compose ps`,
    /// which can omit stopped-container publishers) and admit loopback only.
    async fn verify_retention_port_bindings(
        &self,
        container_id: &str,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let inspect = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|error| {
                ComposeError::Docker(format!(
                    "failed to inspect port bindings for retained container {container_id}: {error}"
                ))
            })?;
        let Some(port_bindings) = inspect
            .host_config
            .and_then(|host_config| host_config.port_bindings)
        else {
            return Ok(());
        };

        for (container_port, bindings) in port_bindings {
            for binding in bindings.into_iter().flatten() {
                let host_ip = binding.host_ip.unwrap_or_default();
                let loopback = host_ip
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback());
                if !loopback {
                    return Err(ComposeError::Docker(format!(
                        "service '{service_name}' publishes container port {container_port} on host address '{}'; failed stacks may be retained only when every host binding is loopback",
                        if host_ip.is_empty() { "all interfaces" } else { &host_ip }
                    )));
                }
            }
        }
        Ok(())
    }

    /// Force-remove a set of candidate containers after re-verifying their
    /// exact Temps ownership labels. Used as the fail-closed fallback for a
    /// failed stack that is unsafe to retain (for example, it publishes a
    /// host port on all interfaces). Every candidate is attempted so one
    /// Docker error cannot leave a sibling publicly reachable.
    pub async fn remove_verified_containers(
        &self,
        containers: &[ComposeServiceResult],
        expected_labels: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let mut failures = Vec::new();
        for container in containers {
            match self
                .docker
                .inspect_container(&container.container_id, None)
                .await
            {
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => continue,
                Err(error) => {
                    failures.push(format!(
                        "inspect {} (service '{}'): {error}",
                        container.container_id, container.service_name
                    ));
                    continue;
                }
                Ok(_) => {}
            }

            if let Err(error) = self
                .verify_labels(
                    &container.container_id,
                    expected_labels,
                    &container.service_name,
                )
                .await
            {
                failures.push(error.to_string());
                continue;
            }

            match self
                .docker
                .remove_container(
                    &container.container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                Ok(())
                | Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(error) => failures.push(format!(
                    "remove {} (service '{}'): {error}",
                    container.container_id, container.service_name
                )),
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(ComposeError::Docker(format!(
                "failed to remove {} ownership-verified Compose candidate container(s): {}",
                failures.len(),
                failures.join("; ")
            )))
        }
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

    /// Generate a docker-compose override that applies the same baseline
    /// sandboxing used by the single-container Docker runtime plus narrowly
    /// scoped health/PID-1 compatibility corrections.
    #[cfg(test)]
    fn generate_security_override(
        &self,
        compose_content: &str,
        relaxed_capability_services: &[String],
        unsandboxed_services: &[String],
    ) -> String {
        self.generate_security_override_with_image_init(
            compose_content,
            relaxed_capability_services,
            unsandboxed_services,
            &HashSet::new(),
            &Self::healthcheck_loopback_overrides(compose_content, None),
        )
    }

    fn generate_security_override_with_image_init(
        &self,
        compose_content: &str,
        relaxed_capability_services: &[String],
        unsandboxed_services: &[String],
        detected_image_owned_init_services: &HashSet<String>,
        healthcheck_loopback_overrides: &HashMap<String, String>,
    ) -> String {
        // Enumerate service names from the parsed YAML mapping so inline
        // mappings (`web: {image: nginx}`), anchors (`web: &app`), and merge
        // keys are all hardened, not just lines that end in `:`.
        let services = self.parse_service_names_yaml(compose_content);
        let built_services = Self::services_with_build(compose_content);
        let explicitly_disabled_init_services =
            Self::services_with_image_owned_init(compose_content);

        // Every service receives platform-owned resource and logging bounds,
        // including services with an explicit runtime-sandbox exemption.
        let affected_services = services.iter().collect::<Vec<_>>();

        if affected_services.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in affected_services {
            override_yaml.push_str(&format!("  {}:\n", service));
            override_yaml.push_str(&format!("    cpus: {COMPOSE_SERVICE_CPU_LIMIT}\n"));
            override_yaml.push_str(&format!("    mem_limit: {COMPOSE_SERVICE_MEMORY_LIMIT}\n"));
            override_yaml.push_str("    logging:\n");
            override_yaml.push_str("      driver: json-file\n");
            override_yaml.push_str("      options:\n");
            override_yaml.push_str("        max-size: 50m\n");
            override_yaml.push_str("        max-file: \"3\"\n");
            override_yaml.push_str("    pids_limit: 512\n");
            if !built_services.contains(service.as_str()) {
                override_yaml.push_str("    pull_policy: always\n");
            }
            let sandboxed = !unsandboxed_services.contains(service);
            // Applied last in the `-f` order, so `privileged: false` here wins
            // over anything that smuggled `privileged: true` past validation
            // (e.g. via runtime interpolation) as a last line of defense.
            if sandboxed {
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
            }
            // An explicit `init: false` is a compatibility contract: the
            // image owns PID 1 (commonly s6-overlay) and Docker's init wrapper
            // would make that entrypoint fail. Keep every other sandbox guard
            // instead of forcing the user to disable the entire sandbox.
            if detected_image_owned_init_services.contains(service.as_str()) {
                // This must be explicit rather than merely omitting `init`:
                // the user's base/override may contain `init: true`, and this
                // trusted final override has to win that scalar merge.
                override_yaml.push_str("    init: false\n");
            } else if sandboxed && !explicitly_disabled_init_services.contains(service.as_str()) {
                override_yaml.push_str("    init: true\n");
            }
            if let Some(test) = healthcheck_loopback_overrides.get(service.as_str()) {
                override_yaml.push_str("    healthcheck:\n");
                override_yaml.push_str(&format!("      test: {test}\n"));
            }
        }

        override_yaml
    }

    fn services_with_build(compose_content: &str) -> HashSet<String> {
        let Ok(mut root) = serde_yaml::from_str::<Value>(compose_content) else {
            return HashSet::new();
        };
        if root.apply_merge().is_err() {
            return HashSet::new();
        }
        root.get("services")
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(Mapping::iter)
            .filter_map(|(name, definition)| {
                definition
                    .as_mapping()
                    .is_some_and(|service| service.contains_key("build"))
                    .then(|| name.as_str().map(ToOwned::to_owned))
                    .flatten()
            })
            .collect()
    }

    fn services_with_image_owned_init(compose_content: &str) -> std::collections::HashSet<String> {
        let Ok(mut root) = serde_yaml::from_str::<Value>(compose_content) else {
            return std::collections::HashSet::new();
        };
        if root.apply_merge().is_err() {
            return std::collections::HashSet::new();
        }
        root.get("services")
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(Mapping::iter)
            .filter_map(|(name, definition)| {
                let owns_init = definition
                    .as_mapping()
                    .and_then(|service| service.get("init"))
                    .and_then(Value::as_bool)
                    == Some(false);
                owns_init
                    .then(|| name.as_str().map(str::to_string))
                    .flatten()
            })
            .collect()
    }

    /// Return only healthcheck tests that need an IPv4 loopback override.
    /// Read the raw Compose documents rather than `docker compose config`
    /// output so interpolation expressions remain expressions: a health probe
    /// containing `${TOKEN}` must never cause its resolved secret to be copied
    /// into a generated Compose file.
    fn healthcheck_loopback_overrides(
        compose_content: &str,
        compose_override: Option<&str>,
    ) -> HashMap<String, String> {
        let mut overrides = HashMap::new();
        for document in [Some(compose_content), compose_override]
            .into_iter()
            .flatten()
        {
            let Ok(mut root) = serde_yaml::from_str::<YamlValue>(document) else {
                continue;
            };
            if root.apply_merge().is_err() {
                continue;
            }
            let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, definition) in services {
                let Some(service_name) = name.as_str() else {
                    continue;
                };
                let Some(healthcheck) = definition.get("healthcheck") else {
                    continue;
                };
                let Some(healthcheck) = healthcheck.as_mapping() else {
                    overrides.remove(service_name);
                    continue;
                };
                if healthcheck.get("disable").and_then(YamlValue::as_bool) == Some(true) {
                    overrides.remove(service_name);
                    continue;
                }
                let Some(mut test) = healthcheck.get("test").cloned() else {
                    continue;
                };
                if Self::rewrite_healthcheck_http_localhost(&mut test) {
                    if let Ok(rendered) = serde_json::to_string(&test) {
                        overrides.insert(service_name.to_string(), rendered);
                    }
                } else {
                    // A later Compose override can replace a base probe with a
                    // probe that no longer needs normalization.
                    overrides.remove(service_name);
                }
            }
        }
        overrides
    }

    fn rewrite_healthcheck_http_localhost(value: &mut YamlValue) -> bool {
        match value {
            YamlValue::String(text) => {
                let normalized = Self::replace_http_localhost(text);
                if normalized == *text {
                    false
                } else {
                    *text = normalized;
                    true
                }
            }
            YamlValue::Sequence(values) => {
                let mut changed = false;
                for value in values {
                    changed |= Self::rewrite_healthcheck_http_localhost(value);
                }
                changed
            }
            YamlValue::Mapping(values) => {
                let mut changed = false;
                for value in values.values_mut() {
                    changed |= Self::rewrite_healthcheck_http_localhost(value);
                }
                changed
            }
            YamlValue::Tagged(value) => Self::rewrite_healthcheck_http_localhost(&mut value.value),
            _ => false,
        }
    }

    fn replace_http_localhost(input: &str) -> String {
        const NEEDLE: &str = "http://localhost";
        const REPLACEMENT: &str = "http://127.0.0.1";

        let mut remainder = input;
        let mut output = String::with_capacity(input.len());
        while let Some(index) = remainder.to_ascii_lowercase().find(NEEDLE) {
            output.push_str(&remainder[..index]);
            let after = &remainder[index + NEEDLE.len()..];
            if after
                .chars()
                .next()
                .is_none_or(|character| matches!(character, ':' | '/' | '?' | '#'))
            {
                output.push_str(REPLACEMENT);
            } else {
                output.push_str(&remainder[index..index + NEEDLE.len()]);
            }
            remainder = after;
        }
        output.push_str(remainder);
        output
    }

    /// Generate a docker-compose override that adds Temps labels to every service.
    /// These labels are required for log collection, monitoring, and container discovery.
    fn generate_labels_override(
        &self,
        compose_content: &str,
        labels: &HashMap<String, String>,
    ) -> String {
        // Reuse the same service parsing logic
        let services = self.parse_service_names_yaml(compose_content);

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
            "image: attacker-controlled:latest",
            "build: ./attacker",
            "env_file: ./override.env",
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
            let key = dangerous_override.split(':').next().unwrap();
            assert!(
                matches!(error, ComposeError::InvalidOverride { .. }),
                "expected {dangerous_override} to be rejected as an invalid override, got {error}"
            );
            assert!(
                error.to_string().contains(key),
                "expected the rejection of {dangerous_override} to name the key '{key}', got {error}"
            );
        }
    }

    /// The log tail an official postgres image leaves when the Temps sandbox
    /// denies its entrypoint the capabilities it needs. Shape preserved from a
    /// real failure; no identifying detail.
    const POSTGRES_CAPABILITY_DENIAL_LOG: &str =
        "chmod: /var/lib/postgresql/data: Operation not permitted\n\
         chmod: /var/run/postgresql: Operation not permitted\n\
         error: failed switching to 'postgres': operation not permitted\n";

    #[test]
    fn test_looks_like_capability_denial_detects_entrypoint_privilege_drop() {
        assert!(looks_like_capability_denial(POSTGRES_CAPABILITY_DENIAL_LOG));
        assert!(looks_like_capability_denial(
            "su-exec: setgroups: Operation not permitted"
        ));
        assert!(looks_like_capability_denial(
            "chown: changing ownership of '/data': Operation not permitted"
        ));
    }

    /// The hint must not fire on ordinary application errors — sending someone
    /// to a capability toggle that cannot fix their problem is worse than
    /// leaving the raw logs to speak for themselves.
    #[test]
    fn test_looks_like_capability_denial_ignores_unrelated_failures() {
        // "operation not permitted" with no privileged operation alongside it.
        assert!(!looks_like_capability_denial(
            "FATAL: password authentication failed for user 'app'"
        ));
        assert!(!looks_like_capability_denial(
            "Error: connect ECONNREFUSED 127.0.0.1:5432"
        ));
        assert!(!looks_like_capability_denial(
            "socket bind: Operation not permitted"
        ));
        // A privileged verb with no denial is just normal startup chatter.
        assert!(!looks_like_capability_denial(
            "chown: adjusting ownership of /var/lib/postgresql/data"
        ));
    }

    #[test]
    fn test_capability_denial_remediation_names_the_service_and_the_toggle() {
        let remediation =
            ComposeExecutor::capability_denial_remediation(&["postgres".to_string()], &[]);

        assert!(remediation.contains("'postgres'"), "{remediation}");
        assert!(
            remediation.contains(ELEVATED_PERMISSIONS_SETTINGS_PATH),
            "{remediation}"
        );
        assert!(remediation.contains("SETUID"), "{remediation}");
    }

    /// A service that is already relaxed and *still* denied needs the opposite
    /// advice: the toggle is on, so it is not the answer.
    #[test]
    fn test_capability_denial_remediation_does_not_resend_already_relaxed_services() {
        let remediation = ComposeExecutor::capability_denial_remediation(
            &["postgres".to_string()],
            &["postgres".to_string()],
        );

        assert!(
            remediation.contains("already have \"Elevated permissions\" enabled"),
            "{remediation}"
        );
        assert!(
            !remediation.contains(ELEVATED_PERMISSIONS_SETTINGS_PATH),
            "should not send the operator to a toggle that is already on: {remediation}"
        );
    }

    #[test]
    fn test_capability_denial_remediation_is_empty_without_a_denial() {
        assert!(
            ComposeExecutor::capability_denial_remediation(&[], &["postgres".to_string()])
                .is_empty()
        );
    }

    /// The two denylists must stay disjoint: a key in both would make the
    /// "never allowed" message win by ordering alone, and silently reintroduce
    /// the misleading advice the split exists to remove.
    #[test]
    fn test_override_key_denylists_are_disjoint() {
        for key in NEVER_ALLOWED_OVERRIDE_KEYS {
            assert!(
                !REPO_ONLY_OVERRIDE_KEYS.contains(key),
                "'{key}' is classified both as never-allowed and as repo-only"
            );
        }
    }

    /// A key the deploy-time policy rejects everywhere must not tell the user
    /// to move it into the repository compose file — that advice costs them a
    /// round trip and then fails with a different error.
    #[test]
    fn test_never_allowed_override_key_does_not_advise_moving_to_repository() {
        let compose = "services:\n  web:\n    image: nginx\n";
        let error = ComposeExecutor::validate_compose_override(
            "temps-test",
            compose,
            "services:\n  web:\n    privileged: true\n",
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("do not permit anywhere"),
            "expected an anywhere-forbidden message, got {message}"
        );
        assert!(
            message.contains("will not help"),
            "expected the message to rule out moving it to the repository, got {message}"
        );
    }

    /// A key the repository compose file legitimately accepts must point there,
    /// since that is the route that actually works.
    #[test]
    fn test_repo_only_override_key_points_at_the_repository_compose_file() {
        let compose = "services:\n  web:\n    image: nginx\n";
        let error = ComposeExecutor::validate_compose_override(
            "temps-test",
            compose,
            "services:\n  web:\n    volumes: ['data:/var/lib/data']\n",
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("declare it in the repository compose file"),
            "expected the message to point at the repository compose file, got {message}"
        );
        assert!(
            !message.contains("do not permit anywhere"),
            "'volumes' is accepted in the repository compose file, got {message}"
        );
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

    /// `has_build_directives` must detect a `build:` stanza that appears only
    /// in the compose override, not in the base file. This is the Bug 2 scenario:
    /// a user override adds `build:` to a service that only has `image:` in the
    /// base, making `compose_pull --ignore-buildable` skip that service (because
    /// the merged view has a build stanza), while the old code skipped
    /// `compose_build` too (because it only scanned the base file). The service
    /// ended up neither built nor pulled.
    #[test]
    fn test_has_build_in_override_only_is_detected() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        // Base file has no build directive.
        let base = "services:\n  web:\n    image: nginx:latest\n";
        // Override adds a build stanza to the same service.
        let override_yaml = "services:\n  web:\n    build: ./custom\n";

        // The base alone should NOT look like a build project.
        assert!(
            !executor.has_build_directives(base),
            "base without build: should not be detected as a build project"
        );
        // The override alone SHOULD be detected.
        assert!(
            executor.has_build_directives(override_yaml),
            "override with build: should be detected"
        );
        // The combined check — as used in prepare_and_pull — must fire when
        // the override has the directive even if the base does not.
        let combined =
            executor.has_build_directives(base) || executor.has_build_directives(override_yaml);
        assert!(
            combined,
            "combined base+override check must detect build: in the override"
        );
    }

    /// Verify that the combined base+override check does NOT produce a false
    /// positive when neither document contains a `build:` directive (e.g. a
    /// purely `image:`-based stack with an override that only changes env vars).
    #[test]
    fn test_no_build_in_base_or_override_is_not_detected() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let base = "services:\n  web:\n    image: nginx:latest\n";
        let override_yaml = "services:\n  web:\n    environment:\n      - FOO=bar\n";

        let combined =
            executor.has_build_directives(base) || executor.has_build_directives(override_yaml);
        assert!(
            !combined,
            "stack with no build: directives in either document must not be flagged as a build project"
        );
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
    fn test_healthcheck_loopback_override_is_generic_and_secret_safe() {
        let compose = r#"
services:
  web:
    image: example/web
    healthcheck:
      test: ["CMD-SHELL", "curl -H 'Authorization: Bearer ${TOKEN}' HTTP://LOCALHOST:8080/ready"]
  lookalike:
    image: example/lookalike
    healthcheck:
      test: ["CMD", "curl", "http://localhost.example/ready"]
  disabled:
    image: example/disabled
    healthcheck:
      test: ["CMD", "curl", "http://localhost:9000/ready"]
"#;
        let compose_override = r#"
services:
  disabled:
    healthcheck:
      disable: true
"#;

        let overrides =
            ComposeExecutor::healthcheck_loopback_overrides(compose, Some(compose_override));
        assert_eq!(overrides.len(), 1);
        let web = overrides.get("web").expect("web probe normalized");
        assert!(web.contains("http://127.0.0.1:8080/ready"));
        assert!(
            web.contains("${TOKEN}"),
            "interpolation must remain unresolved"
        );
        assert!(!overrides.contains_key("lookalike"));
        assert!(!overrides.contains_key("disabled"));

        let executor = ComposeExecutor::new(
            Arc::new(Docker::connect_with_defaults().expect("construct Docker client")),
            PathBuf::from("/tmp/test"),
        );
        let override_yaml = executor.generate_security_override_with_image_init(
            compose,
            &[],
            &["web".to_string()],
            &HashSet::new(),
            &overrides,
        );
        let parsed: YamlValue = serde_yaml::from_str(&override_yaml).unwrap();
        assert_eq!(parsed["services"]["web"].get("privileged"), None);
        let test = parsed["services"]["web"]["healthcheck"]["test"]
            .as_sequence()
            .expect("healthcheck test remains a Compose sequence");
        assert_eq!(test[0].as_str(), Some("CMD-SHELL"));
        assert!(test[1]
            .as_str()
            .is_some_and(|command| command.contains("http://127.0.0.1:8080/ready")));
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
    fn test_explicit_init_false_keeps_sandbox_but_does_not_wrap_image_init() {
        // Constructing the Bollard client does not contact the daemon. Keep
        // this pure YAML-generation regression mandatory in Docker-less CI.
        let executor = ComposeExecutor::new(
            Arc::new(Docker::connect_with_defaults().expect("construct Docker client")),
            PathBuf::from("/tmp/test"),
        );
        let compose = r#"
x-image-init: &image-init
  init: false
services:
  budge:
    image: lscr.io/linuxserver/budge:latest
    <<: *image-init
  worker:
    image: alpine:latest
"#;

        let override_yaml =
            executor.generate_security_override(compose, &["budge".to_string()], &[]);
        let override_value: Value = serde_yaml::from_str(&override_yaml).unwrap();
        let budge = override_value["services"]["budge"].as_mapping().unwrap();
        let worker = override_value["services"]["worker"].as_mapping().unwrap();

        assert_eq!(budge.get("init"), None);
        assert_eq!(budge.get("pids_limit").and_then(Value::as_u64), Some(512));
        assert!(budge.contains_key("cap_drop"));
        assert!(budge.contains_key("cap_add"));
        assert_eq!(worker.get("init").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn test_detects_image_owned_init_entrypoints_without_image_name_rules() {
        for entrypoint in [
            vec!["/init"],
            vec!["/usr/bin/tini", "--"],
            vec!["/usr/local/bin/dumb-init", "--"],
            vec!["/package/admin/s6/command/s6-svscan"],
            vec!["/usr/libexec/s6-overlay/s6-overlay-suexec"],
            vec!["/usr/bin/catatonit", "--"],
            vec!["/usr/bin/runsvdir"],
        ] {
            let entrypoint = entrypoint
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert!(
                ComposeExecutor::entrypoint_owns_pid_one(&entrypoint),
                "expected {entrypoint:?} to own PID 1"
            );
        }

        for entrypoint in [
            vec!["/bin/sh", "-c", "exec /init"],
            vec!["/app/initialize"],
            vec!["/app/my-tini-wrapper"],
            vec!["node", "server.js"],
        ] {
            let entrypoint = entrypoint
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert!(
                !ComposeExecutor::entrypoint_owns_pid_one(&entrypoint),
                "expected {entrypoint:?} to keep Docker init"
            );
        }
        assert!(!ComposeExecutor::entrypoint_owns_pid_one(&[]));
    }

    #[test]
    fn test_detected_image_init_omits_only_init_wrapper() {
        let executor = ComposeExecutor::new(
            Arc::new(Docker::connect_with_defaults().expect("construct Docker client")),
            PathBuf::from("/tmp/test"),
        );
        let compose = r#"
services:
  image-init:
    image: example.com/custom/runtime:latest
  ordinary-app:
    image: example.com/custom/app:latest
"#;
        let detected = HashSet::from(["image-init".to_string()]);

        let override_yaml = executor.generate_security_override_with_image_init(
            compose,
            &[],
            &[],
            &detected,
            &HashMap::new(),
        );
        let override_value: Value = serde_yaml::from_str(&override_yaml).unwrap();
        let image_init = override_value["services"]["image-init"]
            .as_mapping()
            .unwrap();
        let ordinary_app = override_value["services"]["ordinary-app"]
            .as_mapping()
            .unwrap();

        assert_eq!(image_init.get("init").and_then(Value::as_bool), Some(false));
        assert_eq!(
            image_init.get("pids_limit").and_then(Value::as_u64),
            Some(512)
        );
        assert!(image_init.contains_key("cap_drop"));
        assert!(image_init.contains_key("security_opt"));
        assert_eq!(
            ordinary_app.get("init").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn test_resolved_compose_detects_init_from_pulled_image_metadata() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose_available = tokio::process::Command::new("docker")
            .args(["compose", "version"])
            .output()
            .await
            .is_ok_and(|output| output.status.success());
        for image in ["ghcr.io/advplyr/audiobookshelf:2.34.0", "alpine:latest"] {
            if !compose_available || executor.docker.inspect_image(image).await.is_err() {
                println!("Docker Compose or {image} is unavailable; skipping runtime test");
                return;
            }
        }

        let project_dir = tempfile::tempdir().unwrap();
        let compose = r#"
services:
  custom-image-with-init:
    image: ghcr.io/advplyr/audiobookshelf:2.34.0
  ordinary-image:
    image: alpine:latest
"#;
        tokio::fs::write(project_dir.path().join("docker-compose.yml"), compose)
            .await
            .unwrap();
        tokio::fs::write(
            project_dir.path().join("docker-compose.temps-security.yml"),
            executor.generate_security_override(compose, &[], &[]),
        )
        .await
        .unwrap();

        let detected = executor
            .detect_image_owned_init_services(
                project_dir.path(),
                "temps-image-init-metadata-test",
                "docker-compose.yml",
                &[],
            )
            .await
            .unwrap();

        assert_eq!(
            detected,
            HashSet::from(["custom-image-with-init".to_string()])
        );
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

    #[tokio::test]
    async fn failed_compose_startup_returns_containers_without_removing_them() {
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

        let project_dir = tempfile::tempdir().unwrap();
        let project_name = format!(
            "temps-failed-retention-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let compose = r#"
services:
  worker:
    image: alpine:latest
    command: ["sh", "-c", "echo retained-compose-diagnostic; sleep 30"]
    healthcheck:
      test: ["CMD", "false"]
      interval: 1s
      timeout: 1s
      retries: 1
"#;
        let request = ComposeDeployRequest {
            project_name: project_name.clone(),
            compose_content: compose.to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.project_id".to_string(), "42".to_string()),
                ("sh.temps.environment".to_string(), "7".to_string()),
            ]),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
        };
        executor
            .write_compose_files(project_dir.path(), &request, "failed-test")
            .await
            .unwrap();
        let prepared = PreparedComposeDeploy {
            effective_dir: project_dir.path().to_path_buf(),
            project_name: project_name.clone(),
            compose_file: "docker-compose.yml".to_string(),
            redact_values: Vec::new(),
            secret_generation: "failed-test".to_string(),
            has_materialized_secrets: false,
        };

        let failure = executor
            .deploy_prepared(prepared, &request)
            .await
            .expect_err("unhealthy service must fail readiness");

        assert!(
            matches!(
                &failure.error,
                ComposeError::CommandFailed { .. } | ComposeError::ServicesNotReady { .. }
            ),
            "unexpected startup failure: {}",
            failure.error
        );
        assert_eq!(failure.containers.len(), 1);
        assert!(failure.cleanup_containers.is_empty());
        assert!(failure.retention_error.is_none());
        assert_eq!(failure.containers[0].service_name, "worker");
        assert_eq!(failure.containers[0].status, "running");
        let retained_id = failure.containers[0].container_id.clone();
        let inspect = executor
            .docker
            .inspect_container(&retained_id, None)
            .await
            .expect("failed candidate must still exist for log inspection");
        let labels = inspect
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        assert_eq!(
            labels.get("sh.temps.managed").map(String::as_str),
            Some("true")
        );
        let logs = executor.container_log_tail(&retained_id).await;
        assert!(
            logs.contains("retained-compose-diagnostic"),
            "logs were: {logs}"
        );

        executor
            .teardown_at(
                &project_name,
                Some(project_dir.path()),
                Some("docker-compose.yml"),
                &HashMap::new(),
                true,
            )
            .await
            .expect("runtime test must clean up its retained stack");
    }

    #[tokio::test]
    async fn quickly_exiting_compose_service_never_reports_ready() {
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

        let project_dir = tempfile::tempdir().unwrap();
        let project_name = format!(
            "temps-quick-exit-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let compose = r#"
services:
  worker:
    image: alpine:latest
    command: ["sh", "-c", "echo quick-exit-diagnostic; exit 17"]
"#;
        let request = ComposeDeployRequest {
            project_name: project_name.clone(),
            compose_content: compose.to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.project_id".to_string(), "42".to_string()),
                ("sh.temps.environment".to_string(), "7".to_string()),
            ]),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
        };
        executor
            .write_compose_files(project_dir.path(), &request, "quick-exit")
            .await
            .unwrap();
        let prepared = PreparedComposeDeploy {
            effective_dir: project_dir.path().to_path_buf(),
            project_name: project_name.clone(),
            compose_file: "docker-compose.yml".to_string(),
            redact_values: Vec::new(),
            secret_generation: "quick-exit".to_string(),
            has_materialized_secrets: false,
        };

        let failure = executor
            .deploy_prepared(prepared, &request)
            .await
            .expect_err("a quickly exiting service must never be reported ready");
        assert!(matches!(
            failure.error,
            ComposeError::ServicesNotReady { .. }
        ));
        assert_eq!(failure.containers.len(), 1);
        assert_eq!(failure.containers[0].status, "exited");
        let logs = executor
            .container_log_tail(&failure.containers[0].container_id)
            .await;
        assert!(logs.contains("quick-exit-diagnostic"), "logs were: {logs}");

        executor
            .teardown_at(
                &project_name,
                Some(project_dir.path()),
                Some("docker-compose.yml"),
                &HashMap::new(),
                true,
            )
            .await
            .expect("runtime test must clean up its retained stack");
    }

    #[tokio::test]
    async fn failed_compose_with_public_host_binding_is_not_retainable() {
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

        let project_dir = tempfile::tempdir().unwrap();
        let project_name = format!(
            "temps-failed-public-port-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let compose = r#"
services:
  worker:
    image: alpine:latest
    command: ["sh", "-c", "sleep 30"]
    ports:
      - "0.0.0.0::8080"
    healthcheck:
      test: ["CMD", "false"]
      interval: 1s
      timeout: 1s
      retries: 1
"#;
        let request = ComposeDeployRequest {
            project_name: project_name.clone(),
            compose_content: compose.to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.project_id".to_string(), "42".to_string()),
                ("sh.temps.environment".to_string(), "7".to_string()),
            ]),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
        };
        executor
            .write_compose_files(project_dir.path(), &request, "failed-public-port")
            .await
            .unwrap();
        let prepared = PreparedComposeDeploy {
            effective_dir: project_dir.path().to_path_buf(),
            project_name: project_name.clone(),
            compose_file: "docker-compose.yml".to_string(),
            redact_values: Vec::new(),
            secret_generation: "failed-public-port".to_string(),
            has_materialized_secrets: false,
        };

        let failure = executor
            .deploy_prepared(prepared, &request)
            .await
            .expect_err("unhealthy service must fail readiness");
        assert!(failure.containers.is_empty());
        assert_eq!(failure.cleanup_containers.len(), 1);
        let unsafe_container_id = failure.cleanup_containers[0].container_id.clone();
        let retention_error = failure
            .retention_error
            .expect("public host binding must block retention")
            .to_string();
        assert!(retention_error.contains("0.0.0.0"));
        assert!(retention_error.contains("loopback"));

        executor
            .remove_verified_containers(&failure.cleanup_containers, &request.labels)
            .await
            .expect("ownership-verified direct cleanup must remove the unsafe container");
        let simulated_teardown_failure = executor
            .teardown_at(
                &project_name,
                Some(project_dir.path()),
                Some("missing-compose.yml"),
                &HashMap::new(),
                true,
            )
            .await;
        assert!(simulated_teardown_failure.is_err());
        assert!(matches!(
            executor
                .docker
                .inspect_container(&unsafe_container_id, None)
                .await,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ));

        executor
            .teardown_at(
                &project_name,
                Some(project_dir.path()),
                Some("docker-compose.yml"),
                &HashMap::new(),
                true,
            )
            .await
            .expect("runtime test must clean up the unsafe failed stack");
    }

    #[test]
    fn labels_override_supports_inline_service_mappings() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = r#"services: {web: {image: nginx:alpine}, worker: {image: alpine}}"#;
        let labels = HashMap::from([
            ("sh.temps.managed".to_string(), "true".to_string()),
            ("sh.temps.project_id".to_string(), "42".to_string()),
        ]);

        let rendered = executor.generate_labels_override(compose, &labels);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&rendered).unwrap();
        let services = parsed
            .get("services")
            .and_then(serde_yaml::Value::as_mapping)
            .unwrap();

        for service in ["web", "worker"] {
            let service_config = services
                .get(serde_yaml::Value::String(service.to_string()))
                .and_then(serde_yaml::Value::as_mapping)
                .unwrap();
            let generated_labels = service_config
                .get(serde_yaml::Value::String("labels".to_string()))
                .and_then(serde_yaml::Value::as_mapping)
                .unwrap();
            assert_eq!(
                generated_labels.get(serde_yaml::Value::String("sh.temps.managed".to_string())),
                Some(&serde_yaml::Value::String("true".to_string()))
            );
            assert_eq!(
                generated_labels.get(serde_yaml::Value::String("sh.temps.service".to_string())),
                Some(&serde_yaml::Value::String(service.to_string()))
            );
        }
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
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: vec!["webserver".to_string()],
        };

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        assert!(!stale_override.exists());
    }

    #[test]
    fn test_reserved_generated_compose_file_detection() {
        for reserved in RESERVED_GENERATED_COMPOSE_FILES {
            assert!(
                is_reserved_generated_compose_file(Path::new(reserved)),
                "{reserved} must be recognised as generated"
            );
            assert!(
                is_reserved_generated_compose_file(&Path::new("./").join(reserved)),
                "{reserved} must be recognised through a './' prefix"
            );
        }
        // Case-insensitive: the stack directory may live on a case-insensitive
        // filesystem, where `Docker-Compose.Temps-Override.yml` is the same file.
        assert!(is_reserved_generated_compose_file(Path::new(
            "Docker-Compose.Temps-Override.YML"
        )));
        // Only the stack root is passed to `docker compose -f`.
        assert!(!is_reserved_generated_compose_file(Path::new(
            "config/docker-compose.temps-override.yml"
        )));
        assert!(!is_reserved_generated_compose_file(Path::new(".env")));
        assert!(!is_reserved_generated_compose_file(Path::new(
            "docker-compose.yml"
        )));
    }

    /// A tenant compose file must not be able to make Temps materialise an
    /// `env_file` onto one of the generated Compose filenames: `compose_up`
    /// passes those to the daemon unvalidated, so attacker-influenced bytes
    /// there are a container-escape primitive, not just a config oddity.
    #[tokio::test]
    async fn test_write_compose_files_rejects_env_file_targeting_generated_override() {
        let Some(executor) = test_executor() else {
            return;
        };
        let project_dir = tempfile::tempdir().unwrap();
        let request = ComposeDeployRequest {
            project_name: "temps-test".to_string(),
            compose_content: "services:\n  app:\n    image: nginx\n    env_file: \
                              docker-compose.temps-override.yml\n"
                .to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::from([("SECRET".to_string(), "value".to_string())]),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: vec!["app".to_string()],
        };

        let err = executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap_err();
        assert_eq!(violation_field(err), "env_file");

        // And the generated override must not carry tenant env-var content.
        let generated = project_dir.path().join("docker-compose.temps-override.yml");
        if generated.exists() {
            let contents = tokio::fs::read_to_string(&generated).await.unwrap();
            assert!(
                !contents.contains("SECRET"),
                "generated override must never contain env-file content: {contents}"
            );
        }
    }

    /// The long form (`env_file: [{{path: ...}}]`) reaches the same writer, so
    /// it must be rejected identically.
    #[tokio::test]
    async fn test_write_compose_files_rejects_long_form_env_file_targeting_generated_file() {
        let Some(executor) = test_executor() else {
            return;
        };
        let project_dir = tempfile::tempdir().unwrap();
        let request = ComposeDeployRequest {
            project_name: "temps-test".to_string(),
            compose_content: "services:\n  app:\n    image: nginx\n    env_file:\n      \
                              - path: docker-compose.temps-security.yml\n        required: false\n"
                .to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::from([("SECRET".to_string(), "value".to_string())]),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: vec!["app".to_string()],
        };

        let err = executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap_err();
        assert_eq!(violation_field(err), "env_file");
    }

    /// `compose_path` comes from project settings, so it is user-selected too.
    #[tokio::test]
    async fn test_write_compose_files_rejects_compose_path_targeting_generated_file() {
        let Some(executor) = test_executor() else {
            return;
        };
        let project_dir = tempfile::tempdir().unwrap();
        let request = ComposeDeployRequest {
            project_name: "temps-test".to_string(),
            compose_content: "services:\n  app:\n    image: nginx\n".to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: Some("docker-compose.temps-network.yml".to_string()),
            environment_vars: HashMap::new(),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: vec!["app".to_string()],
        };

        let err = executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap_err();
        assert_eq!(violation_field(err), "compose_path");
    }

    /// The guard must not break the ordinary case: a normal `env_file:` is
    /// still materialised from the project's environment variables.
    #[tokio::test]
    async fn test_write_compose_files_still_materialises_normal_env_file() {
        let Some(executor) = test_executor() else {
            return;
        };
        let project_dir = tempfile::tempdir().unwrap();
        let request = ComposeDeployRequest {
            project_name: "temps-test".to_string(),
            compose_content: "services:\n  app:\n    image: nginx\n    env_file: app.env\n"
                .to_string(),
            env_content: None,
            work_dir: project_dir.path().to_path_buf(),
            compose_path: None,
            environment_vars: HashMap::from([("SECRET".to_string(), "value".to_string())]),
            secrets: HashMap::new(),
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: vec!["app".to_string()],
        };

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let materialised = tokio::fs::read_to_string(project_dir.path().join("app.env"))
            .await
            .unwrap();
        assert!(materialised.contains("SECRET"), "got: {materialised}");
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

    /// A request with everything empty, so a test only states the fields it
    /// actually exercises.
    fn secrets_test_request(
        project_name: &str,
        compose_content: &str,
        secrets: HashMap<String, String>,
    ) -> ComposeDeployRequest {
        ComposeDeployRequest {
            project_name: project_name.to_string(),
            compose_content: compose_content.to_string(),
            env_content: None,
            work_dir: PathBuf::from("/tmp"),
            compose_path: None,
            environment_vars: HashMap::new(),
            secrets,
            secret_compose_services: HashMap::new(),
            build_args: HashMap::new(),
            labels: HashMap::new(),
            repo_dir: None,
            compose_override: None,
            relaxed_capability_services: Vec::new(),
            unsandboxed_services: Vec::new(),
        }
    }

    fn one_secret(key: &str, value: &str) -> HashMap<String, String> {
        HashMap::from([(key.to_string(), value.to_string())])
    }

    #[tokio::test]
    async fn test_compose_secrets_are_materialized_and_mounted_read_only() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n  worker:\n    image: nginx\n",
            one_secret("DB_PASSWORD", "hunter2"),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let secret_file = executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("web")
            .join("DB_PASSWORD");
        assert_eq!(
            tokio::fs::read_to_string(&secret_file).await.unwrap(),
            "hunter2"
        );

        let override_yaml =
            tokio::fs::read_to_string(project_dir.path().join(TEMPS_SECRETS_OVERRIDE))
                .await
                .unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&override_yaml).unwrap();
        for service in ["web", "worker"] {
            let volumes = parsed["services"][service]["volumes"]
                .as_sequence()
                .unwrap();
            assert_eq!(volumes.len(), 1);
            let mount = volumes[0].as_str().unwrap();
            assert!(mount.ends_with(":/run/secrets:ro"), "mount was {mount}");
            assert!(mount.starts_with(
                &executor
                    .secret_generation_dir("temps-1-2", "test-generation")
                    .to_string_lossy()
                    .to_string()
            ));
        }

        // The plaintext must not leak into any other generated artifact.
        assert!(!override_yaml.contains("hunter2"));
        let mut entries = tokio::fs::read_dir(project_dir.path()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry.file_type().await.unwrap().is_file() {
                let body = tokio::fs::read_to_string(entry.path()).await.unwrap();
                assert!(
                    !body.contains("hunter2"),
                    "secret leaked into {}",
                    entry.path().display()
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_compose_secrets_root_is_not_traversable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "s3cr3t"),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        // The 0700 root is the whole host-side boundary: the per-stack
        // directory below it must stay traversable by the container's uid.
        let root_mode = tokio::fs::metadata(executor.secrets_root())
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(root_mode, 0o700);

        let file_mode = tokio::fs::metadata(
            executor
                .secret_generation_dir("temps-1-2", "test-generation")
                .join("web")
                .join("TOKEN"),
        )
        .await
        .unwrap()
        .permissions()
        .mode()
            & 0o222;
        assert_eq!(file_mode, 0, "secret files must not be writable");
    }

    #[tokio::test]
    async fn test_compose_secrets_removed_when_last_secret_is_deleted() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  web:\n    image: nginx\n";

        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("OLD_KEY", "value")),
                "test-generation",
            )
            .await
            .unwrap();
        assert!(project_dir.path().join(TEMPS_SECRETS_OVERRIDE).exists());

        // Redeploy after the user deleted every secret.
        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, HashMap::new()),
                "test-generation",
            )
            .await
            .unwrap();

        assert!(
            !executor
                .secret_generation_dir("temps-1-2", "test-generation")
                .exists(),
            "stale plaintext survived a redeploy with no secrets"
        );
        assert!(
            !project_dir.path().join(TEMPS_SECRETS_OVERRIDE).exists(),
            "stale override would still mount an emptied directory"
        );
    }

    #[tokio::test]
    async fn test_compose_secrets_rotated_value_replaces_previous_file() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  web:\n    image: nginx\n";

        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("API_KEY", "old-value")),
                "test-generation",
            )
            .await
            .unwrap();
        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("RENAMED", "new-value")),
                "test-generation",
            )
            .await
            .unwrap();

        let dir = executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("web");
        assert!(
            !dir.join("API_KEY").exists(),
            "renamed key left a stale file"
        );
        assert_eq!(
            tokio::fs::read_to_string(dir.join("RENAMED"))
                .await
                .unwrap(),
            "new-value"
        );
    }

    #[tokio::test]
    async fn test_candidate_secret_generation_preserves_active_generation_until_promotion() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  web:\n    image: nginx\n";

        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("TOKEN", "active")),
                "active-generation",
            )
            .await
            .unwrap();
        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("TOKEN", "candidate")),
                "candidate-generation",
            )
            .await
            .unwrap();

        let active = executor
            .secret_generation_dir("temps-1-2", "active-generation")
            .join("web/TOKEN");
        let candidate = executor
            .secret_generation_dir("temps-1-2", "candidate-generation")
            .join("web/TOKEN");
        assert_eq!(tokio::fs::read_to_string(&active).await.unwrap(), "active");
        assert_eq!(
            tokio::fs::read_to_string(&candidate).await.unwrap(),
            "candidate"
        );

        executor
            .cleanup_secret_generation("temps-1-2", "candidate-generation")
            .await
            .unwrap();
        assert!(
            active.exists(),
            "cancelling preparation removed active secrets"
        );
        assert!(
            !candidate.exists(),
            "candidate plaintext survived cancellation"
        );
    }

    #[tokio::test]
    async fn test_project_lifecycle_lock_serializes_distinct_executor_instances() {
        let docker = Arc::new(
            Docker::connect_with_local_defaults()
                .expect("constructing the Docker client should not contact the daemon"),
        );
        let data_dir = tempfile::tempdir().unwrap();
        let first_executor = ComposeExecutor::new(docker.clone(), data_dir.path().to_path_buf());
        let second_executor = ComposeExecutor::new(docker, data_dir.path().to_path_buf());

        let first = first_executor
            .acquire_project_lifecycle_lock("temps-1-2")
            .await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(25),
            second_executor.acquire_project_lifecycle_lock("temps-1-2"),
        )
        .await;
        assert!(
            blocked.is_err(),
            "a second executor entered the same project lifecycle concurrently"
        );

        drop(first);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            second_executor.acquire_project_lifecycle_lock("temps-1-2"),
        )
        .await
        .expect("the next workflow should acquire the project after release");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_preparation_failure_cleans_candidate_but_preserves_active_secrets() {
        use std::os::unix::fs::symlink;

        let docker = Docker::connect_with_local_defaults()
            .expect("constructing the Docker client should not contact the daemon");
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let active_request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "active"),
        );
        executor
            .write_compose_files(project_dir.path(), &active_request, "active-generation")
            .await
            .unwrap();

        symlink("/", project_dir.path().join("escape")).unwrap();
        let mut candidate_request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n    volumes:\n      - ./escape:/data\n",
            one_secret("TOKEN", "candidate"),
        );
        candidate_request.repo_dir = Some(project_dir.path().to_path_buf());

        let error = match executor
            .prepare_and_pull_for_generation(&candidate_request, "candidate-generation")
            .await
        {
            Ok(_) => panic!("symlink escape should fail before pull"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ComposeError::SecurityPolicyViolation { .. }
        ));

        let active = executor
            .secret_generation_dir("temps-1-2", "active-generation")
            .join("web/TOKEN");
        assert_eq!(tokio::fs::read_to_string(active).await.unwrap(), "active");
        assert!(!executor
            .secret_generation_dir("temps-1-2", "candidate-generation")
            .exists());
    }

    #[tokio::test]
    async fn test_pruning_secret_generations_removes_only_obsolete_plaintext() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  web:\n    image: nginx\n";

        for (generation, value) in [("old", "old-secret"), ("current", "new-secret")] {
            executor
                .write_compose_files(
                    project_dir.path(),
                    &secrets_test_request("temps-1-2", compose, one_secret("TOKEN", value)),
                    generation,
                )
                .await
                .unwrap();
        }

        executor
            .prune_secret_generations("temps-1-2", Some("current"))
            .await
            .unwrap();
        assert!(!executor.secret_generation_dir("temps-1-2", "old").exists());
        assert!(executor
            .secret_generation_dir("temps-1-2", "current")
            .join("web/TOKEN")
            .exists());
    }

    fn scoped_request(
        secrets: HashMap<String, String>,
        scopes: HashMap<String, Vec<String>>,
    ) -> ComposeDeployRequest {
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres:18\n",
            secrets,
        );
        request.secret_compose_services = scopes;
        request
    }

    #[tokio::test]
    async fn test_scoped_secret_is_only_written_for_entitled_services() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = scoped_request(
            HashMap::from([
                ("APP_ONLY".to_string(), "app-value".to_string()),
                ("SHARED".to_string(), "shared-value".to_string()),
            ]),
            HashMap::from([("APP_ONLY".to_string(), vec!["web".to_string()])]),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let stack = executor.secret_generation_dir("temps-1-2", "test-generation");
        // The scoped secret exists only under the entitled service. This is
        // the whole point: `db` has no filesystem path to the value, so it is
        // not merely "not mounted" -- it was never written for that service.
        assert!(stack.join("web").join("APP_ONLY").exists());
        assert!(!stack.join("db").join("APP_ONLY").exists());
        // The unscoped secret still reaches everyone.
        assert!(stack.join("web").join("SHARED").exists());
        assert!(stack.join("db").join("SHARED").exists());
    }

    #[tokio::test]
    async fn test_service_entitled_to_nothing_gets_no_mount_at_all() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = scoped_request(
            one_secret("APP_ONLY", "app-value"),
            HashMap::from([("APP_ONLY".to_string(), vec!["web".to_string()])]),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let parsed: YamlValue = serde_yaml::from_str(
            &tokio::fs::read_to_string(project_dir.path().join(TEMPS_SECRETS_OVERRIDE))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(parsed["services"]["web"].is_mapping());
        // An empty /run/secrets mount would imply "this app has no secrets";
        // no mount at all is the honest representation.
        assert!(parsed["services"]["db"].is_null());
        assert!(!executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("db")
            .exists());
    }

    #[tokio::test]
    async fn test_narrowing_a_scope_removes_the_previous_service_copy() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();

        // Deploy once unscoped, so both services hold a copy.
        executor
            .write_compose_files(
                project_dir.path(),
                &scoped_request(one_secret("TOKEN", "value"), HashMap::new()),
                "test-generation",
            )
            .await
            .unwrap();
        assert!(executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("db")
            .join("TOKEN")
            .exists());

        // Then narrow it to `web` only. Revoking access must actually delete
        // the plaintext the other service already had on disk.
        executor
            .write_compose_files(
                project_dir.path(),
                &scoped_request(
                    one_secret("TOKEN", "value"),
                    HashMap::from([("TOKEN".to_string(), vec!["web".to_string()])]),
                ),
                "test-generation",
            )
            .await
            .unwrap();

        assert!(executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("web")
            .join("TOKEN")
            .exists());
        assert!(
            !executor
                .secret_generation_dir("temps-1-2", "test-generation")
                .join("db")
                .exists(),
            "narrowing a scope left the revoked service's plaintext on disk"
        );
    }

    #[test]
    fn test_empty_scope_means_every_service_not_no_service() {
        let secrets = one_secret("TOKEN", "value");
        // Both shapes a caller can produce for "unconfigured".
        for scopes in [
            HashMap::new(),
            HashMap::from([("TOKEN".to_string(), Vec::<String>::new())]),
        ] {
            let names = ComposeExecutor::secret_names_for_service(&secrets, &scopes, "anything");
            assert_eq!(
                names,
                vec!["TOKEN".to_string()],
                "an unconfigured scope must not withhold the secret"
            );
        }
    }

    #[test]
    fn test_unmatched_scopes_are_reported_for_renamed_services() {
        let scopes = HashMap::from([
            ("TOKEN".to_string(), vec!["worker".to_string()]),
            ("OTHER".to_string(), vec!["web".to_string()]),
        ]);
        let known = vec!["web".to_string(), "db".to_string()];

        let unmatched = ComposeExecutor::unmatched_secret_scopes(&scopes, &known);

        assert_eq!(
            unmatched,
            vec![("TOKEN".to_string(), "worker".to_string())],
            "a scope naming a service that no longer exists must be reported"
        );
    }

    #[test]
    fn test_service_names_that_are_not_path_components_are_rejected() {
        for service in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(
                ComposeExecutor::validate_service_dir_name(service).is_err(),
                "service {service:?} should be rejected"
            );
        }
        assert!(ComposeExecutor::validate_service_dir_name("web-1.api_v2").is_ok());
    }

    #[tokio::test]
    async fn test_compose_override_cannot_introduce_a_service_without_secrets() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "value"),
        );
        request.compose_override = Some("services:\n  worker:\n    image: nginx\n".to_string());

        // Secret coverage is enumerated from the compose documents, so a
        // service the enumeration cannot see would silently get no secrets.
        // `validate_compose_override` is what makes that unreachable: an
        // inline override may not introduce services at all. This test pins
        // that dependency so the guarantee cannot be removed elsewhere
        // without a failure here.
        let error = executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap_err();

        assert!(
            matches!(error, ComposeError::InvalidOverride { ref reason, .. } if reason.contains("cannot add service")),
            "expected the override to be rejected, got {error:?}"
        );
    }

    #[test]
    fn test_all_service_names_merges_both_documents_in_order() {
        let Some(executor) = test_executor() else {
            return;
        };

        let names = executor.all_service_names(&[
            "services:\n  web:\n    image: nginx\n  shared:\n    image: nginx\n",
            "",
            "services:\n  shared:\n    image: nginx\n  extra:\n    image: nginx\n",
        ]);

        assert_eq!(names, vec!["web", "shared", "extra"]);
    }

    #[tokio::test]
    async fn test_compose_secrets_fail_loudly_when_data_dir_has_a_colon() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        // Docker splits bind specs on ':', so this must not silently mount the
        // wrong path.
        let colon_dir = data_dir.path().join("has:colon");
        let executor = ComposeExecutor::new(Arc::new(docker), colon_dir);
        let project_dir = tempfile::tempdir().unwrap();
        let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "value"),
        );

        let error = executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap_err();

        assert!(
            matches!(error, ComposeError::FileWriteFailed { ref reason, .. } if reason.contains("colon")),
            "expected a clear colon error, got {error:?}"
        );
    }

    #[test]
    fn test_compose_path_cannot_shadow_a_generated_override() {
        for reserved in RESERVED_GENERATED_COMPOSE_FILES.iter().copied() {
            assert!(
                ComposeExecutor::validate_compose_path_not_generated(reserved).is_err(),
                "{reserved} should be reserved"
            );
            assert!(
                ComposeExecutor::validate_compose_path_not_generated(&format!("stack/{reserved}"))
                    .is_err(),
                "{reserved} should be reserved in a subdirectory too"
            );
        }
        assert!(ComposeExecutor::validate_compose_path_not_generated("docker-compose.yml").is_ok());
    }

    #[test]
    fn test_secret_keys_that_are_not_filenames_are_rejected() {
        for key in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            "1LEADING_DIGIT",
            "has space",
            "has-dash",
            "has:colon",
            "..\\..\\etc\\passwd",
        ] {
            assert!(
                ComposeExecutor::validate_secret_file_name(key).is_err(),
                "key {key:?} should be rejected"
            );
        }
        assert!(ComposeExecutor::validate_secret_file_name("DB_PASSWORD").is_ok());
    }

    #[test]
    fn test_services_managing_own_secrets_are_detected() {
        let compose = r#"
services:
  short_form:
    image: nginx
    volumes:
      - ./local:/run/secrets:ro
  long_form:
    image: nginx
    volumes:
      - type: bind
        source: ./local
        target: /run/secrets/nested
  compose_secrets:
    image: nginx
    secrets:
      - db_password
  clean:
    image: nginx
    volumes:
      - ./data:/var/lib/data
"#;
        let detected = ComposeExecutor::services_managing_own_secrets(&[compose]);

        assert!(detected.contains("short_form"));
        assert!(detected.contains("long_form"));
        assert!(detected.contains("compose_secrets"));
        assert!(!detected.contains("clean"));
    }

    #[tokio::test]
    async fn test_compose_secrets_skip_services_with_conflicting_mount() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n  legacy:\n    image: nginx\n    volumes:\n      - ./s:/run/secrets\n",
            one_secret("TOKEN", "value"),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let parsed: YamlValue = serde_yaml::from_str(
            &tokio::fs::read_to_string(project_dir.path().join(TEMPS_SECRETS_OVERRIDE))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(parsed["services"]["web"].is_mapping());
        // Mounting on top would make `docker compose up` fail for the whole
        // stack on a duplicate mount target.
        assert!(parsed["services"]["legacy"].is_null());
    }

    #[test]
    fn test_redactable_values_keep_secret_and_env_var_sharing_a_key() {
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "secret-value"),
        );
        request
            .environment_vars
            .insert("TOKEN".to_string(), "env-value".to_string());

        let values = collect_redactable_values(&request);

        assert!(values.contains(&"secret-value".to_string()));
        assert!(values.contains(&"env-value".to_string()));
        let diagnostic = sanitize_compose_diagnostic("saw env-value and secret-value", &values);
        assert!(!diagnostic.contains("secret-value"));
        assert!(!diagnostic.contains("env-value"));
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
    fn test_validate_compose_security_policy_rejects_unsupported_long_form_mount_types() {
        let Some(executor) = test_executor() else {
            return;
        };

        for compose in [
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - type: image\n        source: temps.internal/victim\n        target: /stolen\n",
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - type: ${MOUNT_TYPE:-bind}\n        source: ./data\n        target: /data\n",
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - source: ./data\n        target: /data\n",
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert!(violation_field(err).starts_with("volumes"));
        }
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_home_relative_paths() {
        let Some(executor) = test_executor() else {
            return;
        };

        let cases = [
            (
                "volumes",
                "services:\n  pwn:\n    image: alpine\n    volumes:\n      - ~/.ssh:/stolen:ro\n",
            ),
            (
                "volumes",
                "services:\n  pwn:\n    image: alpine\n    volumes:\n      - type: bind\n        source: ~other/.ssh\n        target: /stolen\n",
            ),
            (
                "build.context",
                "services:\n  pwn:\n    build:\n      context: ~/.ssh\n",
            ),
            (
                "env_file",
                "services:\n  pwn:\n    image: alpine\n    env_file: ~/.env\n",
            ),
        ];

        for (expected_field, compose) in cases {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

        for path in ["~", "~/audit", "~other/.ssh", "~\\secrets"] {
            assert!(ComposeExecutor::is_dangerous_host_path(path));
        }
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
    fn test_validate_compose_security_policy_rejects_interpolated_build_security_options() {
        let Some(executor) = test_executor() else {
            return;
        };

        let cases = [
            (
                "build.privileged",
                "services:\n  app:\n    build:\n      context: .\n      privileged: ${BUILD_PRIVILEGED:-true}\n",
            ),
            (
                "build.network",
                "services:\n  app:\n    build:\n      context: .\n      network: ${BUILD_NETWORK:-host}\n",
            ),
            (
                "build.privileged",
                "services:\n  app:\n    build:\n      context: .\n      privileged: \"true\"\n",
            ),
            (
                "build.privileged",
                "services:\n  app:\n    build:\n      context: .\n      privileged: \"false\"\n",
            ),
        ];

        for (expected_field, compose) in cases {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

        let safe = "services:\n  app:\n    build:\n      context: .\n      privileged: false\n      network: default\n";
        assert!(executor
            .validate_compose_security_policy("compose file", safe)
            .is_ok());
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
        assert_eq!(violation_field(err), "volumes.hostroot.driver_opts");
    }

    #[test]
    fn test_validate_compose_security_policy_isolates_daemon_global_resources() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (expected_field, compose) in [
            (
                "volumes.data.external",
                "services:\n  app:\n    image: alpine\nvolumes:\n  data:\n    external: true\n",
            ),
            (
                "volumes.data.name",
                "services:\n  app:\n    image: alpine\nvolumes:\n  data:\n    name: another-project-data\n",
            ),
            (
                "networks.shared.external",
                "services:\n  app:\n    image: alpine\nnetworks:\n  shared:\n    external: \"true\"\n",
            ),
            (
                "networks.shared.name",
                "services:\n  app:\n    image: alpine\nnetworks:\n  shared:\n    name: another-project-network\n",
            ),
            (
                "networks.shared.ipam",
                "services:\n  app:\n    image: alpine\nnetworks:\n  shared:\n    ipam:\n      config:\n        - subnet: 172.16.0.0/12\n",
            ),
            (
                "configs.app_config.external",
                "services:\n  app:\n    image: alpine\nconfigs:\n  app_config:\n    external: true\n",
            ),
            (
                "secrets.api_key.name",
                "services:\n  app:\n    image: alpine\nsecrets:\n  api_key:\n    name: another-project-secret\n",
            ),
            (
                "external_links",
                "services:\n  app:\n    image: alpine\n    external_links:\n      - victim-db:db\n",
            ),
            (
                "label_file",
                "services:\n  app:\n    image: alpine\n    label_file: /etc/passwd\n",
            ),
            (
                "post_start",
                "services:\n  app:\n    image: alpine\n    post_start:\n      - command: id\n        privileged: true\n",
            ),
            (
                "provider",
                "services:\n  app:\n    provider:\n      type: attacker\n",
            ),
            (
                "container_name",
                "services:\n  app:\n    image: alpine\n    container_name: victim\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

        let safe = "services:\n  app:\n    image: alpine\nvolumes:\n  data:\n    external: false\nnetworks:\n  private:\n    driver: bridge\n    external: false\n";
        assert!(executor
            .validate_compose_security_policy("compose file", safe)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_rejects_unbounded_build_inputs() {
        let Some(executor) = test_executor() else {
            return;
        };
        for field in ["additional_contexts", "cache_from", "cache_to", "tags"] {
            let compose =
                format!("services:\n  app:\n    build:\n      context: .\n      {field}: []\n");
            let err = executor
                .validate_compose_security_policy("compose file", &compose)
                .unwrap_err();
            assert_eq!(violation_field(err), format!("build.{field}"));
        }
    }

    #[test]
    fn test_validate_compose_security_policy_limits_networking_and_replicas() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (expected_field, compose) in [
            (
                "network_mode",
                "services:\n  app:\n    image: alpine\n    network_mode: bridge\n",
            ),
            (
                "network_mode",
                "services:\n  app:\n    image: alpine\n    network_mode: service:missing\n",
            ),
            (
                "scale",
                "services:\n  app:\n    image: alpine\n    scale: 2\n",
            ),
            (
                "deploy.replicas",
                "services:\n  app:\n    image: alpine\n    deploy:\n      replicas: ${REPLICAS:-100}\n",
            ),
            (
                "deploy.mode",
                "services:\n  app:\n    image: alpine\n    deploy:\n      mode: global\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

        let safe = "services:\n  app:\n    image: alpine\n    network_mode: service:sidecar\n    scale: 1\n    deploy:\n      mode: replicated\n      replicas: 1\n  sidecar:\n    image: alpine\n    network_mode: none\n";
        assert!(executor
            .validate_compose_security_policy("compose file", safe)
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_isolates_image_selection() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (expected_field, compose) in [
            (
                "image",
                "services:\n  app:\n    image: temps.internal/victim:latest\n",
            ),
            (
                "image",
                "services:\n  app:\n    image: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "image",
                "services:\n  app:\n    image: alpine\n    build: .\n",
            ),
            (
                "pull_policy",
                "services:\n  app:\n    image: alpine\n    pull_policy: never\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }
        assert!(executor
            .validate_compose_security_policy(
                "compose file",
                "services:\n  app:\n    image: registry.example/app@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            )
            .is_ok());
    }

    #[test]
    fn test_validate_compose_security_policy_confines_env_files_and_ports() {
        let Some(executor) = test_executor() else {
            return;
        };
        for (expected_field, compose) in [
            (
                "env_file",
                "services:\n  app:\n    image: alpine\n    env_file: /etc/passwd\n",
            ),
            (
                "env_file",
                "services:\n  app:\n    image: alpine\n    env_file: ${HOST_ENV:-/etc/passwd}\n",
            ),
            (
                "ports",
                "services:\n  app:\n    image: alpine\n    ports: [\"8080:80\"]\n",
            ),
            (
                "ports",
                "services:\n  app:\n    image: alpine\n    ports:\n      - target: 80\n        published: 8080\n        host_ip: 0.0.0.0\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

        let safe = "services:\n  app:\n    image: alpine\n    env_file: ./app.env\n    ports:\n      - \"127.0.0.1:8080:80\"\n      - target: 443\n        published: 8443\n        host_ip: 127.0.0.1\n";
        assert!(executor
            .validate_compose_security_policy("compose file", safe)
            .is_ok());
    }

    #[test]
    fn test_security_override_enforces_resource_and_log_bounds() {
        let Some(executor) = test_executor() else {
            return;
        };
        let compose = "services:\n  image_app:\n    image: alpine\n  built_app:\n    build: .\n";
        let override_yaml = executor.generate_security_override(compose, &[], &[]);
        let parsed: YamlValue = serde_yaml::from_str(&override_yaml).unwrap();

        for service in ["image_app", "built_app"] {
            assert_eq!(parsed["services"][service]["cpus"].as_f64(), Some(4.0));
            assert_eq!(
                parsed["services"][service]["mem_limit"].as_str(),
                Some("4g")
            );
            assert_eq!(
                parsed["services"][service]["logging"]["driver"].as_str(),
                Some("json-file")
            );
            assert_eq!(
                parsed["services"][service]["logging"]["options"]["max-size"].as_str(),
                Some("50m")
            );
            assert_eq!(
                parsed["services"][service]["logging"]["options"]["max-file"].as_str(),
                Some("3")
            );
        }
        assert_eq!(
            parsed["services"]["image_app"]["pull_policy"].as_str(),
            Some("always")
        );
        assert!(parsed["services"]["built_app"]["pull_policy"].is_null());
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

        // Compose coerces this quoted string to a true boolean. Static policy
        // validation must therefore reject non-boolean values rather than
        // treating them as a harmless false value.
        let quoted_privileged = "services:\n  web:\n    image: alpine\n    privileged: \"true\"\n";
        let err = executor
            .validate_compose_security_policy("compose file", quoted_privileged)
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
        let benign = "services:\n  web:\n    image: alpine\n    deploy:\n      replicas: 1\n";
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
        assert_eq!(violation_field(err), "volumes.vol.driver_opts");

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

        let long_bind = "services:\n  app:\n    image: alpine\n    volumes:\n      - type: bind\n        source: escape\n        target: /host\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            long_bind,
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
                "volumes.type",
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
        assert!(error.to_string().contains("forbidden key 'runtime'"));
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
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("MOUNTED", "known-mounted-secret"),
        );
        request.environment_vars.insert(
            "ARBITRARY_NAME".to_string(),
            "known-environment-secret".to_string(),
        );
        request
            .build_args
            .insert("BUILD_VALUE".to_string(), "known-build-secret".to_string());
        request.env_content = Some("PRIVATE_IMAGE_TAG='known-dotenv-secret'\n".to_string());
        let diagnostic = "known-environment-secret known-build-secret known-mounted-secret \
            known-dotenv-secret \
            password=literal-password Authorization: Bearer abc.def.ghi \
            https://user:literal-uri-password@example.test/path";

        let sanitized =
            sanitize_compose_diagnostic(diagnostic, &collect_redactable_values(&request));

        for secret in [
            "known-environment-secret",
            "known-build-secret",
            "known-mounted-secret",
            "known-dotenv-secret",
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

        let sanitized = sanitize_compose_diagnostic(&diagnostic, &[]);

        assert!(sanitized.len() < diagnostic.len());
        assert!(sanitized.contains("diagnostic truncated"));
    }

    #[test]
    fn image_inspection_errors_redact_secrets_interpolated_into_image_tags() {
        let secret = "registry-token-that-must-not-leak";
        let diagnostic =
            format!("Docker responded with 404 for registry.example/app:{secret}: image not found");

        let sanitized = sanitize_compose_diagnostic(&diagnostic, &[secret.to_string()]);

        assert!(!sanitized.contains(secret));
        assert!(sanitized.contains("registry.example/app:<redacted>"));
    }

    /// `teardown_at` with `remove_secrets: false` must preserve the on-disk
    /// secrets directory even when the project stack directory does not exist
    /// (the early-return path is taken, which is what allows this test to run
    /// without Docker).
    #[tokio::test]
    async fn test_teardown_at_remove_secrets_false_preserves_secrets_dir() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            // Docker unavailable — we can still run this test because
            // teardown_at returns early when the project directory does not
            // exist, before it attempts any Docker call.
        }
        // Use a real temporary directory so the secrets-dir assertion is
        // meaningful (we are asserting on actual filesystem state).
        let tmp = tempfile::tempdir().expect("create temp dir");
        let data_dir = tmp.path().to_path_buf();

        // Build a minimal ComposeExecutor backed by a placeholder Docker
        // handle. teardown_at returns early before any Docker call when the
        // project directory does not exist, so the handle is never used.
        let docker_handle = match Docker::connect_with_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                // No Docker: synthesise a handle via connect_with_local_defaults
                // which also won't be called. If even that fails, skip.
                match Docker::connect_with_local_defaults() {
                    Ok(d) => Arc::new(d),
                    Err(_) => return,
                }
            }
        };
        let executor = ComposeExecutor::new(docker_handle, data_dir.clone());

        // Manually create the secrets directory that teardown_at would
        // otherwise delete, mirroring what materialize_secrets() produces.
        let project_name = "test-remove-secrets-false";
        let secrets_dir = data_dir.join("compose-secrets").join(project_name);
        std::fs::create_dir_all(&secrets_dir).expect("create secrets dir");
        std::fs::write(secrets_dir.join("MY_SECRET"), b"hunter2").expect("write dummy secret");

        // Project directory intentionally does not exist → teardown_at returns
        // early after (conditionally) running the secrets-dir deletion logic.
        executor
            .teardown_at(project_name, None, None, &HashMap::new(), false)
            .await
            .expect("teardown_at should succeed with no project dir");

        assert!(
            secrets_dir.exists(),
            "secrets dir must still exist after teardown_at with remove_secrets=false"
        );
    }

    /// `teardown_at` with `remove_secrets: true` must delete the on-disk
    /// secrets directory (same early-return path as above, no Docker needed).
    #[tokio::test]
    async fn test_teardown_at_remove_secrets_true_deletes_secrets_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let data_dir = tmp.path().to_path_buf();

        let docker_handle = match Docker::connect_with_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => match Docker::connect_with_local_defaults() {
                Ok(d) => Arc::new(d),
                Err(_) => return,
            },
        };
        let executor = ComposeExecutor::new(docker_handle, data_dir.clone());

        let project_name = "test-remove-secrets-true";
        let secrets_dir = data_dir.join("compose-secrets").join(project_name);
        std::fs::create_dir_all(&secrets_dir).expect("create secrets dir");
        std::fs::write(secrets_dir.join("MY_SECRET"), b"hunter2").expect("write dummy secret");

        executor
            .teardown_at(project_name, None, None, &HashMap::new(), true)
            .await
            .expect("teardown_at should succeed with no project dir");

        assert!(
            !secrets_dir.exists(),
            "secrets dir must be deleted after teardown_at with remove_secrets=true"
        );
    }
}
