//! Workflow Execution Service
//!
//! Executes deployment jobs as workflows using the WorkflowExecutor

use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::sync::Arc;
use temps_core::{
    Job, JobQueue, WorkflowBuilder, WorkflowCancellationProvider, WorkflowError, WorkflowExecutor,
};
use temps_database::DbConnection;
use temps_deployer::{static_deployer::StaticDeployer, ContainerDeployer, ImageBuilder};
use temps_entities::{deployment_jobs, deployments, environments, projects};
use temps_error_tracking::services::SourceMapService;
use temps_git::GitProviderManagerTrait;
use temps_logs::LogService;
use tokio::sync::OnceCell;
use tracing::{debug, error, info, warn};

use crate::jobs::{
    AgentSyncService, BuildImageJobBuilder, ConfigureAgentsJobBuilder, ConfigureCronsJobBuilder,
    ConfigureMetricAlertsJobBuilder, CronConfigService, DeployImageJobBuilder,
    DeployStaticBundleJob, DeployStaticJob, DeploymentTarget, DownloadRepoBuilder,
    MetricAlertConfigService, PrepareSourceBundleJob, PullExternalImageJob, ResourceUsage,
    VerifyLocalImageJob,
};
use crate::services::DeploymentJobTracker;
use temps_screenshots::ScreenshotService;

/// Version of the allowlisted failure taxonomy emitted in deployment telemetry.
/// Increment this when matching semantics or wire labels change.
const FAILURE_CLASSIFIER_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeploymentFailureStage {
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

/// Read all per-service Compose runtime selections together so a new setting
/// cannot be persisted successfully and then be silently omitted by the
/// deployment workflow.
fn compose_service_security_settings(
    preset_config: Option<&temps_entities::preset::PresetConfig>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    match preset_config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(config)) => (
            config.excluded_services.clone(),
            config.relaxed_capability_services.clone(),
            config.unsandboxed_services.clone(),
        ),
        _ => (Vec::new(), Vec::new(), Vec::new()),
    }
}

impl DeploymentFailureStage {
    const fn as_str(self) -> &'static str {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeploymentFailureCode {
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
    Unknown,
}

impl DeploymentFailureCode {
    const fn as_str(self) -> &'static str {
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
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeploymentFailureClassification {
    stage: DeploymentFailureStage,
    code: DeploymentFailureCode,
    /// Coarse pre-taxonomy value retained for telemetry consumers that already
    /// group by `reason`.
    legacy_reason: &'static str,
}

impl DeploymentFailureClassification {
    const fn new(
        stage: DeploymentFailureStage,
        code: DeploymentFailureCode,
        legacy_reason: &'static str,
    ) -> Self {
        Self {
            stage,
            code,
            legacy_reason,
        }
    }
}

fn contains_any(reason: &str, signals: &[&str]) -> bool {
    signals.iter().any(|signal| reason.contains(signal))
}

/// Add bounded template context without allowing operator-defined template
/// slugs to become identifying or high-cardinality outbound telemetry.
fn with_template_telemetry(
    event: temps_core::telemetry::TelemetryEvent,
    template_slug: Option<&str>,
) -> temps_core::telemetry::TelemetryEvent {
    let safe_slug = template_slug.and_then(temps_core::templates::telemetry_safe_template_slug);
    let template_source = match (template_slug, safe_slug) {
        (None, _) => "none",
        (Some(_), Some(_)) => "bundled",
        (Some(_), None) => "custom",
    };

    event
        .with("is_template", template_slug.is_some())
        .with("template_source", template_source)
        .with_opt("template_slug", safe_slug.map(str::to_string))
}

/// Preserve the pre-taxonomy `reason` wire value exactly for existing
/// telemetry consumers. New stage/code matching may be more specific, but it
/// must not silently change this compatibility dimension.
fn legacy_failure_reason(reason: Option<&str>) -> &'static str {
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
/// NON-identifying labels. The raw reason can contain secrets, source code,
/// paths, repository names, and dependency names, so it must never leave the
/// instance. Matching order is deliberately most-specific-first.
fn classify_failure_reason(reason: Option<&str>) -> DeploymentFailureClassification {
    let Some(reason) = reason else {
        return DeploymentFailureClassification::new(
            DeploymentFailureStage::Unknown,
            DeploymentFailureCode::Unknown,
            "unknown",
        );
    };
    let r = reason.to_lowercase();

    let classification =
        if contains_any(&r, &["out of memory", "oom", "oomkilled", "exit code 137"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Resource,
                DeploymentFailureCode::OutOfMemory,
                "oom",
            )
        } else if contains_any(
            &r,
            &["no space left on device", "disk quota exceeded", "enospc"],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Resource,
                DeploymentFailureCode::DiskExhausted,
                "unknown",
            )
        } else if contains_any(&r, &["health check", "healthcheck", "unhealthy"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::HealthCheck,
                DeploymentFailureCode::HealthCheckFailed,
                "health_check",
            )
        } else if contains_any(&r, &["timeout", "timed out", "deadline"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::Timeout,
                "timeout",
            )
        } else if contains_any(
            &r,
            &[
                "authentication failed",
                "could not read username",
                "permission denied (publickey)",
                "invalid credentials",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Source,
                DeploymentFailureCode::RepositoryAuthentication,
                "network",
            )
        } else if contains_any(&r, &["repository not found", "remote ref does not exist"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Source,
                DeploymentFailureCode::RepositoryNotFound,
                "network",
            )
        } else if contains_any(&r, &["failed to clone", "git clone", "clone task failed"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Source,
                DeploymentFailureCode::RepositoryClone,
                "network",
            )
        } else if contains_any(
            &r,
            &[
                "could not resolve host",
                "name or service not known",
                "dns lookup failed",
                "dns resolution",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Source,
                DeploymentFailureCode::DnsResolution,
                "network",
            )
        } else if contains_any(
            &r,
            &[
                "err_pnpm_outdated_lockfile",
                "frozen lockfile",
                "lockfile is out of date",
                "package-lock.json is not in sync",
                "yarn.lock needs to be updated",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::DependencyInstall,
                DeploymentFailureCode::DependencyLockfileOutOfSync,
                "build_error",
            )
        } else if contains_any(
            &r,
            &[
                "eresolve",
                "could not resolve dependency",
                "unable to resolve dependency tree",
                "version solving failed",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::DependencyInstall,
                DeploymentFailureCode::DependencyResolution,
                "build_error",
            )
        } else if contains_any(
            &r,
            &[
                "failed to download",
                "error fetching packages",
                "package download failed",
                "registry request failed",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::DependencyInstall,
                DeploymentFailureCode::DependencyDownload,
                "network",
            )
        } else if contains_any(
            &r,
            &[
                "ebadengine",
                "unsupported engine",
                "unsupported runtime",
                "runtime version not found",
                "no matching version found",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Configuration,
                DeploymentFailureCode::RuntimeVersionUnsupported,
                "build_error",
            )
        } else if contains_any(
            &r,
            &[
                "missing script: build",
                "command \"build\" not found",
                "couldn't find a script named \"build\"",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Build,
                DeploymentFailureCode::MissingBuildScript,
                "build_error",
            )
        } else if contains_any(
            &r,
            &[
                "compilation failed",
                "failed to compile",
                "syntax error",
                "type error",
                "typescript error",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Build,
                DeploymentFailureCode::CompileError,
                "build_error",
            )
        } else if r.contains("dockerfile")
            && contains_any(
                &r,
                &["parse error", "invalid", "failed to read", "not found"],
            )
        {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Configuration,
                DeploymentFailureCode::DockerfileInvalid,
                "build_error",
            )
        } else if contains_any(
            &r,
            &[
                "failed to pull image",
                "pull access denied",
                "manifest unknown",
                "failed to resolve source metadata",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Image,
                DeploymentFailureCode::BaseImagePull,
                "network",
            )
        } else if r.contains("image")
            && contains_any(&r, &["not found", "missing", "no such image"])
        {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Image,
                DeploymentFailureCode::ImageMissing,
                "image_missing",
            )
        } else if contains_any(
            &r,
            &[
                "static output directory not found",
                "index.html not found",
                "build output not found",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Build,
                DeploymentFailureCode::StaticOutputMissing,
                "build_error",
            )
        } else if contains_any(
            &r,
            &[
                "address already in use",
                "failed to find available port",
                "no available port",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Deploy,
                DeploymentFailureCode::PortUnavailable,
                "unknown",
            )
        } else if contains_any(&r, &["permission denied", "operation not permitted"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::PermissionDenied,
                "unknown",
            )
        } else if contains_any(
            &r,
            &[
                "failed to parse .temps.yaml",
                "invalid configuration",
                "configuration validation failed",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Configuration,
                DeploymentFailureCode::InvalidConfiguration,
                "unknown",
            )
        } else if contains_any(
            &r,
            &[
                "failed to start container",
                "container failed to start",
                "container exited before",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Runtime,
                DeploymentFailureCode::ContainerStart,
                "unknown",
            )
        } else if contains_any(
            &r,
            &["connection refused", "connection reset", "network error"],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::NetworkConnection,
                "network",
            )
        } else if contains_any(&r, &["build", "compile", "nixpacks"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Build,
                DeploymentFailureCode::BuildError,
                "build_error",
            )
        } else if contains_any(&r, &["clone", "network", "connection", "download", "dns"]) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::NetworkConnection,
                "network",
            )
        } else if r.contains("cancel") {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::Cancelled,
                "cancelled",
            )
        } else if contains_any(
            &r,
            &[
                "workflow execution failed",
                "internal error",
                "job validation failed",
            ],
        ) {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::PlatformInternal,
                "unknown",
            )
        } else {
            DeploymentFailureClassification::new(
                DeploymentFailureStage::Unknown,
                DeploymentFailureCode::Unknown,
                "unknown",
            )
        };

    DeploymentFailureClassification {
        legacy_reason: legacy_failure_reason(Some(reason)),
        ..classification
    }
}

fn deploy_failed_telemetry_event(
    reason: Option<&str>,
    source_type: Option<String>,
    preset: Option<String>,
    template_slug: Option<String>,
) -> temps_core::telemetry::TelemetryEvent {
    let failure = classify_failure_reason(reason);
    with_template_telemetry(
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::DeployFailed,
        )
        .with("reason", failure.legacy_reason)
        .with("failure_stage", failure.stage.as_str())
        .with("failure_code", failure.code.as_str())
        .with("classifier_version", FAILURE_CLASSIFIER_VERSION)
        .with_opt("source_type", source_type)
        .with_opt("preset", preset),
        template_slug.as_deref(),
    )
}

/// Service for executing deployment workflows
pub struct WorkflowExecutionService {
    db: Arc<DbConnection>,
    queue: Arc<dyn JobQueue>,
    git_provider: Arc<dyn GitProviderManagerTrait>,
    image_builder: Arc<dyn ImageBuilder>,
    container_deployer: Arc<dyn ContainerDeployer>,
    static_deployer: Arc<dyn StaticDeployer>,
    log_service: Arc<LogService>,
    cron_service: Arc<dyn CronConfigService>,
    alert_service: Arc<dyn MetricAlertConfigService>,
    agent_sync_service: Arc<dyn AgentSyncService>,
    config_service: Arc<temps_config::ConfigService>,
    screenshot_service: Arc<ScreenshotService>,
    docker: Arc<bollard::Docker>,
    source_map_service: OnceCell<Arc<SourceMapService>>,
    node_scheduler: OnceCell<Arc<crate::services::NodeScheduler>>,
    encryption_service: OnceCell<Arc<temps_core::EncryptionService>>,
    file_store: OnceCell<Arc<dyn temps_file_store::FileStore>>,
    /// Anonymous product telemetry reporter (late-bound, optional). Set via
    /// [`Self::set_telemetry`]; defaults to a no-op when unset so the deploy
    /// path never depends on telemetry being wired.
    telemetry: OnceCell<Arc<dyn temps_core::telemetry::TelemetryReporter>>,
}

impl WorkflowExecutionService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DbConnection>,
        queue: Arc<dyn JobQueue>,
        git_provider: Arc<dyn GitProviderManagerTrait>,
        image_builder: Arc<dyn ImageBuilder>,
        container_deployer: Arc<dyn ContainerDeployer>,
        static_deployer: Arc<dyn StaticDeployer>,
        log_service: Arc<LogService>,
        cron_service: Arc<dyn CronConfigService>,
        alert_service: Arc<dyn MetricAlertConfigService>,
        agent_sync_service: Arc<dyn AgentSyncService>,
        config_service: Arc<temps_config::ConfigService>,
        screenshot_service: Arc<ScreenshotService>,
        docker: Arc<bollard::Docker>,
    ) -> Self {
        Self {
            db,
            queue,
            git_provider,
            image_builder,
            container_deployer,
            static_deployer,
            log_service,
            cron_service,
            alert_service,
            agent_sync_service,
            config_service,
            screenshot_service,
            docker,
            source_map_service: OnceCell::new(),
            node_scheduler: OnceCell::new(),
            encryption_service: OnceCell::new(),
            file_store: OnceCell::new(),
            telemetry: OnceCell::new(),
        }
    }

    /// Set the anonymous telemetry reporter used to emit deploy-funnel events.
    pub fn set_telemetry(&self, reporter: Arc<dyn temps_core::telemetry::TelemetryReporter>) {
        let _ = self.telemetry.set(reporter);
    }

    /// The telemetry reporter, or a no-op when none has been wired. Always safe
    /// to call `report()` on the result; it never blocks or fails.
    fn telemetry(&self) -> Arc<dyn temps_core::telemetry::TelemetryReporter> {
        self.telemetry
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter))
    }

    /// Set the node scheduler for multi-node deployments
    pub fn set_node_scheduler(&self, scheduler: Arc<crate::services::NodeScheduler>) {
        let _ = self.node_scheduler.set(scheduler);
    }

    /// Set the encryption service for decrypting node tokens during remote deployments
    pub fn set_encryption_service(&self, service: Arc<temps_core::EncryptionService>) {
        let _ = self.encryption_service.set(service);
    }

    /// Set the content-addressable file store for deduplication of static assets
    pub fn set_file_store(&self, store: Arc<dyn temps_file_store::FileStore>) {
        let _ = self.file_store.set(store);
    }

    /// Set the source map service for automatic source map capture during deployments
    pub fn set_source_map_service(&self, service: Arc<SourceMapService>) {
        let _ = self.source_map_service.set(service);
    }

    /// Get the container deployer (for cancelling deployments)
    pub fn container_deployer(&self) -> Arc<dyn ContainerDeployer> {
        self.container_deployer.clone()
    }

    /// Execute the workflow for a deployment using its job records
    pub async fn execute_deployment_workflow(
        &self,
        deployment_id: i32,
    ) -> Result<(), WorkflowExecutionError> {
        info!(
            "Starting workflow execution for deployment {}",
            deployment_id
        );

        // Load deployment, project, and environment
        let deployment = self.get_deployment(deployment_id).await?;
        let project = self.get_project(deployment.project_id).await?;
        let environment = self.get_environment(deployment.environment_id).await?;

        // Anonymous telemetry: a deploy is now being attempted. This runs once
        // per deployment workflow. Properties are non-identifying enum labels
        // (source type, build preset) plus whether this is a preview env.
        self.telemetry().report(with_template_telemetry(
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::DeployAttempted,
            )
            .with("source_type", project.source_type.to_string())
            .with(
                "preset",
                temps_presets::runtime_slug(project.preset, project.preset_config.as_ref()),
            )
            .with("is_preview", environment.is_preview),
            project.template_slug.as_deref(),
        ));

        // Load all jobs for this deployment
        let db_jobs = self.get_deployment_jobs(deployment_id).await?;

        if db_jobs.is_empty() {
            return Err(WorkflowExecutionError::NoJobsFound(deployment_id));
        }

        debug!(
            "Found {} jobs for deployment {}",
            db_jobs.len(),
            deployment_id
        );

        // Create a no-op log writer since jobs handle their own logging
        let noop_log_writer = Arc::new(NoOpLogWriter);

        // Build workflow from jobs
        let mut workflow_builder = WorkflowBuilder::new()
            .with_workflow_run_id(format!("deployment-{}", deployment_id))
            .with_deployment_context(
                deployment_id,
                deployment.project_id,
                deployment.environment_id,
            )
            .with_log_writer(noop_log_writer)
            .continue_on_failure(false)
            .with_max_parallel_jobs(3); // Allow parallel execution of configure_crons and take_screenshot

        // Add project metadata as workflow variables
        workflow_builder = workflow_builder.with_var("repo_owner", &project.repo_owner)?;
        workflow_builder = workflow_builder.with_var("repo_name", &project.repo_name)?;

        // Convert database job records to actual job instances
        // Create log paths for each job
        for db_job in &db_jobs {
            // Create log path for this job
            self.log_service
                .create_log_path(&db_job.log_id)
                .await
                .map_err(|e| {
                    WorkflowExecutionError::JobCreationFailed(format!(
                        "Failed to create log path for job {}: {}",
                        db_job.job_id, e
                    ))
                })?;

            debug!(
                "📝 Created log path for job {} at {}",
                db_job.job_id, db_job.log_id
            );

            let job = self
                .create_job_from_record(&project, &environment, &deployment, db_job)
                .await?;

            // Parse dependencies from database record
            let dependencies: Vec<String> = if let Some(ref deps_json) = db_job.dependencies {
                serde_json::from_value(deps_json.clone()).unwrap_or_else(|e| {
                    warn!(
                        "Failed to parse dependencies for job {}: {}",
                        db_job.job_id, e
                    );
                    vec![]
                })
            } else {
                vec![]
            };

            // Parse _required_for_completion from job config (defaults to true for backwards compatibility)
            let required_for_completion = db_job
                .job_config
                .as_ref()
                .and_then(|config| config.get("_required_for_completion"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            workflow_builder =
                workflow_builder.with_job_config(job, dependencies, required_for_completion);
        }

        let workflow = workflow_builder.build()?;

        info!("Built workflow with {} jobs", workflow.jobs.len());

        // Create job tracker for updating deployment_jobs table
        let job_tracker = Arc::new(DeploymentJobTracker::new(self.db.clone(), deployment_id));

        // Execute workflow
        let executor = WorkflowExecutor::new(Some(job_tracker));

        // Create cancellation provider that checks database state
        let cancellation_provider = Arc::new(DatabaseCancellationProvider::new(
            self.db.clone(),
            deployment_id,
        ));

        match executor
            .execute_workflow(workflow, cancellation_provider)
            .await
        {
            Ok(_context) => {
                info!(
                    "Workflow execution completed successfully for deployment {}",
                    deployment_id
                );

                // NOTE: Deployment finalization (status, environment routing, container registration)
                // is now handled by the MarkDeploymentCompleteJob that runs as part of the workflow.
                // We don't perform any additional updates here to avoid duplicate database writes.

                // Anonymous telemetry: the executor only returns Ok once every
                // required job — including MarkDeploymentCompleteJob, which
                // flips the deployment to "completed" — has succeeded, so this
                // is the canonical terminal success point. The Completed arm in
                // update_deployment_status_with_reason is NOT reached on this
                // path (finalization bypasses it), so success must be emitted
                // here to pair with the deploy_attempted event above.
                let telemetry = self.telemetry();
                telemetry.report(with_template_telemetry(
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::DeploySucceeded,
                    )
                    .with("source_type", project.source_type.to_string())
                    .with(
                        "preset",
                        temps_presets::runtime_slug(project.preset, project.preset_config.as_ref()),
                    )
                    .with("is_preview", environment.is_preview),
                    project.template_slug.as_deref(),
                ));
                // Once-per-instance: "this instance shipped its first deploy".
                telemetry.report_once(
                    "first_deploy_succeeded",
                    with_template_telemetry(
                        temps_core::telemetry::TelemetryEvent::new(
                            temps_core::telemetry::TelemetryEventKind::FirstDeploySucceeded,
                        )
                        .with("source_type", project.source_type.to_string())
                        .with(
                            "preset",
                            temps_presets::runtime_slug(
                                project.preset,
                                project.preset_config.as_ref(),
                            ),
                        ),
                        project.template_slug.as_deref(),
                    ),
                );

                // NOW teardown previous deployment for zero-downtime deployment
                // This happens AFTER the new deployment is fully running
                info!("Checking for previous deployments to teardown after successful deployment");
                match self
                    .teardown_previous_deployment(
                        deployment.project_id,
                        deployment.environment_id,
                        &deployment,
                    )
                    .await
                {
                    Ok(Some(stopped_container_id)) => {
                        info!(
                            "Successfully tore down previous deployment: {}",
                            stopped_container_id
                        );
                    }
                    Ok(None) => {
                        debug!("No previous deployment found to teardown");
                    }
                    Err(e) => {
                        warn!("Failed to teardown previous deployment: {}", e);
                        // Don't fail the deployment if teardown fails - the new deployment is already running
                    }
                }

                Ok(())
            }
            Err(e) => {
                // Check if this is a cancellation error
                let error_message = format!("{}", e);
                let lower_error_message = error_message.to_lowercase();
                let is_cancellation = lower_error_message.contains("cancelled")
                    || lower_error_message.contains("canceled");

                if is_cancellation {
                    info!(
                        "Workflow execution cancelled for deployment {}: {}",
                        deployment_id, e
                    );

                    // Deployment status should already be set to cancelled by cancel_deployment
                    // But we'll verify it's in the correct state
                    let deployment = self.get_deployment(deployment_id).await?;
                    if deployment.state != "cancelled" {
                        // Update deployment status to cancelled if it wasn't already
                        self.update_deployment_status_with_reason(
                            deployment_id,
                            temps_entities::types::PipelineStatus::Cancelled,
                            Some("Workflow cancelled".to_string()),
                        )
                        .await?;
                    }

                    info!(
                        "Deployment {} cancellation completed - workflow stopped gracefully",
                        deployment_id
                    );
                } else {
                    error!(
                        "Workflow execution failed for deployment {}: {}",
                        deployment_id, e
                    );

                    // Update deployment status to failed with reason
                    self.update_deployment_status_with_reason(
                        deployment_id,
                        temps_entities::types::PipelineStatus::Failed,
                        Some(error_message),
                    )
                    .await?;
                }

                Err(WorkflowExecutionError::WorkflowFailed(e))
            }
        }
    }

    async fn get_deployment(
        &self,
        deployment_id: i32,
    ) -> Result<deployments::Model, WorkflowExecutionError> {
        deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| WorkflowExecutionError::DeploymentNotFound(deployment_id))
    }

    /// Look up the health_check_path output from the build job for a deployment.
    /// Returns None if no health config was found in .temps.yaml.
    async fn get_health_check_path_from_jobs(&self, deployment_id: i32) -> Option<String> {
        use temps_entities::deployment_jobs;

        let jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::JobType.eq("build_image"))
            .all(self.db.as_ref())
            .await
            .ok()?;

        for job in jobs {
            if let Some(outputs) = &job.outputs {
                if let Some(path) = outputs.get("health_check_path") {
                    return serde_json::from_value::<String>(path.clone()).ok();
                }
            }
        }

        None
    }

    async fn get_project(
        &self,
        project_id: i32,
    ) -> Result<projects::Model, WorkflowExecutionError> {
        projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| WorkflowExecutionError::ProjectNotFound(project_id))
    }

    async fn get_environment(
        &self,
        environment_id: i32,
    ) -> Result<environments::Model, WorkflowExecutionError> {
        environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| WorkflowExecutionError::EnvironmentNotFound(environment_id))
    }

    async fn get_deployment_jobs(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<deployment_jobs::Model>, WorkflowExecutionError> {
        Ok(deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .order_by_asc(deployment_jobs::Column::ExecutionOrder)
            .all(self.db.as_ref())
            .await?)
    }

    async fn create_job_from_record(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        db_job: &deployment_jobs::Model,
    ) -> Result<Arc<dyn temps_core::WorkflowTask>, WorkflowExecutionError> {
        debug!(
            "🔧 Creating job instance for: {} ({})",
            db_job.name, db_job.job_type
        );

        match db_job.job_type.as_str() {
            "DownloadRepoJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let repo_owner = config
                    .get("repo_owner")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig("repo_owner missing".to_string())
                    })?;

                let repo_name = config
                    .get("repo_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig("repo_name missing".to_string())
                    })?;

                // Check if this is a public repo
                let is_public_repo = config
                    .get("is_public_repo")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Get git_url for public repos
                let git_url = config
                    .get("git_url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // Get connection_id for private repos (optional for public repos)
                let connection_id = config
                    .get("git_provider_connection_id")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32);

                // Validate: public repos need git_url, private repos need connection_id
                if is_public_repo && git_url.is_none() {
                    return Err(WorkflowExecutionError::InvalidJobConfig(
                        "git_url is required for public repositories".to_string(),
                    ));
                }
                if !is_public_repo && connection_id.is_none() {
                    return Err(WorkflowExecutionError::InvalidJobConfig(
                        "git_provider_connection_id is required for private repositories"
                            .to_string(),
                    ));
                }

                // Get branch_ref from job config (set by workflow planner based on deployment)
                // Fallback to project.main_branch if not specified
                let branch_ref = config
                    .get("branch_ref")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| deployment.branch_ref.clone().unwrap_or("main".to_string()));

                // Get tag_ref from job config (for tag-based deployments)
                let tag_ref = config
                    .get("tag_ref")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // Get commit_sha from job config (for specific commit
                // deployments). An empty string is "no commit", never a ref —
                // older deployments stored '' for manual triggers.
                let commit_sha = config
                    .get("commit_sha")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());

                let mut builder = DownloadRepoBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .repo_owner(repo_owner.to_string())
                    .repo_name(repo_name.to_string())
                    .is_public_repo(is_public_repo)
                    .branch_ref(branch_ref)
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone());

                // Add git_url for public repos
                if let Some(url) = git_url {
                    builder = builder.git_url(url);
                }

                // Add connection_id for private repos
                if let Some(id) = connection_id {
                    builder = builder.git_provider_connection_id(id);
                }

                // Add tag_ref if present (highest priority in checkout)
                if let Some(tag) = tag_ref {
                    builder = builder.tag_ref(tag);
                }

                // Add commit_sha if present (second priority in checkout)
                if let Some(commit) = commit_sha {
                    builder = builder.commit_sha(commit);
                }

                let job = builder.build(self.git_provider.clone())?;

                Ok(Arc::new(job))
            }

            "BuildImageJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let dockerfile_path = config
                    .get("dockerfile_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Dockerfile");

                // Get dependencies to find the download job
                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let download_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "download_repo".to_string());

                // Fetch deployment to get its URL for naming
                let deployment = deployments::Entity::find_by_id(db_job.deployment_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| {
                        WorkflowExecutionError::DeploymentNotFound(db_job.deployment_id)
                    })?;

                let image_tag = format!("{}:latest", deployment.slug);

                let mut builder = BuildImageJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .download_job_id(download_job_id)
                    .image_tag(image_tag)
                    .dockerfile_path(dockerfile_path.to_string())
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone());

                builder = builder
                    .preset(project.preset)
                    .preset_config(project.preset_config.clone());

                // Unseal build args from job_config. The planner derives build
                // args from env vars and stores them encrypted (build_args
                // contain everything env vars contain, including any value
                // marked secret — they leak into `docker history` if dumped
                // in plaintext).
                let enc_for_build_args = self.encryption_service.get();
                let build_args_map = crate::services::sensitive_envelope::read_sealed(
                    config,
                    enc_for_build_args,
                    "build_args",
                )
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
                if !build_args_map.is_empty() {
                    let build_args: Vec<(String, String)> = build_args_map.into_iter().collect();
                    builder = builder.build_args(build_args);
                }

                // Add build context if present (for monorepo subdirectories)
                if let Some(build_context_value) = config.get("build_context") {
                    if let Some(build_context_str) = build_context_value.as_str() {
                        if !build_context_str.is_empty() && build_context_str != "." {
                            builder = builder.build_context(build_context_str.to_string());
                        }
                    }
                }

                // Cross-build for any worker architecture this deployment could
                // be scheduled onto. Empty on a homogeneous cluster, which is
                // the single native build every deployment does today.
                //
                // This runs at build time even though placement happens later,
                // because the build job precedes scheduling in the workflow;
                // the node selectors below are the same ones the scheduler will
                // apply, so we never build for an architecture this deployment
                // cannot land on.
                //
                // Off unless the operator asked for it. Cross-builds are
                // emulated and slow, and deriving them from cluster topology
                // would let one node joining change build behaviour for every
                // deployment — including breaking builds outright on a control
                // plane without the matching QEMU binfmt handlers. Environment
                // setting wins, inheriting the project's when unset.
                let cross_builds_enabled = environment
                    .deployment_config
                    .as_ref()
                    .and_then(|c| c.cross_architecture_builds)
                    .or_else(|| {
                        project
                            .deployment_config
                            .as_ref()
                            .and_then(|c| c.cross_architecture_builds)
                    })
                    .unwrap_or(false);

                if !cross_builds_enabled {
                    debug!(
                        deployment_id = db_job.deployment_id,
                        "Cross-architecture builds disabled; building for the control plane's \
                         platform only"
                    );
                }

                if let (true, Some(scheduler)) = (cross_builds_enabled, self.node_scheduler.get()) {
                    let target_nodes = environment
                        .deployment_config
                        .as_ref()
                        .and_then(|c| c.target_nodes.clone())
                        .or_else(|| {
                            project
                                .deployment_config
                                .as_ref()
                                .and_then(|c| c.target_nodes.clone())
                        });
                    let target_labels = environment
                        .deployment_config
                        .as_ref()
                        .and_then(|c| c.target_labels.clone())
                        .or_else(|| {
                            project
                                .deployment_config
                                .as_ref()
                                .and_then(|c| c.target_labels.clone())
                        });

                    match scheduler
                        .required_build_platforms(target_labels.as_ref(), target_nodes.as_deref())
                        .await
                    {
                        Ok(platforms) if !platforms.is_empty() => {
                            info!(
                                deployment_id = db_job.deployment_id,
                                platforms = ?platforms,
                                "Cluster spans multiple architectures — building one image per platform"
                            );
                            builder = builder.target_platforms(platforms);
                        }
                        Ok(_) => {}
                        // Not being able to enumerate nodes must not block a
                        // build: fall back to the native-only build and let the
                        // deploy path's architecture check catch a mismatch.
                        Err(e) => {
                            warn!(
                                deployment_id = db_job.deployment_id,
                                "Could not determine cluster architectures ({}); \
                                 building for the control plane's platform only",
                                e
                            );
                        }
                    }
                }

                let job = builder.build(self.image_builder.clone())?;

                Ok(Arc::new(job))
            }

            "DeployContainerJob" | "DeployImageJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let port = config.get("port").and_then(|v| v.as_i64()).unwrap_or(3000) as u16;

                // Get replicas with priority: environment > project > job config > default (1)
                let replicas = environment
                    .deployment_config
                    .as_ref()
                    .map(|c| c.replicas as u32)
                    .or_else(|| {
                        project
                            .deployment_config
                            .as_ref()
                            .map(|c| c.replicas as u32)
                    })
                    .or_else(|| {
                        config
                            .get("replicas")
                            .and_then(|v| v.as_i64())
                            .map(|r| r as u32)
                    })
                    .unwrap_or(1);

                debug!(
                    "🔢 Deploying with {} replicas (env: {:?}, project: {:?})",
                    replicas,
                    environment.deployment_config.as_ref().map(|c| c.replicas),
                    project.deployment_config.as_ref().map(|c| c.replicas)
                );

                // Unseal sensitive maps from job_config. The planner stores
                // env vars / remote env vars / secrets as AES-256-GCM
                // ciphertext under `*_encrypted` keys (with a non-sensitive
                // `*_keys` list for debugging). `read_sealed` also falls back
                // to plaintext for jobs queued before the encryption rollout.
                let enc_for_unseal = self.encryption_service.get();
                let env_variables = crate::services::sensitive_envelope::read_sealed(
                    config,
                    enc_for_unseal,
                    "environment_variables",
                )
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;

                let remote_env_variables =
                    crate::services::sensitive_envelope::read_sealed_optional(
                        config,
                        enc_for_unseal,
                        "remote_environment_variables",
                    )
                    .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;

                // Secrets are mounted as files under /run/secrets/<KEY>; never
                // injected as env vars. Same envelope as above.
                let secrets = crate::services::sensitive_envelope::read_sealed(
                    config,
                    enc_for_unseal,
                    "secrets",
                )
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
                if !secrets.is_empty() {
                    debug!(
                        "🔐 Mounting {} secret file(s) into container: {}",
                        secrets.len(),
                        secrets.keys().cloned().collect::<Vec<_>>().join(", ")
                    );
                }
                debug!(
                    "🌍 Using {} environment variables for deployment (from job config): {}",
                    env_variables.len(),
                    env_variables.keys().cloned().collect::<Vec<_>>().join(", ")
                );

                // Get dependencies to find the build job
                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let build_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "build_image".to_string());

                // Fetch deployment to get its URL for naming
                let deployment = deployments::Entity::find_by_id(db_job.deployment_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| {
                        WorkflowExecutionError::DeploymentNotFound(db_job.deployment_id)
                    })?;

                // Check if this is an external image deployment (from remote deployment)
                let use_external_image = config
                    .get("use_external_image")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let external_image_tag = if use_external_image {
                    config
                        .get("image_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                };

                // Resolve target_nodes from environment or project config
                let target_nodes = environment
                    .deployment_config
                    .as_ref()
                    .and_then(|c| c.target_nodes.clone())
                    .or_else(|| {
                        project
                            .deployment_config
                            .as_ref()
                            .and_then(|c| c.target_nodes.clone())
                    });

                // Resolve target_labels from environment or project config
                let target_labels = environment
                    .deployment_config
                    .as_ref()
                    .and_then(|c| c.target_labels.clone())
                    .or_else(|| {
                        project
                            .deployment_config
                            .as_ref()
                            .and_then(|c| c.target_labels.clone())
                    });

                // Resolve CPU/memory limits + requests from environment first,
                // then project. Each field is resolved independently so an
                // env override can supply only memory while inheriting the
                // project's CPU. When neither side configures a value we leave
                // it unset and the deployer applies no limit at the Docker
                // layer (rather than the previous hardcoded 1000m/512Mi).
                let env_cfg = environment.deployment_config.as_ref();
                let proj_cfg = project.deployment_config.as_ref();
                let resolve_i32 = |getter: fn(
                    &temps_entities::deployment_config::DeploymentConfig,
                ) -> Option<i32>|
                 -> Option<i32> {
                    env_cfg
                        .and_then(getter)
                        .or_else(|| proj_cfg.and_then(getter))
                };
                // CPU values are stored as microcores in the DB (1_000_000 = 1 core);
                // memory is stored as MB. Format with explicit suffixes the deployer
                // understands: `u` for microcores, `Mi` for mebibytes.
                let cpu_limit_micro = resolve_i32(|c| c.cpu_limit);
                let memory_limit_mb = resolve_i32(|c| c.memory_limit);
                let cpu_request_micro = resolve_i32(|c| c.cpu_request);
                let memory_request_mb = resolve_i32(|c| c.memory_request);
                let resources = ResourceUsage {
                    cpu_limit: cpu_limit_micro.map(|u| format!("{}u", u)),
                    memory_limit: memory_limit_mb.map(|mb| format!("{}Mi", mb)),
                    cpu_request: cpu_request_micro.map(|u| format!("{}u", u)),
                    memory_request: memory_request_mb.map(|mb| format!("{}Mi", mb)),
                };

                let mut builder = DeployImageJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .build_job_id(build_job_id)
                    .target(DeploymentTarget::Docker {
                        registry_url: "local".to_string(),
                        network: Some(temps_core::NETWORK_NAME.to_string()),
                    })
                    .service_name(deployment.slug.clone())
                    .namespace("default".to_string())
                    .port(port as u32)
                    .replicas(replicas)
                    .environment_variables(env_variables)
                    .remote_environment_variables(remote_env_variables)
                    .secrets(secrets)
                    .resources(resources)
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone());

                // Apply explicit deploy-time health-check path override (image/static
                // deploys can't read .temps.yaml, so the deploy request carries it on
                // the deployment metadata). When set, it wins over .temps.yaml.
                if let Some(health_path) = deployment
                    .metadata
                    .as_ref()
                    .and_then(|m| m.health_check_path.clone())
                {
                    debug!(
                        "🩺 Applying deploy-time health check path override: {}",
                        health_path
                    );
                    builder = builder.health_check_path_override(Some(health_path));
                }

                // Inject node scheduler for multi-node support
                if let Some(scheduler) = self.node_scheduler.get() {
                    builder = builder.node_scheduler(scheduler.clone());
                }

                // Inject encryption service for decrypting node tokens
                if let Some(enc_service) = self.encryption_service.get() {
                    builder = builder.encryption_service(enc_service.clone());
                }
                // Config service for resolving the cluster CA when deploying to
                // https:// nodes over mTLS (ADR-020 WS-2.1).
                builder = builder.config_service(self.config_service.clone());

                // Inject image builder for transferring images to remote nodes
                builder = builder.image_builder(self.image_builder.clone());

                // Set target nodes if configured
                if let Some(nodes) = target_nodes {
                    builder = builder.target_nodes(nodes);
                }

                // Set target labels for label-based node scheduling
                if let Some(labels) = target_labels {
                    builder = builder.target_labels(labels);
                }

                // Resolve anti-affinity from environment or project config
                let anti_affinity = environment
                    .deployment_config
                    .as_ref()
                    .map(|c| c.anti_affinity)
                    .unwrap_or_else(|| {
                        project
                            .deployment_config
                            .as_ref()
                            .map(|c| c.anti_affinity)
                            .unwrap_or(true)
                    });
                builder = builder.anti_affinity(anti_affinity);

                // Rolling update awareness: query existing containers for this
                // environment so the scheduler can avoid placing new replicas on
                // nodes that still host outgoing containers.
                if anti_affinity {
                    use temps_entities::deployment_containers;

                    // Find the most recent previous deployment for this environment
                    let prev_deployment = deployments::Entity::find()
                        .filter(deployments::Column::EnvironmentId.eq(deployment.environment_id))
                        .filter(deployments::Column::ProjectId.eq(deployment.project_id))
                        .filter(deployments::Column::Id.ne(deployment.id))
                        .filter(deployments::Column::State.eq("deployed"))
                        .order_by_desc(deployments::Column::CreatedAt)
                        .one(self.db.as_ref())
                        .await
                        .ok()
                        .flatten();

                    if let Some(prev) = prev_deployment {
                        let existing_containers = deployment_containers::Entity::find()
                            .filter(deployment_containers::Column::DeploymentId.eq(prev.id))
                            .filter(deployment_containers::Column::DeletedAt.is_null())
                            .all(self.db.as_ref())
                            .await
                            .unwrap_or_default();

                        let exclude_ids: Vec<i32> = existing_containers
                            .iter()
                            .filter_map(|c| c.node_id)
                            .collect::<std::collections::HashSet<_>>()
                            .into_iter()
                            .collect();

                        if !exclude_ids.is_empty() {
                            debug!(
                                "Anti-affinity: excluding {} node(s) with existing containers: {:?}",
                                exclude_ids.len(),
                                exclude_ids
                            );
                            builder = builder.exclude_node_ids(exclude_ids);
                        }
                    }
                }

                // If using external image, set the image tag directly (bypasses build job lookup)
                if let Some(image_tag) = external_image_tag {
                    debug!("🐳 Using external image tag for deployment: {}", image_tag);
                    builder = builder.external_image_tag(image_tag);
                }

                // Apply container log rotation settings from config
                if let Ok(settings) = self.config_service.get_settings().await {
                    builder =
                        builder.container_log_config(temps_deployer::ContainerLogConfig::new(
                            settings.container_logs.max_size.clone(),
                            settings.container_logs.max_file,
                        ));
                }

                let job = builder.build(self.container_deployer.clone())?;

                Ok(Arc::new(job))
            }

            "ConfigureCronsJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                // Get download_job_id from config, fallback to default
                let download_job_id = config
                    .get("download_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("download_repo")
                    .to_string();

                // Get deploy_container_job_id from dependencies
                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let deploy_container_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "deploy_container".to_string());

                // Get cron service
                let cron_service = self.cron_service.clone();

                let job = ConfigureCronsJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .download_job_id(download_job_id)
                    .deploy_container_job_id(deploy_container_job_id)
                    .project_id(project.id)
                    .environment_id(environment.id)
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .build(self.db.clone(), cron_service)?;

                Ok(Arc::new(job))
            }

            "ConfigureMetricAlertsJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let download_job_id = config
                    .get("download_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("download_repo")
                    .to_string();

                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let deploy_container_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "mark_deployment_complete".to_string());

                let alert_service = self.alert_service.clone();

                let job = ConfigureMetricAlertsJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .download_job_id(download_job_id)
                    .deploy_container_job_id(deploy_container_job_id)
                    .project_id(project.id)
                    .environment_id(environment.id)
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .build(self.db.clone(), alert_service)?;

                Ok(Arc::new(job))
            }

            "ConfigureAgentsJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let download_job_id = config
                    .get("download_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("download_repo")
                    .to_string();

                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let deploy_container_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "deploy_container".to_string());

                let agent_sync_service = self.agent_sync_service.clone();

                let job = ConfigureAgentsJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .download_job_id(download_job_id)
                    .deploy_container_job_id(deploy_container_job_id)
                    .project_id(project.id)
                    .log_id(Some(db_job.log_id.clone()))
                    .log_service(self.log_service.clone())
                    .build(self.db.clone(), agent_sync_service)?;

                Ok(Arc::new(job))
            }

            "MarkDeploymentCompleteJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let deployment_id = config
                    .get("deployment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "deployment_id is required".to_string(),
                        )
                    })? as i32;

                let mut builder = crate::jobs::MarkDeploymentCompleteJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .deployment_id(deployment_id)
                    .db(self.db.clone())
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .container_deployer(self.container_deployer.clone())
                    .queue(self.queue.clone())
                    .config_service(self.config_service.clone());

                if let Some(enc_service) = self.encryption_service.get() {
                    builder = builder.encryption_service(enc_service.clone());
                }
                // Config service for resolving the cluster CA when deploying to
                // https:// nodes over mTLS (ADR-020 WS-2.1).
                builder = builder.config_service(self.config_service.clone());

                let job = builder.build()?;

                Ok(Arc::new(job))
            }

            "TakeScreenshotJob" => {
                // Screenshot service is always available now
                let screenshot_service = &self.screenshot_service;

                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let deployment_id = config
                    .get("deployment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "deployment_id is required".to_string(),
                        )
                    })? as i32;

                let job = crate::jobs::TakeScreenshotJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .deployment_id(deployment_id)
                    .screenshot_service(screenshot_service.clone())
                    .config_service(self.config_service.clone())
                    .db(self.db.clone())
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .build()?;

                Ok(Arc::new(job))
            }

            "ScanVulnerabilitiesJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let deployment_id = config
                    .get("deployment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "deployment_id is required".to_string(),
                        )
                    })? as i32;

                let project_id = config
                    .get("project_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "project_id is required".to_string(),
                        )
                    })? as i32;

                let environment_id = config
                    .get("environment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "environment_id is required".to_string(),
                        )
                    })? as i32;

                let branch = config
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig("branch is required".to_string())
                    })?
                    .to_string();

                // Manual triggers (redeploy, import) have no commit — the
                // scan still runs against the built image; the commit is
                // only recorded as scan metadata when known.
                let commit_hash = config
                    .get("commit_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let download_job_id = config
                    .get("download_job_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "download_job_id is required".to_string(),
                        )
                    })?
                    .to_string();

                let build_job_id = config
                    .get("build_job_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "build_job_id is required".to_string(),
                        )
                    })?
                    .to_string();

                let job = crate::jobs::ScanVulnerabilitiesJob::new(
                    db_job.job_id.clone(),
                    deployment_id,
                    project_id,
                    environment_id,
                    branch,
                    commit_hash,
                    download_job_id,
                    build_job_id,
                    self.db.clone(),
                )
                .with_log_id(db_job.log_id.clone())
                .with_log_service(self.log_service.clone());

                Ok(Arc::new(job))
            }

            "CaptureSourceMapsJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let deployment_id = config
                    .get("deployment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "deployment_id is required".to_string(),
                        )
                    })? as i32;

                let project_id = config
                    .get("project_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "project_id is required".to_string(),
                        )
                    })? as i32;

                let release = config
                    .get("release")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig("release is required".to_string())
                    })?
                    .to_string();

                let build_job_id = config
                    .get("build_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("build_image")
                    .to_string();

                let search_paths: Vec<String> = config
                    .get("search_paths")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let path_rewrites: Vec<(String, String)> = config
                    .get("path_rewrites")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let source_map_service = self
                    .source_map_service
                    .get()
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "SourceMapService not configured".to_string(),
                        )
                    })?
                    .clone();

                let job = crate::jobs::CaptureSourceMapsJob::new(
                    db_job.job_id.clone(),
                    deployment_id,
                    project_id,
                    release,
                    build_job_id,
                    search_paths,
                    path_rewrites,
                    self.image_builder.clone(),
                    source_map_service,
                )
                .with_log_id(db_job.log_id.clone())
                .with_log_service(self.log_service.clone());

                Ok(Arc::new(job))
            }

            "CaptureSourceFilesJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let project_id = config
                    .get("project_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "project_id is required".to_string(),
                        )
                    })? as i32;

                let release = config
                    .get("release")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig("release is required".to_string())
                    })?
                    .to_string();

                let download_job_id = config
                    .get("download_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("download_repo")
                    .to_string();

                let build_job_id = config
                    .get("build_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("build_image")
                    .to_string();

                // None (or null) = default to the Docker build context.
                let error_source_root = config
                    .get("error_source_root")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let extensions: Vec<String> = config
                    .get("extensions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let source_map_service = self
                    .source_map_service
                    .get()
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "SourceMapService not configured".to_string(),
                        )
                    })?
                    .clone();

                let job = crate::jobs::CaptureSourceFilesJob::new(
                    db_job.job_id.clone(),
                    project_id,
                    release,
                    download_job_id,
                    build_job_id,
                    error_source_root,
                    extensions,
                    source_map_service,
                )
                .with_log_id(db_job.log_id.clone())
                .with_log_service(self.log_service.clone());

                Ok(Arc::new(job))
            }

            "PersistStaticAssetsJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let deployment_id = config
                    .get("deployment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "deployment_id is required".to_string(),
                        )
                    })? as i32;

                let project_id = config
                    .get("project_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "project_id is required".to_string(),
                        )
                    })? as i32;

                let environment_id = config
                    .get("environment_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "environment_id is required".to_string(),
                        )
                    })? as i32;

                let build_job_id = config
                    .get("build_job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("build_image")
                    .to_string();

                let search_paths: Vec<String> = config
                    .get("search_paths")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let path_rewrites: Vec<(String, String)> = config
                    .get("path_rewrites")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let deployment_slug = config
                    .get("deployment_slug")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&deployment_id.to_string())
                    .to_string();

                let chunks_dir = self
                    .config_service
                    .static_dir()
                    .join("chunks")
                    .join(project_id.to_string())
                    .join(environment_id.to_string())
                    .join(deployment_slug);

                let mut job = crate::jobs::PersistStaticAssetsJob::new(
                    db_job.job_id.clone(),
                    deployment_id,
                    project_id,
                    environment_id,
                    build_job_id,
                    search_paths,
                    path_rewrites,
                    self.image_builder.clone(),
                    chunks_dir,
                )
                .with_log_id(db_job.log_id.clone())
                .with_log_service(self.log_service.clone());

                // Wire file store for CAS blob storage
                if let Some(store) = self.file_store.get() {
                    job = job.with_file_store(store.clone());
                }

                // Wire database for URL→hash mapping
                job = job.with_db(self.db.clone());

                Ok(Arc::new(job))
            }

            "DeployStaticJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                // Get dependencies to find the build job (DeployStaticJob depends on BuildImageJob)
                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let build_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "build_image".to_string());

                // Get static output directory from config (path inside container after build)
                // This is typically "/app/dist" or "/app/build" depending on the framework
                let static_output_dir = config
                    .get("static_output_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("/app/dist")
                    .to_string();

                debug!(
                    "📦 DeployStaticJob will extract files from container path: {}",
                    static_output_dir
                );

                // Fetch deployment to get slugs
                let deployment = deployments::Entity::find_by_id(db_job.deployment_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| {
                        WorkflowExecutionError::DeploymentNotFound(db_job.deployment_id)
                    })?;

                let job = DeployStaticJob::new(
                    db_job.job_id.clone(),
                    build_job_id,
                    static_output_dir,
                    project.slug.clone(),
                    environment.slug.clone(),
                    deployment.slug.clone(),
                    self.static_deployer.clone(),
                    self.image_builder.clone(),
                )
                .with_log_id(db_job.log_id.clone())
                .with_log_service(self.log_service.clone());

                Ok(Arc::new(job))
            }

            "PullExternalImageJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let image_ref = config
                    .get("image_ref")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "image_ref is required".to_string(),
                        )
                    })?
                    .to_string();

                let external_image_id = config
                    .get("external_image_id")
                    .and_then(|v| v.as_i64())
                    .map(|id| id as i32);

                let mut job = PullExternalImageJob::new(
                    db_job.job_id.clone(),
                    image_ref,
                    external_image_id,
                    self.docker.clone(),
                )
                .with_log_service(self.log_service.clone(), db_job.log_id.clone());

                // Pass private registry credentials when configured
                if let Ok(settings) = self.config_service.get_settings().await {
                    let reg = &settings.docker_registry;
                    if reg.enabled {
                        if let (Some(username), Some(password)) =
                            (reg.username.clone(), reg.password.clone())
                        {
                            job = job.with_registry_credentials(bollard::auth::DockerCredentials {
                                username: Some(username),
                                password: Some(password),
                                serveraddress: reg.registry_url.clone(),
                                ..Default::default()
                            });
                        }
                    }
                }

                Ok(Arc::new(job))
            }

            "VerifyLocalImageJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let image_ref = config
                    .get("image_ref")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "image_ref is required".to_string(),
                        )
                    })?
                    .to_string();

                let expected_image_id = config
                    .get("expected_image_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let job = VerifyLocalImageJob::new(
                    db_job.job_id.clone(),
                    image_ref,
                    expected_image_id,
                    self.docker.clone(),
                )
                .with_log_service(self.log_service.clone(), db_job.log_id.clone());

                Ok(Arc::new(job))
            }

            "DeployStaticBundleJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let bundle_path = config
                    .get("bundle_path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "bundle_path is required".to_string(),
                        )
                    })?
                    .to_string();

                let content_type = config
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("application/gzip")
                    .to_string();

                let static_bundle_id = config
                    .get("static_bundle_id")
                    .and_then(|v| v.as_i64())
                    .map(|id| id as i32);
                let source_directory = config
                    .get("source_directory")
                    .and_then(|value| value.as_str())
                    .unwrap_or(".")
                    .to_string();

                // Get data directory from config service for local storage
                let data_dir = self.config_service.data_dir();

                let job = DeployStaticBundleJob::new(
                    db_job.job_id.clone(),
                    project.id,
                    bundle_path,
                    content_type,
                    static_bundle_id,
                    project.slug.clone(),
                    environment.slug.clone(),
                    deployment.slug.clone(),
                    source_directory,
                    data_dir,
                    self.static_deployer.clone(),
                )
                .with_log_service(self.log_service.clone(), db_job.log_id.clone());

                Ok(Arc::new(job))
            }

            "PrepareSourceBundleJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;
                let archive_path = config
                    .get("archive_path")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "archive_path is required".to_string(),
                        )
                    })?;
                let archive_path = std::path::Path::new(archive_path);
                if archive_path.is_absolute()
                    || archive_path.components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::ParentDir
                                | std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                        )
                    })
                {
                    return Err(WorkflowExecutionError::InvalidJobConfig(
                        "source archive path must remain inside the Temps data directory"
                            .to_string(),
                    ));
                }
                let absolute_path = self.config_service.data_dir().join(archive_path);
                Ok(Arc::new(PrepareSourceBundleJob::new(
                    db_job.job_id.clone(),
                    absolute_path,
                    project.slug.clone(),
                )))
            }

            // Unsupported job types - log warning but don't fail the entire workflow
            "HealthCheckJob" | "DeployBasicJob" | "BuildStaticJob" => {
                warn!(
                    "Skipping unsupported job type: {} (not yet implemented)",
                    db_job.job_type
                );
                // Return a no-op job that succeeds immediately
                Err(WorkflowExecutionError::UnsupportedJobType(
                    db_job.job_type.clone(),
                ))
            }

            "DeployComposeJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let compose_path = config
                    .get("compose_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let env_vars = crate::services::sensitive_envelope::read_sealed(
                    config,
                    self.encryption_service.get(),
                    "environment_vars",
                )
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
                let build_args = crate::services::sensitive_envelope::read_sealed(
                    config,
                    self.encryption_service.get(),
                    "build_args",
                )
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;

                // Projects created before directory normalization was applied on
                // every write path can hold "" or "/" here. Both are rejected by
                // the job's path confinement, so normalize to the repo root
                // marker instead of failing the deployment.
                let directory = config
                    .get("directory")
                    .and_then(|v| v.as_str())
                    .map(|d| d.trim().trim_start_matches('/'))
                    .filter(|d| !d.is_empty())
                    .unwrap_or(".")
                    .to_string();

                // Get dependencies to find the download job ID
                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let download_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "download_repo".to_string());

                // Inline compose content (for manual projects without git repo)
                let compose_content = config
                    .get("compose_content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // User-provided compose override from project preset_config
                let compose_override = project.preset_config.as_ref().and_then(|pc| {
                    if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc {
                        cfg.compose_override.clone()
                    } else {
                        None
                    }
                });

                // Services the user opted to exclude from this project's compose
                // stack (e.g. an unmanaged database in favor of a Temps-managed one).
                let (excluded_services, relaxed_capability_services, unsandboxed_services) =
                    compose_service_security_settings(project.preset_config.as_ref());
                let public_ports = project
                    .preset_config
                    .as_ref()
                    .and_then(|config| match config {
                        temps_entities::preset::PresetConfig::DockerCompose(config) => {
                            Some(config.public_ports.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();

                let compose_executor = Arc::new(temps_deployer::compose::ComposeExecutor::new(
                    self.docker.clone(),
                    self.config_service.data_dir(),
                ));

                let job = crate::jobs::DeployComposeJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .deployment_id(deployment.id)
                    .project_id(project.id)
                    .environment_id(environment.id)
                    .compose_executor(compose_executor)
                    .compose_path(compose_path)
                    .directory(directory)
                    .compose_content(compose_content)
                    .compose_override(compose_override)
                    .excluded_services(excluded_services)
                    .relaxed_capability_services(relaxed_capability_services)
                    .unsandboxed_services(unsandboxed_services)
                    .public_ports(public_ports)
                    .download_job_id(download_job_id)
                    .environment_vars(env_vars)
                    .build_args(build_args)
                    .log_id(Some(db_job.log_id.clone()))
                    .log_service(self.log_service.clone())
                    .build()?;

                Ok(Arc::new(job))
            }

            _ => {
                warn!("Unknown job type: {}", db_job.job_type);
                Err(WorkflowExecutionError::UnsupportedJobType(
                    db_job.job_type.clone(),
                ))
            }
        }
    }

    #[allow(dead_code)]
    async fn update_deployment_status(
        &self,
        deployment_id: i32,
        status: temps_entities::types::PipelineStatus,
    ) -> Result<(), WorkflowExecutionError> {
        self.update_deployment_status_with_reason(deployment_id, status, None)
            .await
    }

    async fn update_deployment_status_with_reason(
        &self,
        deployment_id: i32,
        status: temps_entities::types::PipelineStatus,
        cancelled_reason: Option<String>,
    ) -> Result<(), WorkflowExecutionError> {
        use sea_orm::{ActiveModelTrait, Set};

        let deployment = self.get_deployment(deployment_id).await?;
        let mut active_deployment: deployments::ActiveModel = deployment.into();

        // Also update the state field (string representation)
        let state_str = match status {
            temps_entities::types::PipelineStatus::Pending => "pending",
            temps_entities::types::PipelineStatus::Running => "running",
            temps_entities::types::PipelineStatus::Built => "built",
            temps_entities::types::PipelineStatus::Completed => "completed",
            temps_entities::types::PipelineStatus::Failed => "failed",
            temps_entities::types::PipelineStatus::Cancelled => "cancelled",
        };
        active_deployment.state = Set(state_str.to_string());

        if let Some(ref reason) = cancelled_reason {
            active_deployment.cancelled_reason = Set(Some(reason.clone()));
        }

        // Set timestamps based on status
        match status {
            temps_entities::types::PipelineStatus::Running => {
                active_deployment.started_at = Set(Some(chrono::Utc::now()));
            }
            temps_entities::types::PipelineStatus::Completed
            | temps_entities::types::PipelineStatus::Failed
            | temps_entities::types::PipelineStatus::Cancelled => {
                active_deployment.finished_at = Set(Some(chrono::Utc::now()));
            }
            _ => {}
        }

        active_deployment.updated_at = Set(chrono::Utc::now());
        let updated_deployment = active_deployment.update(self.db.as_ref()).await?;

        // Fire deployment lifecycle events to queue
        // Get environment for environment_name
        let environment = environments::Entity::find_by_id(updated_deployment.environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                WorkflowExecutionError::Validation(format!(
                    "Environment {} not found for deployment {}",
                    updated_deployment.environment_id, deployment_id
                ))
            })?;

        match status {
            temps_entities::types::PipelineStatus::Completed => {
                // Get deployment URL from environment: prefer custom host, fall back to preview domain
                let url = if !environment.host.is_empty() {
                    Some(format!("https://{}", environment.host))
                } else if !environment.subdomain.is_empty() {
                    // No custom host set — construct URL from preview domain
                    match self
                        .config_service
                        .get_deployment_url_by_slug(&environment.subdomain)
                        .await
                    {
                        Ok(preview_url) => Some(preview_url),
                        Err(e) => {
                            tracing::debug!(
                                "Failed to construct preview domain URL for environment {}: {}",
                                updated_deployment.environment_id,
                                e
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                // Extract health_check_path from deployment job outputs if available
                let health_check_path = self
                    .get_health_check_path_from_jobs(updated_deployment.id)
                    .await;

                let event = Job::DeploymentSucceeded(temps_core::DeploymentSucceededJob {
                    deployment_id: updated_deployment.id,
                    project_id: updated_deployment.project_id,
                    environment_id: updated_deployment.environment_id,
                    environment_name: environment.name.clone(),
                    commit_sha: updated_deployment.commit_sha.clone(),
                    url,
                    health_check_path,
                });
                if let Err(e) = self.queue.send(event).await {
                    error!("Failed to send DeploymentSucceeded event: {}", e);
                } else {
                    debug!(
                        "Sent DeploymentSucceeded event for deployment {}",
                        deployment_id
                    );
                }

                // Anonymous telemetry for deploy success is NOT emitted here.
                // On the real success path, finalization is done by
                // MarkDeploymentCompleteJob (which writes state="completed"
                // directly), so this Completed arm is never reached for a
                // normal deploy. `deploy_succeeded` / `first_deploy_succeeded`
                // are emitted in execute_deployment_workflow's Ok arm instead —
                // emitting here as well would double-count if this arm ever
                // gains a caller.
            }
            temps_entities::types::PipelineStatus::Failed => {
                // Best-effort labels used only for failure telemetry. Keep the
                // lookup inside this arm so other status transitions do not pay
                // for an unrelated project query.
                let (telemetry_source_type, telemetry_preset, telemetry_template_slug) =
                    match projects::Entity::find_by_id(updated_deployment.project_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        Ok(Some(project)) => (
                            Some(project.source_type.to_string()),
                            Some(temps_presets::runtime_slug(
                                project.preset,
                                project.preset_config.as_ref(),
                            )),
                            project.template_slug,
                        ),
                        _ => (None, None, None),
                    };

                let event = Job::DeploymentFailed(temps_core::DeploymentFailedJob {
                    deployment_id: updated_deployment.id,
                    project_id: updated_deployment.project_id,
                    environment_id: updated_deployment.environment_id,
                    environment_name: environment.name.clone(),
                    error_message: cancelled_reason.clone().map(|s| s.to_string()),
                });
                if let Err(e) = self.queue.send(event).await {
                    error!("Failed to send DeploymentFailed event: {}", e);
                } else {
                    debug!(
                        "Sent DeploymentFailed event for deployment {}",
                        deployment_id
                    );
                }

                // Anonymous telemetry: terminal failure point. The raw
                // `cancelled_reason` may contain build logs / paths / repo
                // names, so it is NEVER sent — only fixed allowlisted stage,
                // code, and legacy category labels. preset/source_type are
                // included so we can compare failure rates by build type.
                self.telemetry().report(deploy_failed_telemetry_event(
                    cancelled_reason.as_deref(),
                    telemetry_source_type.clone(),
                    telemetry_preset.clone(),
                    telemetry_template_slug.clone(),
                ));
            }
            temps_entities::types::PipelineStatus::Cancelled => {
                let event = Job::DeploymentCancelled(temps_core::DeploymentCancelledJob {
                    deployment_id: updated_deployment.id,
                    project_id: updated_deployment.project_id,
                    environment_id: updated_deployment.environment_id,
                    environment_name: environment.name,
                });
                if let Err(e) = self.queue.send(event).await {
                    error!("Failed to send DeploymentCancelled event: {}", e);
                } else {
                    debug!(
                        "Sent DeploymentCancelled event for deployment {}",
                        deployment_id
                    );
                }

                // Anonymous telemetry: this arm only runs when the workflow
                // executor itself detected a "cancelled" error and the
                // deployment wasn't already marked cancelled (see the
                // `deployment.state != "cancelled"` guard in
                // execute_deployment_workflow's Err arm above) — i.e. a
                // cancellation whose origin wasn't the explicit user-cancel
                // path in DeploymentService::cancel_deployment (which emits
                // its own `deploy_cancelled` with trigger="user"). Tagging
                // this one "workflow" keeps the two mutually exclusive so
                // the funnel is never double-counted.
                self.telemetry().report(
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::DeployCancelled,
                    )
                    .with("trigger", "workflow"),
                );
            }
            _ => {}
        }

        Ok(())
    }

    /// DEPRECATED: This logic is now handled by MarkDeploymentCompleteJob
    #[allow(dead_code)]
    async fn update_deployment_from_context(
        &self,
        deployment_id: i32,
        context: &temps_core::WorkflowContext,
    ) -> Result<(), WorkflowExecutionError> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::deployment_containers;

        debug!(
            "📝 Updating deployment {} with workflow outputs",
            deployment_id
        );

        let deployment = self.get_deployment(deployment_id).await?;
        let mut active_deployment: deployments::ActiveModel = deployment.into();

        // Extract image info from build job output
        if let Ok(Some(image_tag)) = context.get_output::<String>("build_image", "image_tag") {
            active_deployment.image_name = Set(Some(image_tag));
        }

        // Extract container info from deploy job output and create deployment_container records
        if let Ok(Some(container_id)) =
            context.get_output::<String>("deploy_container", "container_id")
        {
            let container_name = context
                .get_output::<String>("deploy_container", "container_name")
                .ok()
                .flatten()
                .unwrap_or_else(|| format!("container-{}", deployment_id));

            let container_port = context
                .get_output::<i32>("deploy_container", "container_port")
                .ok()
                .flatten()
                .unwrap_or(8080);

            let host_port = context
                .get_output::<i32>("deploy_container", "host_port")
                .ok()
                .flatten();

            let now = chrono::Utc::now();

            // Create deployment_container record
            let deployment_container = deployment_containers::ActiveModel {
                deployment_id: Set(deployment_id),
                container_id: Set(container_id.clone()),
                container_name: Set(container_name.clone()),
                container_port: Set(container_port),
                host_port: Set(host_port),
                image_name: Set(match &active_deployment.image_name {
                    sea_orm::ActiveValue::Set(v) => v.clone(),
                    sea_orm::ActiveValue::Unchanged(v) => v.clone(),
                    _ => None,
                }),
                status: Set(Some("running".to_string())),
                created_at: Set(now),
                deployed_at: Set(now),
                ready_at: Set(Some(now)), // Assume ready immediately for now
                deleted_at: Set(None),
                ..Default::default()
            };

            deployment_container.insert(self.db.as_ref()).await?;

            info!(
                "Created deployment_container record for container {}",
                container_id
            );
        }

        // Update state to deployed
        active_deployment.state = Set("deployed".to_string());

        active_deployment.update(self.db.as_ref()).await?;

        info!("Updated deployment {} with workflow outputs", deployment_id);
        Ok(())
    }

    /// DEPRECATED: This logic is now handled by MarkDeploymentCompleteJob
    /// Update the environment's current_deployment_id to point to this deployment
    #[allow(dead_code)]
    async fn update_environment_current_deployment(
        &self,
        deployment_id: i32,
    ) -> Result<(), WorkflowExecutionError> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::environments;

        debug!(
            "📝 Updating environment to set current deployment to {}",
            deployment_id
        );

        // Get the deployment to find its environment_id
        let deployment = self.get_deployment(deployment_id).await?;

        // Update the environment (only if not soft-deleted)
        let environment = environments::Entity::find_by_id(deployment.environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                WorkflowExecutionError::EnvironmentNotFound(deployment.environment_id)
            })?;

        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(deployment_id));

        active_environment.update(self.db.as_ref()).await?;

        info!(
            "Updated environment {} to point to deployment {}",
            deployment.environment_id, deployment_id
        );
        Ok(())
    }

    /// Teardown (stop and remove) ALL previous deployments in an environment
    /// Returns the container_id of the first stopped container, if any
    /// Excludes the current deployment_id to avoid stopping the newly deployed container
    ///
    /// This cleans up ALL old containers for this project/environment, not just the most recent one.
    /// This prevents orphaned containers from accumulating when deployments fail or cleanup is missed.
    async fn teardown_previous_deployment(
        &self,
        project_id: i32,
        environment_id: i32,
        current_deployment: &deployments::Model,
    ) -> Result<Option<String>, WorkflowExecutionError> {
        use temps_entities::deployment_containers;

        // Find active deployments that are chronologically older than the
        // current one. An older workflow can finish after a newer workflow;
        // `id != current` would let it tear the newer deployment back down.
        //
        // Scope this to active states and cap the result, newest-first. This is a
        // safety net that runs in addition to MarkDeploymentCompleteJob's own
        // teardown (which also flips torn-down deployments to "stopped"), so the
        // set of candidates here shrinks to truly-active deployments instead of
        // re-scanning the whole environment history on every deploy. Without
        // these bounds this `.all()` grows unbounded with deployment count and
        // tears down containers sequentially — minutes of work on busy
        // environments. "failed"/"stopped" are excluded to preserve history and
        // avoid re-processing already-stopped deployments.
        const MAX_TEARDOWN_DEPLOYMENTS: u64 = 25;
        let previous_deployments = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(
                Condition::any()
                    .add(deployments::Column::CreatedAt.lt(current_deployment.created_at))
                    .add(
                        Condition::all()
                            .add(deployments::Column::CreatedAt.eq(current_deployment.created_at))
                            .add(deployments::Column::Id.lt(current_deployment.id)),
                    ),
            )
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "built",
                "completed",
            ]))
            .order_by_desc(deployments::Column::CreatedAt)
            .limit(MAX_TEARDOWN_DEPLOYMENTS)
            .all(self.db.as_ref())
            .await?;

        let mut first_stopped_container_id: Option<String> = None;
        let mut total_containers_cleaned = 0;

        for deployment in previous_deployments {
            // Get all non-deleted containers for this deployment
            let containers = deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment.id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.db.as_ref())
                .await?;

            if containers.is_empty() {
                continue;
            }

            info!(
                "Tearing down deployment {} ({} containers)",
                deployment.id,
                containers.len()
            );

            for container in containers {
                let container_id = container.container_id.clone();

                // Mark the row deleted *before* stopping the container in Docker.
                // ContainerHealthMonitor (temps-monitoring) polls
                // `deployment_containers` filtered on `DeletedAt.is_null()` on its
                // own independent schedule. If that poll lands between
                // `stop_container()` below (which puts Docker's container state
                // into `Exited`) and this row being marked deleted, it has no way
                // to tell the exit was an intentional teardown and fires a false
                // ContainerCrash alarm. Writing `deleted_at` first closes that
                // window: once this update commits, the health monitor's next
                // query no longer returns this row at all.
                use sea_orm::{ActiveModelTrait, Set};
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("deleted".to_string()));
                active_container.update(self.db.as_ref()).await?;

                // Stop and remove the container
                match self.container_deployer.stop_container(&container_id).await {
                    Ok(_) => {
                        info!("Stopped container {}", container_id);
                    }
                    Err(e) => {
                        warn!("Failed to stop container {}: {}", container_id, e);
                    }
                }

                match self
                    .container_deployer
                    .remove_container(&container_id)
                    .await
                {
                    Ok(_) => {
                        info!("Removed container {}", container_id);
                    }
                    Err(e) => {
                        warn!("Failed to remove container {}: {}", container_id, e);
                    }
                }

                if first_stopped_container_id.is_none() {
                    first_stopped_container_id = Some(container_id);
                }
                total_containers_cleaned += 1;
            }
        }

        if total_containers_cleaned > 0 {
            info!(
                "Cleaned up {} containers from previous deployments",
                total_containers_cleaned
            );
        }

        Ok(first_stopped_container_id)
    }
}

/// No-op log writer since jobs handle their own logging
struct NoOpLogWriter;

#[async_trait::async_trait]
impl temps_core::LogWriter for NoOpLogWriter {
    async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
        // Jobs write to their own log files directly, so this is a no-op
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        0 // Not used since jobs handle their own logging
    }
}

/// Database-backed cancellation provider that checks deployment state
pub(crate) struct DatabaseCancellationProvider {
    db: Arc<DbConnection>,
    deployment_id: i32,
}

impl DatabaseCancellationProvider {
    pub(crate) fn new(db: Arc<DbConnection>, deployment_id: i32) -> Self {
        Self { db, deployment_id }
    }
}

#[async_trait::async_trait]
impl WorkflowCancellationProvider for DatabaseCancellationProvider {
    async fn is_cancelled(&self, workflow_run_id: &str) -> Result<bool, WorkflowError> {
        // Check deployment state in database
        match deployments::Entity::find_by_id(self.deployment_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(deployment)) => {
                let is_cancelled = deployment.state == "cancelled";
                if is_cancelled {
                    info!(
                        "Cancellation detected for deployment {} (workflow: {}) - stopping workflow execution",
                        self.deployment_id, workflow_run_id
                    );
                }
                Ok(is_cancelled)
            }
            Ok(None) => {
                warn!(
                    "Deployment {} not found during cancellation check; treating it as cancelled",
                    self.deployment_id
                );
                Ok(true)
            }
            Err(e) => {
                error!(
                    "Error checking cancellation status for deployment {}: {}",
                    self.deployment_id, e
                );
                // Don't cancel on error to avoid false positives
                Ok(false)
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowExecutionError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Workflow error: {0}")]
    WorkflowFailed(#[from] WorkflowError),

    #[error("Deployment {0} not found")]
    DeploymentNotFound(i32),

    #[error("Project {0} not found")]
    ProjectNotFound(i32),

    #[error("Environment {0} not found")]
    EnvironmentNotFound(i32),

    #[error("No jobs found for deployment {0}")]
    NoJobsFound(i32),

    #[error("Missing job config for job: {0}")]
    MissingJobConfig(String),

    #[error("Invalid job config: {0}")]
    InvalidJobConfig(String),

    #[error("Unsupported job type: {0}")]
    UnsupportedJobType(String),

    #[error("Job creation failed: {0}")]
    JobCreationFailed(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

impl From<anyhow::Error> for WorkflowExecutionError {
    fn from(e: anyhow::Error) -> Self {
        WorkflowExecutionError::JobCreationFailed(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, DatabaseBackend, MockDatabase, Set};
    use std::collections::HashMap;

    use temps_database::test_utils::TestDatabase;
    use temps_entities::{preset::Preset, types::JobStatus, upstream_config::UpstreamList};

    async fn docker_available() -> bool {
        match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        }
    }

    #[test]
    fn compose_runtime_settings_flow_from_persisted_preset_config() {
        let preset_config = temps_entities::preset::PresetConfig::DockerCompose(
            temps_entities::preset::DockerComposeConfig {
                excluded_services: vec!["managed-db".to_string()],
                relaxed_capability_services: vec!["postgres".to_string()],
                unsandboxed_services: vec!["webserver".to_string()],
                ..Default::default()
            },
        );

        let (excluded, relaxed, unsandboxed) =
            compose_service_security_settings(Some(&preset_config));

        assert_eq!(excluded, ["managed-db"]);
        assert_eq!(relaxed, ["postgres"]);
        assert_eq!(unsandboxed, ["webserver"]);
    }

    #[test]
    fn failure_classifier_maps_specific_errors_to_allowlisted_stage_and_code() {
        let cases = [
            (
                "process was OOMKilled (exit code 137)",
                DeploymentFailureStage::Resource,
                DeploymentFailureCode::OutOfMemory,
            ),
            (
                "write failed: no space left on device",
                DeploymentFailureStage::Resource,
                DeploymentFailureCode::DiskExhausted,
            ),
            (
                "workflow deadline exceeded",
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::Timeout,
            ),
            (
                "application health check timed out",
                DeploymentFailureStage::HealthCheck,
                DeploymentFailureCode::HealthCheckFailed,
            ),
            (
                "authentication failed while fetching repository",
                DeploymentFailureStage::Source,
                DeploymentFailureCode::RepositoryAuthentication,
            ),
            (
                "repository not found",
                DeploymentFailureStage::Source,
                DeploymentFailureCode::RepositoryNotFound,
            ),
            (
                "Git clone task failed",
                DeploymentFailureStage::Source,
                DeploymentFailureCode::RepositoryClone,
            ),
            (
                "could not resolve host: example.invalid",
                DeploymentFailureStage::Source,
                DeploymentFailureCode::DnsResolution,
            ),
            (
                "connection refused by build service",
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::NetworkConnection,
            ),
            (
                "ERR_PNPM_OUTDATED_LOCKFILE Cannot install with frozen lockfile",
                DeploymentFailureStage::DependencyInstall,
                DeploymentFailureCode::DependencyLockfileOutOfSync,
            ),
            (
                "npm ERR! ERESOLVE unable to resolve dependency tree",
                DeploymentFailureStage::DependencyInstall,
                DeploymentFailureCode::DependencyResolution,
            ),
            (
                "registry request failed while fetching packages",
                DeploymentFailureStage::DependencyInstall,
                DeploymentFailureCode::DependencyDownload,
            ),
            (
                "npm ERR! code EBADENGINE Unsupported engine",
                DeploymentFailureStage::Configuration,
                DeploymentFailureCode::RuntimeVersionUnsupported,
            ),
            (
                "npm error Missing script: build",
                DeploymentFailureStage::Build,
                DeploymentFailureCode::MissingBuildScript,
            ),
            (
                "TypeScript error: failed to compile",
                DeploymentFailureStage::Build,
                DeploymentFailureCode::CompileError,
            ),
            (
                "Dockerfile parse error on line 4",
                DeploymentFailureStage::Configuration,
                DeploymentFailureCode::DockerfileInvalid,
            ),
            (
                "pull access denied for private/base-image",
                DeploymentFailureStage::Image,
                DeploymentFailureCode::BaseImagePull,
            ),
            (
                "built image not found",
                DeploymentFailureStage::Image,
                DeploymentFailureCode::ImageMissing,
            ),
            (
                "static output directory not found",
                DeploymentFailureStage::Build,
                DeploymentFailureCode::StaticOutputMissing,
            ),
            (
                "failed to find available port",
                DeploymentFailureStage::Deploy,
                DeploymentFailureCode::PortUnavailable,
            ),
            (
                "operation not permitted while creating build directory",
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::PermissionDenied,
            ),
            (
                "failed to parse .temps.yaml: invalid field",
                DeploymentFailureStage::Configuration,
                DeploymentFailureCode::InvalidConfiguration,
            ),
            (
                "failed to start container",
                DeploymentFailureStage::Runtime,
                DeploymentFailureCode::ContainerStart,
            ),
            (
                "Nixpacks build failed",
                DeploymentFailureStage::Build,
                DeploymentFailureCode::BuildError,
            ),
            (
                "workflow execution failed before scheduling",
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::PlatformInternal,
            ),
            (
                "deployment cancelled by workflow",
                DeploymentFailureStage::Platform,
                DeploymentFailureCode::Cancelled,
            ),
        ];

        for (message, expected_stage, expected_code) in cases {
            let classification = classify_failure_reason(Some(message));
            assert_eq!(classification.stage, expected_stage, "message: {message}");
            assert_eq!(classification.code, expected_code, "message: {message}");
        }
    }

    #[test]
    fn deploy_failed_telemetry_never_copies_sensitive_failure_input() {
        let sensitive = "Build failed in /srv/repos/acme-secret with token ghp_private123";
        let event = deploy_failed_telemetry_event(
            Some(sensitive),
            Some("git".to_string()),
            Some("nextjs".to_string()),
            Some("observability-starter".to_string()),
        );
        let serialized = serde_json::to_string(&event).expect("telemetry event serializes");

        assert_eq!(event.event_type, "deploy_failed");
        assert_eq!(event.properties["failure_stage"], "build");
        assert_eq!(event.properties["failure_code"], "build_error");
        assert_eq!(event.properties["classifier_version"], 1);
        assert_eq!(event.properties["is_template"], true);
        assert_eq!(event.properties["template_source"], "bundled");
        assert_eq!(event.properties["template_slug"], "observability-starter");
        assert!(!serialized.contains("acme-secret"));
        assert!(!serialized.contains("ghp_private123"));
        assert!(!serialized.contains("/srv/repos"));

        let regular_project_event = deploy_failed_telemetry_event(
            Some("build failed"),
            Some("git".to_string()),
            Some("nextjs".to_string()),
            None,
        );
        assert_eq!(regular_project_event.properties["is_template"], false);
        assert_eq!(regular_project_event.properties["template_source"], "none");
        assert!(!regular_project_event
            .properties
            .contains_key("template_slug"));

        let private_slug = "customer-acme-private-ghp_secret456";
        let custom_template_event = deploy_failed_telemetry_event(
            Some("build failed"),
            Some("git".to_string()),
            Some("nextjs".to_string()),
            Some(private_slug.to_string()),
        );
        let serialized =
            serde_json::to_string(&custom_template_event).expect("telemetry event serializes");
        assert_eq!(custom_template_event.properties["is_template"], true);
        assert_eq!(
            custom_template_event.properties["template_source"],
            "custom"
        );
        assert!(!custom_template_event
            .properties
            .contains_key("template_slug"));
        assert!(!serialized.contains(private_slug));
    }

    #[test]
    fn failure_classifier_preserves_legacy_reason_wire_values() {
        let cases = [
            ("OOM", "oom"),
            ("deadline reached", "timeout"),
            ("health check timed out", "timeout"),
            ("healthcheck failed", "health_check"),
            ("build command exited", "build_error"),
            ("DNS error", "network"),
            ("image not found", "image_missing"),
            ("deployment cancelled", "cancelled"),
            ("novel error", "unknown"),
        ];

        for (message, expected) in cases {
            assert_eq!(legacy_failure_reason(Some(message)), expected);
            assert_eq!(
                classify_failure_reason(Some(message)).legacy_reason,
                expected,
                "message: {message}"
            );
        }
    }

    #[test]
    fn failure_classifier_handles_missing_and_unrecognized_reasons() {
        for reason in [
            None,
            Some("an entirely novel failure containing customer-data"),
        ] {
            let classification = classify_failure_reason(reason);
            assert_eq!(classification.stage, DeploymentFailureStage::Unknown);
            assert_eq!(classification.code, DeploymentFailureCode::Unknown);
            assert_eq!(classification.legacy_reason, "unknown");
        }
    }

    // Mock services for testing
    struct MockGitProvider;

    #[async_trait]
    impl GitProviderManagerTrait for MockGitProvider {
        async fn get_connection_access_token(
            &self,
            _connection_id: i32,
        ) -> Result<(String, String), temps_git::GitProviderManagerError> {
            Ok(("mock-token".to_string(), "github".to_string()))
        }

        async fn clone_repository(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            target_dir: &std::path::Path,
            _branch_or_ref: Option<&str>,
        ) -> Result<(), temps_git::GitProviderManagerError> {
            tokio::fs::create_dir_all(target_dir)
                .await
                .map_err(|error| temps_git::GitProviderManagerError::Other(error.to_string()))?;
            tokio::fs::write(target_dir.join("package.json"), b"{}")
                .await
                .map_err(|error| temps_git::GitProviderManagerError::Other(error.to_string()))?;
            Ok(())
        }

        async fn get_repository_info(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
        ) -> Result<temps_git::RepositoryInfo, temps_git::GitProviderManagerError> {
            Ok(temps_git::RepositoryInfo {
                clone_url: "https://github.com/test/test".to_string(),
                default_branch: "main".to_string(),
                owner: "test".to_string(),
                name: "test".to_string(),
            })
        }

        async fn download_archive(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            _branch_or_ref: &str,
            _archive_path: &std::path::Path,
            _progress: Option<&temps_git::ArchiveProgressSender>,
        ) -> Result<(), temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "Not implemented".to_string(),
            ))
        }

        async fn push_files_and_create_pr(
            &self,
            _connection_id: i32,
            _owner: &str,
            _repo: &str,
            _branch: &str,
            _base_branch: &str,
            _files: Vec<(String, Vec<u8>)>,
            _commit_message: &str,
            _pr_title: &str,
            _pr_body: &str,
        ) -> Result<temps_git::PullRequest, temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
            ))
        }
        async fn mint_scoped_repo_token(
            &self,
            _: i32,
            _: &str,
            _: &str,
            _: temps_git::ScopedTokenOp,
        ) -> Result<temps_git::ScopedTokenGrant, temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
            ))
        }
    }

    struct MockImageBuilder {
        should_fail: bool,
    }

    #[async_trait]
    impl ImageBuilder for MockImageBuilder {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            if self.should_fail {
                return Err(temps_deployer::BuilderError::BuildFailed(
                    "Mock failure".to_string(),
                ));
            }
            Ok(temps_deployer::BuildResult {
                image_id: "mock-image-id".to_string(),
                image_name: "mock-image:latest".to_string(),
                size_bytes: 1024,
                build_duration_ms: 1000,
            })
        }

        async fn import_image(
            &self,
            _image_path: std::path::PathBuf,
            _tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            Ok("mock-image-id".to_string())
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            Ok(())
        }

        async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
            Ok(vec!["mock-image:latest".to_string()])
        }

        async fn remove_image(
            &self,
            _image_name: &str,
        ) -> Result<(), temps_deployer::BuilderError> {
            Ok(())
        }

        async fn build_image_with_callback(
            &self,
            request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            // Delegate to regular build_image since we don't need callback in tests
            self.build_image(request.request).await
        }

        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
            Ok(temps_deployer::ImageInfo {
                id: "sha256:mock".to_string(),
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 1024,
                tags: vec!["mock-image:latest".to_string()],
                created: None,
                working_dir: None,
            })
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            Ok(())
        }

        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    struct MockContainerDeployer {
        should_fail: bool,
    }

    struct MockStaticDeployer;

    #[async_trait]
    impl temps_deployer::static_deployer::StaticDeployer for MockStaticDeployer {
        async fn deploy(
            &self,
            _request: temps_deployer::static_deployer::StaticDeployRequest,
        ) -> Result<
            temps_deployer::static_deployer::StaticDeployResult,
            temps_deployer::static_deployer::StaticDeployError,
        > {
            Ok(temps_deployer::static_deployer::StaticDeployResult {
                storage_path: "/tmp/test-deployment".to_string(),
                file_count: 10,
                total_size_bytes: 1024,
                deployed_at: Utc::now(),
            })
        }

        async fn get_deployment(
            &self,
            _project_slug: &str,
            _environment_slug: &str,
            _deployment_slug: &str,
        ) -> Result<
            temps_deployer::static_deployer::StaticDeploymentInfo,
            temps_deployer::static_deployer::StaticDeployError,
        > {
            Ok(temps_deployer::static_deployer::StaticDeploymentInfo {
                deployment_slug: "test-deployment".to_string(),
                storage_path: std::path::PathBuf::from("/tmp/test-deployment"),
                deployed_at: Utc::now(),
                file_count: 10,
                total_size_bytes: 1024,
            })
        }

        async fn list_files(
            &self,
            _project_slug: &str,
            _environment_slug: &str,
            _deployment_slug: &str,
        ) -> Result<
            Vec<temps_deployer::static_deployer::FileInfo>,
            temps_deployer::static_deployer::StaticDeployError,
        > {
            Ok(vec![])
        }

        async fn remove(
            &self,
            _project_slug: &str,
            _environment_slug: &str,
            _deployment_slug: &str,
        ) -> Result<(), temps_deployer::static_deployer::StaticDeployError> {
            Ok(())
        }
    }

    #[async_trait]
    impl ContainerDeployer for MockContainerDeployer {
        async fn deploy_container(
            &self,
            _request: temps_deployer::DeployRequest,
        ) -> Result<temps_deployer::DeployResult, temps_deployer::DeployerError> {
            if self.should_fail {
                return Err(temps_deployer::DeployerError::DeploymentFailed(
                    "Mock failure".to_string(),
                ));
            }
            Ok(temps_deployer::DeployResult {
                container_id: "mock-container-id".to_string(),
                container_name: "mock-container".to_string(),
                container_port: 3000,
                host_port: 3000,
                status: temps_deployer::ContainerStatus::Running,
            })
        }

        async fn start_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            Ok(())
        }

        async fn stop_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            Ok(())
        }

        async fn pause_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            Ok(())
        }

        async fn resume_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            Ok(())
        }

        async fn remove_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            Ok(())
        }

        async fn get_container_info(
            &self,
            _container_id: &str,
        ) -> Result<temps_deployer::ContainerInfo, temps_deployer::DeployerError> {
            Ok(temps_deployer::ContainerInfo {
                container_id: "mock-container-id".to_string(),
                container_name: "mock-container".to_string(),
                image_name: "mock-image:latest".to_string(),
                created_at: chrono::Utc::now(),
                ports: vec![],
                environment_vars: HashMap::new(),
                status: temps_deployer::ContainerStatus::Running,
                restart_count: Some(0),
                labels: HashMap::new(),
                ..Default::default()
            })
        }

        async fn get_container_stats(
            &self,
            container_id: &str,
        ) -> Result<temps_deployer::ContainerStats, temps_deployer::DeployerError> {
            Ok(temps_deployer::ContainerStats {
                container_id: container_id.to_string(),
                container_name: "mock-container".to_string(),
                cpu_percent: 10.5,
                memory_bytes: 134217728,
                memory_limit_bytes: Some(1073741824),
                memory_percent: Some(12.5),
                network_rx_bytes: 1024000,
                network_tx_bytes: 512000,
                timestamp: chrono::Utc::now(),
                ..Default::default()
            })
        }

        async fn list_containers(
            &self,
        ) -> Result<Vec<temps_deployer::ContainerInfo>, temps_deployer::DeployerError> {
            Ok(vec![])
        }

        async fn get_container_logs(
            &self,
            _container_id: &str,
        ) -> Result<String, temps_deployer::DeployerError> {
            Ok("Mock logs".to_string())
        }

        async fn stream_container_logs(
            &self,
            _container_id: &str,
        ) -> Result<
            Box<dyn futures::Stream<Item = String> + Unpin + Send>,
            temps_deployer::DeployerError,
        > {
            use futures::stream;
            Ok(Box::new(stream::empty()))
        }
    }

    async fn create_test_data(
        db: &Arc<DbConnection>,
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
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            template_slug: Set(Some("observability-starter".to_string())),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
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

        Ok((project, environment, deployment))
    }

    // Helper function to create mock config service for tests
    fn create_mock_config_service(db: Arc<DbConnection>) -> Arc<temps_config::ConfigService> {
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgres://test:test@localhost/test".to_string(),
                None,
                None,
            )
            .expect("Failed to create test server config"),
        );
        Arc::new(temps_config::ConfigService::new(server_config, db))
    }

    #[tokio::test]
    async fn test_workflow_execution_service_creation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue) as Arc<dyn temps_core::JobQueue>;
        let git_provider = Arc::new(MockGitProvider);
        let image_builder = Arc::new(MockImageBuilder { should_fail: false });
        let container_deployer = Arc::new(MockContainerDeployer { should_fail: false });
        let static_deployer = Arc::new(MockStaticDeployer);
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let cron_service =
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>;
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        );
        let _service = WorkflowExecutionService::new(
            db.clone(),
            queue,
            git_provider,
            image_builder,
            container_deployer,
            static_deployer,
            log_service,
            cron_service,
            Arc::new(crate::jobs::NoOpMetricAlertConfigService)
                as Arc<dyn crate::jobs::MetricAlertConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            config_service,
            screenshot_service,
            docker,
        );

        // Service should be created successfully - compilation itself is the test

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_deployment_workflow_no_jobs() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (_project, _environment, deployment) = create_test_data(&db).await?;

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue) as Arc<dyn temps_core::JobQueue>;
        let git_provider = Arc::new(MockGitProvider);
        let image_builder = Arc::new(MockImageBuilder { should_fail: false });
        let container_deployer = Arc::new(MockContainerDeployer { should_fail: false });
        let static_deployer = Arc::new(MockStaticDeployer);
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let cron_service =
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>;
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        );
        let service = WorkflowExecutionService::new(
            db.clone(),
            queue,
            git_provider,
            image_builder,
            container_deployer,
            static_deployer,
            log_service,
            cron_service,
            Arc::new(crate::jobs::NoOpMetricAlertConfigService)
                as Arc<dyn crate::jobs::MetricAlertConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            config_service,
            screenshot_service,
            docker,
        );

        // Should fail with NoJobsFound error
        let result = service.execute_deployment_workflow(deployment.id).await;
        assert!(result.is_err());

        match result {
            Err(WorkflowExecutionError::NoJobsFound(id)) => {
                assert_eq!(id, deployment.id);
            }
            _ => panic!("Expected NoJobsFound error"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_deployment_workflow_with_jobs() -> Result<(), Box<dyn std::error::Error>>
    {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (_project, _environment, deployment) = create_test_data(&db).await?;

        // Create jobs for the deployment
        let download_job = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("download_repo".to_string()),
            job_type: Set("DownloadRepoJob".to_string()),
            name: Set("Download Repository".to_string()),
            description: Set(Some("Download source code".to_string())),
            status: Set(JobStatus::Pending),
            log_id: Set(format!("deployment-{}-job-download_repo", deployment.id)),
            job_config: Set(Some(serde_json::json!({
                "repo_owner": "test-owner",
                "repo_name": "test-repo",
                "git_provider_connection_id": 1
            }))),
            dependencies: Set(None),
            execution_order: Set(Some(0)),
            ..Default::default()
        };
        download_job.insert(db.as_ref()).await?;

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue) as Arc<dyn temps_core::JobQueue>;
        let git_provider = Arc::new(MockGitProvider);
        let image_builder = Arc::new(MockImageBuilder { should_fail: false });
        let container_deployer = Arc::new(MockContainerDeployer { should_fail: false });
        let static_deployer = Arc::new(MockStaticDeployer);
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let cron_service =
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>;
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        );
        let service = WorkflowExecutionService::new(
            db.clone(),
            queue,
            git_provider,
            image_builder,
            container_deployer,
            static_deployer,
            log_service,
            cron_service,
            Arc::new(crate::jobs::NoOpMetricAlertConfigService)
                as Arc<dyn crate::jobs::MetricAlertConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            config_service,
            screenshot_service,
            docker,
        );

        // Capture telemetry so we can assert the deploy funnel events fire at
        // the right points (attempted always; succeeded/failed on the matching
        // terminal outcome).
        let telemetry = Arc::new(CapturingTelemetryReporter::default());
        service.set_telemetry(telemetry.clone());

        // Execute workflow. This test is the terminal-success proof, so an
        // unexpected mock failure must fail the test rather than being accepted.
        service
            .execute_deployment_workflow(deployment.id)
            .await
            .expect("mock deployment workflow must complete successfully");

        // deploy_attempted fires unconditionally once jobs are loaded.
        assert!(
            telemetry.has_event("deploy_attempted"),
            "deploy_attempted telemetry must fire for every workflow execution"
        );
        let attempted = telemetry
            .event("deploy_attempted")
            .expect("deploy_attempted event must be captured");
        assert_eq!(attempted.properties["is_template"], true);
        assert_eq!(attempted.properties["template_source"], "bundled");
        assert_eq!(
            attempted.properties["template_slug"],
            "observability-starter"
        );

        for event_type in ["deploy_succeeded", "first_deploy_succeeded"] {
            let event = telemetry
                .event(event_type)
                .unwrap_or_else(|| panic!("{event_type} event must be captured"));
            assert_eq!(event.properties["is_template"], true);
            assert_eq!(event.properties["template_source"], "bundled");
            assert_eq!(event.properties["template_slug"], "observability-starter");
        }

        Ok(())
    }

    #[tokio::test]
    async fn failed_status_path_sanitizes_operator_template_and_raw_reason(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, _environment, deployment) = create_test_data(&db).await?;

        let private_slug = "customer-acme-private-ghp_secret789";
        let mut active_project: projects::ActiveModel = project.into();
        active_project.template_slug = Set(Some(private_slug.to_string()));
        active_project.update(db.as_ref()).await?;

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue) as Arc<dyn temps_core::JobQueue>;
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);
        let service = WorkflowExecutionService::new(
            db.clone(),
            queue,
            Arc::new(MockGitProvider),
            Arc::new(MockImageBuilder { should_fail: false }),
            Arc::new(MockContainerDeployer { should_fail: false }),
            Arc::new(MockStaticDeployer),
            Arc::new(LogService::new(std::env::temp_dir())),
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>,
            Arc::new(crate::jobs::NoOpMetricAlertConfigService)
                as Arc<dyn crate::jobs::MetricAlertConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            config_service,
            screenshot_service,
            Arc::new(bollard::Docker::connect_with_local_defaults()?),
        );
        let telemetry = Arc::new(CapturingTelemetryReporter::default());
        service.set_telemetry(telemetry.clone());

        let sensitive_reason =
            "Build failed in /srv/repos/acme-secret with token ghp_private_failure123";
        service
            .update_deployment_status_with_reason(
                deployment.id,
                temps_entities::types::PipelineStatus::Failed,
                Some(sensitive_reason.to_string()),
            )
            .await?;

        let event = telemetry
            .event("deploy_failed")
            .expect("real failed status path must report deploy_failed");
        let serialized = serde_json::to_string(&event)?;
        assert_eq!(event.properties["failure_stage"], "build");
        assert_eq!(event.properties["failure_code"], "build_error");
        assert_eq!(event.properties["is_template"], true);
        assert_eq!(event.properties["template_source"], "custom");
        assert!(!event.properties.contains_key("template_slug"));
        for sensitive in [
            private_slug,
            "/srv/repos/acme-secret",
            "ghp_private_failure123",
        ] {
            assert!(!serialized.contains(sensitive));
        }

        let failed_deployment = deployments::Entity::find_by_id(deployment.id)
            .one(db.as_ref())
            .await?
            .expect("deployment row must exist");
        assert_eq!(failed_deployment.state, "failed");

        Ok(())
    }

    /// Telemetry reporter that records every event so tests can assert which
    /// deploy-funnel events fired.
    #[derive(Default)]
    struct CapturingTelemetryReporter {
        events: std::sync::Mutex<Vec<temps_core::telemetry::TelemetryEvent>>,
    }

    impl CapturingTelemetryReporter {
        fn has_event(&self, event_type: &str) -> bool {
            self.events
                .lock()
                .expect("telemetry capture lock poisoned")
                .iter()
                .any(|e| e.event_type == event_type)
        }

        fn event(&self, event_type: &str) -> Option<temps_core::telemetry::TelemetryEvent> {
            self.events
                .lock()
                .expect("telemetry capture lock poisoned")
                .iter()
                .find(|event| event.event_type == event_type)
                .cloned()
        }
    }

    impl temps_core::telemetry::TelemetryReporter for CapturingTelemetryReporter {
        fn report(&self, event: temps_core::telemetry::TelemetryEvent) {
            self.events
                .lock()
                .expect("telemetry capture lock poisoned")
                .push(event);
        }

        fn is_enabled(&self) -> bool {
            true
        }
    }

    /// Deployer used only by
    /// `test_teardown_previous_deployment_marks_deleted_before_stopping_container`.
    /// Asserts that by the time Docker is asked to stop a container, its
    /// `deployment_containers` row is already marked deleted in the database —
    /// the invariant that closes the race with `ContainerHealthMonitor`'s
    /// independent poll loop (see the comment in `teardown_previous_deployment`).
    struct AssertDeletedBeforeStopDeployer {
        db: Arc<DbConnection>,
    }

    #[async_trait]
    impl ContainerDeployer for AssertDeletedBeforeStopDeployer {
        async fn deploy_container(
            &self,
            _request: temps_deployer::DeployRequest,
        ) -> Result<temps_deployer::DeployResult, temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn start_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn stop_container(
            &self,
            container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            let container = temps_entities::deployment_containers::Entity::find()
                .filter(temps_entities::deployment_containers::Column::ContainerId.eq(container_id))
                .one(self.db.as_ref())
                .await
                .expect("query deployment_containers row")
                .expect("deployment_containers row exists");
            assert!(
                container.deleted_at.is_some(),
                "container {container_id} must be marked deleted before stop_container() \
                 is called — otherwise ContainerHealthMonitor's concurrent poll can \
                 observe an Exited container with no signal that the exit is an \
                 intentional teardown, and fires a false ContainerCrash alarm"
            );
            Ok(())
        }

        async fn pause_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn resume_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn remove_container(
            &self,
            _container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            Ok(())
        }

        async fn get_container_info(
            &self,
            _container_id: &str,
        ) -> Result<temps_deployer::ContainerInfo, temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn get_container_stats(
            &self,
            _container_id: &str,
        ) -> Result<temps_deployer::ContainerStats, temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn list_containers(
            &self,
        ) -> Result<Vec<temps_deployer::ContainerInfo>, temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn get_container_logs(
            &self,
            _container_id: &str,
        ) -> Result<String, temps_deployer::DeployerError> {
            unimplemented!("not exercised by this test")
        }

        async fn stream_container_logs(
            &self,
            _container_id: &str,
        ) -> Result<
            Box<dyn futures::Stream<Item = String> + Unpin + Send>,
            temps_deployer::DeployerError,
        > {
            unimplemented!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn test_teardown_previous_deployment_marks_deleted_before_stopping_container(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (project, environment, current_deployment) = create_test_data(&db).await?;

        // A previous deployment in the same environment, still active, whose
        // container should be torn down now that `current_deployment` is live.
        let previous_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("previous-deployment".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now() - chrono::Duration::minutes(5)),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let container = deployment_containers::ActiveModel {
            deployment_id: Set(previous_deployment.id),
            container_id: Set("old-container-1".to_string()),
            container_name: Set("old-container-1".to_string()),
            container_port: Set(3000),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // Simulate a newer deployment completing while this older workflow is
        // still in its post-success cleanup. It must not be selected as a
        // "previous" deployment merely because its ID differs.
        let newer_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("newer-deployment".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now() + chrono::Duration::minutes(5)),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let newer_container = deployment_containers::ActiveModel {
            deployment_id: Set(newer_deployment.id),
            container_id: Set("newer-container-1".to_string()),
            container_name: Set("newer-container-1".to_string()),
            container_port: Set(3000),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue) as Arc<dyn temps_core::JobQueue>;
        let git_provider = Arc::new(MockGitProvider);
        let image_builder = Arc::new(MockImageBuilder { should_fail: false });
        let container_deployer = Arc::new(AssertDeletedBeforeStopDeployer { db: db.clone() });
        let static_deployer = Arc::new(MockStaticDeployer);
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let cron_service =
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>;
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        );

        let service = WorkflowExecutionService::new(
            db.clone(),
            queue,
            git_provider,
            image_builder,
            container_deployer,
            static_deployer,
            log_service,
            cron_service,
            Arc::new(crate::jobs::NoOpMetricAlertConfigService)
                as Arc<dyn crate::jobs::MetricAlertConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            config_service,
            screenshot_service,
            docker,
        );

        let stopped_container_id = service
            .teardown_previous_deployment(project.id, environment.id, &current_deployment)
            .await?;

        assert_eq!(stopped_container_id, Some("old-container-1".to_string()));

        let refreshed = deployment_containers::Entity::find_by_id(container.id)
            .one(db.as_ref())
            .await?
            .expect("container row still exists");
        assert!(refreshed.deleted_at.is_some());
        assert_eq!(refreshed.status.as_deref(), Some("deleted"));

        let newer_refreshed = deployment_containers::Entity::find_by_id(newer_container.id)
            .one(db.as_ref())
            .await?
            .expect("newer container row still exists");
        assert!(
            newer_refreshed.deleted_at.is_none(),
            "an older workflow cleanup must not remove a newer deployment's container"
        );

        Ok(())
    }

    #[tokio::test]
    async fn missing_deployment_is_terminal_cancellation() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<deployments::Model>::new()])
                .into_connection(),
        );
        let provider = DatabaseCancellationProvider::new(db, 42);

        assert!(provider.is_cancelled("deployment-42").await.unwrap());
    }

    #[tokio::test]
    async fn cancellation_provider_supports_inline_workflow_names() {
        let cancelled = deployments::Model {
            id: 42,
            project_id: 7,
            environment_id: 8,
            slug: "cancelled-inline-deployment".to_string(),
            state: "cancelled".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: Some(chrono::Utc::now()),
            context_vars: None,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: Some("Project deleted".to_string()),
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cancelled.clone()], vec![cancelled]])
                .into_connection(),
        );
        let provider = DatabaseCancellationProvider::new(db, 42);

        assert!(provider.is_cancelled("rollback-42").await.unwrap());
        assert!(provider.is_cancelled("promote-42").await.unwrap());
    }
}
