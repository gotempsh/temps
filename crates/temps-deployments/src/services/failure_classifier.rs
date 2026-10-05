// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment failure classification.
//!
//! A failed deployment stores one free-form reason string
//! (`deployments.cancelled_reason`). It is the `Display` of the workflow error
//! and typically looks like:
//!
//! ```text
//! Job execution failed: Required job 'deploy_compose' failed: Some("Job execution failed: Compose deploy failed: ...")
//! ```
//!
//! This module turns that string into two fixed, NON-identifying labels — a
//! [`DeploymentFailureStage`] and a [`DeploymentFailureCode`] — plus, for the
//! console and CLI, a short title and a concrete remediation. The raw reason
//! can contain secrets, paths, repository names and container logs, so only
//! the labels may ever leave the instance (anonymous telemetry); the
//! remediation text is static and never echoes the input.
//!
//! Matching is deliberately most-specific-first, and it uses the name of the
//! failed job (`Required job '<id>' failed`) as context: the same words
//! ("timed out", "manifest unknown") mean different things in a build, an
//! image pull and a health check. Compose failures embed container log tails
//! after a fixed marker; most rules only look at the text *before* that
//! marker so arbitrary application output cannot hijack the classification.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Version of the allowlisted failure taxonomy emitted in deployment telemetry
/// (`classifier_version`). Increment it when matching semantics or wire labels
/// change so dashboards can separate the populations.
///
/// v2: job-aware stages, Compose and registry-image codes, stage-specific
/// timeout codes, and health-check codes for apps that never listen.
pub const FAILURE_CLASSIFIER_VERSION: u8 = 2;

/// Marker Compose deployments put before embedded container log tails.
const CONTAINER_LOGS_MARKER: &str = "container logs for unhealthy/stopped services:";

/// The pipeline stage a deployment failed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentFailureStage {
    Source,
    Configuration,
    DependencyInstall,
    Build,
    Image,
    Deploy,
    Runtime,
    HealthCheck,
    Resource,
    Platform,
    Unknown,
}

impl DeploymentFailureStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Configuration => "configuration",
            Self::DependencyInstall => "dependency_install",
            Self::Build => "build",
            Self::Image => "image",
            Self::Deploy => "deploy",
            Self::Runtime => "runtime",
            Self::HealthCheck => "health_check",
            Self::Resource => "resource",
            Self::Platform => "platform",
            Self::Unknown => "unknown",
        }
    }

    /// Human label for the console.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::Configuration => "Configuration",
            Self::DependencyInstall => "Dependency install",
            Self::Build => "Build",
            Self::Image => "Image",
            Self::Deploy => "Deploy",
            Self::Runtime => "Runtime",
            Self::HealthCheck => "Health check",
            Self::Resource => "Resources",
            Self::Platform => "Platform",
            Self::Unknown => "Unknown",
        }
    }
}

/// Allowlisted failure codes. Wire values are `snake_case` and stable; add new
/// variants rather than renaming existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentFailureCode {
    OutOfMemory,
    DiskExhausted,
    Timeout,
    HealthCheckFailed,
    RepositoryAuthentication,
    RepositoryNotFound,
    RepositoryClone,
    DnsResolution,
    NetworkConnection,
    DependencyLockfileOutOfSync,
    DependencyResolution,
    DependencyDownload,
    RuntimeVersionUnsupported,
    MissingBuildScript,
    CompileError,
    DockerfileInvalid,
    BaseImagePull,
    ImageMissing,
    StaticOutputMissing,
    PortUnavailable,
    PermissionDenied,
    InvalidConfiguration,
    ContainerStart,
    BuildError,
    PlatformInternal,
    Cancelled,
    // ── v2 ──────────────────────────────────────────────────────────────
    /// The build ran past its time limit.
    BuildTimeout,
    /// Cloning or downloading the source ran past its time limit.
    SourceTimeout,
    /// Pulling a deploy image ran past its time limit.
    ImagePullTimeout,
    /// The app was running but never passed the readiness check in time.
    HealthCheckTimeout,
    /// Nothing ever accepted connections on the port Temps probes.
    AppNotListening,
    /// The container process exited (or kept restarting) during startup.
    ContainerExited,
    /// The deploy image or tag does not exist in the registry.
    ImageNotFound,
    /// The registry refused the pull for lack of (valid) credentials.
    RegistryAuthentication,
    /// The registry rate-limited the pull.
    RegistryRateLimited,
    /// The image was built for a different CPU architecture.
    ImagePlatformMismatch,
    /// The Compose file is invalid (YAML, schema, references).
    ComposeFileInvalid,
    /// A variable the Compose file requires has no value.
    ComposeVariableMissing,
    /// The Compose security policy rejected a setting.
    ComposePolicyRejected,
    /// `docker compose build` failed.
    ComposeBuildFailed,
    /// `docker compose up` failed for a reason not covered above.
    ComposeUpFailed,
    /// The Docker Compose v2 CLI plugin is not installed on the host.
    ComposeUnavailable,
    /// A volume or bind mount could not be created.
    VolumeMount,
    /// The new deployment never became routable.
    RouteActivation,
    Unknown,
}

impl DeploymentFailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutOfMemory => "out_of_memory",
            Self::DiskExhausted => "disk_exhausted",
            Self::Timeout => "timeout",
            Self::HealthCheckFailed => "health_check_failed",
            Self::RepositoryAuthentication => "repository_authentication",
            Self::RepositoryNotFound => "repository_not_found",
            Self::RepositoryClone => "repository_clone",
            Self::DnsResolution => "dns_resolution",
            Self::NetworkConnection => "network_connection",
            Self::DependencyLockfileOutOfSync => "dependency_lockfile_out_of_sync",
            Self::DependencyResolution => "dependency_resolution",
            Self::DependencyDownload => "dependency_download",
            Self::RuntimeVersionUnsupported => "runtime_version_unsupported",
            Self::MissingBuildScript => "missing_build_script",
            Self::CompileError => "compile_error",
            Self::DockerfileInvalid => "dockerfile_invalid",
            Self::BaseImagePull => "base_image_pull",
            Self::ImageMissing => "image_missing",
            Self::StaticOutputMissing => "static_output_missing",
            Self::PortUnavailable => "port_unavailable",
            Self::PermissionDenied => "permission_denied",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::ContainerStart => "container_start",
            Self::BuildError => "build_error",
            Self::PlatformInternal => "platform_internal",
            Self::Cancelled => "cancelled",
            Self::BuildTimeout => "build_timeout",
            Self::SourceTimeout => "source_timeout",
            Self::ImagePullTimeout => "image_pull_timeout",
            Self::HealthCheckTimeout => "health_check_timeout",
            Self::AppNotListening => "app_not_listening",
            Self::ContainerExited => "container_exited",
            Self::ImageNotFound => "image_not_found",
            Self::RegistryAuthentication => "registry_authentication",
            Self::RegistryRateLimited => "registry_rate_limited",
            Self::ImagePlatformMismatch => "image_platform_mismatch",
            Self::ComposeFileInvalid => "compose_file_invalid",
            Self::ComposeVariableMissing => "compose_variable_missing",
            Self::ComposePolicyRejected => "compose_policy_rejected",
            Self::ComposeBuildFailed => "compose_build_failed",
            Self::ComposeUpFailed => "compose_up_failed",
            Self::ComposeUnavailable => "compose_unavailable",
            Self::VolumeMount => "volume_mount",
            Self::RouteActivation => "route_activation",
            Self::Unknown => "unknown",
        }
    }

    /// Every code, for exhaustiveness tests and documentation.
    pub const ALL: &'static [DeploymentFailureCode] = &[
        Self::OutOfMemory,
        Self::DiskExhausted,
        Self::Timeout,
        Self::HealthCheckFailed,
        Self::RepositoryAuthentication,
        Self::RepositoryNotFound,
        Self::RepositoryClone,
        Self::DnsResolution,
        Self::NetworkConnection,
        Self::DependencyLockfileOutOfSync,
        Self::DependencyResolution,
        Self::DependencyDownload,
        Self::RuntimeVersionUnsupported,
        Self::MissingBuildScript,
        Self::CompileError,
        Self::DockerfileInvalid,
        Self::BaseImagePull,
        Self::ImageMissing,
        Self::StaticOutputMissing,
        Self::PortUnavailable,
        Self::PermissionDenied,
        Self::InvalidConfiguration,
        Self::ContainerStart,
        Self::BuildError,
        Self::PlatformInternal,
        Self::Cancelled,
        Self::BuildTimeout,
        Self::SourceTimeout,
        Self::ImagePullTimeout,
        Self::HealthCheckTimeout,
        Self::AppNotListening,
        Self::ContainerExited,
        Self::ImageNotFound,
        Self::RegistryAuthentication,
        Self::RegistryRateLimited,
        Self::ImagePlatformMismatch,
        Self::ComposeFileInvalid,
        Self::ComposeVariableMissing,
        Self::ComposePolicyRejected,
        Self::ComposeBuildFailed,
        Self::ComposeUpFailed,
        Self::ComposeUnavailable,
        Self::VolumeMount,
        Self::RouteActivation,
        Self::Unknown,
    ];
}

/// Settings surface that fixes a given failure. The console maps each value to
/// a deep link; the API stays independent of console routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureSettingsSection {
    /// Project → Build & deploy → Source (repository, branch, image reference).
    Source,
    /// Project → Build & deploy → Build (preset, Dockerfile, Compose).
    Build,
    /// Project → Build & deploy → Deployment (port, resources, startup timeout).
    Deploy,
    /// Project → Environment variables.
    EnvironmentVariables,
    /// Project → Git connection.
    Git,
    /// Instance → Docker registry credentials.
    DockerRegistry,
    /// Instance → Build limits.
    BuildLimits,
}

/// Local classification of a failed deployment's reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentFailureClassification {
    pub stage: DeploymentFailureStage,
    pub code: DeploymentFailureCode,
    /// Coarse pre-taxonomy value retained for telemetry consumers that already
    /// group by `reason`.
    pub legacy_reason: &'static str,
}

impl DeploymentFailureClassification {
    const fn new(stage: DeploymentFailureStage, code: DeploymentFailureCode) -> Self {
        Self {
            stage,
            code,
            legacy_reason: "unknown",
        }
    }
}

/// Static, user-facing guidance for a failure code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureGuidance {
    pub title: &'static str,
    pub remediation: &'static str,
    pub settings_section: Option<FailureSettingsSection>,
}

/// API view of a failed deployment's classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeploymentFailureInfo {
    /// Pipeline stage the deployment failed in.
    pub stage: DeploymentFailureStage,
    /// Allowlisted failure code.
    pub code: DeploymentFailureCode,
    /// Short human title, e.g. "Image tag not found".
    pub title: String,
    /// Concrete, actionable fix.
    pub remediation: String,
    /// Settings surface that fixes it, when one exists.
    pub settings_section: Option<FailureSettingsSection>,
    /// Pipeline job that failed (e.g. `build_image`, `deploy_compose`).
    pub failed_job: Option<String>,
    /// Time limit that was hit, in seconds, when the failure is a timeout and
    /// the reason states it.
    pub timeout_limit_seconds: Option<u64>,
    /// How long the timed-out step ran, in seconds, when the reason states it.
    pub timeout_elapsed_seconds: Option<u64>,
    /// Version of the classifier that produced this view.
    pub classifier_version: u8,
}

/// Build the API view for a failed deployment. Returns `None` when there is no
/// reason to classify.
pub fn describe_failure(reason: Option<&str>) -> Option<DeploymentFailureInfo> {
    let reason = reason?.trim();
    if reason.is_empty() {
        return None;
    }
    let classification = classify_failure_reason(Some(reason));
    let guidance = guidance_for(classification.code);
    let is_timeout = matches!(
        classification.code,
        DeploymentFailureCode::Timeout
            | DeploymentFailureCode::BuildTimeout
            | DeploymentFailureCode::SourceTimeout
            | DeploymentFailureCode::ImagePullTimeout
            | DeploymentFailureCode::HealthCheckTimeout
            | DeploymentFailureCode::AppNotListening
    );
    let lower = reason.to_lowercase();
    let head = classification_head(&lower);
    Some(DeploymentFailureInfo {
        stage: classification.stage,
        code: classification.code,
        title: guidance.title.to_string(),
        remediation: guidance.remediation.to_string(),
        settings_section: guidance.settings_section,
        failed_job: failed_job_id(reason).map(str::to_string),
        timeout_limit_seconds: is_timeout.then(|| timeout_limit_seconds(head)).flatten(),
        timeout_elapsed_seconds: is_timeout.then(|| elapsed_seconds(head)).flatten(),
        classifier_version: FAILURE_CLASSIFIER_VERSION,
    })
}

/// Static guidance per code. Never interpolates the raw reason.
pub const fn guidance_for(code: DeploymentFailureCode) -> FailureGuidance {
    use DeploymentFailureCode as C;
    use FailureSettingsSection as S;
    let (title, remediation, settings_section) = match code {
        C::OutOfMemory => (
            "Out of memory",
            "A process was killed for exceeding its memory limit. Raise the memory limit for this project (or set it to 0 for uncapped), reduce memory use during the build or at startup, or move the build to a larger host.",
            Some(S::Deploy),
        ),
        C::DiskExhausted => (
            "Disk full",
            "The host ran out of disk space. Remove unused images and build cache (Settings → Docker disk usage), lower image retention for projects, or add disk to the host, then redeploy.",
            None,
        ),
        C::Timeout => (
            "Timed out",
            "A step exceeded its time limit. Open the failed stage below to see which step it was; retry if it was a transient network or registry slowdown.",
            None,
        ),
        C::HealthCheckFailed => (
            "Health check failed",
            "The app answered, but kept returning error status codes (or a Compose service reported unhealthy). Check the runtime logs for the error, make sure the health-check path returns 2xx/3xx, and verify required environment variables are set.",
            Some(S::EnvironmentVariables),
        ),
        C::RepositoryAuthentication => (
            "Repository access denied",
            "Temps could not authenticate to the Git repository. Reconnect the Git provider or update the access token, and make sure it can read this repository.",
            Some(S::Git),
        ),
        C::RepositoryNotFound => (
            "Repository or branch not found",
            "The repository, branch or commit no longer exists or is not visible to the connected Git account. Check the repository and branch in the project's source settings.",
            Some(S::Source),
        ),
        C::RepositoryClone => (
            "Could not fetch the source",
            "Cloning or downloading the repository failed. Retry the deployment; if it keeps failing, check that the Git provider is reachable from this host and the repository is not unusually large.",
            Some(S::Git),
        ),
        C::DnsResolution => (
            "DNS lookup failed",
            "A hostname could not be resolved from this host. Check the host's DNS configuration and that the hostname is spelled correctly, then retry.",
            None,
        ),
        C::NetworkConnection => (
            "Network error",
            "A network connection failed (refused, reset or unreachable). This is often transient; retry the deployment. If it persists, check outbound connectivity and firewall rules on the host.",
            None,
        ),
        C::DependencyLockfileOutOfSync => (
            "Lockfile out of date",
            "The package manager refused to install because the lockfile does not match package.json. Run your package manager's install locally, commit the updated lockfile, and push.",
            None,
        ),
        C::DependencyResolution => (
            "Dependencies could not be resolved",
            "The package manager found conflicting dependency versions. Fix the conflict locally (update or pin the conflicting packages), commit the lockfile, and push.",
            None,
        ),
        C::DependencyDownload => (
            "Dependency download failed",
            "Packages could not be downloaded from the registry. Retry the deployment; if you use a private registry, add its credentials as build environment variables.",
            Some(S::EnvironmentVariables),
        ),
        C::RuntimeVersionUnsupported => (
            "Unsupported runtime version",
            "The requested language/runtime version is not available. Pin a supported version (for example the engines field in package.json or a version file) and redeploy.",
            Some(S::Build),
        ),
        C::MissingBuildScript => (
            "Build script missing",
            "The project has no build script. Add a build script to package.json, or change the build command in the project's build settings.",
            Some(S::Build),
        ),
        C::CompileError => (
            "Compilation failed",
            "Your code failed to compile. Open the build logs for the first error, fix it locally (run the same build command), and push.",
            None,
        ),
        C::DockerfileInvalid => (
            "Invalid Dockerfile",
            "The Dockerfile is missing or could not be parsed. Check the Dockerfile path in the build settings and fix the reported line.",
            Some(S::Build),
        ),
        C::BaseImagePull => (
            "Base image pull failed",
            "A base image referenced by the build (a FROM line) could not be pulled. Check the image name and tag; for private base images add registry credentials; if the registry timed out, retry.",
            Some(S::DockerRegistry),
        ),
        C::ImageMissing => (
            "Built image missing",
            "The image produced by the build was not found when deploying. Redeploy to rebuild it; if it keeps happening, check disk space and image retention settings.",
            Some(S::Deploy),
        ),
        C::StaticOutputMissing => (
            "Build output not found",
            "The build finished but the expected output directory was not produced. Set the correct output directory in the build settings (for example dist or build).",
            Some(S::Build),
        ),
        C::PortUnavailable => (
            "Port already in use",
            "A host port the deployment needs is already taken. For Compose, remove fixed host ports (publish only container ports, or bind to 127.0.0.1 with an unused port); otherwise stop the process holding the port.",
            Some(S::Build),
        ),
        C::PermissionDenied => (
            "Permission denied",
            "An operation was denied by file permissions or the container sandbox. Check file ownership in your image and that the app does not need root-only capabilities.",
            None,
        ),
        C::InvalidConfiguration => (
            "Invalid configuration",
            "The project or .temps.yaml configuration is invalid. Fix the reported field in the project settings or in .temps.yaml and redeploy.",
            Some(S::Build),
        ),
        C::ContainerStart => (
            "Container could not be created",
            "Docker refused to create or start the container. Check the error below (missing image, invalid command, mount or resource settings) and the project's deployment settings.",
            Some(S::Deploy),
        ),
        C::BuildError => (
            "Build failed",
            "The build command failed. Open the build logs for the first error and reproduce it locally with the same build command.",
            Some(S::Build),
        ),
        C::PlatformInternal => (
            "Internal Temps error",
            "Temps hit an internal error unrelated to your code. Retry the deployment; if it keeps happening, send the failure report below so it can be fixed.",
            None,
        ),
        C::Cancelled => (
            "Cancelled",
            "The deployment was cancelled before it finished. Redeploy when ready.",
            None,
        ),
        C::BuildTimeout => (
            "Build timed out",
            "The build exceeded its time limit. Speed it up (cache dependencies, avoid compiling in the image), or build on a larger host or worker. Compose builds are limited to 30 minutes.",
            Some(S::BuildLimits),
        ),
        C::SourceTimeout => (
            "Source download timed out",
            "Cloning or downloading the repository exceeded its time limit. Retry; for very large repositories set a root directory so only that subtree is fetched.",
            Some(S::Source),
        ),
        C::ImagePullTimeout => (
            "Image pull timed out",
            "Pulling the image exceeded its time limit. Retry, use a smaller image, or pull from a registry closer to this host.",
            Some(S::Source),
        ),
        C::HealthCheckTimeout => (
            "App did not become ready in time",
            "The container was running but did not pass its readiness check before the startup timeout. If your app is slow to boot, raise the startup timeout in the deployment settings; otherwise check the runtime logs for what it was doing.",
            Some(S::Deploy),
        ),
        C::AppNotListening => (
            "App is not listening on the expected port",
            "Temps received no HTTP readiness response on the configured port. Check the runtime logs, make your app serve HTTP on 0.0.0.0 (not localhost) and on the PORT environment variable, or set the port your app actually uses in the deployment settings.",
            Some(S::Deploy),
        ),
        C::ContainerExited => (
            "Container exited during startup",
            "The app process exited (or kept restarting) right after starting. Check the runtime logs for the crash, verify the start command and required environment variables, and that the image's entrypoint runs a long-lived server. If the logs report exec format error, use an image and application binaries built for the host CPU architecture, or publish a multi-architecture image.",
            Some(S::EnvironmentVariables),
        ),
        C::ImageNotFound => (
            "Image or tag not found",
            "The registry has no image with that name and tag, or it is private and Temps has no credentials for it. Check the image reference for typos and that the tag was pushed; for private images add registry credentials.",
            Some(S::Source),
        ),
        C::RegistryAuthentication => (
            "Registry authentication failed",
            "The registry rejected the pull. Add or update credentials for this registry in Settings → Docker registry and make sure the account can pull this image.",
            Some(S::DockerRegistry),
        ),
        C::RegistryRateLimited => (
            "Registry rate limit reached",
            "The registry rate-limited anonymous pulls from this host. Add registry credentials (authenticated pulls get a higher limit), use a mirror, or retry later.",
            Some(S::DockerRegistry),
        ),
        C::ImagePlatformMismatch => (
            "Image built for a different CPU architecture",
            "The image does not run on this host's CPU architecture (for example arm64 vs amd64). Publish a multi-architecture image (docker buildx build --platform linux/amd64,linux/arm64), or restrict the deployment to nodes with a matching architecture.",
            Some(S::Deploy),
        ),
        C::ComposeFileInvalid => (
            "Invalid Compose file",
            "The Compose file could not be parsed or validated. Run docker compose config locally against the same file to see the exact error, fix it, and push. Check the Compose file path in the build settings.",
            Some(S::Build),
        ),
        C::ComposeVariableMissing => (
            "Compose variable has no value",
            "The Compose file requires a variable that is not set (for example ${VAR:?error}). Add it to the environment variables of this environment, or give it a default in the Compose file.",
            Some(S::EnvironmentVariables),
        ),
        C::ComposePolicyRejected => (
            "Rejected by the Compose security policy",
            "A Compose setting is not allowed by Temps' sandbox (for example privileged, host bind mounts, host networking or extra capabilities). Remove it from the Compose file, or ask an instance administrator to relax the specific check in the Compose security settings.",
            Some(S::Build),
        ),
        C::ComposeBuildFailed => (
            "Compose service build failed",
            "docker compose build failed for one of the services. Reproduce it locally with docker compose build and fix the first error in the build output.",
            Some(S::Build),
        ),
        C::ComposeUpFailed => (
            "Compose stack failed to start",
            "docker compose up failed. Read the error and the retained service logs below, then fix the Compose file or the failing service.",
            Some(S::Build),
        ),
        C::ComposeUnavailable => (
            "Docker Compose is not installed",
            "This host has no Docker Compose v2 plugin. Install it (docker compose version must work) and restart Temps.",
            None,
        ),
        C::VolumeMount => (
            "Volume mount failed",
            "A volume or bind mount could not be created. Use named volumes or paths inside the repository, and make sure referenced files exist in the checkout.",
            Some(S::Build),
        ),
        C::RouteActivation => (
            "New deployment was not routable",
            "The deployment started but the proxy did not confirm its route in time, so it was rolled back. Retry; if it persists check the node's connectivity to the control plane.",
            None,
        ),
        C::Unknown => (
            "Deployment failed",
            "Temps could not classify this failure. Open the failed stage below for the full error and logs; sending the failure report helps improve this message.",
            None,
        ),
    };
    FailureGuidance {
        title,
        remediation,
        settings_section,
    }
}

/// Extract the id of the required job that failed, e.g. `build_image` from
/// `Required job 'build_image' failed: ...`.
pub fn failed_job_id(reason: &str) -> Option<&str> {
    const NEEDLE: &str = "Required job '";
    let start = reason.find(NEEDLE)? + NEEDLE.len();
    let rest = &reason[start..];
    let end = rest.find('\'')?;
    let job = &rest[..end];
    let valid = !job.is_empty()
        && job.len() <= 64
        && job
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then_some(job)
}

/// Coarse pipeline phase of a job id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Source,
    Build,
    ImagePull,
    Deploy,
    Compose,
    Finalize,
}

fn job_phase(job: Option<&str>) -> Option<JobPhase> {
    match job? {
        "download_repo" | "prepare_source_bundle" => Some(JobPhase::Source),
        "build_image" => Some(JobPhase::Build),
        "pull_external_image" | "verify_local_image" => Some(JobPhase::ImagePull),
        "deploy_container"
        | "deploy_static"
        | "deploy_static_bundle"
        | "deploy_static_from_source" => Some(JobPhase::Deploy),
        "deploy_compose" => Some(JobPhase::Compose),
        "mark_deployment_complete" | "mark_complete" => Some(JobPhase::Finalize),
        _ => None,
    }
}

const fn phase_stage(phase: JobPhase) -> DeploymentFailureStage {
    match phase {
        JobPhase::Source => DeploymentFailureStage::Source,
        JobPhase::Build => DeploymentFailureStage::Build,
        JobPhase::ImagePull => DeploymentFailureStage::Image,
        JobPhase::Deploy | JobPhase::Compose | JobPhase::Finalize => DeploymentFailureStage::Deploy,
    }
}

fn contains_any(reason: &str, signals: &[&str]) -> bool {
    signals.iter().any(|signal| reason.contains(signal))
}

/// `true` when `word` occurs in `text` delimited by non-alphanumeric
/// characters, so "oom" matches "OOM killer" but not "room" or "bloom".
fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(idx, _)| {
        let before = text[..idx].chars().next_back();
        let after = text[idx + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// Text before any embedded container-log tail. Compose reasons are stored
/// Debug-escaped, so the marker may follow a literal `\n`.
fn classification_head(lower: &str) -> &str {
    [CONTAINER_LOGS_MARKER, "last log lines:"]
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .map_or(lower, |idx| &lower[..idx])
}

/// Parse the first unsigned integer that immediately follows `prefix`.
fn number_after(text: &str, prefix: &str) -> Option<u64> {
    text.match_indices(prefix).find_map(|(idx, _)| {
        let digits: String = text[idx + prefix.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// Limit stated in a timeout message: "within 300s", "the 300s readiness
/// limit", "after 900 seconds", "after 300s", "its 30-minute deadline".
fn timeout_limit_seconds(lower: &str) -> Option<u64> {
    if let Some(limit) = number_after(lower, "readiness limit of ") {
        return Some(limit);
    }
    for prefix in ["within ", "after ", "exceeded "] {
        for (idx, _) in lower.match_indices(prefix) {
            let rest = &lower[idx + prefix.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let Ok(value) = digits.parse::<u64>() else {
                continue;
            };
            let unit = rest[digits.len()..].trim_start();
            if unit.starts_with("seconds") || unit.starts_with('s') {
                return Some(value);
            }
            if unit.starts_with("minute") || unit.starts_with("-minute") {
                return Some(value * 60);
            }
        }
    }
    if let Some(minutes) = lower.match_indices("-minute").find_map(|(idx, _)| {
        let digits: String = lower[..idx]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse::<u64>().ok()
    }) {
        return Some(minutes * 60);
    }
    None
}

/// Elapsed time stated in a timeout message: "waited 312s".
fn elapsed_seconds(lower: &str) -> Option<u64> {
    number_after(lower, "waited ")
}

/// Preserve the pre-taxonomy `reason` wire value exactly for existing
/// telemetry consumers. New stage/code matching may be more specific, but it
/// must not silently change this compatibility dimension.
pub fn legacy_failure_reason(reason: Option<&str>) -> &'static str {
    let Some(reason) = reason else {
        return "unknown";
    };
    let reason = reason.to_lowercase();

    if reason.contains("out of memory")
        || reason.contains("oom")
        || reason.contains("exit code 137")
    {
        "oom"
    } else if reason.contains("timeout")
        || reason.contains("timed out")
        || reason.contains("deadline")
    {
        "timeout"
    } else if reason.contains("health check")
        || reason.contains("healthcheck")
        || reason.contains("unhealthy")
    {
        "health_check"
    } else if reason.contains("build") || reason.contains("compile") || reason.contains("nixpacks")
    {
        "build_error"
    } else if reason.contains("clone")
        || reason.contains("network")
        || reason.contains("connection")
        || reason.contains("download")
        || reason.contains("dns")
    {
        "network"
    } else if reason.contains("image")
        && (reason.contains("not found")
            || reason.contains("missing")
            || reason.contains("no such"))
    {
        "image_missing"
    } else if reason.contains("cancel") {
        "cancelled"
    } else {
        "unknown"
    }
}

/// Classify a deployment's free-form failure reason locally into fixed,
/// NON-identifying labels. See the module docs for the matching strategy.
pub fn classify_failure_reason(reason: Option<&str>) -> DeploymentFailureClassification {
    let Some(raw) = reason else {
        return DeploymentFailureClassification::new(
            DeploymentFailureStage::Unknown,
            DeploymentFailureCode::Unknown,
        );
    };
    let classification = classify_lowercase(raw);
    DeploymentFailureClassification {
        legacy_reason: legacy_failure_reason(Some(raw)),
        ..classification
    }
}

fn classify_lowercase(raw: &str) -> DeploymentFailureClassification {
    use DeploymentFailureCode as C;
    use DeploymentFailureStage as S;

    let full = raw.to_lowercase();
    let r = classification_head(&full);
    let phase = job_phase(failed_job_id(raw));
    let is_compose = phase == Some(JobPhase::Compose) || r.contains("compose");
    let make = DeploymentFailureClassification::new;

    let timeout_signal = contains_any(
        r,
        &[
            "timeout",
            "timed out",
            "deadline",
            "did not become ready within",
            "exceeded its",
            "took too long",
        ],
    );
    let base_image_signal = contains_any(
        r,
        &[
            "failed to resolve source metadata",
            "failed to fetch oauth token",
            "failed to fetch anonymous token",
            "error pulling image configuration",
            "failed to load metadata for",
        ],
    ) || (phase == Some(JobPhase::Build)
        && contains_any(
            r,
            &[
                "failed to pull image",
                "pull access denied",
                "manifest unknown",
            ],
        ));
    // A deploy-time image pull: the registry image the deployment runs, as
    // opposed to a base image used while building.
    let deploy_image_pull = !base_image_signal
        && (matches!(phase, Some(JobPhase::ImagePull) | Some(JobPhase::Deploy))
            || contains_any(
                r,
                &[
                    "docker compose pull failed",
                    "failed to pull image '",
                    "from its registry",
                    "image pull for '",
                ],
            ));

    // ── Resources ───────────────────────────────────────────────────────
    if contains_any(r, &["out of memory", "oomkilled", "exit code 137"]) || contains_word(r, "oom")
    {
        return make(S::Resource, C::OutOfMemory);
    }
    if contains_any(
        r,
        &["no space left on device", "disk quota exceeded", "enospc"],
    ) {
        return make(S::Resource, C::DiskExhausted);
    }

    // ── Compose configuration and policy (before anything matching
    //    "build"/"network": the policy text names settings pages) ────────
    if contains_any(
        r,
        &[
            "security policy rejected",
            "invalid compose override",
            "check rejects",
            "rejected for field",
        ],
    ) && is_compose
    {
        return make(S::Configuration, C::ComposePolicyRejected);
    }
    if r.contains("docker compose v2 is unavailable") {
        return make(S::Platform, C::ComposeUnavailable);
    }
    if contains_any(
        r,
        &[
            "required compose variable",
            "a required compose variable",
            "is missing a value",
            "required variable",
        ],
    ) {
        return make(S::Configuration, C::ComposeVariableMissing);
    }
    if is_compose
        && contains_any(
            r,
            &[
                "failed to parse compose yaml",
                "compose configuration resolution failed",
                "contains invalid yaml",
                "compose model has an invalid field",
                "expects a value of type",
                "failed to read compose file",
                "compose rejected the source files",
                "a referenced file is missing",
                "required env_file is missing",
                "extends reference names a service",
                "depends on another service that is not defined",
                "depends on undefined service",
                "refers to undefined",
                "additional property",
                "additional properties",
                "yaml: line",
                "yaml: unmarshal",
                "missing top-level services mapping",
                "no configuration file provided",
                "invalid compose project",
                "docker compose config failed",
            ],
        )
    {
        return make(S::Configuration, C::ComposeFileInvalid);
    }

    // ── Architecture mismatch from trusted platform diagnostics ─────────
    let platform_mismatch = contains_any(
        r,
        &[
            "exec format error",
            "no matching manifest for",
            "does not match the specified platform",
        ],
    ) || r.contains("no node can run this image")
        || (r.contains("built for")
            && contains_any(r, &["but the control plane runs", "but node '"]));
    if platform_mismatch {
        return make(S::Image, C::ImagePlatformMismatch);
    }

    // ── Base image (build-time registry) ────────────────────────────────
    if base_image_signal {
        return make(S::Image, C::BaseImagePull);
    }

    // ── Deploy-time registry pulls ──────────────────────────────────────
    if contains_any(
        r,
        &[
            "toomanyrequests",
            "pull rate limit",
            "rate limit exceeded",
            "too many requests",
        ],
    ) && (deploy_image_pull || r.contains("pull"))
    {
        return make(S::Image, C::RegistryRateLimited);
    }
    if deploy_image_pull
        && contains_any(
            r,
            &[
                "registry authentication failed",
                "unauthorized: authentication required",
                "unauthorized: incorrect username",
                "no basic auth credentials",
                "denied: requested access to the resource is denied",
                "authentication required",
                "status code 401",
            ],
        )
    {
        return make(S::Image, C::RegistryAuthentication);
    }
    if deploy_image_pull
        && contains_any(
            r,
            &[
                "manifest unknown",
                "was not found in the registry",
                "pull access denied",
                "repository does not exist",
                "not found: manifest",
                "manifest for",
                "name unknown",
            ],
        )
    {
        return make(S::Image, C::ImageNotFound);
    }

    // ── Health checks and startup (specific runtime signals beat a generic
    //    "timed out") ─────────────────────────────────────────────────────
    if contains_any(
        r,
        &[
            "never accepted connections",
            "never returned an http response",
            "not yet accepting connections",
            "connectivity checks did not pass",
        ],
    ) {
        return make(S::HealthCheck, C::AppNotListening);
    }
    if contains_any(
        r,
        &[
            "container exited",
            "keeps restarting",
            "crashed during startup",
            "is restarting",
            "' exited",
            "' dead",
            "exited with code",
            "exited before",
        ],
    ) {
        return make(S::Runtime, C::ContainerExited);
    }
    if contains_any(r, &["port is already allocated", "address already in use"])
        || contains_any(r, &["failed to find available port", "no available port"])
    {
        return make(S::Deploy, C::PortUnavailable);
    }
    if contains_any(
        r,
        &["health check", "healthcheck", "unhealthy", "readiness"],
    ) {
        return if timeout_signal && !r.contains("error status") && !r.contains("unhealthy") {
            make(S::HealthCheck, C::HealthCheckTimeout)
        } else {
            make(S::HealthCheck, C::HealthCheckFailed)
        };
    }

    // ── Timeouts, attributed to the stage that timed out ────────────────
    if timeout_signal {
        if contains_any(
            r,
            &[
                "docker compose build timed out",
                "build exceeded",
                "worker build",
                "image build",
            ],
        ) || phase == Some(JobPhase::Build)
        {
            return make(S::Build, C::BuildTimeout);
        }
        if contains_any(
            r,
            &[
                "docker compose pull timed out",
                "image pull '",
                "image import '",
                "pull timed out",
            ],
        ) || phase == Some(JobPhase::ImagePull)
        {
            return make(S::Image, C::ImagePullTimeout);
        }
        if contains_any(r, &["clone", "archive download", "source download"])
            || phase == Some(JobPhase::Source)
        {
            return make(S::Source, C::SourceTimeout);
        }
        if contains_any(
            r,
            &[
                "did not become ready",
                "took too long to start",
                "application timeout",
                "readiness",
            ],
        ) {
            return make(S::HealthCheck, C::HealthCheckTimeout);
        }
        let stage = phase.map(phase_stage).unwrap_or(S::Platform);
        return make(stage, C::Timeout);
    }

    // ── Source ──────────────────────────────────────────────────────────
    if contains_any(
        r,
        &[
            "authentication failed",
            "could not read username",
            "permission denied (publickey)",
            "invalid credentials",
        ],
    ) {
        return if deploy_image_pull {
            make(S::Image, C::RegistryAuthentication)
        } else {
            make(S::Source, C::RepositoryAuthentication)
        };
    }
    if contains_any(r, &["repository not found", "remote ref does not exist"]) {
        return make(S::Source, C::RepositoryNotFound);
    }
    if contains_any(r, &["failed to clone", "git clone", "clone task failed"]) {
        return make(S::Source, C::RepositoryClone);
    }
    if contains_any(
        r,
        &[
            "could not resolve host",
            "name or service not known",
            "dns lookup failed",
            "dns resolution",
        ],
    ) {
        return make(S::Source, C::DnsResolution);
    }

    // ── Dependencies and build ──────────────────────────────────────────
    if contains_any(
        r,
        &[
            "err_pnpm_outdated_lockfile",
            "frozen lockfile",
            "lockfile is out of date",
            "package-lock.json is not in sync",
            "yarn.lock needs to be updated",
        ],
    ) {
        return make(S::DependencyInstall, C::DependencyLockfileOutOfSync);
    }
    if contains_any(
        r,
        &[
            "eresolve",
            "could not resolve dependency",
            "unable to resolve dependency tree",
            "version solving failed",
        ],
    ) {
        return make(S::DependencyInstall, C::DependencyResolution);
    }
    if contains_any(
        r,
        &[
            "failed to download",
            "error fetching packages",
            "package download failed",
            "registry request failed",
        ],
    ) {
        return make(S::DependencyInstall, C::DependencyDownload);
    }
    if contains_any(
        r,
        &[
            "ebadengine",
            "unsupported engine",
            "unsupported runtime",
            "runtime version not found",
            "no matching version found",
        ],
    ) {
        return make(S::Configuration, C::RuntimeVersionUnsupported);
    }
    if contains_any(
        r,
        &[
            "missing script: build",
            "command \"build\" not found",
            "command \\\"build\\\" not found",
            "couldn't find a script named \"build\"",
            "couldn't find a script named \\\"build\\\"",
        ],
    ) {
        return make(S::Build, C::MissingBuildScript);
    }
    if contains_any(
        r,
        &[
            "compilation failed",
            "failed to compile",
            "syntax error",
            "type error",
            "typescript error",
        ],
    ) {
        return make(S::Build, C::CompileError);
    }
    if r.contains("dockerfile")
        && contains_any(
            r,
            &["parse error", "invalid", "failed to read", "not found"],
        )
    {
        return make(S::Configuration, C::DockerfileInvalid);
    }
    if r.contains("docker compose build failed") {
        return make(S::Build, C::ComposeBuildFailed);
    }
    if contains_any(
        r,
        &[
            "failed to pull image",
            "pull access denied",
            "manifest unknown",
        ],
    ) {
        return make(S::Image, C::BaseImagePull);
    }
    if r.contains("image") && contains_any(r, &["not found", "missing", "no such image"]) {
        return make(S::Image, C::ImageMissing);
    }
    if contains_any(
        r,
        &[
            "static output directory not found",
            "index.html not found",
            "build output not found",
        ],
    ) {
        return make(S::Build, C::StaticOutputMissing);
    }

    // ── Deploy / runtime ────────────────────────────────────────────────
    if contains_any(
        r,
        &[
            "invalid mount config",
            "bind source path does not exist",
            "error while mounting volume",
            "invalid volume specification",
            "mounts denied",
            "error mounting",
        ],
    ) {
        return make(S::Deploy, C::VolumeMount);
    }
    if contains_any(r, &["permission denied", "operation not permitted"]) {
        return make(S::Platform, C::PermissionDenied);
    }
    if contains_any(
        r,
        &[
            "failed to parse .temps.yaml",
            "invalid configuration",
            "configuration validation failed",
            "must be a non-empty relative path",
            "resolves outside repository directory",
            "must not contain '..'",
        ],
    ) {
        return make(S::Configuration, C::InvalidConfiguration);
    }
    if contains_any(
        r,
        &[
            "failed to start container",
            "container failed to start",
            "failed to create container",
            "failed to deploy container",
        ],
    ) {
        return make(S::Runtime, C::ContainerStart);
    }
    if contains_any(
        r,
        &[
            "route table did not confirm",
            "route/dns propagation did not complete",
        ],
    ) {
        return make(S::Deploy, C::RouteActivation);
    }
    if is_compose
        && contains_any(
            r,
            &[
                "docker compose up failed",
                "compose deploy failed",
                "no containers found after docker compose up",
            ],
        )
    {
        return make(S::Deploy, C::ComposeUpFailed);
    }
    if contains_any(
        r,
        &["connection refused", "connection reset", "network error"],
    ) {
        return make(S::Platform, C::NetworkConnection);
    }
    if contains_any(r, &["build", "compile", "nixpacks"]) {
        return make(S::Build, C::BuildError);
    }
    if contains_any(r, &["clone", "network", "connection", "download", "dns"]) {
        return make(S::Platform, C::NetworkConnection);
    }
    if contains_any(
        r,
        &[
            "workflow was cancelled",
            "build was cancelled",
            "deployment cancelled",
            "cancelled by",
            "job cancelled",
        ],
    ) {
        return make(S::Platform, C::Cancelled);
    }
    if contains_any(
        r,
        &[
            "workflow execution failed",
            "internal error",
            "job validation failed",
        ],
    ) {
        return make(S::Platform, C::PlatformInternal);
    }

    // Nothing specific matched; the failed job still tells the stage.
    match phase {
        Some(JobPhase::Compose) => make(S::Deploy, C::ComposeUpFailed),
        Some(JobPhase::Build) => make(S::Build, C::BuildError),
        Some(phase) => make(phase_stage(phase), C::Unknown),
        None => make(S::Unknown, C::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DeploymentFailureCode as C;
    use DeploymentFailureStage as S;

    /// Wrap an inner job message the way the workflow executor stores it.
    fn wrapped(job: &str, inner: &str) -> String {
        format!(
            "Job execution failed: Required job '{job}' failed: {:?}",
            Some(format!("Job execution failed: {inner}"))
        )
    }

    fn assert_class(reason: &str, stage: S, code: C) {
        let got = classify_failure_reason(Some(reason));
        assert_eq!((got.stage, got.code), (stage, code), "reason: {reason}");
    }

    #[test]
    fn log_only_architecture_diagnostic_keeps_runtime_guidance_without_trusting_log_text() {
        let reason = wrapped("deploy_compose", "Compose deploy failed: Compose stack 'temps-1-2' did not become ready within 300s: service 'api' exited\n\nContainer logs for unhealthy/stopped services:\nexec format error");
        let result = classify_failure_reason(Some(&reason));
        assert_eq!(result.code, C::ContainerExited);
        let remediation = guidance_for(result.code).remediation;
        assert!(remediation.contains("exec format error"));
        assert!(remediation.contains("multi-architecture"));
    }

    #[test]
    fn extracts_failed_job_id() {
        let reason = wrapped("deploy_compose", "Compose deploy failed: boom");
        assert_eq!(failed_job_id(&reason), Some("deploy_compose"));
        assert_eq!(failed_job_id("no job here"), None);
        assert_eq!(failed_job_id("Required job '' failed"), None);
        assert_eq!(failed_job_id("Required job 'a b' failed"), None);
    }

    #[test]
    fn codes_are_unique_snake_case_and_have_guidance() {
        let mut seen = std::collections::HashSet::new();
        for code in DeploymentFailureCode::ALL {
            let wire = code.as_str();
            assert!(seen.insert(wire), "duplicate wire value {wire}");
            assert!(wire
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
            let serialized = serde_json::to_value(code).unwrap();
            assert_eq!(serialized, wire, "serde and as_str disagree for {wire}");
            let guidance = guidance_for(*code);
            assert!(!guidance.title.is_empty());
            assert!(
                guidance.remediation.len() > 40,
                "{wire} remediation too thin"
            );
        }
    }

    // ── Compose from git ────────────────────────────────────────────────

    #[test]
    fn compose_invalid_yaml() {
        let reason = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Failed to parse compose YAML for 'docker-compose.yml': did not find expected key at line 4 column 3",
        );
        assert_class(&reason, S::Configuration, C::ComposeFileInvalid);
    }

    #[test]
    fn compose_schema_error_from_docker_compose_config() {
        let reason = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose config failed while resolving image entrypoints: validating docker-compose.yml: services.web Additional property imagee is not allowed",
        );
        assert_class(&reason, S::Configuration, C::ComposeFileInvalid);
    }

    #[test]
    fn compose_safe_config_failure_causes() {
        for cause in [
            "A Compose file contains invalid YAML. Check its syntax and indentation.",
            "services.web.ports expects a value of type array.",
            "A referenced file is missing. Check Compose file, include, and extends paths.",
            "A service depends on another service that is not defined in the combined Compose project.",
        ] {
            let reason = wrapped(
                "deploy_compose",
                &format!("Failed to resolve Compose policy for project 7: Compose command failed for project 'temps-7-9': Compose configuration resolution failed (exit status: 15). {cause} Run docker compose config against the same source files, override, and environment for the full diagnostic."),
            );
            assert_class(&reason, S::Configuration, C::ComposeFileInvalid);
        }
    }

    #[test]
    fn compose_missing_file() {
        let reason = wrapped(
            "deploy_compose",
            "Failed to read compose file at /tmp/temps-deployments/deployment-3/repo/compose.yaml: No such file or directory (os error 2)",
        );
        assert_class(&reason, S::Configuration, C::ComposeFileInvalid);
    }

    #[test]
    fn compose_missing_variable() {
        let reason = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose pull failed: error while interpolating services.api.environment.DATABASE_URL: required variable DATABASE_URL is missing a value: set DATABASE_URL",
        );
        assert_class(&reason, S::Configuration, C::ComposeVariableMissing);

        let safe = wrapped(
            "deploy_compose",
            "Failed to resolve Compose policy for project 1: Compose command failed for project 'temps-1-2': Compose configuration resolution failed (exit status: 1). Required Compose variable API_KEY has no value. Set it in the project environment or .env file.",
        );
        assert_class(&safe, S::Configuration, C::ComposeVariableMissing);
    }

    #[test]
    fn compose_security_policy_is_not_a_build_error() {
        let reason = wrapped(
            "deploy_compose",
            "Compose security policy rejected deployment: Compose security policy rejected privileged for service 'worker': privileged containers can bypass the host sandbox. Review Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings; only instance administrators can change security checks.",
        );
        assert_class(&reason, S::Configuration, C::ComposePolicyRejected);

        let bind = wrapped(
            "deploy_compose",
            "Compose filesystem security policy rejected deployment: Compose security policy rejected volumes for service 'app': host bind mount source '/etc' is not allowed. Review Project Settings → Build & deploy → Build",
        );
        assert_class(&bind, S::Configuration, C::ComposePolicyRejected);

        let network = wrapped(
            "deploy_compose",
            "Compose security policy rejected deployment: Compose security policy rejected network_mode for service 'app': network_mode may not join host, default bridge, arbitrary container, or external namespaces. Review Project Settings → Build & deploy → Build",
        );
        assert_class(&network, S::Configuration, C::ComposePolicyRejected);
    }

    #[test]
    fn compose_service_build_failure() {
        let reason = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose build failed: target web: failed to solve: process \"/bin/sh -c make release\" did not complete successfully: exit code: 2",
        );
        // BuildKit's "failed to solve" without a registry cause is a service
        // build failure, not a base image pull.
        assert_class(&reason, S::Build, C::ComposeBuildFailed);
    }

    #[test]
    fn compose_service_build_specific_cause_wins() {
        let reason = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose build failed: npm error Missing script: build",
        );
        assert_class(&reason, S::Build, C::MissingBuildScript);
    }

    #[test]
    fn compose_port_conflict() {
        let reason = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose command failed for project 'temps-1-2': docker compose up failed: Error response from daemon: driver failed programming external connectivity on endpoint temps-1-2-db-1: Bind for 127.0.0.1:5432 failed: port is already allocated",
        );
        assert_class(&reason, S::Deploy, C::PortUnavailable);
    }

    #[test]
    fn compose_service_exited_ignores_log_tail_words() {
        let reason = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose stack 'temps-1-2' did not become ready within 300s: service 'api' exited\n\nContainer logs for unhealthy/stopped services:\n\n--- temps-1-2-api-1 (service 'api', state=exited, health=n/a) ---\nError: request timed out while building cache; health check skipped",
        );
        assert_class(&reason, S::Runtime, C::ContainerExited);
    }

    #[test]
    fn compose_service_unhealthy() {
        let reason = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose stack 'temps-1-2' did not become ready within 300s: service 'web' is unhealthy",
        );
        assert_class(&reason, S::HealthCheck, C::HealthCheckFailed);
    }

    #[test]
    fn compose_service_not_listening() {
        let reason = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose stack 'temps-1-2' did not become ready within 300s: service 'web' published port(s) 8080 not yet accepting connections",
        );
        assert_class(&reason, S::HealthCheck, C::AppNotListening);
    }

    #[test]
    fn compose_stack_slow_to_start_is_health_check_timeout() {
        let reason = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose stack 'temps-1-2' did not become ready within 300s: service 'web' is starting",
        );
        let info = describe_failure(Some(&reason)).unwrap();
        assert_eq!(info.code, C::HealthCheckTimeout);
        assert_eq!(info.stage, S::HealthCheck);
        assert_eq!(info.timeout_limit_seconds, Some(300));
        assert_eq!(info.failed_job.as_deref(), Some("deploy_compose"));
    }

    #[test]
    fn compose_image_pull_failures() {
        let not_found = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose pull failed: web Error manifest for registry.example.test/team/web:v9 not found: manifest unknown: manifest unknown",
        );
        assert_class(&not_found, S::Image, C::ImageNotFound);

        let auth = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose pull failed: api Error Head \"https://registry.example.test/v2/team/api/manifests/1\": unauthorized: authentication required",
        );
        assert_class(&auth, S::Image, C::RegistryAuthentication);

        let rate = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose pull failed: toomanyrequests: You have reached your pull rate limit.",
        );
        assert_class(&rate, S::Image, C::RegistryRateLimited);
    }

    #[test]
    fn compose_volume_mount_error() {
        let reason = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose command failed for project 'temps-1-2': docker compose up failed: Error response from daemon: invalid mount config for type \"bind\": bind source path does not exist: /srv/data",
        );
        assert_class(&reason, S::Deploy, C::VolumeMount);
    }

    #[test]
    fn compose_unavailable_and_generic_up_failure() {
        let unavailable = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Docker Compose v2 is unavailable: failed to run `docker compose version`: No such file or directory. Install the Docker Compose CLI plugin",
        );
        assert_class(&unavailable, S::Platform, C::ComposeUnavailable);

        let generic = wrapped(
            "deploy_compose",
            "Compose deploy failed: Compose command failed for project 'temps-1-2': docker compose up failed: Error response from daemon: Conflict. The container name is already in use",
        );
        assert_class(&generic, S::Deploy, C::ComposeUpFailed);
    }

    #[test]
    fn compose_timeouts_name_the_stage() {
        let build = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose build timed out after 1800 seconds",
        );
        let info = describe_failure(Some(&build)).unwrap();
        assert_eq!((info.stage, info.code), (S::Build, C::BuildTimeout));
        assert_eq!(info.timeout_limit_seconds, Some(1800));

        let pull = wrapped(
            "deploy_compose",
            "Compose prepare/pull failed: Compose command failed for project 'temps-1-2': docker compose pull timed out after 300 seconds",
        );
        assert_class(&pull, S::Image, C::ImagePullTimeout);
    }

    // ── Registry image deployments ──────────────────────────────────────

    #[test]
    fn image_tag_not_found() {
        let reason = format!(
            "Job execution failed: Required job 'pull_external_image' failed: {:?}",
            Some("Failed to pull image registry.example.test/team/app:missing from its registry: Pull error: Docker responded with status code 404: manifest for registry.example.test/team/app:missing not found: manifest unknown. Local daemon images are never used as a fallback")
        );
        assert_class(&reason, S::Image, C::ImageNotFound);
    }

    #[test]
    fn image_repository_missing_or_private() {
        let reason = format!(
            "Job execution failed: Required job 'pull_external_image' failed: {:?}",
            Some("Failed to pull image example/nonexistent:latest from its registry: Pull error: Docker responded with status code 404: pull access denied for example/nonexistent, repository does not exist or may require 'docker login'")
        );
        assert_class(&reason, S::Image, C::ImageNotFound);
    }

    #[test]
    fn image_registry_auth_on_worker_is_not_a_git_failure() {
        let reason = wrapped(
            "deploy_container",
            "Failed to pull image 'registry.example.test/team/app:1' from registry on node 'worker-1': Image pull for 'registry.example.test/team/app:1' failed on node 3 (401): Registry authentication failed for 'registry.example.test/team/app:1': check credentials",
        );
        assert_class(&reason, S::Image, C::RegistryAuthentication);
    }

    #[test]
    fn image_rate_limited() {
        let reason = format!(
            "Job execution failed: Required job 'pull_external_image' failed: {:?}",
            Some("Failed to pull image library/app:1 from its registry: Pull error: toomanyrequests: You have reached your pull rate limit")
        );
        assert_class(&reason, S::Image, C::RegistryRateLimited);
    }

    #[test]
    fn image_platform_mismatch() {
        let preflight = wrapped(
            "deploy_container",
            "Image 'team/app:1' is built for linux/arm64 but the control plane runs linux/amd64. The container would fail to start with 'exec format error'. Build for linux/amd64 (multi-arch build), or restrict this environment to linux/arm64 nodes with target nodes/labels.",
        );
        assert_class(&preflight, S::Image, C::ImagePlatformMismatch);

        let runtime = wrapped(
            "deploy_container",
            "Container exited during startup (Exit code 1) after 2s. Last log lines: exec /usr/local/bin/server: exec format error",
        );
        assert_class(&runtime, S::Runtime, C::ContainerExited);

        let pull = format!(
            "Job execution failed: Required job 'pull_external_image' failed: {:?}",
            Some("Failed to pull image team/app:1 from its registry: no matching manifest for linux/amd64 in the manifest list entries")
        );
        assert_class(&pull, S::Image, C::ImagePlatformMismatch);
    }

    #[test]
    fn image_exits_immediately() {
        for inner in [
            "Container exited during startup (Exit code 1) after 1s. Last log lines: Error: Cannot find module '/app/server.js'",
            "Container keeps restarting during startup (restarted 5 times; last exit: Exit code 1)",
            "Container crashed during startup - check container logs for details",
        ] {
            assert_class(&wrapped("deploy_container", inner), S::Runtime, C::ContainerExited);
        }
    }

    #[test]
    fn startup_log_tail_cannot_override_container_exit_diagnosis() {
        assert_class(&wrapped("deploy_container", "Container exited during startup (exit code 1). Last log lines: OOMKilled cancelled"), S::Runtime, C::ContainerExited);
    }

    #[test]
    fn image_runtime_oom_is_resource() {
        let reason = wrapped(
            "deploy_container",
            "Container exited during startup (OOMKilled (exit code 137)) after 4s",
        );
        assert_class(&reason, S::Resource, C::OutOfMemory);
    }

    #[test]
    fn wrong_port_is_app_not_listening_with_limit_and_elapsed() {
        let reason = wrapped(
            "deploy_container",
            "Application never accepted connections on port 3000 within the readiness limit of 300s (waited 302s; last check: connection refused)",
        );
        let info = describe_failure(Some(&reason)).unwrap();
        assert_eq!(
            (info.stage, info.code),
            (S::HealthCheck, C::AppNotListening)
        );
        assert_eq!(info.timeout_limit_seconds, Some(300));
        assert_eq!(info.timeout_elapsed_seconds, Some(302));
        assert_eq!(info.settings_section, Some(FailureSettingsSection::Deploy));

        // Legacy wording from older instances.
        let legacy = wrapped(
            "deploy_container",
            "Application timeout - connectivity checks did not pass in time",
        );
        assert_class(&legacy, S::HealthCheck, C::AppNotListening);
    }

    #[test]
    fn closed_http_connections_get_port_and_protocol_guidance() {
        assert_class(
            "Application never returned an HTTP response on port 8081 within the readiness limit of 30s (waited 31s)",
            S::HealthCheck,
            C::AppNotListening,
        );
    }

    #[test]
    fn slow_app_is_health_check_timeout() {
        let reason = wrapped(
            "deploy_container",
            "Application readiness timed out on port 8080: health checks did not pass within the readiness limit of 120s (waited 121s; last check: request timed out)",
        );
        let info = describe_failure(Some(&reason)).unwrap();
        assert_eq!(
            (info.stage, info.code),
            (S::HealthCheck, C::HealthCheckTimeout)
        );
        assert_eq!(info.timeout_limit_seconds, Some(120));
    }

    #[test]
    fn health_check_error_statuses() {
        let reason = wrapped(
            "deploy_container",
            "Application health check failed - server returned error status codes for 60 seconds (last status 500 Internal Server Error from / on port 3000)",
        );
        assert_class(&reason, S::HealthCheck, C::HealthCheckFailed);
    }

    // ── Timeouts by stage (the nixpacks bucket) ─────────────────────────

    #[test]
    fn build_timeouts_are_build_stage() {
        assert_class(
            &wrapped(
                "build_image",
                "Failed to build image: Build failed: Build failed: Timeout error",
            ),
            S::Build,
            C::BuildTimeout,
        );
        assert_class(
            &wrapped(
                "build_image",
                "Worker build failed: Worker build exceeded its 30-minute deadline",
            ),
            S::Build,
            C::BuildTimeout,
        );
    }

    #[test]
    fn registry_timeout_while_resolving_from_is_base_image_pull() {
        let reason = wrapped(
            "build_image",
            "Failed to build image: Build failed: failed to resolve source metadata for docker.io/library/debian:bookworm-slim: failed to do request: Head \"https://registry-1.docker.io/v2/library/debian/manifests/bookworm-slim\": net/http: TLS handshake timeout",
        );
        assert_class(&reason, S::Image, C::BaseImagePull);
    }

    #[test]
    fn clone_timeout_is_source_stage() {
        let reason = wrapped(
            "download_repo",
            "Git sparse clone of example/repo timed out after 300s",
        );
        let info = describe_failure(Some(&reason)).unwrap();
        assert_eq!((info.stage, info.code), (S::Source, C::SourceTimeout));
        assert_eq!(info.timeout_limit_seconds, Some(300));
    }

    #[test]
    fn worker_image_pull_deadline_is_image_pull_timeout() {
        let reason = wrapped(
            "deploy_container",
            "Image pull 'team/app:1' exceeded the 30-minute worker deadline",
        );
        let info = describe_failure(Some(&reason)).unwrap();
        assert_eq!((info.stage, info.code), (S::Image, C::ImagePullTimeout));
        assert_eq!(info.timeout_limit_seconds, Some(1800));
    }

    #[test]
    fn unknown_text_still_gets_the_job_stage() {
        let reason = wrapped("deploy_container", "something nobody anticipated");
        assert_class(&reason, S::Deploy, C::Unknown);
        assert_class("an entirely novel failure", S::Unknown, C::Unknown);
    }

    #[test]
    fn word_matching_avoids_substring_false_positives() {
        assert!(contains_word("kernel's oom killer", "oom"));
        assert!(!contains_word("no room left in the bloom filter", "oom"));
    }

    #[test]
    fn docker_context_canceled_is_not_a_user_cancellation() {
        let reason = wrapped(
            "deploy_container",
            "Failed to deploy container: Deployment failed: Failed to start container: context canceled",
        );
        assert_class(&reason, S::Runtime, C::ContainerStart);
    }

    #[test]
    fn describe_failure_never_echoes_the_reason() {
        let secret = "ghp_shouldNeverLeak123";
        let info = describe_failure(Some(&wrapped(
            "build_image",
            &format!("Build failed with token {secret}"),
        )))
        .unwrap();
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains(secret));
        assert_eq!(info.classifier_version, FAILURE_CLASSIFIER_VERSION);
        assert!(describe_failure(None).is_none());
        assert!(describe_failure(Some("   ")).is_none());
    }
}
