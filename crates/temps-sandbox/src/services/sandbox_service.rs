// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core service for the standalone sandbox API.
//!
//! Responsibilities:
//! - Lifecycle (create/get/list/stop/extend_timeout) against the
//!   `sandboxes` DB table + the in-memory `StandaloneSandboxRegistry`.
//! - Ownership check (every operation validates `user_id` matches).
//! - Translating between the public opaque ID and the internal `i32`
//!   used by the underlying `SandboxProvider`.
//!
//! Exec/fs/domain methods live in sibling modules but are re-exported
//! here so handlers have a single service to call into.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};

use temps_agents::sandbox::SandboxCreateConfig;
use temps_agents::services::run_service::TERMINAL_RUN_STATUSES;
use temps_config::ConfigService;
use temps_entities::{
    agent_runs, ai_application_projects, ai_application_workspaces, ai_applications, environments,
    external_services, project_services, projects, sandbox_events, sandboxes, service_members,
};
use temps_git::GitProviderManager;

use crate::error::{from_agent_error, SandboxError};
use crate::services::exec::ExecOptions;
use crate::services::job_tracker::JobTracker;
use crate::services::preview_urls::{self, PreviewUrlParts};
use crate::services::public_id;
use crate::services::registry::StandaloneSandboxRegistry;
use crate::services::snapshot_service::SnapshotService;

/// Optional initial content to seed into the sandbox after create.
/// Mirrors `@vercel/sandbox`'s `source: { type, url, revision?, username?,
/// password?, depth? }` option, plus a temps-native `git_connection_id`
/// that resolves a stored provider token server-side.
#[derive(Debug, Clone)]
pub enum SandboxSource {
    /// Clone a git repository into the sandbox work dir.
    Git {
        url: String,
        /// Branch, tag, or commit SHA. `None` → default branch.
        revision: Option<String>,
        /// Shallow-clone depth. `None` → full history.
        depth: Option<u32>,
        /// HTTP Basic username for private repos. For GitHub tokens the
        /// conventional value is `"x-access-token"`.
        username: Option<String>,
        /// HTTP Basic password / token. Paired with `username`. The token
        /// is injected via `GIT_ASKPASS` — never in the URL or argv.
        password: Option<String>,
        /// Reference to a stored git provider connection. When set, temps
        /// resolves the token server-side via `GitProviderManager` and
        /// injects it as `username="x-access-token" + password=<token>`.
        /// Mutually exclusive with `username`/`password`.
        git_connection_id: Option<i32>,
        /// Optional path relative to the sandbox work directory. This lets a
        /// persistent sandbox host multiple projects without granting callers
        /// an arbitrary container filesystem write primitive.
        destination: Option<String>,
        /// Remove the imported repository's `.git` directory after a
        /// successful clone. AI workspaces use one outer repository and must
        /// not retain nested Git metadata.
        strip_git_metadata: bool,
    },
    /// Download a tarball (tar, tar.gz, tgz) from `url` and extract it
    /// into the sandbox work dir. The file at `url` must be reachable
    /// from inside the container (public URL, or the container network
    /// can reach it).
    Tarball { url: String },
}

/// Lifecycle class of a sandbox (ADR-036).
///
/// All sandbox compute may suspend on idle. A workspace is durable because its
/// bind-mounted files and home volume never expire and are resumed or rebuilt
/// automatically on the next access; it does not keep idle compute running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxLifecycle {
    /// Throwaway sandbox. Default for every caller that doesn't ask.
    #[default]
    Ephemeral,
    /// Long-lived files: compute may suspend, but is never auto-destroyed.
    Workspace,
}

impl SandboxLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxLifecycle::Ephemeral => "ephemeral",
            SandboxLifecycle::Workspace => "workspace",
        }
    }

    /// Parse the API-facing string. Unknown values are a validation
    /// error rather than a silent fallback: a caller who typo'd
    /// `"workspaces"` and got an ephemeral sandbox would lose their work
    /// to the first idle sweep without ever being told.
    pub fn parse(value: &str) -> Result<Self, SandboxError> {
        match value {
            "ephemeral" => Ok(SandboxLifecycle::Ephemeral),
            "workspace" => Ok(SandboxLifecycle::Workspace),
            other => Err(SandboxError::Validation {
                message: format!(
                    "unknown lifecycle '{}' (expected \"ephemeral\" or \"workspace\")",
                    other
                ),
            }),
        }
    }

    /// Read the class off a persisted row. Unlike [`Self::parse`] this
    /// cannot fail: a row carrying a value we don't recognise (written
    /// by a newer binary, then rolled back) is treated as ephemeral,
    /// which is the conservative reading — it never auto-starts a
    /// container the caller didn't ask for.
    pub fn from_row(row: &sandboxes::Model) -> Self {
        match row.lifecycle.as_str() {
            "workspace" => SandboxLifecycle::Workspace,
            _ => SandboxLifecycle::Ephemeral,
        }
    }
}

impl std::fmt::Display for SandboxLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Input to `create_sandbox`. A subset of the `@vercel/sandbox` create
/// options — we accept what SDK clients send and ignore the rest.
#[derive(Debug, Clone, Default)]
pub struct CreateSandboxRequest {
    /// Optional Docker image override. `None` → platform default.
    pub image: Option<String>,
    /// Optional human-readable name. Defaults to the internal ID.
    pub name: Option<String>,
    /// Idle timeout in seconds. Clamped to `[60, 86400]`.
    pub timeout_secs: Option<u64>,
    /// Environment variables to bake into the container at startup.
    pub env: HashMap<String, String>,
    /// Resource limits (null → provider defaults).
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<u64>,
    pub pids_limit: Option<i64>,
    /// Root disk size in MB. Only honored by the Firecracker backend
    /// (Docker ignores it). `None` uses the provider default (1 GiB).
    pub disk_size_mb: Option<u64>,
    /// Optional initial content to seed into the work dir.
    pub source: Option<SandboxSource>,
    /// Optional preview-URL password applied atomically at create. Same
    /// validation rules as `set_preview_password` (8–256 chars, argon2-
    /// hashed server-side). `None` leaves preview URLs open (public once
    /// the sandbox ID is known). The plaintext is never returned — only
    /// the last-4 hint round-trips in `SandboxSummary`.
    pub preview_password: Option<String>,
    /// Ports the sandbox is expected to listen on. Persisted to the
    /// `metadata` JSON column and surfaced as `routes[]` in the SDK-
    /// shaped responses.
    pub ports: Vec<u16>,
    /// Isolation backend: "docker" (default) or "firecracker". `None` →
    /// the host's configured default (docker unless the operator changed
    /// it). Unknown values are a validation error, and requesting a
    /// backend the host doesn't have fails create rather than downgrading.
    pub backend: Option<String>,
    /// Lifecycle class (ADR-036). `None` → ephemeral, so existing
    /// callers and SDK clients are unaffected.
    pub lifecycle: Option<String>,
    /// Project to derive the repo from when `source` is absent, and to
    /// attribute the sandbox to. `None` → unattached sandbox.
    pub project_id: Option<i32>,
    /// Pre-resolved snapshot artifact (ADR-037). When `Some`, the sandbox
    /// is created via `provider.create_from_snapshot` instead of
    /// `provider.create`. Mutually exclusive with `image`; validated in the
    /// handler before this field is populated.
    pub from_snapshot_artifact: Option<temps_agents::sandbox::SnapshotArtifact>,
    /// Trusted host work directory for an internal caller. HTTP handlers
    /// always leave this unset; it exists so the AI application workspace can
    /// remain in its user-owned `<TEMPS_DATA_DIR>/ai-applications` tree while
    /// still receiving a first-class sandbox row and preview identity.
    pub host_work_dir_override: Option<PathBuf>,
}

/// First-class sandbox identity for a durable AI application workspace.
/// `public_id` is safe to use with the preview gateway; the host path never
/// crosses an HTTP boundary.
#[derive(Debug, Clone)]
pub struct ApplicationWorkspaceSandbox {
    pub public_id: String,
}

#[derive(Debug, Clone)]
pub struct ApplicationWorkspaceConfig {
    pub desired_state: String,
    pub image: Option<String>,
    pub cpu_limit: f64,
    pub memory_limit_mb: u64,
    pub pids_limit: i64,
    pub disk_limit_mb: u64,
    pub idle_timeout_secs: u64,
}

impl Default for ApplicationWorkspaceConfig {
    fn default() -> Self {
        Self {
            desired_state: "running".to_string(),
            // Persist and attest the effective default just like an explicit
            // runtime selection. Leaving this as `None` made creation store
            // the provider-resolved Node image, then made the very next lookup
            // treat that healthy row as configuration-mismatched and rebuild
            // the shared workspace container on every new chat.
            image: Some(temps_agents::sandbox::docker::image_name_for_runtime(
                "node",
            )),
            cpu_limit: 4.0,
            memory_limit_mb: 8192,
            pids_limit: 512,
            disk_limit_mb: 10_240,
            idle_timeout_secs: 900,
        }
    }
}

impl From<&ai_application_workspaces::Model> for ApplicationWorkspaceConfig {
    fn from(value: &ai_application_workspaces::Model) -> Self {
        Self {
            desired_state: value.desired_state.clone(),
            image: value.image.clone().or_else(|| {
                (value.runtime != "custom")
                    .then(|| temps_agents::sandbox::docker::image_name_for_runtime(&value.runtime))
            }),
            cpu_limit: value.cpu_limit,
            memory_limit_mb: value.memory_limit_mb.max(0) as u64,
            pids_limit: value.pids_limit,
            disk_limit_mb: value.disk_limit_mb.max(0) as u64,
            idle_timeout_secs: value.idle_timeout_secs.max(0) as u64,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApplicationWorkspaceUsage {
    pub memory_used_bytes: Option<u64>,
    pub pids_used: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
    pub open_ports: Vec<u16>,
}

const APPLICATION_WORKSPACE_NAME_PREFIX: &str = "ai-application:";
const SOURCE_IMPORT_TIMEOUT: Duration = Duration::from_secs(120);
const SOURCE_IMPORT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(130);
const SOURCE_IMPORT_MAX_FILES: usize = 5_000;
const SOURCE_IMPORT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SOURCE_IMPORT_SYSTEM_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn managed_application_id_from_name(name: &str) -> Option<&str> {
    name.strip_prefix(APPLICATION_WORKSPACE_NAME_PREFIX)
        .filter(|application_id| !application_id.is_empty())
}

fn application_workspace_row_is_attested(
    row: &sandboxes::Model,
    application_public_id: &str,
    host_work_dir: &Path,
    config: &ApplicationWorkspaceConfig,
) -> bool {
    let metadata = row.metadata.as_ref().and_then(serde_json::Value::as_object);
    let attested_application = metadata
        .and_then(|value| value.get("managed_application_id"))
        .and_then(serde_json::Value::as_str);
    let attested_work_dir = metadata
        .and_then(|value| value.get("managed_host_work_dir"))
        .and_then(serde_json::Value::as_str);
    attested_application == Some(application_public_id)
        && attested_work_dir == host_work_dir.to_str()
        && row.lifecycle == "workspace"
        && row.work_dir == temps_agents::sandbox::SANDBOX_WORK_DIR
        && row.image == config.image
}

const APPLICATION_WORKSPACE_USAGE_COMMAND: &str = "memory=$(cat /sys/fs/cgroup/memory.current 2>/dev/null || true); pids=$(cat /sys/fs/cgroup/pids.current 2>/dev/null || true); cpu=$(sed -n 's/^usage_usec //p' /sys/fs/cgroup/cpu.stat 2>/dev/null || true); disk=$(du -sb /home/temps/workspace 2>/dev/null | awk 'NR==1 {print $1}'); ports=$(if command -v ss >/dev/null 2>&1; then ss -ltnH 2>/dev/null | awk '{print $4}' | sed 's/.*://'; else for socket_table in /proc/net/tcp /proc/net/tcp6; do test -r \"$socket_table\" || continue; awk 'NR > 1 && $4 == \"0A\" {split($2, address, \":\"); print address[2]}' \"$socket_table\"; done | while IFS= read -r hex_port; do printf '%d\\n' \"0x$hex_port\"; done; fi | sort -nu | paste -sd, -); printf 'memory=%s\\n' \"$memory\"; printf 'pids=%s\\n' \"$pids\"; printf 'cpu=%s\\n' \"$cpu\"; printf 'disk=%s\\n' \"$disk\"; printf 'ports=%s\\n' \"$ports\"";

fn parse_application_workspace_usage(stdout: &str) -> ApplicationWorkspaceUsage {
    let mut usage = ApplicationWorkspaceUsage::default();
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("memory=") {
            usage.memory_used_bytes = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("pids=") {
            usage.pids_used = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("cpu=") {
            usage.cpu_usage_usec = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("disk=") {
            usage.disk_used_bytes = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("ports=") {
            usage.open_ports = value
                .split(',')
                .filter_map(|port| port.trim().parse::<u16>().ok())
                .collect();
        }
    }
    usage
}

/// Output DTO — what the service returns to handlers and what handlers
/// serialize into the response JSON. Wraps the DB model to keep internal
/// columns out of the public surface.
#[derive(Debug, Clone)]
pub struct SandboxSummary {
    pub public_id: String,
    pub name: String,
    pub status: String,
    pub image: Option<String>,
    pub work_dir: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Present iff a preview password is configured. The hint is the
    /// last 4 chars of the plaintext — safe to display in the UI so
    /// users can tell two passwords apart.
    pub preview_password_hint: Option<String>,
    /// Ports the sandbox advertises (read from the `metadata` JSON
    /// column). Empty when the sandbox was created without declaring
    /// any ports.
    pub ports: Vec<u16>,
    /// Isolation backend the sandbox runs on ("docker" | "firecracker").
    /// `None` on rows created before the column existed.
    pub backend: Option<String>,
    /// Configured root disk size in MB (from metadata). `None` = default.
    pub disk_size_mb: Option<u64>,
    /// Set when this sandbox executes an agent run (autofixer / workflow
    /// agent). `None` for sandboxes created via the standalone API.
    pub agent_run_id: Option<i32>,
    /// Lifecycle class: `"ephemeral"` or `"workspace"` (ADR-036).
    pub lifecycle: String,
    /// Project this sandbox belongs to, when created from one.
    pub project_id: Option<i32>,
    /// Repo the work dir was seeded from, for display.
    pub source_repo_url: Option<String>,
}

impl From<&sandboxes::Model> for SandboxSummary {
    fn from(m: &sandboxes::Model) -> Self {
        Self {
            public_id: m.public_id.clone(),
            name: m.name.clone(),
            status: m.status.clone(),
            image: m.image.clone(),
            work_dir: m.work_dir.clone(),
            created_at: m.created_at,
            expires_at: m.expires_at,
            preview_password_hint: m.preview_password_hint.clone(),
            ports: ports_from_metadata(m.metadata.as_ref()),
            backend: m.backend.clone(),
            disk_size_mb: m
                .metadata
                .as_ref()
                .and_then(|v| v.get("disk_size_mb"))
                .and_then(|v| v.as_u64()),
            agent_run_id: m.agent_run_id,
            lifecycle: m.lifecycle.clone(),
            project_id: m.project_id,
            source_repo_url: m.source_repo_url.clone(),
        }
    }
}

fn source_kind(source: &SandboxSource) -> &'static str {
    match source {
        SandboxSource::Git { .. } => "git",
        SandboxSource::Tarball { .. } => "tarball",
    }
}

/// Validate an optional source destination beneath the sandbox work dir.
///
/// This validation lives in the service layer as well as the HTTP DTO path
/// because project-derived and future internal callers can bypass handlers.
pub(crate) fn validate_source_destination(destination: Option<&str>) -> Result<(), SandboxError> {
    let Some(destination) = destination else {
        return Ok(());
    };
    if destination.is_empty() || destination.len() > 512 {
        return Err(SandboxError::Validation {
            message: "source destination must contain 1 to 512 characters".into(),
        });
    }
    let path = Path::new(destination);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SandboxError::Validation {
            message:
                "source destination must be a relative path without '.', '..', or platform prefixes"
                    .into(),
        });
    }
    Ok(())
}

fn bounded_command_error(stderr: &str, stdout: &str) -> String {
    const MAX_CHARS: usize = 1_000;
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        return "command exited without diagnostic output".to_string();
    }
    detail.chars().take(MAX_CHARS).collect()
}

/// Extract the `ports` array from a sandbox's `metadata` JSON blob.
/// Tolerates missing/malformed data — a sandbox created before ports
/// were tracked simply returns `[]`, and a value we can't parse is
/// dropped silently rather than erroring out list reads.
fn ports_from_metadata(metadata: Option<&serde_json::Value>) -> Vec<u16> {
    metadata
        .and_then(|v| v.get("ports"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_u64())
                .filter_map(|p| u16::try_from(p).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The idle deadline for a sandbox: the instant it becomes eligible for
/// suspension if nothing touches it in the meantime.
///
/// One helper rather than five open-coded `now + Duration::seconds(...)`
/// sites, because every path that records activity — create, touch,
/// resume, wake — must agree on it. When they disagreed, `expires_at`
/// meant "wall-clock deadline" on some paths and "idle deadline" on
/// others, which is the bug ADR-036 §3 describes.
fn idle_deadline(
    now: chrono::DateTime<chrono::Utc>,
    timeout_secs: i32,
) -> chrono::DateTime<chrono::Utc> {
    now + chrono::Duration::seconds(timeout_secs as i64)
}

/// Security validation for a seed source *after* it has been resolved.
///
/// The handler validates the caller-supplied `source` DTO, which covers the
/// request-shape rules it owns (mutually exclusive auth fields, and so on).
/// It cannot cover a source this layer derives from a project row, so the
/// two checks that actually matter for safety are repeated here on whatever
/// we finally decided to clone:
///
/// - **SSRF**, including DNS resolution. `validate_external_url` rejects
///   hosts that resolve to private, loopback, link-local or cloud-metadata
///   addresses — the project-side URL guard only checks IP *literals*, so a
///   name like `metadata.google.internal` would otherwise pass.
/// - **Embedded credentials.** A `user:password@` in the URL would be
///   persisted to `sandboxes.source_repo_url` and echoed back out of the
///   API. Rows predating the project-side guard can still carry one.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceNetworkPin {
    host: String,
    port: u16,
    address: std::net::IpAddr,
}

impl SourceNetworkPin {
    fn curl_resolve_value(&self) -> String {
        match self.address {
            std::net::IpAddr::V4(address) => {
                format!("{}:{}:{}", self.host, self.port, address)
            }
            std::net::IpAddr::V6(address) => {
                format!("{}:{}:[{}]", self.host, self.port, address)
            }
        }
    }
}

fn tarball_extract_command(
    url: &str,
    staging_dir: &str,
    network_pin: Option<&SourceNetworkPin>,
) -> String {
    let resolve = network_pin
        .map(|pin| {
            format!(
                " --resolve {}",
                shell_escape_service(&pin.curl_resolve_value())
            )
        })
        .unwrap_or_default();
    let network_environment = format!(
        "env -i PATH={} HOME=/var/empty/temps-source-import CURL_HOME=/var/empty/temps-source-import XDG_CONFIG_HOME=/var/empty/temps-source-import NO_PROXY='*'",
        shell_escape_service(SOURCE_IMPORT_SYSTEM_PATH),
    );
    format!(
        "{network_environment} curl -q --noproxy '*' --proxy '' -fsS --max-redirs 0{resolve} {url} | {network_environment} tar --no-same-owner --no-same-permissions -C {stage} -xzf -",
        url = shell_escape_service(url),
        stage = shell_escape_service(staging_dir),
    )
}

fn clean_git_environment(auth_dir: &str, askpass_path: Option<&str>) -> String {
    let mut environment = format!(
        "env -i PATH={} HOME={} GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_COUNT=0 GIT_SSL_NO_VERIFY=false GIT_TERMINAL_PROMPT=0",
        shell_escape_service(SOURCE_IMPORT_SYSTEM_PATH),
        shell_escape_service(auth_dir),
    );
    if let Some(askpass_path) = askpass_path {
        environment.push_str(&format!(
            " GIT_ASKPASS={}",
            shell_escape_service(askpass_path)
        ));
    }
    environment
}

async fn validate_resolved_source(
    source: &SandboxSource,
) -> Result<Option<SourceNetworkPin>, SandboxError> {
    let (url, kind) = match source {
        SandboxSource::Git { url, .. } => (url.as_str(), "git"),
        SandboxSource::Tarball { url } => (url.as_str(), "tarball"),
    };

    if let SandboxSource::Git {
        depth,
        destination,
        strip_git_metadata,
        ..
    } = source
    {
        if depth.is_some_and(|value| value == 0 || value > 1_000) {
            return Err(SandboxError::Validation {
                message: "git clone depth must be between 1 and 1000".into(),
            });
        }
        validate_source_destination(destination.as_deref())?;
        if *strip_git_metadata && destination.is_none() {
            return Err(SandboxError::Validation {
                message: "strip_git_metadata requires a destination so the sandbox root repository cannot be removed"
                    .into(),
            });
        }
    }

    if url_has_embedded_credentials(url) {
        return Err(SandboxError::Validation {
            message: format!(
                "{kind} source: url must not contain embedded credentials — \
                 use a stored git connection instead"
            ),
        });
    }

    // Sync pass first: scheme + IP-literal blocklist. Cheap, and it avoids a
    // DNS lookup for URLs that are already disqualified.
    let parsed = temps_core::url_validation::validate_external_url(url).map_err(|_| {
        SandboxError::Validation {
            message: format!(
                "{kind} source: scheme must be http or https, and the host must \
                 not be a private, loopback, or metadata address"
            ),
        }
    })?;

    // Then resolve: an IP literal is already covered above, but a *name*
    // pointing at 169.254.169.254 is only caught here.
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        let port = parsed.port_or_known_default().unwrap_or(443);
        let addresses = temps_core::url_validation::resolve_and_validate_domain(domain, port)
            .await
            .map_err(|_| SandboxError::Validation {
                message: format!(
                    "{kind} source: host resolves to a private, loopback, or \
                     metadata address"
                ),
            })?;
        let address = addresses
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| addresses.first())
            .map(std::net::SocketAddr::ip)
            .ok_or_else(|| SandboxError::Validation {
                message: format!("{kind} source: host did not resolve to a usable address"),
            })?;
        return Ok(Some(SourceNetworkPin {
            host: domain.to_string(),
            port,
            address,
        }));
    }
    Ok(None)
}

/// Does the URL carry any userinfo before the host?
///
/// Even a bare `user@` may be a token or sensitive account identifier. Clone
/// credentials must travel only through the dedicated ephemeral credential
/// channel, never through argv, process inspection, errors, or `.git/config`.
pub(crate) fn url_has_embedded_credentials(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| !parsed.username().is_empty() || parsed.password().is_some())
        .unwrap_or(false)
}

/// A credential passed to a process inside a custom image is already disclosed
/// to that image: it controls `/bin/sh`, `git`, dynamic-loader configuration,
/// and the network stack. Until credentialed clones move to a host-owned
/// staging helper, allow them only in the versioned images built by Temps.
fn sandbox_image_is_trusted_for_credentials(handle: &temps_agents::sandbox::SandboxHandle) -> bool {
    handle.backend == temps_agents::sandbox::SandboxBackend::Docker
        && handle.image.starts_with("ghcr.io/gotempsh/temps-sandbox-")
}

/// Docker exec inherits the image's configured environment. A custom image
/// could otherwise preload code or ask the shell to source a workspace file
/// before a root-owned import command begins. Explicitly neutralize the
/// interpreter, loader, Git, and language startup hooks relevant to the fixed
/// import toolchain.
fn sanitize_privileged_import_environment(environment: &mut HashMap<String, String>) {
    for name in [
        "BASH_ENV",
        "ENV",
        "GIT_EXEC_PATH",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "NODE_OPTIONS",
        "PERL5LIB",
        "PYTHONHOME",
        "PYTHONPATH",
        "RUBYLIB",
    ] {
        environment.insert(name.to_string(), String::new());
    }
    for name in [
        "ALL_PROXY",
        "FTP_PROXY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "all_proxy",
        "ftp_proxy",
        "http_proxy",
        "https_proxy",
        "CURL_CA_BUNDLE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_PROXY_COMMAND",
        "GIT_SSL_CAINFO",
        "GIT_SSL_CAPATH",
        "SSL_CERT_DIR",
        "SSL_CERT_FILE",
    ] {
        environment.insert(name.to_string(), String::new());
    }
    environment.insert("NO_PROXY".to_string(), "*".to_string());
    environment.insert("no_proxy".to_string(), "*".to_string());
    environment.insert("GIT_CONFIG_COUNT".to_string(), "0".to_string());
    environment.insert("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string());
    environment.insert("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string());
    environment.insert("GIT_CONFIG_SYSTEM".to_string(), "/dev/null".to_string());
    environment.insert("GIT_SSL_NO_VERIFY".to_string(), "false".to_string());
    // Root-owned import helpers must not inherit image-provided user config.
    // `curl -q` remains the primary defense because a custom image can ship
    // files at any path, while these values also keep other tooling away from
    // the sandbox user's writable home directory.
    for name in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
        environment.insert(
            name.to_string(),
            "/var/empty/temps-source-import".to_string(),
        );
    }
}

fn default_git_provider_base_url(provider_type: &str) -> Option<&'static str> {
    match provider_type.to_ascii_lowercase().as_str() {
        "github" => Some("https://github.com"),
        "gitlab" => Some("https://gitlab.com"),
        "bitbucket" => Some("https://bitbucket.org"),
        _ => None,
    }
}

fn validate_connection_clone_origin(
    clone_url: &str,
    provider: &temps_entities::git_providers::Model,
) -> Result<(), SandboxError> {
    let provider_url = provider
        .base_url
        .as_deref()
        .or_else(|| default_git_provider_base_url(&provider.provider_type))
        .ok_or_else(|| SandboxError::Validation {
            message: format!(
                "Git provider {} has no clone origin configured",
                provider.id
            ),
        })?;
    let clone = url::Url::parse(clone_url).map_err(|error| SandboxError::Validation {
        message: format!("git source: invalid clone URL: {error}"),
    })?;
    let allowed = url::Url::parse(provider_url).map_err(|error| SandboxError::Validation {
        message: format!(
            "Git provider {} has an invalid base URL: {error}",
            provider.id
        ),
    })?;
    let same_origin = clone.scheme() == allowed.scheme()
        && clone
            .host_str()
            .zip(allowed.host_str())
            .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        && clone.port_or_known_default() == allowed.port_or_known_default();
    if !same_origin {
        return Err(SandboxError::Validation {
            message: format!(
                "Git connection {} can only authenticate clones from {}",
                provider.id,
                allowed.origin().ascii_serialization()
            ),
        });
    }
    Ok(())
}

/// Reject standalone lifecycle ops (`pause`/`resume`/`restart`/`resize`)
/// on rows attributed to an agent run. Those containers are named
/// `temps-sandbox-<run_id>` — not by the row's public id — so the
/// standalone registry's name-based recovery would miss them and the DB
/// row would drift from the real container state. The run owns the
/// lifecycle; the user must act on the run instead.
fn ensure_not_agent_run(row: &sandboxes::Model) -> Result<(), SandboxError> {
    if let Some(run_id) = row.agent_run_id {
        return Err(SandboxError::ManagedByAgentRun {
            sandbox_id: row.public_id.clone(),
            run_id,
        });
    }
    Ok(())
}

/// Is the owning agent run finished (or gone)? `None` means the run row
/// no longer exists — nothing owns the sandbox anymore, so treat it as
/// terminal and let cleanup proceed. Shared by `destroy_sandbox`'s
/// agent-run branch and `release_orphaned_agent_run_sandboxes`.
fn run_status_is_terminal(status: Option<&str>) -> bool {
    status
        .map(|s| TERMINAL_RUN_STATUSES.contains(&s))
        .unwrap_or(true)
}

/// The container-name suffix for a standalone sandbox: the `public_id` with
/// its `sbx_` prefix stripped.
///
/// The same label the preview hostname embeds, so the gateway can
/// DNS-resolve `temps-sandbox-<label>` straight from the URL — and, because
/// the Docker provider derives the home volume name from the container
/// name, the thing that decides which volume this sandbox owns. Extracted
/// so `sandbox_service_and_provider_agree_on_volume_naming` can pin the
/// cross-crate half of that agreement; the original leak was exactly two
/// places deriving one identity independently.
fn container_label_for(public_id_value: &str) -> &str {
    public_id_value
        .strip_prefix(public_id::PUBLIC_ID_PREFIX)
        .unwrap_or(public_id_value)
}

/// Which host directory (if any) `destroy_sandbox` may recursively delete
/// for a given public id.
///
/// Split out from `remove_work_dir` so the guard is unit-testable without
/// standing up a whole service. `data_root.join(s)` silently resolves
/// absolute paths and `..` segments, so a public id of `../../etc` would
/// aim a recursive delete outside the data dir entirely. Ids are generated
/// internally today, but "the caller is trusted" is not something a
/// `remove_dir_all` should rely on — accept only the exact `sbx_<16 hex>`
/// shape `public_id::generate` produces.
fn work_dir_to_remove(data_root: &Path, public_id_value: &str) -> Option<PathBuf> {
    if !public_id::is_valid(public_id_value) {
        return None;
    }
    Some(data_root.join(public_id_value))
}

/// Bounds on `timeout_secs` at the service layer. The upper bound
/// protects against "sandbox leaks" where a caller creates sandboxes
/// with absurd timeouts and relies on the server never cleaning up.
/// How long a wake-on-access container start may take before the request
/// gives up. A wake blocks a user's exec/fs call, so it needs a ceiling: a
/// wedged container runtime must surface as a clear error, not an
/// indefinitely hung request.
const WAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

const MIN_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60; // 24 hours
const DEFAULT_TIMEOUT_SECS: u64 = 60 * 60; // 1 hour

// Application workspaces are repositories, not disposable command scratch
// space. This bootstrap runs *inside* the sandbox after create/recovery, so
// both repository metadata and commits live on the same durable bind mount as
// the files the development harness edits.
const APPLICATION_GIT_BOOTSTRAP: &str = r#"set -eu
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git init --initial-branch=main . >/dev/null
fi
git config --local --get user.name >/dev/null 2>&1 || git config --local user.name 'Temps Workspace'
git config --local --get user.email >/dev/null 2>&1 || git config --local user.email 'workspace@temps.sh'
git config --local --get commit.gpgSign >/dev/null 2>&1 || git config --local commit.gpgSign false
"#;

pub struct SandboxService {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
    jobs: Arc<JobTracker>,
    platform_config: Arc<ConfigService>,
    cookie_crypto: Arc<temps_core::CookieCrypto>,
    /// Resolves stored provider connections to access tokens so callers
    /// can clone private repos by `git_connection_id` without handing us
    /// raw credentials. Required — the git plugin registers it, and the
    /// sandbox plugin fails to start if it's absent (`require_service`).
    git_provider_manager: Arc<GitProviderManager>,
    /// Root on the host where per-sandbox working directories are
    /// allocated. Each sandbox gets `{data_dir}/{public_id}/` bind-mounted
    /// to `/workspace` inside the container.
    data_root: PathBuf,
    /// Snapshot service for nullifying `source_sandbox_id` references when
    /// a sandbox is destroyed (ADR-037). `None` when snapshots are not
    /// enabled for this deployment (e.g. Firecracker-only).
    snapshot_service: Option<Arc<SnapshotService>>,
    /// Serializes one-to-one application workspace realization. Concurrent
    /// preview, diff and chat requests must never create two containers over
    /// the same persistent bind mount.
    application_workspace_lock: Arc<tokio::sync::Mutex<()>>,
    /// One source import per process prevents two requests targeting the same
    /// sandbox from sharing its root-owned staging boundary.
    source_import_lock: Arc<tokio::sync::Mutex<()>>,
    /// Issues project/environment-scoped database variables without giving a
    /// sandbox a reusable Temps API token. Values are held only for the caller
    /// that launches the sandbox process and are never persisted here.
    runtime_credentials: Option<Arc<dyn temps_core::SandboxRuntimeCredentialsProvider>>,
}

impl SandboxService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        registry: Arc<StandaloneSandboxRegistry>,
        jobs: Arc<JobTracker>,
        platform_config: Arc<ConfigService>,
        cookie_crypto: Arc<temps_core::CookieCrypto>,
        git_provider_manager: Arc<GitProviderManager>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            db,
            registry,
            jobs,
            platform_config,
            cookie_crypto,
            git_provider_manager,
            data_root,
            snapshot_service: None,
            application_workspace_lock: Arc::new(tokio::sync::Mutex::new(())),
            source_import_lock: Arc::new(tokio::sync::Mutex::new(())),
            runtime_credentials: None,
        }
    }

    pub fn with_runtime_credentials(
        mut self,
        provider: Arc<dyn temps_core::SandboxRuntimeCredentialsProvider>,
    ) -> Self {
        self.runtime_credentials = Some(provider);
        self
    }

    /// Set the snapshot service after construction (two-phase init).
    /// Called by the plugin once both services are registered (ADR-037).
    pub fn with_snapshot_service(mut self, svc: Arc<SnapshotService>) -> Self {
        self.snapshot_service = Some(svc);
        self
    }

    pub fn registry(&self) -> &StandaloneSandboxRegistry {
        self.registry.as_ref()
    }

    pub fn jobs(&self) -> &JobTracker {
        self.jobs.as_ref()
    }

    /// Compute preview URL parts once per call. Platform settings can
    /// change while the server is live (admin edits), so we don't cache.
    pub async fn preview_parts(&self) -> PreviewUrlParts {
        preview_urls::load(&self.platform_config).await
    }

    // ── Lookups ──────────────────────────────────────────────────────────

    /// Load a sandbox row by public ID, enforcing ownership. The typical
    /// entrypoint for every op that takes an ID from the URL.
    pub async fn find_by_public_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        if !public_id::is_valid(public_id_value) {
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            })?;
        if row.user_id != Some(user_id) {
            // Don't leak existence to non-owners.
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        if row.status == "destroyed" {
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        Ok(row)
    }

    /// Application workspaces are managed exclusively through the application
    /// API, where every linked project is re-authorized. The generic sandbox
    /// HTTP surface uses this discriminator to hide them completely.
    pub async fn is_application_workspace(
        &self,
        public_id_value: &str,
    ) -> Result<bool, SandboxError> {
        if !public_id::is_valid(public_id_value) {
            return Ok(false);
        }
        Ok(sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(sandboxes::Column::Name.starts_with("ai-application:"))
            .count(self.db.as_ref())
            .await?
            > 0)
    }

    /// List all of the caller's non-destroyed sandboxes, newest first.
    /// Creation source is presentation metadata, never a capability boundary.
    ///
    /// `lifecycle` and `project_id` are optional filters, both backed by
    /// partial indexes from the ADR-036 migration — but only for the
    /// selective side. The lifecycle index is `WHERE lifecycle <> 'ephemeral'`,
    /// so `?lifecycle=workspace` avoids the ephemeral rows (the overwhelming
    /// majority) while `?lifecycle=ephemeral` — an equally valid call — gets
    /// no index and falls back to the `user_id` scan, which is the right
    /// trade: one user's sandboxes are few, and indexing the common value
    /// would cost writes on every row for no gain.
    pub async fn list_for_user(
        &self,
        user_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
        lifecycle: Option<SandboxLifecycle>,
        project_id: Option<i32>,
    ) -> Result<(Vec<SandboxSummary>, u64), SandboxError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let mut query = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(user_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .filter(
                sandboxes::Column::Name.not_like(format!("{APPLICATION_WORKSPACE_NAME_PREFIX}%")),
            );
        if let Some(lifecycle) = lifecycle {
            query = query.filter(sandboxes::Column::Lifecycle.eq(lifecycle.as_str()));
        }
        if let Some(project_id) = project_id {
            query = query.filter(sandboxes::Column::ProjectId.eq(project_id));
        }
        let paginator = query
            .order_by_desc(sandboxes::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;
        let items = rows.iter().map(SandboxSummary::from).collect();
        Ok((items, total))
    }

    // ── Agent-run sandboxes ──────────────────────────────────────────────

    /// Release every non-destroyed sandbox row attributed to an agent run
    /// that is already terminal (or whose run row no longer exists).
    ///
    /// Called by the plugin on startup, *after* the agents plugin's
    /// `AgentRunService::recover_stuck_runs` (register phase) has failed
    /// every run that was in flight when the server died. Without this,
    /// those runs' `sandboxes` rows stay "running" forever: the standalone
    /// recovery scan and the expiration sweeper both skip agent-run rows
    /// by design, and the run itself will never call `release` again —
    /// zombie row, possibly a leaked container.
    ///
    /// Per-run failures are logged and skipped so one bad row can't block
    /// cleanup of the rest. Returns the number of runs whose sandboxes
    /// were released.
    pub async fn release_orphaned_agent_run_sandboxes(&self) -> Result<usize, SandboxError> {
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.is_not_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        if rows.is_empty() {
            return Ok(0);
        }

        // One lookup for all runs (no per-row query), deduped: a run can
        // in principle have several stale rows, and `release_for_agent_run`
        // already sweeps every row for its run.
        let mut run_ids: Vec<i32> = rows.iter().filter_map(|r| r.agent_run_id).collect();
        run_ids.sort_unstable();
        run_ids.dedup();
        let runs = agent_runs::Entity::find()
            .filter(agent_runs::Column::Id.is_in(run_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let status_by_run: HashMap<i32, String> =
            runs.into_iter().map(|r| (r.id, r.status)).collect();

        let mut released = 0usize;
        for run_id in run_ids {
            let terminal = run_status_is_terminal(status_by_run.get(&run_id).map(|s| s.as_str()));
            if !terminal {
                // Legitimately active run (e.g. re-triggered right after
                // restart) — its sandbox is alive and owned; leave it.
                continue;
            }
            match self.release_for_agent_run(run_id, None).await {
                Ok(()) => released += 1,
                Err(e) => {
                    tracing::warn!(
                        "orphaned agent-run sandbox cleanup: failed to release sandbox(es) \
                         for terminal agent run {}: {}",
                        run_id,
                        e
                    );
                }
            }
        }
        if released > 0 {
            tracing::info!(
                "Released sandboxes for {} terminal agent run(s) on startup",
                released
            );
        }
        Ok(released)
    }

    /// Create a sandbox for an agent run (autofixer / workflow agent) as a
    /// first-class `sandboxes` row, then create the container through the
    /// shared provider. Called by the agents' `SandboxRegistry` via the
    /// `RunSandboxService` seam (see `temps_agents::sandbox::managed`).
    ///
    /// Unlike standalone sandboxes, the container keeps the historical
    /// `temps-sandbox-<run_id>` naming (no `container_name_override`) so
    /// run recovery after a server restart keeps working, and the handle is
    /// keyed by `run_id` in the agents' registry — NOT by this row's `id`.
    /// The row is bookkeeping + API visibility; the agent run owns the
    /// lifecycle (the expiration sweeper skips rows with `agent_run_id`).
    pub async fn create_for_agent_run(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<temps_agents::sandbox::SandboxHandle, SandboxError> {
        let run_id = config.run_id;

        // A run can recreate its sandbox after a dead container — retire
        // any previous non-destroyed row for the same run first so the API
        // never shows two "running" sandboxes for one run.
        let stale = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in stale {
            self.mark_destroyed(row.id).await.ok();
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "reason": "superseded by new container for agent run" })),
            )
            .await;
        }

        let public_id_value = public_id::generate();
        let now = Utc::now();
        let timeout = config
            .idle_timeout
            .as_secs()
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(config.owner_user_id),
            agent_run_id: Set(Some(run_id)),
            name: Set(format!("temps-sandbox-{}", run_id)),
            status: Set("running".to_string()),
            image: Set(config.image.clone()),
            work_dir: Set(temps_agents::sandbox::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(Some(serde_json::json!({ "source": "agent_run" }))),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(now + chrono::Duration::seconds(timeout as i64)),
            ..Default::default()
        };
        let row = active.insert(self.db.as_ref()).await?;

        let handle = match self.registry.provider_arc().create(config).await {
            Ok(h) => h,
            Err(e) => {
                // Roll the row over to destroyed so the API doesn't show a
                // zombie "running" sandbox for a container that never started.
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id: 0,
                    reason: format!("agent run {}: {}", run_id, e),
                });
            }
        };

        if let Err(e) = self
            .record_backend(
                row.id,
                handle.backend,
                (!handle.image.is_empty()).then(|| handle.image.clone()),
            )
            .await
        {
            tracing::warn!(
                "failed to record backend/image for agent-run sandbox {} (run {}): {}",
                public_id_value,
                run_id,
                e
            );
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "agent_run_id": run_id,
                "backend": handle.backend.to_string(),
                "image": handle.image,
            })),
        )
        .await;

        tracing::info!(
            "Created agent-run sandbox {} (internal {}) for run {}",
            public_id_value,
            row.id,
            run_id
        );

        Ok(handle)
    }

    /// Destroy the container backing an agent run's sandbox and mark its
    /// row(s) destroyed. `handle` is the agents' registry cached handle
    /// when available; otherwise the container is recovered by run id.
    pub async fn release_for_agent_run(
        &self,
        run_id: i32,
        handle: Option<&temps_agents::sandbox::SandboxHandle>,
    ) -> Result<(), SandboxError> {
        let provider = self.registry.provider_arc();
        let resolved = match handle {
            Some(h) => Some(h.clone()),
            None => provider.recover(run_id).await.unwrap_or_else(|e| {
                tracing::warn!(
                    "release_for_agent_run: recover for run {} failed: {}",
                    run_id,
                    e
                );
                None
            }),
        };
        if let Some(h) = resolved {
            // Agent runs are ephemeral — purge the home volume too.
            if let Err(e) = provider.destroy(&h, true).await {
                tracing::warn!(
                    "Failed to destroy agent-run sandbox {} for run {}: {}",
                    h.sandbox_name,
                    run_id,
                    e
                );
            }
        }

        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in rows {
            self.mark_destroyed(row.id).await?;
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "agent_run_id": run_id })),
            )
            .await;
        }
        Ok(())
    }

    /// Derive a git seed source from a project's connected repository.
    ///
    /// The point of `project_id` on create: a user who already deploys a
    /// project should be able to say "give me a workspace on this" without
    /// re-typing the clone URL or re-picking the credential. The project
    /// row already knows both, so we read them here.
    ///
    /// Checks out the project's `main_branch` and reuses its
    /// `git_provider_connection_id`, so private repos clone without the
    /// caller ever handling a token. Full history (no shallow depth) —
    /// this is a workspace someone will branch and rebase in, not a
    /// one-shot build.
    ///
    /// Authorization is the caller's responsibility: handlers gate this
    /// with `project_scope_guard!` + `project_access_guard!` before the
    /// request reaches the service.
    async fn source_from_project(
        &self,
        project_id: i32,
    ) -> Result<Option<SandboxSource>, SandboxError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or(SandboxError::ProjectNotFound { project_id })?;

        let url = project
            .git_url
            .as_ref()
            .map(|u| u.trim())
            .filter(|u| !u.is_empty());

        let Some(url) = url else {
            // A Git project with no repository is inconsistent state and
            // must still fail closed. Git-less project types, however, are
            // intentionally valid deployment targets: their persistent
            // workspace begins empty and is populated by the agent or a file
            // upload instead of a clone.
            if project.source_type.requires_git_info() {
                return Err(SandboxError::ProjectHasNoRepo {
                    project_id,
                    name: project.name.clone(),
                });
            }
            return Ok(None);
        };

        Ok(Some(SandboxSource::Git {
            url: url.to_string(),
            revision: Some(project.main_branch.clone()),
            depth: None,
            username: None,
            password: None,
            git_connection_id: project.git_provider_connection_id,
            destination: None,
            strip_git_metadata: false,
        }))
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Create a new standalone sandbox. Inserts the DB row first (to get
    /// the internal ID the provider indexes by), then asks the provider
    /// to create the container. On provider failure the DB row is marked
    /// "destroyed" so list doesn't show zombie entries.
    pub async fn create_sandbox(
        &self,
        user_id: i32,
        req: CreateSandboxRequest,
    ) -> Result<sandboxes::Model, SandboxError> {
        let lifecycle = match req.lifecycle.as_deref() {
            None => SandboxLifecycle::default(),
            Some(value) => SandboxLifecycle::parse(value)?,
        };

        if req.from_snapshot_artifact.is_some() && req.source.is_some() {
            return Err(SandboxError::Validation {
                message: "source cannot be combined with from_snapshot; the snapshot already owns the workspace contents"
                    .to_string(),
            });
        }

        // Resolve the effective seed source before anything is created.
        // An explicit `source` always wins — `project_id` only supplies a
        // default, so a caller can create a workspace attributed to a
        // project while seeding it from a fork or a different branch.
        let source = if req.from_snapshot_artifact.is_some() {
            // project_id remains valid attribution, but must not implicitly
            // clone over the workspace restored from the snapshot.
            None
        } else {
            match (req.source.clone(), req.project_id) {
                // Caller-supplied: already validated by the handler, which owns
                // the request-shape rules too. Re-checking here would duplicate a
                // DNS lookup on every create for no gain.
                (Some(explicit), _) => Some(explicit),
                // Project-derived: the handler never saw this URL, so it is
                // validated here — the layer that produced it. `projects.git_url`
                // is checked for IP literals on the project side but never
                // resolved, so a name pointing at the metadata endpoint would
                // otherwise pass; and a row predating that guard can still carry
                // `user:password@`, which would land in `source_repo_url` and be
                // echoed back out of the API.
                (None, Some(project_id)) => {
                    let derived = self.source_from_project(project_id).await?;
                    if let Some(source) = derived.as_ref() {
                        validate_resolved_source(source).await?;
                    }
                    derived
                }
                (None, None) => None,
            }
        };

        let source_repo_url = source.as_ref().and_then(|s| match s {
            SandboxSource::Git { url, .. } => Some(url.clone()),
            SandboxSource::Tarball { .. } => None,
        });

        let timeout = req
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let public_id_value = public_id::generate();
        let name = req.name.clone().unwrap_or_else(|| public_id_value.clone());
        let managed_application_id = managed_application_id_from_name(&name);
        if managed_application_id.is_some() && req.host_work_dir_override.is_none() {
            return Err(SandboxError::Validation {
                message: format!(
                    "sandbox names beginning with '{APPLICATION_WORKSPACE_NAME_PREFIX}' are reserved for managed application workspaces"
                ),
            });
        }
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(timeout as i64);

        // Validate the requested backend before any DB/container work.
        // Only the two public backends are accepted — "local" is a dev
        // fallback, never a caller choice. `None` = host default (docker
        // unless the operator changed it), so existing clients see no
        // behavior change.
        let backend = match req.backend.as_deref() {
            None => None,
            Some("docker") => Some(temps_agents::sandbox::SandboxBackend::Docker),
            Some("firecracker") => Some(temps_agents::sandbox::SandboxBackend::Firecracker),
            Some(other) => {
                return Err(SandboxError::Validation {
                    message: format!(
                        "unknown backend '{}' (expected \"docker\" or \"firecracker\")",
                        other
                    ),
                })
            }
        };

        // Fail closed if the caller asked for a backend this host can't
        // provide (e.g. `firecracker` on a Docker-only host). Isolation
        // level is a security property — silently downgrading to Docker
        // would be worse than a clear error.
        if let Some(b) = backend {
            if !self.registry.provider_arc().supports_backend(b) {
                return Err(SandboxError::Validation {
                    message: format!("backend '{}' is not available on this host", b),
                });
            }
        }

        // Validate + hash the optional preview password *before* any
        // container/workdir work starts. A caller passing junk should fail
        // fast with a 400 rather than leaving an orphan container behind.
        let preview = match req.preview_password.as_deref() {
            Some(pw) => {
                crate::services::preview_password::validate(pw)
                    .map_err(|message| SandboxError::Validation { message })?;
                let hp =
                    crate::services::preview_password::hash_password(pw).map_err(|reason| {
                        SandboxError::PasswordHashFailed {
                            sandbox_id: public_id_value.clone(),
                            reason,
                        }
                    })?;
                Some(hp)
            }
            None => None,
        };

        let metadata_value = {
            let mut meta = serde_json::Map::new();
            if !req.ports.is_empty() {
                meta.insert("ports".into(), serde_json::json!(req.ports));
            }
            if let Some(disk) = req.disk_size_mb {
                meta.insert("disk_size_mb".into(), serde_json::json!(disk));
            }
            if let Some(application_id) = managed_application_id {
                meta.insert(
                    "managed_application_id".into(),
                    serde_json::json!(application_id),
                );
                if let Some(host_work_dir) = req.host_work_dir_override.as_ref() {
                    meta.insert(
                        "managed_host_work_dir".into(),
                        serde_json::json!(host_work_dir.to_string_lossy()),
                    );
                }
            }
            (!meta.is_empty()).then_some(serde_json::Value::Object(meta))
        };
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(Some(user_id)),
            name: Set(name.clone()),
            status: Set("running".to_string()),
            image: Set(req.image.clone()),
            // The real directory inside the container, not a nominal
            // "/workspace" — that path does not exist in the sandbox image
            // and never has. The agent-run path above already used this
            // constant; this one was hardcoded and wrong, which stayed
            // invisible because `exec` and the filesystem ops take the work
            // dir from the provider handle rather than from this row. It
            // only surfaced once the terminal used the row's value as a
            // PTY cwd and every shell died with ENOENT.
            work_dir: Set(temps_agents::sandbox::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(metadata_value),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(expires_at),
            preview_password_hash: Set(preview.as_ref().map(|p| p.hash.clone())),
            preview_password_hint: Set(preview.as_ref().map(|p| p.hint.clone())),
            lifecycle: Set(lifecycle.as_str().to_string()),
            project_id: Set(req.project_id),
            source_repo_url: Set(source_repo_url),
            ..Default::default()
        };
        let mut row = active.insert(self.db.as_ref()).await?;

        // Allocate host-side working directory.
        let using_trusted_work_dir = req.host_work_dir_override.is_some();
        let host_work_dir = req
            .host_work_dir_override
            .clone()
            .unwrap_or_else(|| self.data_root.join(&public_id_value));
        if let Err(e) = tokio::fs::create_dir_all(&host_work_dir).await {
            // Roll back the DB row so a failed-to-create sandbox doesn't
            // linger as a "running" record with no container.
            self.mark_destroyed(row.id).await.ok();
            return Err(SandboxError::CreateFailed {
                user_id,
                reason: format!("create work dir: {}", e),
            });
        }

        let container_label = container_label_for(&public_id_value).to_string();

        let config = SandboxCreateConfig {
            owner_user_id: None,
            run_id: row.id,
            container_name_override: Some(container_label.clone()),
            host_work_dir,
            workspace_volume: None,
            image: req.image.clone(),
            cpu_limit: req.cpu_limit,
            memory_limit_mb: req.memory_limit_mb,
            pids_limit: req.pids_limit,
            disk_size_mb: req.disk_size_mb,
            network_mode: None,
            env_vars: req.env,
            idle_timeout: Duration::from_secs(timeout),
            backend,
        };

        // Choose create path: snapshot restore vs normal create.
        let create_result = if let Some(ref artifact) = req.from_snapshot_artifact {
            // ADR-037: restore from snapshot. The provider ensures the image
            // is loaded into the daemon before creating the container.
            self.registry.create_from_snapshot(artifact, config).await
        } else {
            self.registry.create(config).await
        };

        let handle = match create_result {
            Ok(h) => h,
            Err(e) => {
                // Tear the container down before touching the work dir.
                // A provider `create` can fail *after* the container is
                // running — the ownership-normalisation step propagates its
                // error — and containers carry `restart_policy: unless-stopped`,
                // so skipping this leaves a live container holding the
                // caller's env vars, unreachable through the API once the
                // row is destroyed, with its /workspace deleted underneath
                // it. Same order as the seeding-failure arm below.
                let _ = self.registry.destroy(row.id, &public_id_value).await;
                // The work dir was already created above; without this the
                // row goes to "destroyed" and no later `destroy_sandbox`
                // can ever reach the directory again.
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: e.to_string(),
                });
            }
        };

        // Persist the *effective* backend + image the provider actually
        // used. When the request omitted them, the host default / backend
        // default decided; the handle carries the real answers. Stored so
        // the API/UI show what actually booted (e.g. "alpine:3.20") instead
        // of a vague "platform default".
        let effective_image =
            (req.image.is_none() && !handle.image.is_empty()).then(|| handle.image.clone());
        if let Err(e) = self
            .record_backend(row.id, handle.backend, effective_image.clone())
            .await
        {
            tracing::warn!(
                "failed to record backend/image for sandbox {}: {}",
                public_id_value,
                e
            );
        }
        // Mirror the recorded values into the in-memory row so the create
        // response matches what a later GET returns (record_backend wrote
        // them to the DB after the initial insert).
        row.backend = Some(handle.backend.to_string());
        if let Some(image) = effective_image {
            row.image = Some(image);
        }

        // If the caller asked us to seed the work dir, run the clone /
        // extract now. On failure we tear the sandbox down so the user
        // isn't left with a half-initialized container that's billing
        // their timeout budget.
        if let Some(source) = source {
            if let Err(e) = self
                .seed_source(row.id, &public_id_value, user_id, &source)
                .await
            {
                tracing::warn!(
                    "Seeding source into sandbox {} failed: {} — destroying",
                    public_id_value,
                    e
                );
                let _ = self.registry.destroy(row.id, &public_id_value).await;
                // Seeding runs after the clone/extract, so by here the work
                // dir can already hold a full repository — the largest
                // single thing this service puts on disk.
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                return Err(e);
            }
            self.record_event(
                row.id,
                "source_seeded",
                Some(serde_json::json!({ "type": source_kind(&source) })),
            )
            .await;
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "backend": row.backend,
                "image": row.image,
                "disk_size_mb": req.disk_size_mb,
            })),
        )
        .await;

        tracing::info!(
            "Created standalone sandbox {} (internal {}) for user {}",
            public_id_value,
            row.id,
            user_id
        );
        Ok(row)
    }

    /// Return the durable, preview-addressable sandbox assigned to one AI
    /// application.  This is deliberately an internal service method rather
    /// than an HTTP option: callers cannot choose an arbitrary host directory
    /// or container label.
    pub async fn get_or_create_application_workspace(
        &self,
        user_id: i32,
        application_public_id: &str,
        project_id: Option<i32>,
        host_work_dir: PathBuf,
    ) -> Result<ApplicationWorkspaceSandbox, SandboxError> {
        let authorized_project_ids = project_id.into_iter().collect::<Vec<_>>();
        self.get_or_create_application_workspace_with_config(
            user_id,
            application_public_id,
            project_id,
            host_work_dir,
            ApplicationWorkspaceConfig::default(),
            &authorized_project_ids,
        )
        .await
    }

    pub async fn get_or_create_application_workspace_with_config(
        &self,
        user_id: i32,
        application_public_id: &str,
        project_id: Option<i32>,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
        authorized_project_ids: &[i32],
    ) -> Result<ApplicationWorkspaceSandbox, SandboxError> {
        let _workspace_mutation = self.application_workspace_lock.lock().await;
        if application_public_id.is_empty()
            || application_public_id.len() > 200
            || !application_public_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(SandboxError::Validation {
                message: "application workspace identifier is invalid".to_string(),
            });
        }
        if !host_work_dir.is_absolute() {
            return Err(SandboxError::Validation {
                message: "application workspace path must be absolute".to_string(),
            });
        }

        let name = format!("{APPLICATION_WORKSPACE_NAME_PREFIX}{application_public_id}");
        let mut existing = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(&name))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await?;
        if existing.as_ref().is_some_and(|row| {
            !application_workspace_row_is_attested(
                row,
                application_public_id,
                &host_work_dir,
                &config,
            )
        }) {
            if let Some(untrusted) = existing.take() {
                tracing::warn!(
                    sandbox_id = %untrusted.public_id,
                    application_id = %application_public_id,
                    "Replacing unattested or configuration-mismatched application workspace compute"
                );
                self.destroy_sandbox(&untrusted.public_id, user_id).await?;
            }
        }
        let sandbox = if let Some(mut existing) = existing {
            // `project_id` is the generic sandbox credential scope. Keep it
            // aligned when an application changes its primary project.
            if existing.project_id != project_id {
                let mut active: sandboxes::ActiveModel = existing.clone().into();
                active.project_id = Set(project_id);
                existing = active.update(self.db.as_ref()).await?;
            }
            // Docker/VM state is not authoritative. If an operator or the
            // daemon removed running compute, recreate it against the same
            // application-owned host directory and named home volume.
            if existing.status == "running"
                && self
                    .registry
                    .get(existing.id, &existing.public_id)
                    .await
                    .is_err()
            {
                self.rebuild_application_workspace_locked(
                    user_id,
                    &existing.public_id,
                    host_work_dir.clone(),
                    config.clone(),
                )
                .await?;
            }
            ApplicationWorkspaceSandbox {
                public_id: existing.public_id,
            }
        } else {
            // A per-workspace password makes the bare opaque hostname useless on
            // its own. The chat API returns only short-lived preview grants; this
            // random value is immediately hashed and is never persisted or shown.
            let preview_password =
                format!("temps-preview-{}", hex::encode(rand::random::<[u8; 16]>()));
            let row = self
                .create_sandbox(
                    user_id,
                    CreateSandboxRequest {
                        name: Some(name),
                        timeout_secs: Some(config.idle_timeout_secs),
                        preview_password: Some(preview_password),
                        ports: Vec::new(),
                        lifecycle: Some("workspace".to_string()),
                        project_id,
                        image: config.image,
                        cpu_limit: Some(config.cpu_limit),
                        memory_limit_mb: Some(config.memory_limit_mb),
                        pids_limit: Some(config.pids_limit),
                        disk_size_mb: Some(config.disk_limit_mb),
                        host_work_dir_override: Some(host_work_dir),
                        ..Default::default()
                    },
                )
                .await?;
            ApplicationWorkspaceSandbox {
                public_id: row.public_id,
            }
        };

        // Reconcile the complete authorized data plane while stopped compute
        // cannot use stale memberships. Only after this succeeds may an idle
        // workspace be resumed. New/rebuilt compute starts with no application
        // data network; if reconciliation fails, stop it before returning.
        let row = self.find_by_public_id(&sandbox.public_id, user_id).await?;
        let configure_result = self
            .configure_application_data_network_locked(&row, authorized_project_ids)
            .await;
        if let Err(error) = configure_result {
            if row.status == "running" {
                if let Err(stop_error) = self.pause_sandbox(&row.public_id, user_id).await {
                    tracing::error!(
                        sandbox_id = %row.public_id,
                        %stop_error,
                        "failed to stop application workspace after data-network reconciliation failed"
                    );
                }
            }
            return Err(error);
        }
        if row.status == "stopped" && config.desired_state == "running" {
            self.resume_sandbox(&sandbox.public_id, user_id).await?;
        }
        if config.desired_state == "running" {
            self.prepare_application_git_workspace(&sandbox.public_id, user_id)
                .await?;
        }
        Ok(sandbox)
    }

    pub async fn rebuild_application_workspace(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
    ) -> Result<SandboxSummary, SandboxError> {
        let _workspace_mutation = self.application_workspace_lock.lock().await;
        self.rebuild_application_workspace_locked(user_id, sandbox_public_id, host_work_dir, config)
            .await
    }

    async fn rebuild_application_workspace_locked(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
    ) -> Result<SandboxSummary, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        ensure_not_agent_run(&row)?;
        self.jobs.abort_all(row.id).await;
        let create_config =
            application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;
        let handle = self
            .registry
            .rebuild(row.id, sandbox_public_id, create_config)
            .await
            .map_err(|error| from_agent_error(sandbox_public_id, error))?;
        let managed_application_id = managed_application_id_from_name(&row.name).map(str::to_owned);
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.image = Set(Some(handle.image));
        active.timeout_secs = Set(config.idle_timeout_secs as i32);
        active.last_activity_at = Set(Utc::now());
        active.expires_at = Set(idle_deadline(Utc::now(), config.idle_timeout_secs as i32));
        active.metadata = Set(Some(serde_json::json!({
            "disk_size_mb": config.disk_limit_mb,
            "managed_application_id": managed_application_id,
            "managed_host_work_dir": host_work_dir.to_string_lossy(),
        })));
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(updated.id, "rebuilt", None).await;
        Ok(SandboxSummary::from(&updated))
    }

    pub async fn restore_application_workspace(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        host_work_dir: PathBuf,
        config: ApplicationWorkspaceConfig,
        artifact: &temps_agents::sandbox::SnapshotArtifact,
    ) -> Result<SandboxSummary, SandboxError> {
        let _workspace_mutation = self.application_workspace_lock.lock().await;
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        ensure_not_agent_run(&row)?;
        self.jobs.abort_all(row.id).await;
        let backup = host_work_dir
            .with_extension(format!("restore-backup-{}", Utc::now().timestamp_micros()));
        tokio::fs::rename(&host_work_dir, &backup)
            .await
            .map_err(|source| SandboxError::CreateFailed {
                user_id,
                reason: format!("preserve workspace before restore: {source}"),
            })?;
        if let Err(source) = tokio::fs::create_dir_all(&host_work_dir).await {
            let _ = tokio::fs::rename(&backup, &host_work_dir).await;
            return Err(SandboxError::CreateFailed {
                user_id,
                reason: format!("create empty workspace restore target: {source}"),
            });
        }
        let create_config =
            application_sandbox_create_config(&row, host_work_dir.clone(), &config)?;
        let restored = self
            .registry
            .restore(row.id, sandbox_public_id, artifact, create_config)
            .await;
        let handle = match restored {
            Ok(handle) => handle,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&host_work_dir).await;
                let _ = tokio::fs::rename(&backup, &host_work_dir).await;
                return Err(from_agent_error(sandbox_public_id, error));
            }
        };
        if let Err(error) = tokio::fs::remove_dir_all(&backup).await {
            tracing::warn!(path = %backup.display(), %error, "restored workspace but could not remove backup directory");
        }
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.image = Set(Some(handle.image));
        active.last_activity_at = Set(Utc::now());
        active.expires_at = Set(idle_deadline(Utc::now(), config.idle_timeout_secs as i32));
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(updated.id, "restored", None).await;
        Ok(SandboxSummary::from(&updated))
    }

    pub async fn application_workspace_summary(
        &self,
        user_id: i32,
        application_public_id: &str,
    ) -> Result<Option<SandboxSummary>, SandboxError> {
        let name = format!("{APPLICATION_WORKSPACE_NAME_PREFIX}{application_public_id}");
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(name))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut summary = SandboxSummary::from(&row);
        if row.status == "running" && self.registry.get(row.id, &row.public_id).await.is_err() {
            // The durable database row is desired state, not proof that
            // disposable compute still exists. Callers can now distinguish a
            // killed container and trigger idempotent recovery with the saved
            // application configuration and host volume.
            summary.status = "recovering".to_string();
        }
        Ok(Some(summary))
    }

    /// Make a newly-created application project directory writable by the
    /// non-root sandbox user. A stopped or not-yet-created sandbox needs no
    /// special handling: the provider normalizes the complete bind mount when
    /// compute is next created or resumed.
    pub async fn normalize_application_project_permissions(
        &self,
        user_id: i32,
        application_public_id: &str,
        project_slug: &str,
    ) -> Result<(), SandboxError> {
        if application_public_id.is_empty()
            || project_slug.is_empty()
            || !project_slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(SandboxError::Validation {
                message: "application project workspace identifier is invalid".to_string(),
            });
        }
        let Some(row) = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(format!("ai-application:{application_public_id}")))
            .filter(sandboxes::Column::Status.eq("running"))
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(());
        };
        let container_path = format!(
            "{}/projects/{project_slug}",
            temps_agents::sandbox::user::SANDBOX_WORK_DIR
        );
        self.registry
            .normalize_workspace_path(row.id, &row.public_id, &container_path)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))
    }

    /// Put the workspace and only the databases linked to its application
    /// projects on a private, internal Docker network. This grants direct
    /// data-plane reachability without placing a reusable Temps API token in
    /// the sandbox and without exposing unrelated tenants' services.
    pub async fn synchronize_application_data_network(
        &self,
        user_id: i32,
        _application_public_id: &str,
        sandbox_public_id: &str,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        self.synchronize_data_network(user_id, sandbox_public_id, project_ids)
            .await
    }

    /// Reconcile the isolated data network for any sandbox from an authorized
    /// list of project IDs. The network identity belongs to the sandbox, not
    /// to whichever product surface created it.
    pub async fn synchronize_data_network(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let _workspace_mutation = self.application_workspace_lock.lock().await;
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.configure_application_data_network_locked(&row, project_ids)
            .await
    }

    async fn configure_application_data_network_locked(
        &self,
        row: &sandboxes::Model,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let containers = self
            .application_data_service_containers(project_ids)
            .await?;
        let network_name = sandbox_data_network_name(&row.public_id)?;
        self.registry
            .configure_application_network(row.id, &row.public_id, &network_name, &containers)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))?;
        Ok(containers)
    }

    /// Return the database containers currently implied by the committed
    /// project topology without changing container or network state.
    pub async fn application_data_service_count(
        &self,
        project_ids: &[i32],
    ) -> Result<usize, SandboxError> {
        Ok(self
            .application_data_service_containers(project_ids)
            .await?
            .len())
    }

    /// Resolve the runtime variables available to this sandbox's attached
    /// project. This is a sandbox primitive: application workspaces and
    /// ordinary API-created sandboxes use the same path.
    ///
    /// The sandbox row is the authorization boundary. A caller cannot pass a
    /// different project ID and use sandbox ownership to reveal that project's
    /// credentials. Application workspaces keep `project_id` synchronized to
    /// their primary project when they are prepared.
    pub async fn runtime_environment(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<HashMap<String, String>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let project_id = row.project_id.ok_or_else(|| SandboxError::Validation {
            message: format!(
                "sandbox {sandbox_public_id} is not attached to a project; attach a project before requesting database runtime variables"
            ),
        })?;
        // Credentials without connectivity are unusable. Reconcile the generic
        // sandbox data network at issuance time so API-created sandboxes get
        // the same behavior as application workspaces, including services
        // linked after the sandbox itself was created.
        self.synchronize_data_network(user_id, sandbox_public_id, &[project_id])
            .await?;
        let environments = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;
        let environment = environments
            .iter()
            .find(|environment| environment.name.eq_ignore_ascii_case("production"))
            .or_else(|| environments.first())
            .ok_or_else(|| SandboxError::RuntimeEnvironmentNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                project_id,
            })?;
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .order_by_asc(project_services::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let provider =
            self.runtime_credentials
                .as_ref()
                .ok_or_else(|| SandboxError::Unavailable {
                    reason: "runtime credential provider is not registered".to_string(),
                })?;
        let service_count = links.len();
        let mut variables = HashMap::new();
        for link in links {
            let issued = provider
                .issue(link.service_id, project_id, environment.id)
                .await
                .map_err(|error| SandboxError::RuntimeCredentialsFailed {
                    sandbox_id: sandbox_public_id.to_string(),
                    service_id: link.service_id,
                    reason: error.to_string(),
                })?;
            merge_runtime_variables(&mut variables, issued).map_err(|variable| {
                SandboxError::RuntimeVariableConflict {
                    sandbox_id: sandbox_public_id.to_string(),
                    variable,
                }
            })?;
        }
        let mut variable_names = variables.keys().cloned().collect::<Vec<_>>();
        variable_names.sort();
        self.record_event(
            row.id,
            "runtime_credentials_issued",
            Some(serde_json::json!({
                "project_id": project_id,
                "environment_id": environment.id,
                "service_count": service_count,
                "variable_names": variable_names,
            })),
        )
        .await;
        Ok(variables)
    }

    async fn application_data_service_containers(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.is_in(project_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let service_ids = links
            .iter()
            .map(|link| link.service_id)
            .collect::<std::collections::HashSet<_>>();
        let services = if service_ids.is_empty() {
            Vec::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids.iter().copied()))
                .all(self.db.as_ref())
                .await?
        };
        let members = if service_ids.is_empty() {
            Vec::new()
        } else {
            service_members::Entity::find()
                .filter(service_members::Column::ServiceId.is_in(service_ids.iter().copied()))
                .filter(service_members::Column::NodeId.is_null())
                .all(self.db.as_ref())
                .await?
        };
        let mut containers = Vec::new();
        for service in services {
            if service.node_id.is_some() {
                continue;
            }
            let service_members = members
                .iter()
                .filter(|member| member.service_id == service.id)
                .map(|member| member.container_name.clone())
                .collect::<Vec<_>>();
            if !service_members.is_empty() {
                containers.extend(service_members);
                continue;
            }
            let derived =
                service
                    .container_name
                    .unwrap_or_else(|| match service.service_type.as_str() {
                        "postgres" => format!("postgres-{}", service.name),
                        "redis" => format!("redis-{}", service.name),
                        "mongodb" => format!("temps-mongodb-{}", service.name),
                        "mariadb" | "mysql" => format!("mariadb-{}", service.name),
                        _ => String::new(),
                    });
            if !derived.is_empty() {
                containers.push(derived);
            }
        }
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    /// Reconcile every running application workspace linked to a project
    /// after that project's service topology changes. Revocations are applied
    /// from the committed database topology; if Docker cannot confirm the
    /// narrower membership, compute is stopped so existing sockets cannot
    /// retain access behind the control plane's back.
    pub async fn reconcile_application_networks_for_project(
        &self,
        project_id: i32,
    ) -> Result<(), SandboxError> {
        let affected_links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        if affected_links.is_empty() {
            return Ok(());
        }
        let application_ids = affected_links
            .iter()
            .map(|link| link.application_id)
            .collect::<Vec<_>>();
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::Id.is_in(application_ids))
            .filter(ai_applications::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;

        let mut first_error = None;
        for application in applications {
            let project_ids = ai_application_projects::Entity::find()
                .filter(ai_application_projects::Column::ApplicationId.eq(application.id))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|link| link.project_id)
                .collect::<Vec<_>>();
            let Some(summary) = self
                .application_workspace_summary(application.created_by, &application.public_id)
                .await?
                .filter(|summary| matches!(summary.status.as_str(), "running" | "recovering"))
            else {
                continue;
            };
            if let Err(error) = self
                .synchronize_application_data_network(
                    application.created_by,
                    &application.public_id,
                    &summary.public_id,
                    &project_ids,
                )
                .await
            {
                let quarantine_error = format!(
                    "Application data-network reconciliation failed; workspace compute was stopped: {error}"
                );
                if let Err(persist_error) = ai_application_workspaces::Entity::update_many()
                    .col_expr(
                        ai_application_workspaces::Column::DesiredState,
                        Expr::value("quarantined"),
                    )
                    .col_expr(
                        ai_application_workspaces::Column::LastError,
                        Expr::value(Some(quarantine_error)),
                    )
                    .col_expr(
                        ai_application_workspaces::Column::UpdatedAt,
                        Expr::value(Utc::now()),
                    )
                    .filter(ai_application_workspaces::Column::ApplicationId.eq(application.id))
                    .exec(self.db.as_ref())
                    .await
                {
                    tracing::error!(
                        application_id = %application.public_id,
                        error = %persist_error,
                        "Failed to persist fail-closed application workspace quarantine"
                    );
                }
                if let Err(stop_error) = self
                    .pause_sandbox(&summary.public_id, application.created_by)
                    .await
                {
                    tracing::error!(
                        application_id = %application.public_id,
                        sandbox_id = %summary.public_id,
                        error = %stop_error,
                        "Failed to stop workspace after data-network reconciliation failure"
                    );
                }
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub async fn application_workspace_usage(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<ApplicationWorkspaceUsage, SandboxError> {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), SOURCE_IMPORT_SYSTEM_PATH.to_string());
        let output = self
            .exec(
                sandbox_public_id,
                user_id,
                ExecOptions {
                    cmd: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        APPLICATION_WORKSPACE_USAGE_COMMAND.to_string(),
                    ],
                    env,
                    ..Default::default()
                },
            )
            .await?;
        let usage = parse_application_workspace_usage(&output.stdout);
        if usage.disk_used_bytes.is_none() {
            return Err(SandboxError::ExecFailed {
                sandbox_id: sandbox_public_id.to_string(),
                reason: "workspace disk usage measurement returned no value".to_string(),
            });
        }
        Ok(usage)
    }

    async fn prepare_application_git_workspace(
        &self,
        sandbox_public_id: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let result = self
            .exec(
                sandbox_public_id,
                user_id,
                ExecOptions {
                    cmd: vec![
                        "sh".to_string(),
                        "-lc".to_string(),
                        APPLICATION_GIT_BOOTSTRAP.to_string(),
                    ],
                    ..Default::default()
                },
            )
            .await?;
        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: sandbox_public_id.to_string(),
                reason: format!(
                    "initialize persistent Git workspace: {}",
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    /// Seed a fresh sandbox's `/workspace` with the requested content.
    /// Uses the provider's exec to keep the source-specific commands
    /// (`git`, `curl`, `tar`) out of the service crate.
    ///
    /// For git sources, credentials are injected via `GIT_ASKPASS` + a
    /// per-clone shim script rather than embedded in the URL or argv.
    /// This keeps the token out of `.git/config`, `ps`, and the provider's
    /// exec logs. The shim is shredded immediately after clone.
    pub(crate) async fn seed_source(
        &self,
        internal_id: i32,
        public_id: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<(), SandboxError> {
        // Resolve once immediately before the network operation and force the
        // client to dial that validated address. Validation performed earlier
        // in request handling is useful for fast failure, but cannot prevent a
        // DNS answer from changing before this command actually connects.
        let network_pin = validate_resolved_source(source).await?;
        let handle = self
            .registry
            .get(internal_id, public_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("resolve handle for seed: {}", e),
            })?;

        let work_dir = handle.work_dir.to_string_lossy().to_string();

        match source {
            SandboxSource::Git {
                url,
                revision,
                depth,
                username,
                password,
                git_connection_id,
                destination,
                strip_git_metadata,
            } => {
                let credentialed = git_connection_id.is_some()
                    || (username.as_deref().is_some() && password.as_deref().is_some());
                if credentialed && !sandbox_image_is_trusted_for_credentials(&handle) {
                    return Err(SandboxError::Validation {
                        message: format!(
                            "credentialed Git imports require a Temps-managed sandbox image; sandbox '{}' uses an untrusted custom or local runtime",
                            public_id
                        ),
                    });
                }
                // Resolve credentials. Priority: explicit (username,password)
                // pair > git_connection_id > anonymous. The validator rejects
                // the "both set" combination before we get here.
                let creds = if let Some(conn_id) = git_connection_id {
                    Some(
                        self.resolve_connection_creds(user_id, *conn_id, url)
                            .await?,
                    )
                } else if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
                    Some((u.to_string(), p.to_string()))
                } else {
                    None
                };

                validate_source_destination(destination.as_deref())?;
                if *strip_git_metadata && destination.is_none() {
                    return Err(SandboxError::Validation {
                        message: "strip_git_metadata requires a destination so the sandbox root repository cannot be removed"
                            .into(),
                    });
                }
                let target_dir = destination
                    .as_deref()
                    .map(|relative| Path::new(&work_dir).join(relative))
                    .unwrap_or_else(|| PathBuf::from(&work_dir));
                let target_dir = target_dir.to_string_lossy().to_string();

                self.run_git_clone(
                    &handle,
                    internal_id,
                    &work_dir,
                    &target_dir,
                    url,
                    revision.as_deref(),
                    *depth,
                    creds,
                    *strip_git_metadata,
                    network_pin.as_ref(),
                )
                .await
            }
            SandboxSource::Tarball { url } => {
                // Stream into a bounded staging directory. The monitor kills
                // both curl and tar if extraction exceeds the deadline/quota,
                // and only a complete import is copied into the workspace.
                let staging_dir = source_import_staging_dir(handle.backend, internal_id);
                let target_guard = source_target_guard_script(&work_dir, &work_dir);
                let staging_prepare = source_import_staging_prepare_script(&staging_dir);
                let extract = tarball_extract_command(url, &staging_dir, network_pin.as_ref());
                let bounded_extract = source_import_command_script(&staging_dir, &extract);
                let bounds = source_import_bounds_script(&staging_dir);
                let aggregate_bounds =
                    source_import_aggregate_bounds_script(&work_dir, &staging_dir);
                let finalize = source_import_staging_finalize_script(&staging_dir);
                let restore_target = source_target_restore_script();
                let script = format!(
                    "set -eu; {target_guard} trap '{restore_target}' EXIT; {staging_prepare} \
                     {bounded_extract}{bounds}{aggregate_bounds}; {finalize}",
                );
                self.exec_seed_script_as_root(&handle, internal_id, script)
                    .await
            }
        }
    }

    /// Resolve a stored git provider connection to an HTTP-Basic
    /// (username, password) pair. Enforces that the connection is owned
    /// by the calling user so one user can't clone repos using another
    /// user's token.
    async fn resolve_connection_creds(
        &self,
        user_id: i32,
        connection_id: i32,
        clone_url: &str,
    ) -> Result<(String, String), SandboxError> {
        let connection = self
            .git_provider_manager
            .get_connection(connection_id)
            .await
            .map_err(|e| SandboxError::Validation {
                message: format!("Git connection {} not available: {}", connection_id, e),
            })?;

        // Ownership check. Connections without a user_id are
        // organization/platform-level and not usable from per-user
        // sandboxes.
        match connection.user_id {
            Some(owner) if owner == user_id => {}
            _ => {
                return Err(SandboxError::Validation {
                    message: format!(
                        "Git connection {} is not owned by the requesting user",
                        connection_id
                    ),
                });
            }
        }

        let provider = self
            .git_provider_manager
            .get_provider(connection.provider_id)
            .await
            .map_err(|error| SandboxError::Validation {
                message: format!(
                    "Git provider {} for connection {} is not available: {}",
                    connection.provider_id, connection_id, error
                ),
            })?;
        if !provider.is_active {
            return Err(SandboxError::Validation {
                message: format!(
                    "Git provider {} for connection {} is inactive",
                    provider.id, connection_id
                ),
            });
        }
        validate_connection_clone_origin(clone_url, &provider)?;

        let token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: format!("connection#{}", connection_id),
                reason: format!("resolve git token: {}", e),
            })?;

        // GitHub/GitLab both accept `x-access-token` as the username for
        // token-based HTTPS auth. The token goes in the password slot and
        // is injected via GIT_ASKPASS (never argv, never URL).
        Ok(("x-access-token".to_string(), token))
    }

    /// Run the actual `git clone` inside the sandbox. Credentials (if any)
    /// are injected via an ephemeral `GIT_ASKPASS` shim and exported env
    /// vars. We never log `env_map` values and never embed the token in
    /// argv or the URL.
    #[allow(clippy::too_many_arguments)]
    async fn run_git_clone(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        work_dir: &str,
        target_dir: &str,
        url: &str,
        revision: Option<&str>,
        depth: Option<u32>,
        creds: Option<(String, String)>,
        strip_git_metadata: bool,
        network_pin: Option<&SourceNetworkPin>,
    ) -> Result<(), SandboxError> {
        // Destination creation is deliberately performed as the sandbox user.
        // The privileged phase only opens and validates an already-existing
        // directory, so a malicious symlink cannot make root create paths
        // outside the workspace before validation.
        self.ensure_source_target_as_sandbox_user(handle, internal_id, target_dir)
            .await?;
        let private_root = match handle.backend {
            temps_agents::sandbox::SandboxBackend::Local => "/tmp",
            // Docker mounts /run/secrets as tmpfs. Credential material must
            // never enter the writable layer where a concurrent `commit`
            // snapshot could retain it.
            _ => "/run/secrets",
        };
        let auth_dir = format!("{private_root}/.temps-source-auth-{internal_id}");
        let askpass_path = format!("{auth_dir}/askpass.sh");
        let askpass_user_path = format!("{auth_dir}/username");
        let askpass_password_path = format!("{auth_dir}/password");
        let staging_dir = source_import_staging_dir(handle.backend, internal_id);

        // Build the clone command. `-c credential.helper=` disables any
        // host-level credential helper so the password lands only via
        // the askpass shim and is never persisted to `.git/config`.
        // Redirects are disabled for anonymous and authenticated clones. The
        // source host was DNS-validated before this point; following a new
        // location would otherwise bypass that decision.
        let git_network_config = network_pin
            .map(|pin| {
                format!(
                    " -c http.curloptResolve={}",
                    shell_escape_service(&pin.curl_resolve_value())
                )
            })
            .unwrap_or_default();
        let git_environment =
            clean_git_environment(&auth_dir, creds.is_some().then_some(askpass_path.as_str()));
        let mut clone_cmd = format!(
            "{git_environment} git -c credential.helper= -c http.followRedirects=false{git_network_config}"
        );
        clone_cmd.push_str(&format!(
            " clone --depth {} --filter=blob:limit=33554432",
            depth.unwrap_or(1)
        ));
        if let Some(r) = revision {
            if !r.is_empty() {
                // `--branch` accepts branches and tags. For raw commit
                // SHAs this fails, and we fall back to a post-clone
                // `checkout` below.
                clone_cmd.push_str(&format!(" --branch {}", shell_escape_service(r)));
            }
        }
        clone_cmd.push_str(&format!(
            " {} {}",
            shell_escape_service(url),
            shell_escape_service(&staging_dir)
        ));

        // If revision didn't resolve to a branch/tag (e.g. raw SHA),
        // fall back to a post-clone checkout. Harmless when --branch
        // already did the right thing.
        let checkout_cmd = match revision {
            Some(r) if !r.is_empty() => format!(
                " || ({git_environment} git -C {wd} -c credential.helper= -c http.followRedirects=false{git_network_config} fetch origin {rev} && {git_environment} git -C {wd} checkout {rev})",
                wd = shell_escape_service(&staging_dir),
                rev = shell_escape_service(r),
            ),
            _ => String::new(),
        };
        let target_guard = source_target_guard_script(work_dir, target_dir);
        let staging_prepare = source_import_staging_prepare_script(&staging_dir);
        let auth_prepare = source_import_staging_prepare_script(&auth_dir);
        let bounded_clone =
            source_import_command_script(&staging_dir, &format!("{clone_cmd}{checkout_cmd}"));
        let cleanup = git_metadata_cleanup_script(&staging_dir, strip_git_metadata);
        let bounds = source_import_bounds_script(&staging_dir);
        let aggregate_bounds = source_import_aggregate_bounds_script(work_dir, &staging_dir);
        let finalize = source_import_staging_finalize_script(&staging_dir);
        let restore_target = source_target_restore_script();

        // Compose the shell script. When creds are present we write the
        // root-only credential files plus an askpass shim. Git itself runs
        // under `env -i`; the shim reads those files, so secrets enter neither
        // Git's argv nor its inherited environment. We always shred/remove the
        // whole auth directory before returning so a subsequent user
        // shell in the sandbox can't read stale state.
        let script = if creds.is_some() {
            format!(
                "set +x; set -eu; \
                 {target_guard} {staging_prepare} {auth_prepare} \
                 trap 'find {auth_dir} -type f -exec shred -u {{}} \\; 2>/dev/null || true; find {auth_dir} -depth -delete 2>/dev/null || true; {restore_target}' EXIT; \
                 umask 077; printf '%s' \"$GIT_USER\" > {user_file}; printf '%s' \"$GIT_PASS\" > {password_file}; \
                 cat > {ask} <<'TEMPS_ASKPASS_EOF'\n\
#!/bin/sh\n\
case \"$1\" in\n\
  Username*) cat {user_file} ;;\n\
  *)         cat {password_file} ;;\n\
esac\n\
TEMPS_ASKPASS_EOF\n\
                 chmod 700 {ask}; \
                 {bounded_clone}{cleanup}{bounds}{aggregate_bounds}; {finalize}",
                target_guard = target_guard,
                staging_prepare = staging_prepare,
                auth_prepare = auth_prepare,
                ask = askpass_path,
                user_file = shell_escape_service(&askpass_user_path),
                password_file = shell_escape_service(&askpass_password_path),
                auth_dir = shell_escape_service(&auth_dir),
                restore_target = restore_target,
                bounded_clone = bounded_clone,
                cleanup = cleanup,
                bounds = bounds,
                aggregate_bounds = aggregate_bounds,
                finalize = finalize,
            )
        } else {
            format!(
                "set -eu; {target_guard} {staging_prepare} {auth_prepare} \
                 trap 'find {auth_dir} -depth -delete 2>/dev/null || true; {restore_target}' EXIT; \
                 GIT_TERMINAL_PROMPT=0 {bounded_clone}{cleanup}{bounds}{aggregate_bounds}; {finalize}",
                target_guard = target_guard,
                staging_prepare = staging_prepare,
                auth_prepare = auth_prepare,
                auth_dir = shell_escape_service(&auth_dir),
                restore_target = restore_target,
                bounded_clone = bounded_clone,
                cleanup = cleanup,
                bounds = bounds,
                aggregate_bounds = aggregate_bounds,
                finalize = finalize,
            )
        };

        let mut env_map: HashMap<String, String> = HashMap::new();
        if let Some((u, p)) = creds {
            env_map.insert("GIT_USER".into(), u);
            env_map.insert("GIT_PASS".into(), p);
        }
        env_map.insert("HOME".into(), auth_dir);
        env_map.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env_map.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        env_map.insert("GIT_CONFIG_SYSTEM".into(), "/dev/null".into());

        self.exec_seed_script_with_env_as_root(handle, internal_id, script, env_map)
            .await
    }

    async fn ensure_source_target_as_sandbox_user(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        target_dir: &str,
    ) -> Result<(), SandboxError> {
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("mkdir -p -- {}", shell_escape_service(target_dir)),
        ];
        let result = self
            .registry
            .provider()
            .exec(
                handle,
                cmd,
                HashMap::from([("PATH".to_string(), SOURCE_IMPORT_SYSTEM_PATH.to_string())]),
                None,
            )
            .await
            .map_err(|error| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("prepare source destination: {error}"),
            })?;
        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "prepare source destination exited with code {}: {}",
                    result.exit_code,
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    async fn exec_seed_script_as_root(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        script: String,
    ) -> Result<(), SandboxError> {
        self.exec_seed_script_with_env_as_root(handle, internal_id, script, HashMap::new())
            .await
    }

    /// Execute a seed script with an env map. Never logs env values — the
    /// map may contain tokens. On non-zero exit we surface stdout/stderr
    /// but the sandbox layer scrubs them before they reach the user (the
    /// provider's exec impl is expected to honor this).
    async fn exec_seed_script_with_env_as_root(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        script: String,
        mut env_map: HashMap<String, String>,
    ) -> Result<(), SandboxError> {
        sanitize_privileged_import_environment(&mut env_map);
        env_map.insert("PATH".into(), SOURCE_IMPORT_SYSTEM_PATH.into());
        let cmd = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let execution = self
            .registry
            .provider()
            .exec_as_root(handle, cmd, env_map, None);
        let result = tokio::time::timeout(SOURCE_IMPORT_PROVIDER_TIMEOUT, execution)
            .await
            .map_err(|_| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "source import exceeded the {} second safety limit",
                    SOURCE_IMPORT_PROVIDER_TIMEOUT.as_secs()
                ),
            })?
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("seed source exec: {}", e),
            })?;

        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "seed source exited with code {}: {}",
                    result.exit_code,
                    bounded_command_error(&result.stderr, &result.stdout)
                ),
            });
        }
        Ok(())
    }

    /// Stop + destroy a sandbox. Aborts any background jobs, asks the
    /// provider to tear down the container + volumes, and marks the
    /// DB row "destroyed".
    ///
    /// Agent-run sandboxes (`agent_run_id` set) are special-cased: their
    /// container is named `temps-sandbox-<run_id>`, so the standalone
    /// registry (which recovers by `public_id`) would miss it, flip the
    /// row to "destroyed", and leave the real container running. While
    /// the run is active we refuse with [`SandboxError::ManagedByAgentRun`]
    /// (destroying the sandbox under a live run would break it); once the
    /// run is terminal we route through `release_for_agent_run`, which
    /// targets the container by run id.
    pub async fn destroy_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;

        if let Some(run_id) = row.agent_run_id {
            let run = agent_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await?;
            let terminal = run_status_is_terminal(run.as_ref().map(|r| r.status.as_str()));
            if !terminal {
                return Err(SandboxError::ManagedByAgentRun {
                    sandbox_id: public_id_value.to_string(),
                    run_id,
                });
            }
            self.jobs.abort_all(row.id).await;
            return self.release_for_agent_run(run_id, None).await;
        }

        self.jobs.abort_all(row.id).await;
        if let Err(e) = self.registry.destroy(row.id, public_id_value).await {
            // Even if the container destroy failed, mark the row
            // destroyed — otherwise the user is stuck with a zombie
            // they can't delete. Log the provider error loudly.
            tracing::error!(
                "Provider destroy failed for sandbox {} (internal {}): {} — marking row destroyed anyway",
                public_id_value,
                row.id,
                e
            );
        }
        // Remove the work dir even when the provider destroy failed. There
        // is no background sweep to fall back on — this call is the only
        // thing that ever frees the directory, so skipping it here means
        // leaking it permanently, which is the bug this fixes. The tradeoff
        // is that a failed destroy may leave a container still running with
        // this directory bind-mounted at /workspace, and the delete then
        // races that writer. Deleting is still the right call: the user
        // explicitly asked for the sandbox to be gone, and the row is
        // marked destroyed either way.
        self.remove_work_dir(public_id_value).await;
        self.record_event(row.id, "destroyed", None).await;
        self.mark_destroyed(row.id).await?;

        // Standalone snapshots may outlive their deleted sandbox without
        // retaining an internal identity. Managed application snapshots keep
        // immutable provenance so they can never become visible through the
        // generic snapshot API after the compute row is destroyed.
        if !row.name.starts_with(APPLICATION_WORKSPACE_NAME_PREFIX) {
            if let Some(ref snap_svc) = self.snapshot_service {
                if let Err(e) = snap_svc.nullify_source_sandbox(row.id).await {
                    tracing::warn!(
                        sandbox_id = %public_id_value,
                        internal_id = row.id,
                        "destroy: failed to nullify source_sandbox_id on snapshots: {}",
                        e
                    );
                }
            }
        }

        Ok(())
    }

    /// Delete the host-side `/workspace` directory backing a destroyed
    /// sandbox.
    ///
    /// `create_sandbox` makes `data_root/<public_id>` and bind-mounts it
    /// into the container; nothing used to remove it, so every
    /// create+destroy cycle left the full working tree (node_modules, build
    /// output, cloned repos — routinely hundreds of MB) on the host
    /// forever. Deliberately best-effort and non-fatal: the sandbox is
    /// already gone from the user's point of view, and failing the API call
    /// over leftover bytes would strand the row instead.
    ///
    /// Only called for standalone sandboxes. Agent-run sandboxes get their
    /// work dir from the executor, which owns that directory's lifecycle.
    async fn remove_work_dir(&self, public_id_value: &str) {
        let Some(work_dir) = work_dir_to_remove(&self.data_root, public_id_value) else {
            tracing::warn!(
                "Refusing to remove sandbox work dir for malformed public id {:?}",
                public_id_value
            );
            return;
        };
        match tokio::fs::remove_dir_all(&work_dir).await {
            Ok(()) => tracing::debug!("Removed sandbox work dir {}", work_dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                "Failed to remove sandbox work dir {}: {} — disk space stays claimed",
                work_dir.display(),
                e
            ),
        }
    }

    /// Pause a running sandbox (non-destructive). Stops the underlying
    /// container but leaves the DB row, volumes, and filesystem intact so
    /// the user can resume later. Idempotent on already-stopped sandboxes.
    pub async fn pause_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "stopped" {
            return Ok(row);
        }
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "pause".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        match self.registry.stop(row.id, public_id_value).await {
            Ok(()) | Err(temps_agents::error::AgentError::SandboxNotFound { .. }) => {}
            Err(error) => return Err(from_agent_error(public_id_value, error)),
        }
        let now = Utc::now();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("stopped".to_string());
        active.last_activity_at = Set(now);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "stopped", None).await;
        Ok(updated)
    }

    /// Resume a paused sandbox. Restarts the container and bumps
    /// `expires_at` to `now + timeout_secs` so the user gets a fresh
    /// idle window. Idempotent on already-running sandboxes.
    pub async fn resume_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "running" {
            return Ok(row);
        }
        if row.status != "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "resume".into(),
            });
        }
        self.registry
            .start(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let now = Utc::now();
        let new_expires = idle_deadline(now, row.timeout_secs);
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.last_activity_at = Set(now);
        active.expires_at = Set(new_expires);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "resumed", None).await;
        Ok(updated)
    }

    /// Restart a running sandbox in-place (stop + start). Filesystem and
    /// volumes survive. Rejected on stopped sandboxes (use resume instead).
    pub async fn restart_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "restart".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        self.registry
            .restart(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let now = Utc::now();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.last_activity_at = Set(now);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "restarted", None).await;
        Ok(updated)
    }

    /// Grow the sandbox's root disk to `new_size_mb`. Firecracker only —
    /// the resize is done offline (stop → grow ext4 → start), so the VM
    /// reboots but its filesystem and data survive. Grow-only. Records a
    /// `resized` event and persists the new size in `metadata`.
    pub async fn resize_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
        new_size_mb: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        const MIN_DISK_MB: u64 = 256;
        const MAX_DISK_MB: u64 = 64 * 1024;
        if !(MIN_DISK_MB..=MAX_DISK_MB).contains(&new_size_mb) {
            return Err(SandboxError::Validation {
                message: format!(
                    "disk_size_mb must be between {} and {}",
                    MIN_DISK_MB, MAX_DISK_MB
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.backend.as_deref() != Some("firecracker") {
            return Err(SandboxError::Validation {
                message: "disk resize is only supported on Firecracker sandboxes".into(),
            });
        }
        let old_mb = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("disk_size_mb"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1024);
        let sandbox_id = row.id;

        self.registry
            .resize_disk(row.id, public_id_value, new_size_mb)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;

        // Persist the new size into metadata (preserving ports).
        let mut meta = row
            .metadata
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        meta.insert("disk_size_mb".into(), serde_json::json!(new_size_mb));
        let mut active: sandboxes::ActiveModel = row.into();
        active.metadata = Set(Some(serde_json::Value::Object(meta)));
        active.last_activity_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;

        self.record_event(
            sandbox_id,
            "resized",
            Some(serde_json::json!({ "from_mb": old_mb, "to_mb": new_size_mb })),
        )
        .await;
        Ok(updated)
    }

    /// Seed an already-running sandbox with additional content. Mirrors
    /// the SDK's ability to attach a source *after* create; useful when
    /// the caller wants to clone a repo using a token that wasn't
    /// available at create time, or to layer a second repo on top.
    ///
    /// Rejects non-running sandboxes with `InvalidState`. The underlying
    /// `seed_source` applies the same credential-safe flow used on create.
    pub async fn clone_source(
        &self,
        public_id_value: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<sandboxes::Model, SandboxError> {
        let _source_import = self.source_import_lock.lock().await;
        validate_resolved_source(source).await?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "clone source".into(),
            });
        }
        self.seed_source(row.id, &row.public_id, user_id, source)
            .await?;
        let mut active: sandboxes::ActiveModel = row.into();
        active.last_activity_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Extend the sandbox's `expires_at` by `extra_secs`. Does not
    /// change `timeout_secs` — just pushes the deadline forward. Used
    /// by the SDK's `extendTimeout()` so long-running operations can
    /// keep the sandbox alive without recreating it.
    pub async fn extend_timeout(
        &self,
        public_id_value: &str,
        user_id: i32,
        extra_secs: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        if extra_secs == 0 {
            return Err(SandboxError::Validation {
                message: "extra_secs must be greater than zero".into(),
            });
        }
        if extra_secs > MAX_TIMEOUT_SECS {
            return Err(SandboxError::Validation {
                message: format!(
                    "extra_secs {} exceeds maximum of {}",
                    extra_secs, MAX_TIMEOUT_SECS
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let sandbox_id = row.id;
        let new_expires = row.expires_at + chrono::Duration::seconds(extra_secs as i64);
        let mut active: sandboxes::ActiveModel = row.into();
        active.expires_at = Set(new_expires);
        active.last_activity_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(
            sandbox_id,
            "timeout_extended",
            Some(serde_json::json!({
                "extra_secs": extra_secs,
                "expires_at": new_expires.to_rfc3339(),
            })),
        )
        .await;
        Ok(updated)
    }

    /// Record activity on a sandbox: bump `last_activity_at` **and** push
    /// `expires_at` out to `now + timeout_secs`.
    ///
    /// Moving `expires_at` here is what makes `timeout_secs` an *idle*
    /// timeout, which is what the column has always been documented as
    /// (and what the API calls it). Before this, `expires_at` was set once
    /// at create and never moved by activity, so a sandbox in continuous
    /// use was still stopped at its original deadline.
    ///
    /// The deadline is materialised in the indexed column rather than
    /// computed by the sweeper (`last_activity_at + timeout_secs`) on
    /// purpose: the sweep query rides the partial index on
    /// `(expires_at) WHERE status = 'running'`, and a predicate over an
    /// expression would not use it.
    ///
    /// Called on every exec and filesystem operation, so it must stay a
    /// single UPDATE with no read-modify-write. `timeout_secs` is taken
    /// from the caller (who already has the row) to avoid a SELECT here.
    ///
    /// **The deadline only ever moves forward.** `extend_timeout` (the SDK's
    /// `extendTimeout()`) pushes `expires_at` past `now + timeout_secs`
    /// precisely so a long operation can outlive the idle window — and the
    /// first thing that operation does is exec, which lands here. Assigning
    /// the idle deadline unconditionally would silently undo the extension
    /// the caller just paid for, so the write is `CASE WHEN`-guarded in SQL:
    /// still one statement, still no read-modify-write, but never a
    /// regression. `last_activity_at` is always bumped — it records activity,
    /// not a deadline.
    ///
    /// Swallows DB errors: activity bumps are best-effort, and failing a
    /// user's exec because a bookkeeping write lost a race would be worse
    /// than a sandbox expiring slightly early.
    pub async fn touch(&self, sandbox_id: i32, timeout_secs: i32) {
        let now = Utc::now();
        let deadline = idle_deadline(now, timeout_secs);
        let result = sandboxes::Entity::update_many()
            .col_expr(sandboxes::Column::LastActivityAt, Expr::value(now))
            .col_expr(
                sandboxes::Column::ExpiresAt,
                Expr::cust_with_values(
                    "CASE WHEN expires_at < $1 THEN $2 ELSE expires_at END",
                    [deadline, deadline],
                ),
            )
            .filter(sandboxes::Column::Id.eq(sandbox_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            tracing::debug!(
                "touch: failed to bump last_activity_at for {}: {}",
                sandbox_id,
                e
            );
        }
    }

    async fn mark_destroyed(&self, id: i32) -> Result<(), SandboxError> {
        let now = Utc::now();
        let active = sandboxes::ActiveModel {
            id: Set(id),
            status: Set("destroyed".to_string()),
            last_activity_at: Set(now),
            ..Default::default()
        };
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Rootfs storage inventory across backends (Firecracker cache + per-VM
    /// disks). Powers the management API's inspection endpoint.
    pub async fn rootfs_report(&self) -> Result<temps_agents::sandbox::RootfsReport, SandboxError> {
        self.registry
            .provider_arc()
            .rootfs_report()
            .await
            .map_err(|e| SandboxError::Unavailable {
                reason: format!("rootfs report: {}", e),
            })
    }

    /// Reclaim rootfs cache entries not backing any live sandbox.
    pub async fn gc_rootfs(&self) -> Result<temps_agents::sandbox::RootfsGcReport, SandboxError> {
        self.registry
            .provider_arc()
            .gc_rootfs()
            .await
            .map_err(|e| SandboxError::Unavailable {
                reason: format!("rootfs gc: {}", e),
            })
    }

    /// Append one operation to a sandbox's timeline. Best-effort — a failed
    /// insert is logged, never fatal to the operation it records.
    async fn record_event(
        &self,
        sandbox_id: i32,
        event_type: &str,
        detail: Option<serde_json::Value>,
    ) {
        let active = sandbox_events::ActiveModel {
            sandbox_id: Set(sandbox_id),
            event_type: Set(event_type.to_string()),
            detail: Set(detail),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        if let Err(e) = active.insert(self.db.as_ref()).await {
            tracing::warn!(
                "failed to record '{}' event for sandbox {}: {}",
                event_type,
                sandbox_id,
                e
            );
        }
    }

    /// The operations timeline for a sandbox, newest first. Ownership is
    /// enforced by resolving the sandbox through `find_by_public_id`.
    pub async fn list_events(
        &self,
        public_id_value: &str,
        user_id: i32,
        limit: u64,
    ) -> Result<Vec<sandbox_events::Model>, SandboxError> {
        // Bounded — the timeline is append-only and a UI only ever shows the
        // most recent slice. Cap the page so a long-lived sandbox can't force
        // an unbounded scan.
        let limit = limit.clamp(1, 500);
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let events = sandbox_events::Entity::find()
            .filter(sandbox_events::Column::SandboxId.eq(row.id))
            .order_by_desc(sandbox_events::Column::CreatedAt)
            .order_by_desc(sandbox_events::Column::Id)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        Ok(events)
    }

    /// Persist the effective backend (and, when the request didn't specify
    /// one, the resolved image) a sandbox runs on.
    async fn record_backend(
        &self,
        id: i32,
        backend: temps_agents::sandbox::SandboxBackend,
        effective_image: Option<String>,
    ) -> Result<(), SandboxError> {
        let mut active = sandboxes::ActiveModel {
            id: Set(id),
            backend: Set(Some(backend.to_string())),
            ..Default::default()
        };
        if let Some(image) = effective_image {
            active.image = Set(Some(image));
        }
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    // ── Preview password ────────────────────────────────────────────────

    /// Set (or rotate) the preview password for a sandbox. The plaintext
    /// is hashed with argon2id and only the hash + last-4 hint are stored.
    /// Returns the hint to the caller — the plaintext is never persisted
    /// or echoed back (the caller already has it).
    ///
    /// Rotating an existing password invalidates every live preview cookie
    /// immediately: the proxy folds a digest of the argon2 hash into the
    /// cookie payload, so a new hash = a new fingerprint = every existing
    /// cookie fails verification.
    pub async fn set_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
        plaintext: &str,
    ) -> Result<String, SandboxError> {
        crate::services::preview_password::validate(plaintext)
            .map_err(|message| SandboxError::Validation { message })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let hp = crate::services::preview_password::hash_password(plaintext).map_err(|reason| {
            SandboxError::PasswordHashFailed {
                sandbox_id: public_id_value.to_string(),
                reason,
            }
        })?;
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(Some(hp.hash));
        active.preview_password_hint = Set(Some(hp.hint.clone()));
        active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "preview_password_set", None)
            .await;
        Ok(hp.hint)
    }

    /// Remove the preview password. Subsequent preview requests fall back
    /// to URL-only protection (the unguessable hex public_id). Idempotent —
    /// clearing an already-unset password is a no-op, not an error.
    pub async fn clear_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let had_password = row.preview_password_hash.is_some();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(None);
        active.preview_password_hint = Set(None);
        active.update(self.db.as_ref()).await?;
        if had_password {
            self.record_event(sandbox_id, "preview_password_cleared", None)
                .await;
        }
        Ok(())
    }

    // ── Helpers shared with the exec/fs modules ──────────────────────────

    /// Load + authorize + return the internal ID **without** waking a
    /// suspended workspace.
    ///
    /// For endpoints that only need to prove ownership and then answer from
    /// the row or from in-memory state — preview URLs, job listings, job
    /// status. Those are reads; booting a container as a side effect of a
    /// `GET` is a surprise that costs real memory on a 3 vCPU / 4 GB host, and
    /// in the job cases it is pure waste: the in-memory job table was emptied
    /// when the container stopped, so waking it cannot change the answer.
    ///
    /// Use [`Self::resolve_id`] on paths that genuinely need the container
    /// running (exec, filesystem, terminal).
    pub async fn resolve_id_no_wake(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let id = row.id;
        Ok((row, id))
    }

    /// Load + authorize + return the internal ID, or a typed error that
    /// already includes the public ID. Exec/fs modules call this first.
    /// Explicitly stopped ephemeral sandboxes return `InvalidState` (→ HTTP
    /// 409); durable workspaces wake transparently when either the row or the
    /// provider reports that the runtime is stopped.
    ///
    /// **Wakes a suspended workspace as a side effect** (ADR-036), so only
    /// call it where the container must actually be running.
    pub async fn resolve_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let lifecycle = SandboxLifecycle::from_row(&row);
        if row.status == "stopped" {
            // Workspaces (ADR-036) are places a human comes back to, so a
            // suspended one wakes instead of erroring. Ephemeral sandboxes
            // keep the 409: their lifetime belongs to whoever created them,
            // and `@vercel/sandbox` consumers rely on a stopped sandbox
            // staying stopped rather than silently restarting under them.
            if lifecycle == SandboxLifecycle::Workspace {
                return self.wake_workspace(row).await;
            }
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "exec or filesystem operation".into(),
            });
        }

        // Docker (or another provider) can stop a container without passing
        // through SandboxService: daemon pressure, an operator `docker kill`,
        // or the application harness hard deadline are all legitimate ways to
        // reach that state. In those cases the DB row still says `running`.
        // Trusting only the row makes the first application-workspace access
        // fail in `registry.get()` before the later harness recovery path gets
        // a chance to start the container.
        //
        // Only workspaces auto-recover. Ephemeral sandboxes retain their
        // caller-owned lifecycle semantics, and `registry.get()` below the
        // service layer will still return a typed not-found error for them.
        if row.status == "running" && lifecycle == SandboxLifecycle::Workspace {
            match self.registry.get(row.id, public_id_value).await {
                Ok(_) => {}
                Err(temps_agents::error::AgentError::SandboxNotFound { .. }) => {
                    tracing::warn!(
                        sandbox_id = public_id_value,
                        internal_id = row.id,
                        "Workspace row says running but its provider runtime is stopped; waking it on access"
                    );
                    return self.wake_workspace(row).await;
                }
                Err(error) => return Err(from_agent_error(public_id_value, error)),
            }
        }
        let id = row.id;
        Ok((row, id))
    }

    /// Start a suspended workspace's container and reconcile its row to
    /// `running`. Called from [`Self::resolve_id`] on the access path for both
    /// an explicitly stopped row and a stale `running` row whose provider
    /// runtime has died, so the caller experiences recovery as a slow first
    /// request rather than a failure.
    ///
    /// A wake is a container start, not a create: the image is local, the
    /// home volume exists, and the work dir is still populated on the
    /// host. Nothing is re-cloned and nothing is lost.
    ///
    /// If the provider can't start it, the row stays `stopped` and the
    /// caller gets the provider's error — deliberately not swallowed. A
    /// workspace that silently fails to wake and then reports "not found"
    /// on every subsequent call is exactly the debugging dead end
    /// self-hosted users can't escape.
    async fn wake_workspace(
        &self,
        row: sandboxes::Model,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let public_id_value = row.public_id.clone();
        let sandbox_id = row.id;
        tracing::info!(
            "Waking workspace sandbox {} (internal {}) on access",
            public_id_value,
            sandbox_id
        );
        // Bounded: a wake happens inline on a user's request, and a wedged
        // container runtime would otherwise hang that request forever with no
        // way for the caller to tell a slow start from a dead daemon.
        match tokio::time::timeout(
            WAKE_TIMEOUT,
            self.registry.start(sandbox_id, &public_id_value),
        )
        .await
        {
            Ok(result) => result.map_err(|e| from_agent_error(&public_id_value, e))?,
            Err(_) => {
                return Err(SandboxError::InvalidState {
                    sandbox_id: public_id_value.clone(),
                    state: "stopped".to_string(),
                    operation: format!(
                        "wake timed out after {}s; the container runtime did not \
                         start the workspace. Check the daemon, then retry",
                        WAKE_TIMEOUT.as_secs()
                    ),
                });
            }
        }

        let now = Utc::now();
        let new_expires = idle_deadline(now, row.timeout_secs);
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.last_activity_at = Set(now);
        active.expires_at = Set(new_expires);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "woken", None).await;
        Ok((updated, sandbox_id))
    }

    /// Build a typed provider error into a `SandboxError` carrying the
    /// public ID. Thin wrapper — module-private modules call this.
    pub(crate) fn provider_err(
        public_id_value: &str,
        err: temps_agents::error::AgentError,
    ) -> SandboxError {
        from_agent_error(public_id_value, err)
    }

    // ── Preview URL (`sandbox.domain(port)`) ─────────────────────────────

    /// Resolve the public URL for a port inside the sandbox. Returns the
    /// same `ws-<id>-<port>.<domain>` hostname the preview gateway already
    /// routes for workspace sessions, so standalone sandboxes don't
    /// require any gateway changes.
    ///
    /// Validation: `port` must be in [1, 65535]. Port `0` is rejected
    /// because the gateway matches exact numbers — surfacing a URL with
    /// `port=0` would be useless.
    pub async fn domain(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
    ) -> Result<String, SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // Ownership + validity check. The numeric id is intentionally
        // discarded — the preview URL never embeds it. No wake: building a
        // URL string does not need the container running.
        let _ = self.resolve_id_no_wake(public_id_value, user_id).await?;
        let parts = self.preview_parts().await;
        Ok(parts.url_for(public_id_value, port))
    }

    /// Build a shareable preview link that carries its own authorization.
    ///
    /// `domain()` returns the bare preview URL, which is useless to anyone who
    /// does not already hold the preview password — so sharing a protected
    /// preview meant sharing the password itself, which then cannot be revoked
    /// for one recipient. This returns that URL plus a minted `session_grant`
    /// aimed at the login bridge: the recipient's browser exchanges it for the
    /// ordinary preview cookie and lands on `path`.
    ///
    /// The grant is scoped to this one sandbox, expires, and is never
    /// forwarded to the sandbox itself.
    pub async fn preview_share_link(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
        path: &str,
        ttl: Duration,
    ) -> Result<(String, u64), SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // No wake: minting a share link reads the row's password hash.
        let (row, _) = self.resolve_id_no_wake(public_id_value, user_id).await?;
        let password_hash =
            row.preview_password_hash
                .as_deref()
                .ok_or_else(|| SandboxError::InvalidState {
                    sandbox_id: public_id_value.to_string(),
                    state: "preview password disabled".into(),
                    operation: "mint an expiring preview link; set a preview password first".into(),
                })?;

        let now = SystemTime::now();
        let (grant, expires_at) = temps_core::encode_preview_session_grant(
            &self.cookie_crypto,
            public_id_value,
            password_hash,
            ttl,
            now,
        )
        .map_err(|source| SandboxError::PreviewGrantFailed {
            sandbox_id: public_id_value.to_string(),
            source,
        })?;
        let granted_ttl = ttl.min(temps_core::PREVIEW_SESSION_GRANT_MAX_TTL);

        let next = temps_core::sanitize_preview_next(path);

        let base = self.preview_parts().await.url_for(public_id_value, port);
        let url = format!(
            "{}/__temps/preview/login?grant=1&next={}#session_grant={}",
            base.trim_end_matches('/'),
            url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>(),
            url::form_urlencoded::byte_serialize(grant.as_bytes()).collect::<String>(),
        );
        self.record_event(
            row.id,
            "preview_share_link_created",
            Some(serde_json::json!({
                "actor_user_id": user_id,
                "port": port,
                "path": next,
                "ttl_seconds": granted_ttl.as_secs(),
                "expires_at": expires_at,
            })),
        )
        .await;
        Ok((url, expires_at))
    }
}

fn merge_runtime_variables(
    destination: &mut HashMap<String, String>,
    issued: HashMap<String, String>,
) -> Result<(), String> {
    for (name, value) in issued {
        if destination
            .get(&name)
            .is_some_and(|existing| existing != &value)
        {
            return Err(name);
        }
        destination.insert(name, value);
    }
    Ok(())
}

#[async_trait::async_trait]
impl temps_core::ApplicationDataNetworkReconciler for SandboxService {
    async fn project_topology_changed(
        &self,
        project_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reconcile_application_networks_for_project(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

fn sandbox_data_network_name(sandbox_public_id: &str) -> Result<String, SandboxError> {
    let opaque_id = sandbox_public_id
        .strip_prefix("sbx_")
        .unwrap_or(sandbox_public_id);
    if opaque_id.is_empty()
        || opaque_id.len() > 40
        || !opaque_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SandboxError::Validation {
            message: "sandbox data-network identifier is invalid".to_string(),
        });
    }
    Ok(format!("temps-sandbox-data-{opaque_id}"))
}

fn application_sandbox_create_config(
    row: &sandboxes::Model,
    host_work_dir: PathBuf,
    config: &ApplicationWorkspaceConfig,
) -> Result<SandboxCreateConfig, SandboxError> {
    let backend = row
        .backend
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|_| SandboxError::Validation {
            message: format!(
                "application workspace '{}' has an unsupported backend",
                row.public_id
            ),
        })?;
    Ok(SandboxCreateConfig {
        owner_user_id: None,
        run_id: row.id,
        container_name_override: Some(container_label_for(&row.public_id).to_string()),
        host_work_dir,
        workspace_volume: None,
        image: config.image.clone(),
        cpu_limit: Some(config.cpu_limit),
        memory_limit_mb: Some(config.memory_limit_mb),
        pids_limit: Some(config.pids_limit),
        disk_size_mb: Some(config.disk_limit_mb),
        network_mode: None,
        env_vars: HashMap::new(),
        idle_timeout: Duration::from_secs(config.idle_timeout_secs),
        backend,
    })
}

/// Placeholder "public id" used in error messages when the source-seed
/// step fails before we propagate it upward. We already mapped the
/// real public ID into the top-level Create error; this just gives the
/// inner ExecFailed a non-empty identifier.
fn handle_id_fallback(internal_id: i32) -> String {
    format!("sandbox#{}", internal_id)
}

/// POSIX-style single-quoted escape for embedding into `sh -c` scripts
/// from the service layer. Duplicated from `services::exec::shell_escape`
/// so we don't introduce a module cycle for a 10-line helper.
fn shell_escape_service(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

/// Open and lock the destination directory for a source import.
///
/// Publication later uses the retained descriptor at `/dev/fd/9`, not
/// the mutable pathname. The descriptor is validated after it is opened and
/// the directory is made root-only for the duration of the import. A sandbox
/// process can therefore rename or replace the pathname while Git is running,
/// but it cannot redirect the privileged copy or add symlinks beneath the
/// directory being published into.
fn source_target_guard_script(work_dir: &str, target_dir: &str) -> String {
    let work_dir = shell_escape_service(work_dir);
    let target_dir = shell_escape_service(target_dir);
    if work_dir == target_dir {
        return format!(
            "test -d {work_dir} && test ! -L {work_dir} || {{ echo 'sandbox work directory must be a real directory' >&2; exit 64; }}; \
             work_real=$(readlink -f -- {work_dir}); exec 9< {work_dir}; target_fd=/dev/fd/9; \
             target_real=$(readlink -f -- \"$target_fd\"); [ \"$target_real\" = \"$work_real\" ] || {{ echo 'source destination changed while opening it' >&2; exit 64; }}; \
             test -z \"$(find \"$target_fd\" -mindepth 1 -print -quit)\" || {{ echo 'source destination must be empty' >&2; exit 66; }}; \
             if [ \"$(id -u)\" -eq 0 ]; then chown root:root \"$target_fd\"; chmod 700 \"$target_fd\"; fi;"
        );
    }
    format!(
        "test -d {work_dir} && test ! -L {work_dir} || {{ echo 'sandbox work directory must be a real directory' >&2; exit 64; }}; \
         test -d {target_dir} || {{ echo 'source destination must already be a directory' >&2; exit 64; }}; \
         work_real=$(readlink -f -- {work_dir}); exec 9< {target_dir}; target_fd=/dev/fd/9; \
         target_real=$(readlink -f -- \"$target_fd\"); \
         case \"$target_real/\" in \"$work_real/\"*) ;; *) echo 'source destination escapes the sandbox work directory' >&2; exit 64 ;; esac; \
         test -z \"$(find \"$target_fd\" -mindepth 1 -print -quit)\" || {{ echo 'source destination must be an empty directory' >&2; exit 66; }}; \
         if [ \"$(id -u)\" -eq 0 ]; then chown root:root \"$target_fd\"; chmod 700 \"$target_fd\"; fi;"
    )
}

fn source_target_restore_script() -> &'static str {
    "if [ \"$(id -u)\" -eq 0 ] && [ -d /dev/fd/9 ] && id -u temps >/dev/null 2>&1; then chown \"$(id -u temps):$(id -g temps)\" /dev/fd/9; chmod 755 /dev/fd/9; fi"
}

fn source_import_staging_dir(
    backend: temps_agents::sandbox::SandboxBackend,
    internal_id: i32,
) -> String {
    let private_root = match backend {
        temps_agents::sandbox::SandboxBackend::Local => "/tmp",
        temps_agents::sandbox::SandboxBackend::Docker => "/run/temps-source-import",
        temps_agents::sandbox::SandboxBackend::Firecracker => "/root",
    };
    format!("{private_root}/.temps-source-import-{internal_id}")
}

fn source_import_staging_prepare_script(staging_dir: &str) -> String {
    let staging_dir = shell_escape_service(staging_dir);
    let require_hard_limit = if staging_dir.starts_with("/run/temps-source-import/") {
        "grep -q ' /run/temps-source-import ' /proc/self/mountinfo || { echo 'secure source-import staging is unavailable; rebuild this sandbox before importing' >&2; exit 69; }; "
    } else if staging_dir.starts_with("/run/secrets/") {
        "grep -q ' /run/secrets ' /proc/self/mountinfo || { echo 'secure source-import credential storage is unavailable; rebuild this sandbox before importing' >&2; exit 69; }; "
    } else {
        // Local and Firecracker do not currently expose a provider-enforced,
        // size-bounded staging filesystem. Sampling `du` after extraction is
        // not a quota and can exhaust the host/VM disk between samples, so
        // reject the import instead of pretending the soft monitor is safe.
        "echo 'secure source-import staging is unavailable for this sandbox backend' >&2; exit 69; "
    };
    format!(
        "{require_hard_limit}find {staging_dir} -depth -delete 2>/dev/null || true; mkdir -m 700 -p {staging_dir}; \
         test ! -L {staging_dir} || {{ echo 'source staging directory must not be a symbolic link' >&2; exit 64; }};"
    )
}

fn source_import_staging_finalize_script(staging_dir: &str) -> String {
    let staging_dir = shell_escape_service(staging_dir);
    format!(
        "target_fd=/dev/fd/9; test -d \"$target_fd\" || exit 67; \
         cd -P -- \"$target_fd\"; \
         test -z \"$(find . -mindepth 1 -print -quit)\" || {{ echo 'source destination changed during import' >&2; exit 66; }}; \
         cp -a {staging_dir}/. . || {{ find . -mindepth 1 -depth -delete 2>/dev/null || true; exit 67; }}; \
         find {staging_dir} -depth -delete; \
         if id -u temps >/dev/null 2>&1; then \
           chown -R \"$(id -u temps):$(id -g temps)\" . || {{ find . -mindepth 1 -depth -delete 2>/dev/null || true; exit 67; }}; \
           chmod 755 .; \
         fi"
    )
}

/// Execute Git under an in-container deadline while continuously enforcing
/// the import quota. The outer Tokio timeout only drops a future; this inner
/// process is what actually terminates Git and removes a partial checkout.
fn source_import_command_script(target_dir: &str, command: &str) -> String {
    let target_dir = shell_escape_service(target_dir);
    let command = shell_escape_service(command);
    format!(
        "set +e; timeout -k 5 {timeout_secs} sh -c {command} & import_pid=$!; limit_exceeded=0; \
         while kill -0 \"$import_pid\" 2>/dev/null; do \
           entry_count=$(find {target_dir} -mindepth 1 -print 2>/dev/null | head -n {entry_probe} | wc -l | tr -d ' '); \
           byte_count=$(du -sk {target_dir} 2>/dev/null | awk '{{print $1 * 1024}}'); byte_count=${{byte_count:-0}}; \
           if [ \"$entry_count\" -gt {max_entries} ] || [ \"$byte_count\" -gt {max_bytes} ]; then \
             limit_exceeded=1; kill -TERM \"$import_pid\" 2>/dev/null || true; break; \
           fi; sleep 1; \
         done; \
         wait \"$import_pid\"; import_status=$?; set -e; \
         if [ \"$limit_exceeded\" -eq 1 ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import exceeds the 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi; \
         if [ \"$import_status\" -ne 0 ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; exit \"$import_status\"; \
         fi",
        timeout_secs = SOURCE_IMPORT_TIMEOUT.as_secs(),
        entry_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_entries = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

fn git_metadata_cleanup_script(target_dir: &str, strip_git_metadata: bool) -> String {
    if strip_git_metadata {
        format!(
            "; find {git_dir} -depth -delete",
            git_dir = shell_escape_service(&Path::new(target_dir).join(".git").to_string_lossy())
        )
    } else {
        String::new()
    }
}

fn source_import_bounds_script(target_dir: &str) -> String {
    let target_dir = shell_escape_service(target_dir);
    format!(
        "; file_count=$(find {target_dir} -mindepth 1 -print | head -n {file_probe} | wc -l | tr -d ' '); \
         byte_count=$(du -sk {target_dir} | awk '{{print $1 * 1024}}'); \
         if [ \"$file_count\" -gt {max_files} ] || [ \"$byte_count\" -gt {max_bytes} ]; then \
           find {target_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import exceeds the 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi",
        file_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_files = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

/// Enforce the quota across the durable workspace plus the staged import.
/// Checking before finalization lets us reject only the new staging tree and
/// preserve files that already belong to the workspace.
fn source_import_aggregate_bounds_script(work_dir: &str, staging_dir: &str) -> String {
    let work_dir = shell_escape_service(work_dir);
    let staging_dir = shell_escape_service(staging_dir);
    format!(
        "; existing_count=$(find {work_dir} -mindepth 1 -print | head -n {entry_probe} | wc -l | tr -d ' '); \
         staged_count=$(find {staging_dir} -mindepth 1 -print | head -n {entry_probe} | wc -l | tr -d ' '); \
         existing_bytes=$(du -sk {work_dir} | awk '{{print $1 * 1024}}'); \
         staged_bytes=$(du -sk {staging_dir} | awk '{{print $1 * 1024}}'); \
         if [ $((existing_count + staged_count)) -gt {max_entries} ] || \
            [ $((existing_bytes + staged_bytes)) -gt {max_bytes} ]; then \
           find {staging_dir} -depth -delete 2>/dev/null || true; \
           echo 'source import would exceed the aggregate 5000 entry or 256 MiB workspace limit' >&2; exit 65; \
         fi",
        entry_probe = SOURCE_IMPORT_MAX_FILES + 1,
        max_entries = SOURCE_IMPORT_MAX_FILES,
        max_bytes = SOURCE_IMPORT_MAX_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::{Job, JobQueue, JobReceiver, QueueError};

    #[test]
    fn sandbox_data_network_names_are_stable_and_scoped() {
        assert_eq!(
            sandbox_data_network_name("sbx_abc123").expect("valid sandbox id"),
            "temps-sandbox-data-abc123"
        );
    }

    #[test]
    fn source_destinations_are_bounded_relative_paths() {
        for valid in ["project", "projects/web", "packages/api-v2"] {
            assert!(validate_source_destination(Some(valid)).is_ok(), "{valid}");
        }
        for invalid in ["", ".", "../project", "projects/../escape", "/workspace"] {
            assert!(
                validate_source_destination(Some(invalid)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn stripped_git_metadata_targets_only_the_imported_repository() {
        let cleanup = git_metadata_cleanup_script("/workspace/projects/web", true);
        assert!(cleanup.contains("/workspace/projects/web/.git"));
        assert!(!cleanup.contains("/workspace/.git "));
        assert!(git_metadata_cleanup_script("/workspace/projects/web", false).is_empty());
    }

    #[test]
    fn source_imports_use_bounded_staging_and_process_deadlines() {
        let nested_stage =
            source_import_staging_dir(temps_agents::sandbox::SandboxBackend::Docker, 42);
        assert_eq!(
            nested_stage,
            "/run/temps-source-import/.temps-source-import-42"
        );
        let local_stage =
            source_import_staging_dir(temps_agents::sandbox::SandboxBackend::Local, 42);
        assert_eq!(local_stage, "/tmp/.temps-source-import-42");

        let command = source_import_command_script(&nested_stage, "git clone repo target");
        assert!(command.contains("timeout -k 5 120"));
        assert!(
            command.contains("find /run/temps-source-import/.temps-source-import-42 -mindepth 1")
        );
        assert!(command.contains("kill -TERM"));
        assert!(command
            .contains("find /run/temps-source-import/.temps-source-import-42 -depth -delete"));

        let prepare = source_import_staging_prepare_script(&nested_stage);
        assert!(prepare.contains(" /run/temps-source-import "));
        assert!(prepare.contains("rebuild this sandbox before importing"));

        let local_prepare = source_import_staging_prepare_script(&local_stage);
        assert!(local_prepare.contains("unavailable for this sandbox backend"));
        assert!(local_prepare.contains("exit 69"));

        let auth_prepare =
            source_import_staging_prepare_script("/run/secrets/.temps-source-auth-42");
        assert!(auth_prepare.contains(" /run/secrets "));
        assert!(auth_prepare.contains("credential storage is unavailable"));

        let bounds = source_import_bounds_script(&nested_stage);
        assert!(bounds.contains("-mindepth 1"));
        assert!(!bounds.contains("-type f"));
    }

    #[test]
    fn source_import_quota_includes_existing_workspace_content() {
        let bounds = source_import_aggregate_bounds_script(
            "/workspace",
            "/run/temps-source-import/.temps-source-import-7",
        );
        assert!(bounds.contains("existing_count + staged_count"));
        assert!(bounds.contains("existing_bytes + staged_bytes"));
        assert!(bounds.contains("aggregate 5000 entry or 256 MiB"));
    }

    #[test]
    fn source_destination_guard_rejects_non_empty_targets() {
        let guard = source_target_guard_script("/workspace", "/workspace/projects/web");
        assert!(guard.contains("source destination must be an empty directory"));
        assert!(guard.contains("exec 9< /workspace/projects/web"));
        assert!(guard.contains("find \"$target_fd\" -mindepth 1"));
        assert!(guard.contains("chmod 700 \"$target_fd\""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_publish_uses_retained_destination_after_symlink_swap() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("temporary source import root");
        let workspace = temp.path().join("workspace");
        let target = workspace.join("projects/web");
        let original = workspace.join("projects/web-original");
        let outside = temp.path().join("outside");
        let staging = temp.path().join("staging");
        std::fs::create_dir_all(&target).expect("source destination");
        std::fs::create_dir_all(&outside).expect("outside directory");
        std::fs::create_dir_all(&staging).expect("staging directory");
        std::fs::write(staging.join("index.js"), b"safe").expect("staged source");

        let guard = source_target_guard_script(
            workspace.to_str().expect("utf-8 workspace"),
            target.to_str().expect("utf-8 target"),
        );
        let finalize = source_import_staging_finalize_script(
            staging.to_str().expect("utf-8 staging directory"),
        );
        let script = format!(
            "set -eu; {guard} mv {target} {original}; ln -s {outside} {target}; {finalize}",
            target = shell_escape_service(target.to_str().expect("utf-8 target")),
            original = shell_escape_service(original.to_str().expect("utf-8 original")),
            outside = shell_escape_service(outside.to_str().expect("utf-8 outside")),
        );
        let status = Command::new("/bin/sh")
            .args(["-c", &script])
            .status()
            .expect("execute source publication script");

        assert!(status.success());
        assert_eq!(
            std::fs::read(original.join("index.js")).expect("published source"),
            b"safe"
        );
        assert!(!outside.join("index.js").exists());
        assert!(std::fs::symlink_metadata(&target)
            .expect("replacement symlink")
            .file_type()
            .is_symlink());
    }

    #[test]
    fn sandbox_data_network_names_reject_docker_name_injection() {
        for invalid in ["", "../shared", "app id", "app/name"] {
            assert!(matches!(
                sandbox_data_network_name(invalid),
                Err(SandboxError::Validation { .. })
            ));
        }
    }

    #[test]
    fn application_workspace_reuse_requires_server_attestation() {
        let host_work_dir = PathBuf::from("/var/lib/temps/ai-applications/app_example");
        let config = ApplicationWorkspaceConfig::default();
        let now = Utc::now();
        let mut managed = sandboxes::Model {
            id: 7,
            public_id: "sbx_deadbeef01234567".to_string(),
            user_id: Some(1),
            agent_run_id: None,
            name: "ai-application:app_example".to_string(),
            status: "running".to_string(),
            image: config.image.clone(),
            work_dir: temps_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            timeout_secs: 3600,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": host_work_dir,
            })),
            backend: Some("docker".to_string()),
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::hours(1),
            preview_password_hash: None,
            preview_password_hint: None,
            lifecycle: "workspace".to_string(),
            project_id: None,
            source_repo_url: None,
        };

        assert!(config.image.is_some(), "the default image must be attested");

        assert!(application_workspace_row_is_attested(
            &managed,
            "app_example",
            &host_work_dir,
            &config
        ));

        managed.image = Some("attacker/custom-image:latest".to_string());
        assert!(!application_workspace_row_is_attested(
            &managed,
            "app_example",
            &host_work_dir,
            &config
        ));
        assert_eq!(
            managed_application_id_from_name("ai-application:app_example"),
            Some("app_example")
        );
        assert_eq!(managed_application_id_from_name("ordinary-sandbox"), None);
    }

    struct NoopJobQueue;

    #[async_trait::async_trait]
    impl JobQueue for NoopJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("preview link tests never subscribe to the job queue")
        }
    }

    fn test_server_config() -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:0".into(),
            database_url: "postgres://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:0".into(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: PathBuf::from("/tmp/temps-sandbox-preview-tests"),
            auth_secret: "default-32-byte-key-for-testing!".into(),
            encryption_key: "another-32-byte-key-for-testing!".into(),
            api_base_url: "/api".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    fn preview_test_service(
        query_results: Vec<Vec<sandboxes::Model>>,
    ) -> (SandboxService, Arc<DatabaseConnection>) {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(query_results)
                .into_connection(),
        );
        let service = preview_test_service_with_db(db.clone());
        (service, db)
    }

    fn preview_test_service_with_db(db: Arc<DatabaseConnection>) -> SandboxService {
        let platform_config = Arc::new(ConfigService::new(
            Arc::new(test_server_config()),
            db.clone(),
        ));
        let git_provider_manager = Arc::new(GitProviderManager::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password("test")),
            Arc::new(NoopJobQueue),
            platform_config.clone(),
        ));
        let registry = Arc::new(StandaloneSandboxRegistry::new(Arc::new(
            temps_agents::sandbox::local::LocalSandboxProvider::new(),
        )));
        SandboxService::new(
            db.clone(),
            registry,
            Arc::new(JobTracker::new()),
            platform_config,
            Arc::new(
                temps_core::CookieCrypto::new("default-32-byte-key-for-testing!")
                    .expect("valid test cookie key"),
            ),
            git_provider_manager,
            PathBuf::from("/tmp/temps-sandbox-preview-tests"),
        )
    }

    #[test]
    fn default_request_is_empty() {
        let r = CreateSandboxRequest::default();
        assert!(r.image.is_none());
        assert!(r.env.is_empty());
        assert!(r.timeout_secs.is_none());
        assert!(r.preview_password.is_none());
    }

    #[test]
    fn request_carries_preview_password() {
        // The field is plumbed through the service input DTO so handlers
        // don't need to reach around the service to wire it in.
        let r = CreateSandboxRequest {
            preview_password: Some("hunter2secret".to_string()),
            ..Default::default()
        };
        assert_eq!(r.preview_password.as_deref(), Some("hunter2secret"));
    }

    /// Destroy must actually target the directory create allocated —
    /// nothing removed it before, so every create/destroy cycle left the
    /// full working tree (node_modules, build output, cloned repos) on the
    /// host and the disk filled up with directories no API could reach.
    #[test]
    fn work_dir_to_remove_targets_the_directory_create_allocated() {
        let root = Path::new("/var/lib/temps/sandboxes");
        let id = "sbx_deadbeef01234567";
        assert_eq!(
            work_dir_to_remove(root, id),
            // Same expression create_sandbox uses to build host_work_dir.
            Some(root.join(id))
        );
    }

    /// The guard in front of a recursive delete. A public id is only ever
    /// generated internally, but if a malformed one ever reached here
    /// `data_root.join()` would happily resolve out of the data dir — an
    /// absolute id replaces the root outright.
    #[test]
    fn work_dir_to_remove_refuses_ids_that_escape_the_data_root() {
        let root = Path::new("/var/lib/temps/sandboxes");
        for bad in [
            "../../etc",
            "sbx_../../etc",
            "/etc",
            "sbx_",
            "sbx_short",
            "sbx_zzzzzzzzzzzzzzzz",
            "",
        ] {
            assert_eq!(
                work_dir_to_remove(root, bad),
                None,
                "{:?} must not resolve to a directory we would recursively delete",
                bad
            );
        }
    }

    /// The cross-crate half of the naming agreement.
    ///
    /// `temps-sandbox` decides the container label; `temps-agents` turns a
    /// container name into a volume name. Nothing at the type level forces
    /// those to line up, and the leak this PR fixes was exactly one identity
    /// derived independently in two places. If either side changes its
    /// scheme without the other, this goes red — and it needs no Docker.
    #[test]
    fn sandbox_service_and_provider_agree_on_volume_naming() {
        let public_id = "sbx_a1b2c3d4e5f60718";
        let label = container_label_for(public_id);
        assert_eq!(label, "a1b2c3d4e5f60718", "label is public_id minus sbx_");

        // What the provider will name the volume for a container built from
        // this label, i.e. what `destroy` will try to remove.
        assert_eq!(
            temps_agents::sandbox::home_volume_name_for_label(label),
            "temps-sandbox-home-v2-a1b2c3d4e5f60718"
        );

        // And it must not be a name from the pre-fix namespace, where a
        // stranded volume from another tenant could still be sitting.
        assert!(!temps_agents::sandbox::home_volume_name_for_label(label)
            .trim_start_matches("temps-sandbox-home-")
            .chars()
            .all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn timeout_constants_are_sane() {
        const _: () = assert!(MIN_TIMEOUT_SECS < DEFAULT_TIMEOUT_SECS);
        const _: () = assert!(DEFAULT_TIMEOUT_SECS < MAX_TIMEOUT_SECS);
        assert_eq!(MAX_TIMEOUT_SECS, 86400);
    }

    fn agent_run_row(run_id: Option<i32>) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            public_id: "sbx_deadbeef01234567".into(),
            user_id: Some(1),
            agent_run_id: run_id,
            name: match run_id {
                Some(id) => format!("temps-sandbox-{}", id),
                None => "sbx-7".into(),
            },
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    fn protected_preview_row(user_id: i32) -> sandboxes::Model {
        let mut row = agent_run_row(None);
        row.user_id = Some(user_id);
        row.preview_password_hash = Some("argon2-current".into());
        row.preview_password_hint = Some("rent".into());
        row
    }

    #[tokio::test]
    async fn preview_share_link_uses_fragment_clamps_ttl_and_records_safe_event() {
        let row = protected_preview_row(1);
        let (service, db) = preview_test_service(vec![vec![row]]);

        let before = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock after epoch")
            .as_secs();
        let (url, expires_at) = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/\\evil.example",
                temps_core::PREVIEW_SESSION_GRANT_MAX_TTL * 30,
            )
            .await
            .expect("protected sandbox should mint a link");

        assert!(url.starts_with(
            "http://ws-deadbeef01234567-3000.localho.st:0/__temps/preview/login?grant=1&next=%2F#session_grant="
        ));
        assert!(!url.contains("?session_grant="));
        assert!(expires_at >= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs());
        assert!(expires_at <= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs() + 1);

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service owns no database references after drop")
            .into_transaction_log();
        let statements = format!("{log:?}");
        assert!(statements.contains("sandbox_events"), "log: {statements}");
        assert!(
            statements.contains("preview_share_link_created"),
            "log: {statements}"
        );
        assert!(!statements.contains("session_grant"), "log: {statements}");
    }

    #[tokio::test]
    async fn preview_share_link_requires_a_preview_password() {
        let row = agent_run_row(None);
        let (service, _) = preview_test_service(vec![vec![row]]);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/pricing",
                Duration::from_secs(60),
            )
            .await
            .expect_err("open sandbox must not receive a non-expiring share link");

        assert!(matches!(error, SandboxError::InvalidState { .. }));
        assert!(error.to_string().contains("set a preview password first"));
    }

    #[tokio::test]
    async fn preview_share_link_hides_cross_user_sandboxes() {
        let row = protected_preview_row(2);
        let (service, _) = preview_test_service(vec![vec![row]]);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("another user's sandbox must stay hidden");

        assert!(matches!(error, SandboxError::NotFound { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_rejects_zero_port_before_database_access() {
        let (service, _) = preview_test_service(Vec::new());

        let error = service
            .preview_share_link("sbx_deadbeef01234567", 1, 0, "/", Duration::from_secs(60))
            .await
            .expect_err("port zero must be rejected");

        assert!(matches!(error, SandboxError::Validation { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_preserves_database_errors() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "preview lookup unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = preview_test_service_with_db(db);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("database failures must not be collapsed into not-found");

        assert!(matches!(error, SandboxError::Database(_)));
        assert!(error.to_string().contains("preview lookup unavailable"));
    }

    /// Lifecycle ops on agent-run rows must be rejected with a typed
    /// error naming both the sandbox and the run — pre-fix, pause/resume/
    /// restart went through the standalone registry, missed the run's
    /// `temps-sandbox-<run_id>` container, and let the DB row drift from
    /// the real container state.
    #[test]
    fn ensure_not_agent_run_rejects_attributed_rows() {
        let row = agent_run_row(Some(42));
        let err = ensure_not_agent_run(&row).expect_err("agent-run row must be rejected");
        match err {
            SandboxError::ManagedByAgentRun { sandbox_id, run_id } => {
                assert_eq!(sandbox_id, "sbx_deadbeef01234567");
                assert_eq!(run_id, 42);
            }
            other => panic!("expected ManagedByAgentRun, got {:?}", other),
        }
    }

    #[test]
    fn ensure_not_agent_run_allows_standalone_rows() {
        let row = agent_run_row(None);
        assert!(ensure_not_agent_run(&row).is_ok());
    }

    /// Terminal decision shared by `destroy_sandbox` (agent-run branch)
    /// and `release_orphaned_agent_run_sandboxes`: every terminal status
    /// allows cleanup, every active status keeps the run's ownership, and
    /// a missing run row (deleted run) counts as terminal so the sandbox
    /// isn't orphaned forever.
    #[test]
    fn run_status_is_terminal_matches_run_lifecycle() {
        for s in TERMINAL_RUN_STATUSES {
            assert!(run_status_is_terminal(Some(s)), "{} must be terminal", s);
        }
        for s in temps_agents::services::run_service::ACTIVE_RUN_STATUSES {
            assert!(
                !run_status_is_terminal(Some(s)),
                "{} must NOT be terminal",
                s
            );
        }
        assert!(
            run_status_is_terminal(None),
            "missing run row must count as terminal so cleanup can proceed"
        );
    }

    #[test]
    fn summary_from_model_copies_fields() {
        let now = Utc::now();
        let m = sandboxes::Model {
            id: 1_000_042,
            public_id: "sbx_abc1234567890def".into(),
            user_id: Some(7),
            agent_run_id: Some(42),
            name: "my-sbx".into(),
            status: "running".into(),
            image: Some("node:20".into()),
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        };
        let s = SandboxSummary::from(&m);
        assert_eq!(s.public_id, "sbx_abc1234567890def");
        assert_eq!(s.status, "running");
        assert_eq!(s.image.as_deref(), Some("node:20"));
        assert_eq!(s.agent_run_id, Some(42));
    }

    /// `extend_timeout` deliberately pushes `expires_at` past the idle window
    /// and leaves `timeout_secs` alone. `touch` then runs on the very next
    /// exec — the long operation the extension was bought for — so if it
    /// assigned the idle deadline unconditionally it would silently cancel the
    /// extension the caller just paid for. The guard has to live in the SQL,
    /// because `touch` must stay a single statement with no read-modify-write
    /// on the exec hot path.
    #[tokio::test]
    async fn touch_never_moves_the_deadline_backwards() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = preview_test_service_with_db(db.clone());

        service.touch(42, 3600).await;

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service owns no database references after drop")
            .into_transaction_log();
        let statements = format!("{log:?}");

        assert!(
            statements.contains("CASE WHEN"),
            "touch must guard the deadline in SQL so an extendTimeout() is \
             never silently undone; log: {statements}"
        );
        assert!(statements.contains("expires_at"), "log: {statements}");
        assert!(
            statements.contains("last_activity_at"),
            "activity is always recorded, only the deadline is guarded; \
             log: {statements}"
        );
    }
}

/// Service-level tests for the storage-cleanup paths.
///
/// These exist because the unit tests above only cover the *guard*
/// (`work_dir_to_remove`), not the call sites — and the call sites are where
/// the bug was. Deleting the `remove_work_dir` call from `destroy_sandbox`,
/// or from either create-failure arm, used to leave every test green.
///
/// No Docker daemon required: the provider is faked and the database is
/// `MockDatabase`. The work dir is real, under a unique temp directory, so
/// a `remove_dir_all` → `remove_dir` regression (non-recursive) fails here.
#[cfg(test)]
mod storage_cleanup_tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use temps_agents::error::AgentError;
    use temps_agents::sandbox::{
        SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider,
    };

    /// Minimal provider. Each knob corresponds to a failure the service is
    /// supposed to clean up after.
    struct FakeProvider {
        /// `create` fails — the provider-failure arm of `create_sandbox`.
        fail_create: bool,
        /// `exec` returns a non-zero exit — the seeding-failure arm.
        fail_exec: bool,
        /// `destroy` errors — the case where the container may still be
        /// running and we delete the work dir anyway.
        fail_destroy: bool,
        /// `start` errors — the wake-on-access failure path.
        fail_start: bool,
        /// `configure_application_network` errors before compute may start.
        fail_network_config: bool,
        destroys: AtomicUsize,
        creates: Arc<AtomicUsize>,
        starts: Arc<AtomicUsize>,
        lifecycle_calls: Arc<Mutex<Vec<&'static str>>>,
        /// Provider truth can differ from the database after an out-of-band
        /// container kill. `start` flips this back to true so the complete
        /// wake-and-retry path can be exercised without Docker.
        alive: Arc<AtomicBool>,
    }

    impl FakeProvider {
        fn new() -> Self {
            Self {
                fail_create: false,
                fail_exec: false,
                fail_destroy: false,
                fail_start: false,
                fail_network_config: false,
                destroys: AtomicUsize::new(0),
                creates: Arc::new(AtomicUsize::new(0)),
                starts: Arc::new(AtomicUsize::new(0)),
                lifecycle_calls: Arc::new(Mutex::new(Vec::new())),
                alive: Arc::new(AtomicBool::new(true)),
            }
        }
    }

    fn handle_for(name: &str) -> SandboxHandle {
        SandboxHandle {
            sandbox_id: format!("docker-id-{}", name),
            sandbox_name: format!("temps-sandbox-{}", name),
            work_dir: PathBuf::from("/workspace"),
            backend: SandboxBackend::Docker,
            image: String::new(),
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeProvider {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            if self.fail_create {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "fake".into(),
                    reason: "image pull failed".into(),
                });
            }
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.alive.store(true, Ordering::SeqCst);
            Ok(handle_for(
                config.container_name_override.as_deref().unwrap_or("x"),
            ))
        }

        async fn exec(
            &self,
            _handle: &SandboxHandle,
            _cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<temps_agents::ai_cli::OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            Ok(SandboxExecResult {
                exit_code: if self.fail_exec { 1 } else { 0 },
                stdout: String::new(),
                stderr: if self.fail_exec {
                    "clone failed".into()
                } else {
                    String::new()
                },
            })
        }

        async fn is_alive(&self, _handle: &SandboxHandle) -> Result<bool, AgentError> {
            Ok(self.alive.load(Ordering::SeqCst))
        }

        async fn configure_application_network(
            &self,
            handle: &SandboxHandle,
            _network_name: &str,
            _service_containers: &[String],
        ) -> Result<(), AgentError> {
            self.lifecycle_calls
                .lock()
                .expect("lifecycle call mutex")
                .push("configure");
            if self.fail_network_config {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!("network reconciliation failed for {}", handle.sandbox_name),
                });
            }
            Ok(())
        }

        async fn write_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn read_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            Ok(Vec::new())
        }

        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            _pattern: &str,
            _signal: temps_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn destroy(
            &self,
            handle: &SandboxHandle,
            _purge_volumes: bool,
        ) -> Result<(), AgentError> {
            self.destroys.fetch_add(1, Ordering::SeqCst);
            if self.fail_destroy {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!(
                        "daemon unreachable while destroying {}",
                        handle.sandbox_name
                    ),
                });
            }
            Ok(())
        }

        /// The trait's default `start` returns "not supported", so the
        /// wake path needs a real implementation here.
        async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
            self.lifecycle_calls
                .lock()
                .expect("lifecycle call mutex")
                .push("start");
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.fail_start {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!("daemon unreachable while starting {}", handle.sandbox_name),
                });
            }
            self.alive.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }

        async fn recover_by_name(
            &self,
            container_name: &str,
        ) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(Some(handle_for(container_name)))
        }

        fn name(&self) -> &str {
            "fake"
        }

        async fn is_available(&self) -> bool {
            true
        }

        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".into()))
        }

        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".into())
        }
    }

    /// A `ServerConfig` built by struct literal rather than `ServerConfig::new`,
    /// which has filesystem side effects (creates `~/.temps` and writes an
    /// auth secret + encryption key). A test must not touch the developer's
    /// real data dir.
    fn test_server_config(data_dir: PathBuf) -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:3000".into(),
            database_url: "postgresql://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:3001".into(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir,
            auth_secret: "test-auth-secret".into(),
            encryption_key: "0123456789abcdef0123456789abcdef\
                             0123456789abcdef0123456789abcdef"
                .into(),
            api_base_url: "http://127.0.0.1:3000".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    struct PanicOnSendJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for PanicOnSendJobQueue {
        async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            panic!("no job should be queued in these tests, got: {job:?}");
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            panic!("subscribe not needed for these tests")
        }
    }

    /// A unique data root per test so concurrent tests can't collide.
    fn unique_data_root(tag: &str) -> PathBuf {
        let uniq = std::process::id() as u64
            + std::sync::atomic::AtomicU64::new(0).fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("temps-sbx-test-{}-{}", tag, uniq))
    }

    fn build_service(
        db: sea_orm::DatabaseConnection,
        provider: FakeProvider,
        data_root: PathBuf,
    ) -> (Arc<SandboxService>, Arc<StandaloneSandboxRegistry>) {
        let db = Arc::new(db);
        let registry = Arc::new(StandaloneSandboxRegistry::new(
            Arc::new(provider) as Arc<dyn SandboxProvider>
        ));
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(test_server_config(data_root.clone())),
            db.clone(),
        ));
        let encryption = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key"),
        );
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption,
            Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
            config.clone(),
        ));
        let cookie_crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test cookie key"),
        );
        let service = Arc::new(SandboxService::new(
            db,
            registry.clone(),
            Arc::new(JobTracker::new()),
            config,
            cookie_crypto,
            git,
            data_root,
        ));
        (service, registry)
    }

    fn row(public_id: &str, agent_run_id: Option<i32>) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            public_id: public_id.to_string(),
            user_id: Some(1),
            agent_run_id,
            name: "sbx-7".into(),
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::seconds(3600),
            preview_password_hash: None,
            preview_password_hint: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    /// Seed a work dir with nested, non-empty content — a shallow
    /// `remove_dir` would fail on this, a recursive delete succeeds.
    fn seed_work_dir(data_root: &Path, public_id: &str) -> PathBuf {
        let dir = data_root.join(public_id);
        let nested = dir.join("node_modules").join("pkg");
        std::fs::create_dir_all(&nested).expect("seed work dir");
        std::fs::write(nested.join("index.js"), b"module.exports = {}").expect("seed file");
        dir
    }

    const PUBLIC_ID: &str = "sbx_deadbeef01234567";

    fn attested_application_workspace(
        status: &str,
        application_work_dir: &Path,
    ) -> sandboxes::Model {
        sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            status: status.to_string(),
            lifecycle: "workspace".to_string(),
            work_dir: temps_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            image: ApplicationWorkspaceConfig::default().image,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": application_work_dir,
            })),
            ..row(PUBLIC_ID, None)
        }
    }

    #[tokio::test]
    async fn stopped_application_workspace_reconciles_network_before_start() {
        let data_root = unique_data_root("application-network-before-start");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let stopped = attested_application_workspace("stopped", &application_work_dir);
        let running = attested_application_workspace("running", &application_work_dir);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Existing workspace lookup and the re-read used for network
            // preparation, then the authorized project's empty topology.
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            // Resume re-reads the stopped row.
            .append_query_results([vec![stopped]])
            // Resume update returns the running row.
            .append_query_results([vec![running.clone()]])
            // Resume bookkeeping and Git bootstrap re-resolve the now-running
            // workspace through the same durable identity.
            .append_query_results([vec![running.clone()]])
            .append_query_results([vec![running.clone()]])
            .append_query_results([vec![running]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let provider = FakeProvider::new();
        let lifecycle_calls = provider.lifecycle_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        service
            .get_or_create_application_workspace_with_config(
                1,
                "app_example",
                None,
                application_work_dir,
                ApplicationWorkspaceConfig::default(),
                &[42],
            )
            .await
            .expect("network reconciliation must complete before the stopped workspace starts");

        assert_eq!(
            *lifecycle_calls.lock().expect("lifecycle call mutex"),
            vec!["configure", "start"]
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn failed_network_reconciliation_never_starts_stopped_application_workspace() {
        let data_root = unique_data_root("application-network-failure");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let stopped = attested_application_workspace("stopped", &application_work_dir);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stopped.clone()]])
            .append_query_results([vec![stopped]])
            .append_query_results([Vec::<project_services::Model>::new()])
            .into_connection();
        let provider = FakeProvider {
            fail_network_config: true,
            ..FakeProvider::new()
        };
        let starts = provider.starts.clone();
        let lifecycle_calls = provider.lifecycle_calls.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .get_or_create_application_workspace_with_config(
                1,
                "app_example",
                None,
                application_work_dir,
                ApplicationWorkspaceConfig::default(),
                &[42],
            )
            .await
            .expect_err("failed network reconciliation must abort workspace preparation");

        assert!(error.to_string().contains("network reconciliation failed"));
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert_eq!(
            *lifecycle_calls.lock().expect("lifecycle call mutex"),
            vec!["configure"]
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn application_workspace_fails_when_in_sandbox_git_bootstrap_fails() {
        let data_root = unique_data_root("application-git-bootstrap");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let existing = sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            lifecycle: "workspace".to_string(),
            work_dir: temps_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            image: ApplicationWorkspaceConfig::default().image,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": application_work_dir,
            })),
            ..row(PUBLIC_ID, None)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Existing application sandbox lookup.
            .append_query_results([vec![existing.clone()]])
            // Network preparation re-reads the row and resolves the empty
            // authorized service topology before Git bootstrap may execute.
            .append_query_results([vec![existing.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            // `exec` ownership lookup.
            .append_query_results([vec![existing]])
            // Activity touch performed before provider exec.
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let mut provider = FakeProvider::new();
        provider.fail_exec = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let error = service
            .get_or_create_application_workspace(1, "app_example", None, application_work_dir)
            .await
            .expect_err("a failed Git bootstrap must fail workspace preparation");

        assert!(matches!(error, SandboxError::ExecFailed { .. }));
        assert!(error
            .to_string()
            .contains("initialize persistent Git workspace"));
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn application_workspace_rebuilds_killed_provider_before_git_bootstrap() {
        let data_root = unique_data_root("application-provider-recovery");
        std::fs::create_dir_all(&data_root).expect("data root");
        let application_work_dir = data_root.join("app_example");
        let existing = sandboxes::Model {
            name: "ai-application:app_example".to_string(),
            lifecycle: "workspace".to_string(),
            work_dir: temps_agents::sandbox::SANDBOX_WORK_DIR.to_string(),
            image: ApplicationWorkspaceConfig::default().image,
            metadata: Some(serde_json::json!({
                "managed_application_id": "app_example",
                "managed_host_work_dir": application_work_dir,
            })),
            ..row(PUBLIC_ID, None)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Existing application sandbox lookup.
            .append_query_results([vec![existing.clone()]])
            // Rebuild lookup and update return the same durable identity.
            .append_query_results([vec![existing.clone()]])
            .append_query_results([vec![existing.clone()]])
            // Rebuild bookkeeping and network preparation re-read the durable
            // row, then resolve no data services for the empty authorized set.
            .append_query_results([vec![existing.clone()]])
            .append_query_results([vec![existing.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            // Git-bootstrap exec ownership lookup sees the rebuilt row.
            .append_query_results([vec![existing]])
            // Activity touch performed before provider exec.
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let creates = provider.creates.clone();
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let sandbox = service
            .get_or_create_application_workspace(1, "app_example", None, application_work_dir)
            .await
            .expect("application preparation must wake its stopped provider runtime");

        assert_eq!(sandbox.public_id, PUBLIC_ID);
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "Git bootstrap must be preceded by exactly one provider rebuild"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// A row in the given lifecycle class and status. Used by the
    /// workspace wake tests below.
    fn row_with(public_id: &str, lifecycle: &str, status: &str) -> sandboxes::Model {
        sandboxes::Model {
            status: status.to_string(),
            lifecycle: lifecycle.to_string(),
            ..row(public_id, None)
        }
    }

    // ── Lifecycle class (ADR-036) ────────────────────────────────────────

    #[test]
    fn lifecycle_parses_both_public_values() {
        assert_eq!(
            SandboxLifecycle::parse("ephemeral").expect("ephemeral is valid"),
            SandboxLifecycle::Ephemeral
        );
        assert_eq!(
            SandboxLifecycle::parse("workspace").expect("workspace is valid"),
            SandboxLifecycle::Workspace
        );
    }

    #[test]
    fn lifecycle_rejects_unknown_values_with_a_helpful_message() {
        // Silently falling back to ephemeral would cost the caller their
        // work at the first idle sweep, so a typo must be a 400.
        let err = SandboxLifecycle::parse("workspaces").expect_err("typo must not be accepted");
        let msg = err.to_string();
        assert!(msg.contains("workspaces"), "msg: {}", msg);
        assert!(msg.contains("ephemeral"), "must list valid values: {}", msg);
        assert!(msg.contains("workspace"), "must list valid values: {}", msg);
    }

    #[test]
    fn lifecycle_from_row_treats_unrecognised_values_as_ephemeral() {
        // Forward-compat: a row written by a newer binary and then rolled
        // back must not auto-start containers this binary doesn't
        // understand. Ephemeral is the conservative reading.
        let unknown = row_with(PUBLIC_ID, "hibernating", "stopped");
        assert_eq!(
            SandboxLifecycle::from_row(&unknown),
            SandboxLifecycle::Ephemeral
        );
    }

    #[test]
    fn lifecycle_default_is_ephemeral() {
        // Every existing caller and SDK client omits the field.
        assert_eq!(SandboxLifecycle::default(), SandboxLifecycle::Ephemeral);
        assert_eq!(SandboxLifecycle::default().as_str(), "ephemeral");
    }

    #[test]
    fn idle_deadline_is_now_plus_timeout() {
        let now = Utc::now();
        assert_eq!(idle_deadline(now, 3600), now + chrono::Duration::hours(1));
        // Degenerate but representable: a zero timeout is already expired
        // rather than infinite. The service clamps to >= 60 before this
        // is reached, so this only pins the arithmetic.
        assert_eq!(idle_deadline(now, 0), now);
    }

    #[tokio::test]
    async fn resolve_id_wakes_a_stopped_workspace() {
        let data_root = unique_data_root("wake-workspace");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id → a suspended workspace
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "stopped")]])
            // wake_workspace's status update (RETURNING the running row)
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "running")]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let (row, id) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a stopped workspace must wake, not error");

        assert_eq!(id, 7);
        assert_eq!(
            row.status, "running",
            "the caller must receive the woken row, not the stale stopped one"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_still_rejects_a_stopped_ephemeral_sandbox() {
        // `@vercel/sandbox` consumers rely on a stopped sandbox staying
        // stopped. Waking one under them would restart billing on a
        // container they deliberately shut down.
        let data_root = unique_data_root("no-wake-ephemeral");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "ephemeral", "stopped")]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let err = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect_err("ephemeral sandboxes must not auto-wake");

        match err {
            SandboxError::InvalidState { state, .. } => assert_eq!(state, "stopped"),
            other => panic!("expected InvalidState, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_passes_through_a_running_workspace_without_starting_it() {
        // A running workspace is the common case and must not pay for the
        // wake path. Proven with a provider whose `start` always fails: if
        // the resolve path touched it, this would error.
        let data_root = unique_data_root("running-workspace");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "running")]])
            .into_connection();

        let provider = FakeProvider {
            fail_start: true,
            ..FakeProvider::new()
        };
        let (service, _) = build_service(db, provider, data_root.clone());
        let (row, _) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a running workspace resolves without a provider start");
        assert_eq!(row.status, "running");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_wakes_workspace_when_row_is_running_but_provider_is_stopped() {
        // Reproduces an application harness hitting its hard deadline: the
        // provider stops the Docker container to kill the orphaned process,
        // but the standalone sandbox row remains `running`. The next Git
        // bootstrap/exec must recover the workspace instead of failing before
        // the harness-level recovery path can run.
        let data_root = unique_data_root("wake-stale-running-workspace");
        let stale_row = row_with(PUBLIC_ID, "workspace", "running");
        let recovered_row = stale_row.clone();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // resolve_id ownership lookup returns the stale DB state.
            .append_query_results([vec![stale_row]])
            // wake_workspace status reconciliation returns a live row.
            .append_query_results([vec![recovered_row]])
            .into_connection();

        let provider = FakeProvider::new();
        provider.alive.store(false, Ordering::SeqCst);
        let starts = provider.starts.clone();
        let (service, _) = build_service(db, provider, data_root.clone());

        let (row, id) = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect("a provider-stopped workspace must wake despite a stale running row");

        assert_eq!(id, 7);
        assert_eq!(row.status, "running");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "provider start must run exactly once"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn resolve_id_surfaces_a_failed_wake_instead_of_swallowing_it() {
        // A workspace that silently fails to wake and then reports
        // "not found" on every later call is exactly the dead end a
        // self-hosted user can't debug. The provider error must reach them.
        let data_root = unique_data_root("wake-fails");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with(PUBLIC_ID, "workspace", "stopped")]])
            .into_connection();

        let provider = FakeProvider {
            fail_start: true,
            ..FakeProvider::new()
        };
        let (service, _) = build_service(db, provider, data_root.clone());
        let err = service
            .resolve_id(PUBLIC_ID, 1)
            .await
            .expect_err("a provider start failure must not be swallowed");

        let msg = err.to_string();
        assert!(
            msg.contains("daemon unreachable"),
            "the provider's reason must reach the caller: {}",
            msg
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Workspace-from-project (ADR-036 §5) ──────────────────────────────

    fn project_row(git_url: Option<&str>) -> projects::Model {
        let now = Utc::now();
        projects::Model {
            id: 12,
            name: "acme-api".into(),
            repo_name: "api".into(),
            repo_owner: "acme".into(),
            directory: String::new(),
            main_branch: "main".into(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "acme-api".into(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: true,
            git_url: git_url.map(str::to_string),
            git_provider_connection_id: Some(3),
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_api_traffic_summary_enabled: None,
            allow_alternate_sources: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 0,
            preview_envs_wake_timeout_seconds: 0,
            source_type: Default::default(),
            project_type: temps_entities::types::ProjectType::Server,
            template_slug: None,
            service_template: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: false,
            image_retention_hours: None,
        }
    }

    #[tokio::test]
    async fn source_from_project_uses_the_projects_repo_branch_and_connection() {
        let data_root = unique_data_root("project-source-ok");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_row(Some("https://github.com/acme/api.git"))]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let source = service
            .source_from_project(12)
            .await
            .expect("a connected project yields a git source");

        match source.expect("a Git project has a seed source") {
            SandboxSource::Git {
                url,
                revision,
                depth,
                git_connection_id,
                username,
                password,
                destination,
                strip_git_metadata,
            } => {
                assert_eq!(url, "https://github.com/acme/api.git");
                assert_eq!(revision.as_deref(), Some("main"));
                assert!(
                    depth.is_none(),
                    "a workspace gets full history — you cannot rebase in a shallow clone"
                );
                assert_eq!(git_connection_id, Some(3));
                assert!(destination.is_none());
                assert!(!strip_git_metadata);
                assert!(
                    username.is_none() && password.is_none(),
                    "credentials must resolve server-side from the connection, \
                     never be materialised here"
                );
            }
            other => panic!("expected a git source, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn source_from_project_errors_when_the_project_has_no_repo() {
        let data_root = unique_data_root("project-no-repo");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_row(None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let err = service
            .source_from_project(12)
            .await
            .expect_err("a project with no git_url cannot seed a workspace");

        match err {
            SandboxError::ProjectHasNoRepo { project_id, name } => {
                assert_eq!(project_id, 12);
                assert_eq!(name, "acme-api");
            }
            other => panic!("expected ProjectHasNoRepo, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn source_from_project_allows_an_empty_workspace_for_manual_projects() {
        let data_root = unique_data_root("manual-project-no-repo");
        let mut manual_project = project_row(None);
        manual_project.source_type = temps_entities::source_type::SourceType::Manual;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![manual_project]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let source = service
            .source_from_project(12)
            .await
            .expect("a manual project may start with an empty workspace");

        assert!(source.is_none());
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn source_from_project_errors_when_the_project_is_missing() {
        let data_root = unique_data_root("project-missing");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let err = service
            .source_from_project(999)
            .await
            .expect_err("a missing project is a 404, not a silent empty workspace");

        assert!(matches!(
            err,
            SandboxError::ProjectNotFound { project_id: 999 }
        ));
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Resolved-source validation ───────────────────────────────────────

    #[test]
    fn all_embedded_userinfo_is_rejected() {
        assert!(url_has_embedded_credentials(
            "https://user:secret@github.com/org/repo.git"
        ));
        // A bare user can itself be a token or sensitive account identifier.
        assert!(url_has_embedded_credentials(
            "https://user@github.com/org/repo.git"
        ));
        assert!(!url_has_embedded_credentials(
            "https://github.com/org/repo.git"
        ));
        // An `@` in the *path* must not be mistaken for userinfo, or every
        // pinned-ref URL would be rejected as leaking a password.
        assert!(!url_has_embedded_credentials(
            "https://github.com/org/repo@v1.git"
        ));
        assert!(!url_has_embedded_credentials("git@github.com:org/repo.git"));
        // A `:` and `@` in the query or fragment are not userinfo either —
        // userinfo always precedes the first `/`, `?` or `#`. Splitting on
        // `/` alone read this as `user:password` and rejected a valid URL.
        assert!(!url_has_embedded_credentials(
            "https://github.com/org/repo.git?ref=a:b@c"
        ));
        assert!(!url_has_embedded_credentials("https://host?ref=a:b@c"));
        assert!(!url_has_embedded_credentials("https://host#frag=a:b@c"));
        // ...but real userinfo before a query is still caught.
        assert!(url_has_embedded_credentials(
            "https://user:pw@github.com/org/repo.git?ref=main"
        ));
    }

    #[test]
    fn credentials_only_enter_temps_managed_sandbox_images() {
        let mut managed = handle_for("managed");
        managed.image = "ghcr.io/gotempsh/temps-sandbox-node:v1".to_string();
        assert!(sandbox_image_is_trusted_for_credentials(&managed));

        let mut custom = handle_for("custom");
        custom.image = "registry.example/customer/image:latest".to_string();
        assert!(!sandbox_image_is_trusted_for_credentials(&custom));

        let mut local = handle_for("local");
        local.backend = SandboxBackend::Local;
        local.image = managed.image;
        assert!(!sandbox_image_is_trusted_for_credentials(&local));
    }

    #[test]
    fn privileged_imports_clear_image_startup_hooks() {
        let mut environment = HashMap::from([
            ("LD_PRELOAD".to_string(), "/workspace/steal.so".to_string()),
            ("BASH_ENV".to_string(), "/workspace/startup.sh".to_string()),
            ("GIT_PASS".to_string(), "ephemeral".to_string()),
        ]);

        sanitize_privileged_import_environment(&mut environment);

        assert_eq!(environment.get("LD_PRELOAD").map(String::as_str), Some(""));
        assert_eq!(environment.get("BASH_ENV").map(String::as_str), Some(""));
        assert_eq!(
            environment.get("GIT_PASS").map(String::as_str),
            Some("ephemeral")
        );
        for name in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
            assert_eq!(
                environment.get(name).map(String::as_str),
                Some("/var/empty/temps-source-import")
            );
        }
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "GIT_CONFIG_PARAMETERS",
            "GIT_SSL_CAINFO",
        ] {
            assert_eq!(environment.get(name).map(String::as_str), Some(""));
        }
        assert_eq!(environment.get("NO_PROXY").map(String::as_str), Some("*"));
        assert_eq!(environment.get("no_proxy").map(String::as_str), Some("*"));
        assert_eq!(
            environment.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            environment.get("GIT_SSL_NO_VERIFY").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn source_network_pin_formats_curl_resolve_values() {
        let ipv4 = SourceNetworkPin {
            host: "git.example".to_string(),
            port: 443,
            address: "203.0.113.8".parse().expect("IPv4 fixture"),
        };
        assert_eq!(ipv4.curl_resolve_value(), "git.example:443:203.0.113.8");

        let ipv6 = SourceNetworkPin {
            host: "git.example".to_string(),
            port: 443,
            address: "2001:db8::8".parse().expect("IPv6 fixture"),
        };
        assert_eq!(ipv6.curl_resolve_value(), "git.example:443:[2001:db8::8]");
    }

    #[test]
    fn tarball_import_disables_image_curl_configuration_before_other_flags() {
        let command = tarball_extract_command(
            "https://downloads.example/source.tar.gz",
            "/tmp/staging",
            None,
        );

        assert!(
            command.contains(" curl -q "),
            "unexpected command: {command}"
        );
        assert!(command.starts_with("env -i "));
        assert!(command.contains("--noproxy '*' --proxy ''"));
        assert!(command.contains("--max-redirs 0"));
        assert!(command.contains("| env -i "));
    }

    #[test]
    fn git_import_uses_an_allowlisted_environment_without_secret_values() {
        let environment = clean_git_environment(
            "/root/.temps-source-auth-7",
            Some("/root/.temps-source-auth-7/askpass.sh"),
        );

        assert!(environment.starts_with("env -i "));
        assert!(environment.contains("GIT_CONFIG_COUNT=0"));
        assert!(environment.contains("GIT_SSL_NO_VERIFY=false"));
        assert!(environment.contains("GIT_ASKPASS="));
        assert!(!environment.contains("GIT_PASS="));
        assert!(!environment.contains("GIT_USER="));
        assert!(!environment.contains("GIT_TRACE"));
    }

    fn git_provider(
        provider_type: &str,
        base_url: Option<&str>,
    ) -> temps_entities::git_providers::Model {
        let now = Utc::now();
        temps_entities::git_providers::Model {
            id: 7,
            name: "fixture".to_string(),
            provider_type: provider_type.to_string(),
            base_url: base_url.map(str::to_string),
            api_url: None,
            auth_method: "pat".to_string(),
            auth_config: serde_json::json!({}),
            webhook_secret: None,
            is_active: true,
            is_default: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn stored_git_credentials_are_bound_to_the_provider_origin() {
        let github = git_provider("github", None);
        assert!(
            validate_connection_clone_origin("https://github.com/acme/repo.git", &github).is_ok()
        );
        assert!(validate_connection_clone_origin(
            "https://credentials.example/acme/repo.git",
            &github
        )
        .is_err());

        let enterprise = git_provider("github", Some("https://git.example.test:8443"));
        assert!(validate_connection_clone_origin(
            "https://git.example.test:8443/acme/repo.git",
            &enterprise
        )
        .is_ok());
        assert!(validate_connection_clone_origin(
            "https://git.example.test/acme/repo.git",
            &enterprise
        )
        .is_err());
    }

    #[tokio::test]
    async fn resolved_source_rejects_embedded_credentials() {
        // This is the project-derived path's guard: a legacy `projects.git_url`
        // carrying a password would otherwise be persisted to
        // `source_repo_url` and echoed back out of the API.
        let source = SandboxSource::Git {
            url: "https://user:secret@github.com/org/repo.git".into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        let err = validate_resolved_source(&source)
            .await
            .expect_err("credentials in the url must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("embedded credentials"), "msg: {msg}");
        // The message must not echo the secret back at the caller.
        assert!(!msg.contains("secret"), "error leaked the password: {msg}");
    }

    #[tokio::test]
    async fn resolved_source_rejects_loopback_and_metadata_hosts() {
        for url in [
            "http://127.0.0.1/repo.git",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/repo.git",
        ] {
            let source = SandboxSource::Git {
                url: url.into(),
                revision: None,
                depth: None,
                username: None,
                password: None,
                git_connection_id: None,
                destination: None,
                strip_git_metadata: false,
            };
            assert!(
                validate_resolved_source(&source).await.is_err(),
                "{url} must be rejected as an SSRF target"
            );
        }
    }

    #[tokio::test]
    async fn resolved_source_rejects_non_http_schemes() {
        let source = SandboxSource::Tarball {
            url: "file:///etc/passwd".into(),
        };
        assert!(validate_resolved_source(&source).await.is_err());
    }

    #[tokio::test]
    async fn destroy_sandbox_removes_the_work_dir() {
        let data_root = unique_data_root("destroy-ok");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // record_event insert (RETURNING)
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            // mark_destroyed update (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        service
            .destroy_sandbox(PUBLIC_ID, 1)
            .await
            .expect("destroy should succeed");

        assert!(
            !dir.exists(),
            "destroy must remove the work dir {} — nothing else ever will, \
             so leaving it is a permanent leak",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The documented tradeoff: with no background sweep, skipping cleanup
    /// when the provider destroy fails would leak the directory forever, so
    /// we delete regardless. If someone reintroduces a `container_gone`
    /// guard around the call, this goes red.
    #[tokio::test]
    async fn destroy_sandbox_removes_the_work_dir_even_when_provider_destroy_fails() {
        let data_root = unique_data_root("destroy-fail");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_destroy = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        // The user must still be able to delete a sandbox when Docker is
        // unhappy — the call succeeds and the row is marked destroyed.
        service
            .destroy_sandbox(PUBLIC_ID, 1)
            .await
            .expect("destroy must succeed even when the provider fails");

        assert!(
            !dir.exists(),
            "work dir {} must be removed even when the provider destroy \
             failed — there is no sweeper to reclaim it later",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// Agent-run sandboxes take an early return through
    /// `release_for_agent_run`, and their work dir belongs to the executor,
    /// not to `data_root`. If that early return is ever lost, this test
    /// catches it before it recursively deletes a live run's workspace.
    #[tokio::test]
    async fn destroy_sandbox_for_agent_run_leaves_the_work_dir_alone() {
        let data_root = unique_data_root("agent-run");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id → row owned by agent run 42
            .append_query_results([vec![row(PUBLIC_ID, Some(42))]])
            // agent_runs lookup → empty, i.e. run is gone ⇒ terminal
            .append_query_results([Vec::<agent_runs::Model>::new()])
            // release_for_agent_run: rows for this run
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let _ = service.destroy_sandbox(PUBLIC_ID, 1).await;

        assert!(
            dir.exists(),
            "an agent-run sandbox must not have {} deleted — that directory \
             is owned by the executor, and this path is only for standalone \
             sandboxes",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn create_sandbox_provider_failure_leaves_no_work_dir() {
        let data_root = unique_data_root("create-fail");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // insert of the new row (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed update
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_create = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .create_sandbox(1, CreateSandboxRequest::default())
            .await
            .expect_err("provider create failed, so create_sandbox must fail");
        assert!(
            matches!(err, SandboxError::CreateFailed { .. }),
            "{:?}",
            err
        );

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed create must not strand a work dir; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The costly one: seeding runs after the clone, so by the time it fails
    /// the directory can hold an entire repository, and the row is already
    /// destroyed so no later `destroy_sandbox` can reach it.
    #[tokio::test]
    async fn create_sandbox_seed_failure_leaves_no_work_dir() {
        let data_root = unique_data_root("seed-fail");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // insert of the new row
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // update after create (status/metadata write-back)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_exec = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let req = CreateSandboxRequest {
            source: Some(SandboxSource::Tarball {
                // A public IP literal keeps this cleanup regression test
                // deterministic: source validation succeeds without DNS and
                // the fake provider remains the intended seed failure.
                url: "https://93.184.216.34/src.tar.gz".into(),
            }),
            ..Default::default()
        };
        let err = service
            .create_sandbox(1, req)
            .await
            .expect_err("seeding failed, so create_sandbox must fail");
        assert!(matches!(err, SandboxError::ExecFailed { .. }), "{:?}", err);

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed seed must not strand a work dir holding a cloned \
             repository; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Major 5: from_snapshot routing ───────────────────────────────────────

    /// When `CreateSandboxRequest.from_snapshot_artifact` is `Some`, the
    /// service routes to `registry.create_from_snapshot` instead of
    /// `registry.create`. FakeProvider doesn't override `create_from_snapshot`
    /// so it returns the trait-default "not supported" error — the test
    /// confirms the routing by observing that error rather than a normal
    /// create-failure reason.
    #[tokio::test]
    async fn create_sandbox_from_snapshot_routes_to_create_from_snapshot() {
        let data_root = unique_data_root("from-snapshot-route");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // insert of the new row (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed update (RETURNING) — cleanup after create fails
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());

        let artifact = temps_agents::sandbox::SnapshotArtifact {
            content_path: std::path::PathBuf::from("/tmp/test.tar"),
            content_digest: "sha256fake".to_string(),
            primary_digest: "sha256fake".to_string(),
            size_bytes: 1024,
            backend: temps_agents::sandbox::SandboxBackend::Docker,
            image_ref: Some("temps-snapshot/test:v1".to_string()),
            image_id: Some("sha256:fake".to_string()),
            workspace: None,
        };

        let req = CreateSandboxRequest {
            from_snapshot_artifact: Some(artifact),
            project_id: Some(42),
            ..Default::default()
        };

        let err = service
            .create_sandbox(1, req)
            .await
            .expect_err("FakeProvider::create_from_snapshot returns NotSupported, must fail");

        // The error message proves we went through the from-snapshot path, not
        // the normal create path.
        let msg = err.to_string();
        assert!(
            msg.contains("not supported") || msg.contains("from_snapshot"),
            "error must reference the snapshot create path, got: {:?}",
            err
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    // ── Major 5: non-Docker provider trait defaults ───────────────────────────

    /// Providers that don't override `take_snapshot` return a typed "not
    /// supported" error rather than silently succeeding. Regression guard for
    /// any future provider that inherits the trait without implementing it.
    #[tokio::test]
    async fn non_docker_provider_take_snapshot_default_returns_not_supported() {
        let handle = handle_for("test");
        let result = FakeProvider::new().take_snapshot(&handle, None, 1024).await;
        assert!(
            result.is_err(),
            "default take_snapshot must return an error, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported"),
            "error must say 'not supported', got: {}",
            msg
        );
    }

    /// Providers that don't override `create_from_snapshot` return a typed
    /// "not supported" error. Ensures the from-snapshot routing fails loudly
    /// on backends that don't implement it, rather than silently no-oping.
    #[tokio::test]
    async fn non_docker_provider_create_from_snapshot_default_returns_not_supported() {
        let artifact = temps_agents::sandbox::SnapshotArtifact {
            content_path: std::path::PathBuf::from("/tmp/test.tar"),
            content_digest: "sha256fake".to_string(),
            primary_digest: "sha256fake".to_string(),
            size_bytes: 0,
            backend: temps_agents::sandbox::SandboxBackend::Docker,
            image_ref: None,
            image_id: None,
            workspace: None,
        };
        let config = SandboxCreateConfig {
            run_id: 0,
            container_name_override: None,
            host_work_dir: std::path::PathBuf::from("/tmp"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: std::time::Duration::from_secs(3600),
            backend: None,
            owner_user_id: None,
        };
        let result = FakeProvider::new()
            .create_from_snapshot(&artifact, config)
            .await;
        assert!(
            result.is_err(),
            "default create_from_snapshot must return an error, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported"),
            "error must say 'not supported', got: {}",
            msg
        );
    }

    /// Providers that don't override `delete_image` should no-op successfully.
    /// The default is intentionally a no-op for backends (Local, Firecracker)
    /// that have no image store concept.
    #[tokio::test]
    async fn non_docker_provider_delete_image_default_is_noop() {
        let result = FakeProvider::new()
            .delete_image("temps-snapshot/test:v1")
            .await;
        assert!(
            result.is_ok(),
            "default delete_image must succeed (no-op), got: {:?}",
            result
        );
    }

    // ── Major 5: nullify_source_sandbox failure isolation ─────────────────────

    /// `destroy_sandbox` calls `nullify_source_sandbox` best-effort. When the
    /// snapshot DB fails (e.g. transient network error), the destroy must still
    /// complete and return `Ok(())`. A DB error in nullify must never strand the
    /// user with an un-destroyable sandbox.
    ///
    /// We attach a SnapshotService backed by a MockDatabase that errors on the
    /// `update_many` query, then verify destroy_sandbox still returns Ok.
    #[tokio::test]
    async fn destroy_sandbox_nullify_failure_does_not_fail_destroy() {
        use crate::services::snapshot_service::SnapshotService;
        use temps_agents::sandbox::local::LocalSandboxProvider;

        let data_root = unique_data_root("nullify-fail");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        // SandboxService DB: three queries for destroy path
        let sandbox_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        // SnapshotService DB: update_many for nullify fails with a DB error
        let snap_db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "simulated nullify transient failure".to_string(),
                )])
                .into_connection(),
        );

        let snap_provider = Arc::new(LocalSandboxProvider::new()) as Arc<dyn SandboxProvider>;
        let snap_registry = Arc::new(StandaloneSandboxRegistry::new(snap_provider.clone()));
        let snapshot_service =
            Arc::new(SnapshotService::new(snap_db, snap_registry, snap_provider));

        // Build the full SandboxService with the snapshot_service injected,
        // then wrap in Arc (note: with_snapshot_service must be called before Arc::new).
        let db = Arc::new(sandbox_db);
        let registry = Arc::new(StandaloneSandboxRegistry::new(
            Arc::new(FakeProvider::new()) as Arc<dyn SandboxProvider>,
        ));
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(test_server_config(data_root.clone())),
            db.clone(),
        ));
        let encryption = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key"),
        );
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption,
            Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
            config.clone(),
        ));
        let cookie_crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test cookie key"),
        );
        let service = Arc::new(
            SandboxService::new(
                db,
                registry,
                Arc::new(JobTracker::new()),
                config,
                cookie_crypto,
                git,
                data_root.clone(),
            )
            .with_snapshot_service(snapshot_service),
        );

        // destroy_sandbox must succeed even though nullify_source_sandbox failed.
        let result = service.destroy_sandbox(PUBLIC_ID, 1).await;
        assert!(
            result.is_ok(),
            "destroy_sandbox must succeed even when nullify_source_sandbox fails, got: {:?}",
            result
        );

        // Work dir still cleaned up
        assert!(
            !dir.exists(),
            "work dir must be removed even when nullify failed"
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[test]
    fn application_workspace_usage_parses_atomic_metrics_and_ports() {
        let usage = parse_application_workspace_usage(
            "memory=1048576\npids=17\ncpu=332401\ndisk=26376\nports=3000,8080\n",
        );

        assert_eq!(usage.memory_used_bytes, Some(1_048_576));
        assert_eq!(usage.pids_used, Some(17));
        assert_eq!(usage.cpu_usage_usec, Some(332_401));
        assert_eq!(usage.disk_used_bytes, Some(26_376));
        assert_eq!(usage.open_ports, vec![3000, 8080]);
    }

    #[test]
    fn application_workspace_usage_command_has_proc_socket_fallback() {
        assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("/proc/net/tcp"));
        assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("/proc/net/tcp6"));
        assert!(APPLICATION_WORKSPACE_USAGE_COMMAND.contains("du -sb /home/temps/workspace"));
    }

    #[test]
    fn runtime_variables_merge_without_losing_identical_shared_values() {
        let mut variables = HashMap::from([("SHARED_HOST".to_string(), "database".to_string())]);

        merge_runtime_variables(
            &mut variables,
            HashMap::from([
                ("SHARED_HOST".to_string(), "database".to_string()),
                ("REDIS_URL".to_string(), "redis://database:6379".to_string()),
            ]),
        )
        .expect("identical shared values should be accepted");

        assert_eq!(variables.len(), 2);
        assert_eq!(variables["REDIS_URL"], "redis://database:6379");
    }

    #[test]
    fn runtime_variables_reject_ambiguous_service_credentials() {
        let mut variables =
            HashMap::from([("DATABASE_URL".to_string(), "postgres://one".to_string())]);

        let conflict = merge_runtime_variables(
            &mut variables,
            HashMap::from([("DATABASE_URL".to_string(), "postgres://two".to_string())]),
        )
        .expect_err("different values for one runtime name must be rejected");

        assert_eq!(conflict, "DATABASE_URL");
        assert_eq!(variables["DATABASE_URL"], "postgres://one");
    }
}
