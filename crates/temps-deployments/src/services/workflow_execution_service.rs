// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workflow Execution Service
//!
//! Executes deployment jobs as workflows using the WorkflowExecutor

use chrono::Timelike;
use futures::StreamExt;
use sea_orm::{
    sea_query::Expr, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use std::sync::Arc;
use temps_core::{
    DockerHandle, Job, JobQueue, JobTracker, WorkflowBuilder, WorkflowCancellationProvider,
    WorkflowError, WorkflowExecutor,
};
use temps_database::DbConnection;
use temps_deployer::{static_deployer::StaticDeployer, ContainerDeployer, ImageBuilder};
use temps_deployment_logs::LogService;
use temps_entities::{deployment_jobs, deployments, environments, projects};
use temps_error_tracking::services::SourceMapService;
use temps_git::GitProviderManagerTrait;
use tokio::sync::OnceCell;
use tracing::{debug, error, info, warn};

use crate::jobs::{
    AgentSyncService, BuildImageJobBuilder, BuilderNodeResolver, ConfigureAgentsJobBuilder,
    ConfigureCronsJobBuilder, ConfigureMetricAlertsJobBuilder, CronConfigService,
    DeployImageJobBuilder, DeployStaticBundleJob, DeployStaticFromSourceJob, DeployStaticJob,
    DeploymentTarget, DownloadRepoBuilder, MetricAlertConfigService, OffloadFallback,
    PrepareSourceBundleJob, PullExternalImageJob, ResourceUsage, VerifyLocalImageJob,
};
use crate::services::DeploymentJobTracker;
use temps_screenshots::ScreenshotService;

use crate::services::failure_classifier::{classify_failure_reason, FAILURE_CLASSIFIER_VERSION};

/// A lexically-last retained status used after a cleanup attempt fails.
///
/// Cleanup orders by status before row ID. Normal retained states (Docker's
/// created/running/exited/etc. and `failed-readiness`) are therefore attempted
/// first, while retry rows rotate by their timestamp instead of permanently
/// monopolizing the bounded cleanup window.
const RETAINED_CLEANUP_RETRY_PREFIX: &str = "retained:zz-cleanup-retry:";

fn retained_cleanup_retry_status() -> String {
    retained_cleanup_retry_status_at(chrono::Utc::now())
}

/// How a registered container is torn down.
enum TeardownPath {
    /// Started through the workload plane: retired by deleting its workload.
    Plane(crate::services::plane_deployer::ReplicaWorkload),
    /// A legacy container: stopped and removed by its node's deployer.
    Deployer(Arc<dyn ContainerDeployer>),
}

fn retained_cleanup_retry_status_at(at: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "{RETAINED_CLEANUP_RETRY_PREFIX}{}",
        at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    )
}

fn cleanup_snapshot_condition(
    container: &temps_entities::deployment_containers::Model,
) -> Condition {
    use temps_entities::deployment_containers;

    let status_condition = match &container.status {
        Some(status) => deployment_containers::Column::Status.eq(status.clone()),
        None => deployment_containers::Column::Status.is_null(),
    };
    let deleted_condition = match container.deleted_at {
        Some(deleted_at) => deployment_containers::Column::DeletedAt.eq(deleted_at),
        None => deployment_containers::Column::DeletedAt.is_null(),
    };

    Condition::all()
        .add(deployment_containers::Column::Id.eq(container.id))
        .add(status_condition)
        .add(deleted_condition)
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

/// Whether a deployment's stored commit names something to check out.
///
/// An empty string is "no commit" (older manual triggers stored ''), and the
/// symbolic `HEAD` that older first deployments of public repositories
/// recorded means the same thing: build the branch. Treating either as a
/// commit forces a full-history clone just to land on the branch tip.
fn is_concrete_commit_ref(commit: &str) -> bool {
    let commit = commit.trim();
    !commit.is_empty() && !commit.eq_ignore_ascii_case("HEAD")
}

/// Add bounded template context without allowing operator-defined template
/// slugs to become identifying or high-cardinality outbound telemetry.
fn with_template_telemetry(
    event: temps_core::telemetry::TelemetryEvent,
    template_slug: Option<&str>,
) -> temps_core::telemetry::TelemetryEvent {
    event.with_template_provenance(template_slug)
}

/// Cancellation is a typed control-flow outcome, never a substring of
/// user-controlled build output or runtime logs.
fn is_workflow_cancellation(error: &WorkflowError, _message: &str) -> bool {
    matches!(
        error,
        WorkflowError::WorkflowCancelled | WorkflowError::BuildCancelled
    )
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

/// The node chosen to build a source image in the control-plane profile.
struct SelectedNodeBuilder {
    node_id: i32,
    node_name: String,
    platform: String,
    remote: Arc<dyn ImageBuilder>,
    /// The same connection as `remote`, for the node-specific calls an
    /// `ImageBuilder` has no place for (build capacity).
    agent: Arc<temps_deployer::remote::RemoteNodeDeployer>,
}

/// How many images a worker build of a non-static deployment produces.
#[derive(Debug, PartialEq, Eq)]
enum WorkerBuildScope {
    /// Every target node runs one architecture: one build, as always.
    SingleArchitecture,
    /// The target nodes span several architectures: one native build per
    /// platform, each on a worker of that architecture.
    PerArchitecture,
}

fn validate_worker_build_scope(
    deployment_id: i32,
    platforms: &[String],
) -> Result<WorkerBuildScope, WorkflowExecutionError> {
    match platforms.len() {
        0 => Err(WorkflowExecutionError::InvalidJobConfig(format!(
            "Deployment {deployment_id}: no target worker has reported a buildable \
             architecture, so there is no platform to build for. Check that a worker is active \
             and has completed a heartbeat (`temps join`), and that the target nodes/labels \
             select it"
        ))),
        1 => Ok(WorkerBuildScope::SingleArchitecture),
        _ => Ok(WorkerBuildScope::PerArchitecture),
    }
}

/// Open an authenticated agent connection to `node` for image work.
async fn connect_build_node(
    node: &temps_entities::nodes::Model,
    config: &temps_config::ConfigService,
    encryption: &temps_core::EncryptionService,
) -> Result<temps_deployer::remote::RemoteNodeDeployer, String> {
    let sealed_token = node.token_encrypted.as_deref().ok_or_else(|| {
        format!(
            "Build node '{}' (id={}) has no encrypted agent token; re-register it",
            node.name, node.id
        )
    })?;
    let token = encryption.decrypt(sealed_token).map_err(|error| {
        format!(
            "Cannot decrypt build node '{}' (id={}) token: {error}",
            node.name, node.id
        )
    })?;
    let token = String::from_utf8(token).map_err(|error| {
        format!(
            "Build node '{}' (id={}) token is not UTF-8: {error}",
            node.name, node.id
        )
    })?;
    let limits = node
        .build_limits
        .clone()
        .map(serde_json::from_value::<temps_deployer::build_protocol::BuildLimits>)
        .transpose()
        .map_err(|error| format!("Node {} has invalid builder limits: {error}", node.id))?
        .unwrap_or_default();
    limits.validate()?;
    let version = node
        .build_limits
        .as_ref()
        .and_then(|value| value.get("_version"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    crate::cluster_ca::build_node_deployer(node, token, config, encryption)
        .await
        .map(|remote| {
            remote
                .with_build_limits(limits)
                .with_build_limits_version(version)
        })
        .map_err(|error| {
            format!(
                "Cannot connect to build node '{}' (id={}): {error}",
                node.name, node.id
            )
        })
}

/// How long a per-architecture build candidate has to answer its health probe.
const BUILDER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Connect to `node` and confirm, over its authenticated agent channel, that
/// it is reachable and runs `platform` -- what its agent reports now, not
/// only what its node row last recorded. `Err` is why it was passed over.
async fn probe_platform_builder(
    node: &temps_entities::nodes::Model,
    platform: &str,
    config: &temps_config::ConfigService,
    encryption: &temps_core::EncryptionService,
) -> Result<temps_deployer::remote::RemoteNodeDeployer, String> {
    let remote = connect_build_node(node, config, encryption).await?;
    // Bounded: a worker that accepts the connection and never answers must
    // not hold up the deployment for the client's full request timeout.
    let reported = tokio::time::timeout(BUILDER_PROBE_TIMEOUT, remote.refresh_platform())
        .await
        .map_err(|_| {
            format!(
                "worker '{}' (id={}) did not answer on its agent connection within {}s",
                node.name,
                node.id,
                BUILDER_PROBE_TIMEOUT.as_secs()
            )
        })?;
    match reported {
        Some(reported) if temps_deployer::platform::platforms_match(&reported, platform) => {
            Ok(remote)
        }
        Some(reported) => Err(format!(
            "worker '{}' (id={}) is recorded as {platform} but its agent reports {reported}",
            node.name, node.id
        )),
        None => Err(format!(
            "worker '{}' (id={}) did not answer on its agent connection",
            node.name, node.id
        )),
    }
}

/// Reaches the worker that built an image, for jobs that read files out of
/// it after the build (static output, immutable assets).
struct NodeBuilderResolver {
    scheduler: Arc<crate::services::NodeScheduler>,
    config: Arc<temps_config::ConfigService>,
    encryption: Arc<temps_core::EncryptionService>,
}

#[async_trait::async_trait]
impl BuilderNodeResolver for NodeBuilderResolver {
    async fn image_builder_for_node(
        &self,
        node_id: i32,
    ) -> Result<Arc<dyn ImageBuilder>, WorkflowError> {
        let node = self
            .scheduler
            .node_service()
            .get_by_id(node_id)
            .await
            .map_err(|error| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to load build node {node_id}: {error}"
                ))
            })?;
        let platform = node.architecture.clone();
        let remote = connect_build_node(&node, &self.config, &self.encryption)
            .await
            .map_err(WorkflowError::JobExecutionFailed)?;
        Ok(Arc::new(remote.with_platform(platform)))
    }
}

/// What a worker must build for a deployment whose build location is
/// `node` on a control plane that could also build it itself.
#[derive(Debug, PartialEq, Eq)]
struct OffloadBuildCandidate {
    /// The platform the image must be built for. `None` for a static site:
    /// its output is architecture-independent and the image never runs, so
    /// any worker may build it.
    platform: Option<String>,
}

/// Secret variables a framework inlines into the application, which keep an
/// offloaded build on the control plane: a worker never receives a secret
/// value, so the bundle it built would hold them empty. Names only.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{names} {verb} inlined into the application at build time, but {pronoun} secret, and \
     secret values never leave the control plane for a worker build",
    names = .variables.join(", "),
    verb = if .variables.len() == 1 { "is" } else { "are" },
    pronoun = if .variables.len() == 1 { "it is" } else { "they are" }
)]
struct InlinedVariablesKeepBuildLocal {
    variables: Vec<String>,
}

/// A worker build receives every value that is not secret -- the project's
/// own variables and the platform's public ones alike -- so only a secret
/// variable the framework inlines changes what a worker would build.
fn inlined_variables_allow_offload(
    withheld_inlined_variables: &[String],
) -> Result<(), InlinedVariablesKeepBuildLocal> {
    if withheld_inlined_variables.is_empty() {
        return Ok(());
    }
    Err(InlinedVariablesKeepBuildLocal {
        variables: withheld_inlined_variables.to_vec(),
    })
}

/// The inlined variables among `build_args` whose values a worker build
/// would not receive (not in the planner's `worker_build_arg_keys`).
fn withheld_inlined_variables<'a>(
    build_args: impl IntoIterator<Item = &'a String>,
    worker_build_arg_keys: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    crate::jobs::build_image::build_inlined_variables(
        build_args
            .into_iter()
            .filter(|name| !worker_build_arg_keys.contains(*name))
            .map(String::as_str),
    )
}

/// Whether a build that the control plane would run can move to a worker
/// node without changing what it produces, and for which platform.
///
/// `Err` is the reason it stays on the control plane, phrased for the build
/// log. Every reason is a limit of worker build protocol v1 or of what this
/// server knows, never a configuration error, so the deployment still ships.
fn offload_build_candidate(
    static_output_only: bool,
    needs_npm_credentials: bool,
    cross_build_platforms: &[String],
    control_plane_platform: Option<&str>,
    scheduler_available: bool,
) -> Result<OffloadBuildCandidate, String> {
    if !scheduler_available {
        return Err("node scheduling is not available on this server".to_string());
    }
    // `.npmrc` is generated from these on the control plane only; a worker
    // build would fail outright, and the values must not leave this host.
    if needs_npm_credentials {
        return Err(
            "this build uses NPM_TOKEN or NPM_RC for private packages, and worker \
             builds cannot receive registry credentials yet"
                .to_string(),
        );
    }
    // DeployStaticJob reads the output files from the build node, and the
    // image itself runs nowhere.
    if static_output_only {
        return Ok(OffloadBuildCandidate { platform: None });
    }
    if cross_build_platforms.len() > 1 {
        return Err(format!(
            "cross-architecture builds need {} images ({}), and a worker builds exactly one. \
             Turn off cross-architecture builds or restrict target nodes to one architecture",
            cross_build_platforms.len(),
            cross_build_platforms.join(", ")
        ));
    }
    // One requested platform is still one image; otherwise the native build
    // is for the control plane's own platform.
    let platform = cross_build_platforms
        .first()
        .map(String::as_str)
        .or(control_plane_platform)
        .map(str::trim)
        .filter(|platform| temps_deployer::platform::is_buildable_platform(platform))
        .ok_or_else(|| {
            "the control plane's Docker platform is not known yet, so no worker of the \
             matching architecture can be chosen"
                .to_string()
        })?;
    Ok(OffloadBuildCandidate {
        platform: Some(temps_deployer::platform::canonicalize_platform(platform)),
    })
}

/// A deployment's placement constraints: the environment's `target_nodes`
/// and `target_labels`, each inheriting the project's when the environment
/// does not set it. One resolver for the build, the pre-build placement check
/// and the deploy, so all three reason about the same pool of nodes.
fn resolve_placement_constraints(
    environment_config: Option<&temps_entities::deployment_config::DeploymentConfig>,
    project_config: Option<&temps_entities::deployment_config::DeploymentConfig>,
) -> (Option<Vec<i32>>, Option<serde_json::Value>) {
    let configs = [environment_config, project_config];
    let target_nodes = configs
        .iter()
        .flatten()
        .find_map(|config| config.configured_target_nodes().map(<[i32]>::to_vec));
    let target_labels = configs
        .iter()
        .flatten()
        .find_map(|config| config.configured_target_labels().cloned());
    (target_nodes, target_labels)
}

/// Whether any of a deployment's jobs places replicas on nodes. Static sites
/// and compose stacks are served or run without the node scheduler.
fn places_replicas(db_jobs: &[deployment_jobs::Model]) -> bool {
    db_jobs.iter().any(|job| {
        matches!(
            job.job_type.as_str(),
            "DeployImageJob" | "DeployContainerJob"
        )
    })
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
    /// Shared with on-demand `take_screenshot` operations, so the pipeline's
    /// capture never overlaps one of the same deployment.
    capture_guard: Arc<crate::jobs::DeploymentCaptureGuard>,
    docker_handle: Arc<DockerHandle>,
    source_map_service: OnceCell<Arc<SourceMapService>>,
    node_scheduler: OnceCell<Arc<crate::services::NodeScheduler>>,
    /// ADR-055 Phase 5: the unified scheduler's inputs, for `shadow` and
    /// `enforce` environments.
    workload_placement: OnceCell<Arc<crate::services::WorkloadPlacementService>>,
    /// The workload plane `enforce` environments start replicas through.
    workload_plane: OnceCell<Arc<temps_workload_plane::WorkloadPlane>>,
    encryption_service: OnceCell<Arc<temps_core::EncryptionService>>,
    file_store: OnceCell<Arc<dyn temps_file_store::FileStore>>,
    /// Anonymous product telemetry reporter (late-bound, optional). Set via
    /// [`Self::set_telemetry`]; defaults to a no-op when unset so the deploy
    /// path never depends on telemetry being wired.
    telemetry: OnceCell<Arc<dyn temps_core::telemetry::TelemetryReporter>>,
    /// ADR 068 D5: the deploy warning (late-bound, optional).
    environment_addresses: OnceCell<Arc<temps_core::environment_address::EnvironmentAddressSlot>>,
    /// Audit sink for deploy-path security events (late-bound, optional).
    ///
    /// Currently the ADR-045 "this deployment received the host Docker socket"
    /// record. Late-bound like `telemetry` so the deploy path never depends on
    /// auditing being wired, and a missing sink degrades to a log line rather
    /// than failing a deployment.
    audit_logger: OnceCell<Arc<dyn temps_core::AuditLogger>>,
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
        capture_guard: Arc<crate::jobs::DeploymentCaptureGuard>,
        docker_handle: Arc<DockerHandle>,
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
            capture_guard,
            docker_handle,
            source_map_service: OnceCell::new(),
            node_scheduler: OnceCell::new(),
            workload_placement: OnceCell::new(),
            workload_plane: OnceCell::new(),
            encryption_service: OnceCell::new(),
            file_store: OnceCell::new(),
            telemetry: OnceCell::new(),
            environment_addresses: OnceCell::new(),
            audit_logger: OnceCell::new(),
        }
    }

    /// Set the audit sink used for deploy-path security events (ADR 045).
    pub fn set_audit_logger(&self, logger: Arc<dyn temps_core::AuditLogger>) {
        let _ = self.audit_logger.set(logger);
    }

    /// Set the anonymous telemetry reporter used to emit deploy-funnel events.
    pub fn set_telemetry(&self, reporter: Arc<dyn temps_core::telemetry::TelemetryReporter>) {
        let _ = self.telemetry.set(reporter);
    }

    /// Wire the ADR 068 D5 deploy warning.
    pub fn set_environment_addresses(
        &self,
        slot: Arc<temps_core::environment_address::EnvironmentAddressSlot>,
    ) {
        let _ = self.environment_addresses.set(slot);
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

    /// Set the placement service `shadow`/`enforce` deployments use.
    pub fn set_workload_placement(
        &self,
        placement: Arc<crate::services::WorkloadPlacementService>,
    ) {
        let _ = self.workload_placement.set(placement);
    }

    /// Set the workload plane `enforce` deployments start replicas through.
    pub fn set_workload_plane(&self, plane: Arc<temps_workload_plane::WorkloadPlane>) {
        let _ = self.workload_plane.set(plane);
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

        // Create the job tracker before anything can fail below. The planner
        // has already inserted one `pending` `deployment_jobs` row per job, and
        // only the tracker ever moves those rows out of `pending` — so any
        // failure between here and `WorkflowExecutor` running must go through
        // it, or the deployment ends up terminally failed while its job
        // timeline shows every step still "pending" forever.
        let job_tracker = Arc::new(DeploymentJobTracker::new(
            self.db.clone(),
            deployment_id,
            self.log_service.clone(),
        ));

        // Convert database job records to actual job instances
        let workflow_builder = match self
            .add_jobs_to_workflow(
                workflow_builder,
                &project,
                &environment,
                &deployment,
                &db_jobs,
            )
            .await
        {
            Ok(builder) => builder,
            Err(e) => {
                self.cancel_pending_jobs_after_setup_failure(&job_tracker, deployment_id, &e)
                    .await;
                return Err(e);
            }
        };

        let workflow = match workflow_builder.build() {
            Ok(workflow) => workflow,
            Err(e) => {
                let e = WorkflowExecutionError::from(e);
                self.cancel_pending_jobs_after_setup_failure(&job_tracker, deployment_id, &e)
                    .await;
                return Err(e);
            }
        };

        info!("Built workflow with {} jobs", workflow.jobs.len());

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
                // Check if this is a cancellation error. `detail()` rather
                // than `to_string()`: this becomes the failure reason shown in
                // the console, and the "Job execution failed:" prefix only
                // repeats what the "Deployment failed" banner already says.
                let error_message = e.detail();
                let is_cancellation = is_workflow_cancellation(&e, &error_message);

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
                } else if e.is_workload_outcome() {
                    // The user's build/app failed: the outcome is recorded on
                    // the deployment (state, reason, job logs) below. Not a
                    // Temps fault, so not an ERROR.
                    warn!(
                        "Workflow execution failed for deployment {}: {}",
                        deployment_id, e
                    );
                } else {
                    error!(
                        "Workflow execution failed for deployment {}: {}",
                        deployment_id, e
                    );

                    // Re-read the deployment state before writing "failed".
                    // A concurrent rollback may have called
                    // stop_environment_containers which atomically flips a
                    // mid-flight deployment from "running" to "stopped" to
                    // signal supersession. Overwriting "stopped" with "failed"
                    // here would prevent promote/rollback from reusing this
                    // deployment's image later. Skip the write when already
                    // in a stable terminal state set by the control plane.
                    let current_state = self
                        .get_deployment(deployment_id)
                        .await
                        .map(|d| d.state)
                        .unwrap_or_default();
                    if matches!(
                        current_state.as_str(),
                        "cancelled" | "stopped" | "completed" | "failed"
                    ) {
                        info!(
                            deployment_id,
                            state = %current_state,
                            "Workflow ended with an error after the deployment reached a terminal state; preserving that state"
                        );
                    } else {
                        // Update deployment status to failed with reason
                        self.update_deployment_status_with_reason(
                            deployment_id,
                            temps_entities::types::PipelineStatus::Failed,
                            Some(error_message),
                        )
                        .await?;
                    }
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

    /// Turn every planned `deployment_jobs` row into a runnable job and add it
    /// to the workflow.
    ///
    /// Split out of `execute_deployment_workflow` so the caller has one
    /// fallible unit to recover from: every error raised here happens *before*
    /// `WorkflowExecutor` exists, which is the only component that otherwise
    /// moves `deployment_jobs` rows out of `pending`.
    async fn add_jobs_to_workflow(
        &self,
        mut workflow_builder: WorkflowBuilder,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        db_jobs: &[deployment_jobs::Model],
    ) -> Result<WorkflowBuilder, WorkflowExecutionError> {
        self.preflight_dedicated_placement(project, environment, deployment, db_jobs)
            .await?;

        for db_job in db_jobs {
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
                .create_job_from_record(project, environment, deployment, db_job)
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

        Ok(workflow_builder)
    }

    /// Fail a deployment whose replicas could only land on dedicated nodes it
    /// does not pin, before its source is downloaded or built.
    ///
    /// Replicas are placed by `DeployImageJob`, after the build, because the
    /// image's platforms narrow the pool. This runs that same placement now,
    /// with no platform constraint: platforms only ever remove nodes, so a
    /// pool that holds nothing but unpinned dedicated nodes now holds nothing
    /// later either, and the build would be wasted. Only that answer fails
    /// the deployment here. Every other placement outcome is still decided by
    /// `DeployImageJob` with the image in hand, as before.
    async fn preflight_dedicated_placement(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        db_jobs: &[deployment_jobs::Model],
    ) -> Result<(), WorkflowExecutionError> {
        if !places_replicas(db_jobs) {
            return Ok(());
        }
        let Some(scheduler) = self.node_scheduler.get() else {
            return Ok(());
        };
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );
        let outcome = scheduler
            .schedule_placement(crate::services::node_scheduler::ReplicaPlacementRequest {
                replica_count: 1,
                labels: target_labels.as_ref(),
                target_node_ids: target_nodes.as_deref(),
                anti_affinity: false,
                exclude_node_ids: &[],
                image_platforms: &[],
                project_slug: Some(&project.slug),
                exclude_control_plane: false,
            })
            .await;
        match outcome {
            Err(
                source @ crate::services::node_service::NodeError::DedicatedNodesNotPinned {
                    ..
                },
            ) => {
                warn!(
                    deployment_id = deployment.id,
                    project_slug = %project.slug,
                    environment_id = environment.id,
                    "Failing deployment before the build: {}",
                    source
                );
                Err(WorkflowExecutionError::DedicatedNodesNotPinned {
                    deployment_id: deployment.id,
                    source,
                })
            }
            Err(error) => {
                debug!(
                    deployment_id = deployment.id,
                    "Pre-build placement check inconclusive ({}); placement is decided after \
                     the build",
                    error
                );
                Ok(())
            }
            Ok(_) => Ok(()),
        }
    }

    /// Close out the planned-but-never-started `deployment_jobs` rows when the
    /// workflow could not be assembled at all.
    ///
    /// Without this the deployment is marked `failed` by the job processor
    /// while every one of its job rows stays `pending` — a timeline that never
    /// resolves, on a page the operator is watching for an answer. Best-effort:
    /// the setup error is what the caller returns, and failing to tidy the rows
    /// must not replace it with a less informative one.
    async fn cancel_pending_jobs_after_setup_failure(
        &self,
        job_tracker: &DeploymentJobTracker,
        deployment_id: i32,
        error: &WorkflowExecutionError,
    ) {
        let reason = format!("Deployment {} could not start: {}", deployment_id, error);
        if let Err(cancel_error) = job_tracker
            .cancel_pending_jobs(&format!("deployment-{}", deployment_id), reason)
            .await
        {
            error!(
                deployment_id,
                error = %cancel_error,
                "Failed to cancel pending deployment jobs after workflow setup failure; \
                 the job timeline may show rows stuck in pending",
            );
        }
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
                    .filter(|s| is_concrete_commit_ref(s))
                    .map(|s| s.to_string());

                let mut builder = DownloadRepoBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .repo_owner(repo_owner.to_string())
                    .repo_name(repo_name.to_string())
                    .is_public_repo(is_public_repo)
                    .branch_ref(branch_ref)
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .db(self.db.clone());

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

                if let Some(directory) = config.get("directory").and_then(|v| v.as_str()) {
                    builder = builder.project_directory(directory.to_string());
                }
                if config
                    .get("pull_only_root_directory")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    builder = builder.pull_only_root_directory(true);
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

                // Route implicit Docker Hub base images through the
                // operator's configured registry mirror/prefix, if any.
                // Falls back to unconfigured (no rewriting) rather than
                // failing the build if settings can't be read -- the
                // existing anonymous-pull behavior is always a safe default.
                let registry_mirror_prefix = self
                    .config_service
                    .get_settings()
                    .await
                    .ok()
                    .and_then(|settings| settings.registry_mirror_prefix);

                // Same source of truth as placement: a control-plane profile
                // must select a remote builder before constructing the job.
                let local_workloads_enabled = self
                    .node_scheduler
                    .get()
                    .map(|scheduler| scheduler.local_workloads_enabled())
                    .unwrap_or(true);

                let mut builder = BuildImageJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .download_job_id(download_job_id)
                    .image_tag(image_tag)
                    .dockerfile_path(dockerfile_path.to_string())
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .registry_mirror_prefix(registry_mirror_prefix)
                    .local_workloads_enabled(local_workloads_enabled);

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
                // Worker builds cannot receive registry credentials, so a
                // build that needs them stays on the control plane.
                let needs_npm_credentials =
                    crate::jobs::npmrc::plan_npmrc(&build_args_map).is_some();
                // Names whose values may be sent to a worker build (names
                // only, plaintext). A job planned before the field existed
                // has none: a worker then receives no value at all.
                let worker_build_arg_keys: std::collections::BTreeSet<String> = config
                    .get(crate::services::workflow_planner::WORKER_BUILD_ARG_KEYS_FIELD)
                    .and_then(|value| value.as_array())
                    .map(|keys| {
                        keys.iter()
                            .filter_map(|key| key.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                // Only a secret value the framework inlines changes what a
                // worker builds; such a build stays here when it can.
                let inlined_build_variables =
                    withheld_inlined_variables(build_args_map.keys(), &worker_build_arg_keys);
                builder = builder.worker_build_arg_keys(worker_build_arg_keys);
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

                if !cross_builds_enabled && local_workloads_enabled {
                    debug!(
                        deployment_id = db_job.deployment_id,
                        "Cross-architecture builds disabled; building for the control plane's \
                         platform only"
                    );
                }

                // The platforms a multi-arch build must cover; empty for the
                // usual single native build.
                let mut cross_build_platforms: Vec<String> = Vec::new();
                if let (true, Some(scheduler)) = (
                    cross_builds_enabled && local_workloads_enabled,
                    self.node_scheduler.get(),
                ) {
                    let (target_nodes, target_labels) = resolve_placement_constraints(
                        environment.deployment_config.as_ref(),
                        project.deployment_config.as_ref(),
                    );

                    match scheduler
                        .required_build_platforms_for_project(
                            target_labels.as_ref(),
                            target_nodes.as_deref(),
                            Some(&project.slug),
                        )
                        .await
                    {
                        Ok(platforms) if !platforms.is_empty() => {
                            info!(
                                deployment_id = db_job.deployment_id,
                                platforms = ?platforms,
                                "Cluster spans multiple architectures — building one image per platform"
                            );
                            cross_build_platforms = platforms.clone();
                            builder = builder.target_platforms(platforms);
                        }
                        Ok(_) => {}
                        Err(crate::services::node_service::NodeError::Validation { message }) => {
                            return Err(WorkflowExecutionError::InvalidJobConfig(message));
                        }
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

                let build_location =
                    temps_entities::deployment_config::DeploymentConfig::resolve_build_location(
                        environment.deployment_config.as_ref(),
                        project.deployment_config.as_ref(),
                    );
                let image_builder: Arc<dyn ImageBuilder> = if build_location
                    == temps_entities::deployment_config::BuildLocation::Ci
                {
                    inlined_variables_allow_offload(&inlined_build_variables).map_err(
                        |reason| {
                            WorkflowExecutionError::InvalidJobConfig(format!(
                                "Deployment {} CI build cannot transfer inlined secrets: {reason}",
                                deployment.id
                            ))
                        },
                    )?;
                    if needs_npm_credentials {
                        return Err(WorkflowExecutionError::InvalidJobConfig(format!(
                            "Deployment {} CI build cannot transfer package-manager credentials",
                            deployment.id
                        )));
                    }
                    let encryption = self.encryption_service.get().ok_or_else(|| {
                        WorkflowExecutionError::InvalidJobConfig(
                            "CI build encryption service unavailable".into(),
                        )
                    })?;
                    let service = crate::services::ci_dispatch::CiDispatchService::new(
                        self.db.clone(),
                        encryption.clone(),
                        self.config_service.data_dir(),
                    )
                    .map_err(|error| WorkflowExecutionError::InvalidJobConfig(error.to_string()))?;
                    builder = builder.external_build(true);
                    let (target_nodes, target_labels) = resolve_placement_constraints(
                        environment.deployment_config.as_ref(),
                        project.deployment_config.as_ref(),
                    );
                    let local_platform = if local_workloads_enabled {
                        self.image_builder.ensure_platform_discovered().await
                    } else {
                        None
                    };
                    let selected = if let Some(platform) = &local_platform {
                        let platforms = if cross_build_platforms.is_empty() {
                            vec![platform.clone()]
                        } else {
                            cross_build_platforms.clone()
                        };
                        self.select_platform_builders(
                            deployment.id,
                            &project.slug,
                            target_nodes.as_deref(),
                            target_labels.as_ref(),
                            &platforms,
                        )
                        .await
                    } else {
                        let static_output_only = deployment_jobs::Entity::find()
                            .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
                            .filter(deployment_jobs::Column::JobType.eq("DeployStaticJob"))
                            .one(self.db.as_ref())
                            .await?
                            .is_some();
                        self.select_node_builder(
                            deployment.id,
                            &project.slug,
                            target_nodes.as_deref(),
                            target_labels.as_ref(),
                            static_output_only,
                        )
                        .await
                    };
                    match selected {
                        Ok(selected) => {
                            let targets: Vec<_> = selected
                                .into_iter()
                                .map(|selected| crate::jobs::build_image::WorkerPlatformBuilder {
                                    platform: selected.platform,
                                    node_id: selected.node_id,
                                    node_name: selected.node_name,
                                    builder: Arc::new(
                                        crate::services::ci_dispatch::CiImageBuilder {
                                            service: service.clone(),
                                            deployment_id: deployment.id,
                                            target: selected.remote,
                                        },
                                    ),
                                })
                                .collect();
                            let primary = targets
                                .first()
                                .ok_or_else(|| {
                                    WorkflowExecutionError::InvalidJobConfig(
                                        "CI build has no artifact target worker".into(),
                                    )
                                })?
                                .builder
                                .clone();
                            builder = builder.platform_builders(targets);
                            primary
                        }
                        Err(error) if local_platform.is_some() => {
                            if cross_build_platforms.is_empty() {
                                builder =
                                    builder.target_platforms(local_platform.into_iter().collect());
                            }
                            builder = builder.build_location_notice(format!("CI builds run remotely. Artifacts use the control plane's daemon because no eligible worker can receive them: {error}"));
                            Arc::new(crate::services::ci_dispatch::CiImageBuilder {
                                service,
                                deployment_id: deployment.id,
                                target: self.image_builder.clone(),
                            })
                        }
                        Err(error) => return Err(error),
                    }
                } else if local_workloads_enabled
                    && build_location
                        == temps_entities::deployment_config::BuildLocation::ControlPlane
                {
                    self.image_builder.clone()
                } else if local_workloads_enabled {
                    // The operator asked to keep this build off the control
                    // plane. A worker build is a preference: when no worker
                    // can produce the same image the control plane would, the
                    // build stays here and the build log says why.
                    let (target_nodes, target_labels) = resolve_placement_constraints(
                        environment.deployment_config.as_ref(),
                        project.deployment_config.as_ref(),
                    );
                    match self
                        .select_offload_builder(
                            deployment.id,
                            &project.slug,
                            target_nodes.as_deref(),
                            target_labels.as_ref(),
                            &cross_build_platforms,
                            needs_npm_credentials,
                            &inlined_build_variables,
                        )
                        .await
                    {
                        Ok(selected) => {
                            // Whether the node has a free build slot is only
                            // known when the build is about to be sent, after
                            // the repository download. The build job asks
                            // then, and builds on the control plane instead
                            // when the node stays busy. This must come before
                            // `target_platforms`, which it records to restore.
                            builder = builder.offload_fallback(OffloadFallback::new(
                                selected.agent.clone(),
                                selected.node_id,
                                selected.node_name.clone(),
                                selected.agent.build_queue_wait(),
                                self.image_builder.clone(),
                            ));
                            // The control plane's own post-build jobs read
                            // the image from its daemon, so the finished
                            // image is streamed back once the build is done.
                            builder = builder
                                .remote_builder_node_id(selected.node_id)
                                .target_platforms(vec![selected.platform.clone()])
                                .copy_image_to_control_plane(
                                    self.image_builder.clone(),
                                    selected.node_name.clone(),
                                );
                            selected.remote
                        }
                        Err(reason) => {
                            warn!(
                                deployment_id = deployment.id,
                                project_slug = %project.slug,
                                "Build location is 'node' but building on the control plane: {}",
                                reason
                            );
                            builder = builder.build_location_notice(format!(
                                "⚠️ Build location is set to 'node', but this build runs on the \
                                 control plane: {reason}"
                            ));
                            self.image_builder.clone()
                        }
                    }
                } else {
                    // A static site's image is only read for its output
                    // files, which DeployStaticJob fetches from the build node
                    // and the control plane serves; it never runs anywhere.
                    let static_output_only = deployment_jobs::Entity::find()
                        .filter(deployment_jobs::Column::DeploymentId.eq(deployment.id))
                        .filter(deployment_jobs::Column::JobType.eq("DeployStaticJob"))
                        .one(self.db.as_ref())
                        .await?
                        .is_some();
                    // No local daemon: build on a node. The image is then
                    // handed to each replica's node by DeployImageJob.
                    let (target_nodes, target_labels) = resolve_placement_constraints(
                        environment.deployment_config.as_ref(),
                        project.deployment_config.as_ref(),
                    );
                    let mut selected = self
                        .select_node_builder(
                            deployment.id,
                            &project.slug,
                            target_nodes.as_deref(),
                            target_labels.as_ref(),
                            static_output_only,
                        )
                        .await?;
                    if selected.len() > 1 {
                        // A mixed-architecture deployment: one native build
                        // per architecture, each on its own worker.
                        let primary = selected[0].remote.clone();
                        builder = builder.platform_builders(
                            selected
                                .into_iter()
                                .map(|selected| crate::jobs::build_image::WorkerPlatformBuilder {
                                    platform: selected.platform,
                                    node_id: selected.node_id,
                                    node_name: selected.node_name,
                                    builder: selected.remote,
                                })
                                .collect(),
                        );
                        primary
                    } else {
                        let selected = selected.pop().ok_or_else(|| {
                            WorkflowExecutionError::JobCreationFailed(format!(
                                "Deployment {}: no build node was selected",
                                deployment.id
                            ))
                        })?;
                        builder = builder
                            .remote_builder_node_id(selected.node_id)
                            .target_platforms(vec![selected.platform.clone()]);
                        selected.remote
                    }
                };
                let job = builder.build(image_builder)?;

                Ok(Arc::new(job))
            }

            "DeployContainerJob" | "DeployImageJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let port = config.get("port").and_then(|v| v.as_i64()).unwrap_or(3000) as u16;

                // Explicit environment/project port override, as resolved by the
                // planner. `Some` here means `resolve_container_port()` must use
                // it directly instead of falling back to image EXPOSE detection.
                let configured_port = config
                    .get("configured_port")
                    .and_then(|v| v.as_i64())
                    .map(|p| p as u16);

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

                // Same constraints the pre-build placement check used
                let (target_nodes, target_labels) = resolve_placement_constraints(
                    environment.deployment_config.as_ref(),
                    project.deployment_config.as_ref(),
                );

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
                // Readiness budget: environment override, then project, then
                // the platform default. A slow-booting app gets more time by
                // raising this, instead of failing as a generic timeout.
                let health_check_timeout_secs =
                    temps_entities::deployment_config::resolve_health_check_timeout_seconds(
                        resolve_i32(|c| c.health_check_timeout_seconds),
                    );
                let resources = ResourceUsage {
                    cpu_limit: cpu_limit_micro.map(|u| format!("{}u", u)),
                    memory_limit: memory_limit_mb.map(|mb| format!("{}Mi", mb)),
                    cpu_request: cpu_request_micro.map(|u| format!("{}u", u)),
                    memory_request: memory_request_mb.map(|mb| format!("{}Mi", mb)),
                };

                // ADR 045: the executing host compares this against its own
                // grant. A constructor argument, so no deploy path can omit it.
                //
                // The authority comes from the plan, not from an `AuthContext`
                // — there is no request here, this runs from the queue. The
                // planner recorded whether the principal that asked for the
                // deployment was allowed to deploy a host-root project, and
                // `planned_deploy_caller` fails closed when it did not say.
                let mut builder = DeployImageJobBuilder::new(
                    project.slug.clone(),
                    crate::services::workflow_planner::planned_deploy_caller(config),
                )
                .job_id(db_job.job_id.clone())
                .build_job_id(build_job_id)
                .target(DeploymentTarget::Docker {
                    registry_url: "local".to_string(),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                })
                .service_name(deployment.slug.clone())
                .namespace("default".to_string())
                .audit_logger(self.audit_logger.get().cloned())
                .port(port as u32)
                .configured_port(configured_port)
                .health(
                    env_cfg
                        .and_then(|c| c.health.clone())
                        .or_else(|| proj_cfg.and_then(|c| c.health.clone())),
                )
                .health_check_timeout_secs(health_check_timeout_secs)
                .replicas(replicas)
                .environment_variables(env_variables)
                .remote_environment_variables(remote_env_variables)
                .cross_node_service_blockers(
                    crate::services::workflow_planner::read_cross_node_blockers(config),
                )
                .secrets(secrets)
                .resources(resources)
                .log_id(db_job.log_id.clone())
                .log_service(self.log_service.clone())
                .failed_container_retention(self.db.clone(), deployment.id);

                let runtime = deployment
                    .metadata
                    .as_ref()
                    .map(|metadata| metadata.container_runtime())
                    .unwrap_or_default();
                builder = builder
                    .command(runtime.command)
                    .entrypoint(runtime.entrypoint);

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

                // ADR-055: the environment's workload mode. `legacy` gets
                // nothing extra, so its deploy path is exactly today's.
                let workload_mode = environment.workload_mode;
                let workload_plane = match (workload_mode, self.workload_placement.get()) {
                    (
                        temps_entities::environment_workload_mode::EnvironmentWorkloadMode::Legacy,
                        _,
                    )
                    | (_, None) => None,
                    (_, Some(placement)) => Some(crate::jobs::WorkloadPlaneDeploy {
                        placement: placement.clone(),
                        plane: self.workload_plane.get().cloned(),
                    }),
                };
                builder = builder.workload_mode(workload_mode, workload_plane);
                // ADR-055 Phase 10: only `enforce` runs replicas through the
                // plane, so only it honours the isolation setting. `legacy`
                // does not even read it.
                let reads_isolation = workload_mode
                    != temps_entities::environment_workload_mode::EnvironmentWorkloadMode::Legacy;
                if let Some(placement) = self.workload_placement.get().filter(|_| reads_isolation) {
                    let isolation = placement
                        .environment_workload_isolation(environment.id)
                        .await
                        .map_err(|e| {
                            WorkflowExecutionError::InvalidJobConfig(format!(
                                "Deployment {}: cannot read the workload isolation of \
                                 environment {}: {e}",
                                deployment.id, environment.id
                            ))
                        })?;
                    if workload_mode
                        == temps_entities::environment_workload_mode::EnvironmentWorkloadMode::Enforce
                    {
                        builder = builder.workload_isolation(
                            crate::services::workload_placement::isolation_for(isolation),
                        );
                    } else if isolation
                        != temps_entities::environment_workload_isolation::EnvironmentWorkloadIsolation::Process
                    {
                        info!(
                            deployment_id = deployment.id,
                            environment_id = environment.id,
                            workload_isolation = isolation.as_str(),
                            workload_mode = workload_mode.as_str(),
                            "workload isolation is ignored outside workload_mode enforce; \
                             deploying a container"
                        );
                    }
                }

                // ADR-055 Phase 9: the environment's attached volumes. One
                // indexed query; empty for every environment without one,
                // which then deploys exactly as before. Unreadable volumes
                // fail the deploy: a replica must never start without its
                // data.
                let volume_owner =
                    crate::services::workload_volumes::environment_volume_owner(environment.id);
                let volume_records = temps_workload_plane::VolumeStore::new(self.db.clone())
                    .all_of(&volume_owner)
                    .await
                    .map_err(|e| {
                        WorkflowExecutionError::JobCreationFailed(format!(
                            "reading the volumes of environment {} for deployment {}: {e}",
                            environment.id, deployment.id
                        ))
                    })?;
                let volume_mounts: Vec<temps_workload::VolumeMount> = volume_records
                    .iter()
                    .filter_map(temps_workload_plane::VolumeRecord::mount)
                    .collect();
                if !volume_mounts.is_empty() {
                    let pin = temps_workload_plane::volumes::pin_for(
                        temps_workload::WorkloadRef::new(
                            temps_workload::WorkloadKind::Deployment,
                            i64::from(deployment.id),
                        ),
                        &volume_mounts,
                        &volume_records,
                        None,
                    )
                    .map_err(|e| {
                        WorkflowExecutionError::JobCreationFailed(format!(
                            "the volumes of environment {} cannot be mounted by deployment {}: {e}",
                            environment.id, deployment.id
                        ))
                    })?;
                    builder = builder.volume_mounts(volume_mounts, pin);
                }

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
                if let Some(ref image_tag) = external_image_tag {
                    debug!("🐳 Using external image tag for deployment: {}", image_tag);
                    builder = builder.external_image_tag(image_tag.clone());
                }

                // Where the image lives (registry vs. only on this control
                // plane) — decides whether a remote worker pulls it or receives
                // it via import. Absent on job configs planned before the field
                // existed; the job then derives it (see `DeployImageSource::resolve`).
                builder = builder.image_source_from_job_config(config);

                // Apply container log rotation settings from config, and — for a
                // registry-sourced image — the same private-registry credentials
                // `PullExternalImageJob` uses, so a worker node can pull the image
                // itself via `POST /agent/images/pull` without the control plane
                // ever needing a Docker daemon. Only forwarded when the image's
                // registry matches the configured registry (same matching rule as
                // `PullExternalImageJob`, never send credentials to a registry that
                // didn't ask for them).
                if let Ok(settings) = self.config_service.get_settings().await {
                    builder =
                        builder.container_log_config(temps_deployer::ContainerLogConfig::new(
                            settings.container_logs.max_size.clone(),
                            settings.container_logs.max_file,
                        ));

                    if let Some(ref image_tag) = external_image_tag {
                        let reg = &settings.docker_registry;
                        if reg.enabled {
                            if let (Some(username), Some(password), Some(registry_url)) = (
                                reg.username.clone(),
                                reg.password.clone(),
                                reg.registry_url.clone(),
                            ) {
                                let image_registry =
                                    PullExternalImageJob::registry_from_image_ref(image_tag);
                                let configured_registry =
                                    PullExternalImageJob::registry_host_from_url(&registry_url);
                                if matches!(
                                    (&image_registry, &configured_registry),
                                    (Some(image_registry), Some(configured_registry))
                                        if image_registry == configured_registry
                                ) {
                                    builder = builder.registry_credentials(
                                        temps_deployer::remote::RemotePullCredentials {
                                            username: Some(username),
                                            password: Some(password),
                                            identity_token: None,
                                            server_address: Some(registry_url),
                                        },
                                    );
                                } else {
                                    warn!(
                                        image_registry = ?image_registry,
                                        configured_registry = ?configured_registry,
                                        "Skipping Docker registry credentials for remote pull because the image registry does not match"
                                    );
                                }
                            }
                        }
                    }
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
                    .environment_addresses(self.environment_addresses.get().cloned())
                    .job_id(db_job.job_id.clone())
                    .deployment_id(deployment_id)
                    .db(self.db.clone())
                    .log_id(db_job.log_id.clone())
                    .log_service(self.log_service.clone())
                    .container_deployer(self.container_deployer.clone())
                    .queue(self.queue.clone())
                    .config_service(self.config_service.clone())
                    // Superseded `enforce` replicas are retired through it.
                    .workload_plane(self.workload_plane.get().cloned());

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
                    .capture_guard(self.capture_guard.clone())
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
                    self.docker_handle.clone(),
                )
                .with_log_id(db_job.log_id.clone())
                .with_log_service(self.log_service.clone())
                .with_image_builder(self.image_builder.clone());
                let job = match self.builder_node_resolver() {
                    Some(resolver) => job.with_builder_node_resolver(resolver),
                    None => job,
                };

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
                let job = match self.builder_node_resolver() {
                    Some(resolver) => job.with_builder_node_resolver(resolver),
                    None => job,
                };

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

                if let Some(resolver) = self.builder_node_resolver() {
                    job = job.with_builder_node_resolver(resolver);
                }

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
                let job = match self.builder_node_resolver() {
                    Some(resolver) => job.with_builder_node_resolver(resolver),
                    None => job,
                };

                Ok(Arc::new(job))
            }

            "DeployStaticFromSourceJob" => {
                let config = db_job.job_config.as_ref().ok_or_else(|| {
                    WorkflowExecutionError::MissingJobConfig(db_job.job_id.clone())
                })?;

                let dependencies: Vec<String> = db_job
                    .dependencies
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let download_job_id = dependencies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "download_repo".to_string());

                let directory = config
                    .get("directory")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".")
                    .to_string();

                let deployment = deployments::Entity::find_by_id(db_job.deployment_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| {
                        WorkflowExecutionError::DeploymentNotFound(db_job.deployment_id)
                    })?;

                let job = DeployStaticFromSourceJob::new(
                    db_job.job_id.clone(),
                    download_job_id,
                    directory,
                    project.slug.clone(),
                    environment.slug.clone(),
                    deployment.slug.clone(),
                    self.static_deployer.clone(),
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

                let image_registry = PullExternalImageJob::registry_from_image_ref(&image_ref);

                let mut job = PullExternalImageJob::new(
                    db_job.job_id.clone(),
                    image_ref,
                    external_image_id,
                    self.docker_handle.clone(),
                )
                .with_log_service(self.log_service.clone(), db_job.log_id.clone());

                // Pass private registry credentials only to the configured registry.
                if let Ok(settings) = self.config_service.get_settings().await {
                    let reg = &settings.docker_registry;
                    if reg.enabled {
                        if let (Some(username), Some(password), Some(registry_url)) = (
                            reg.username.clone(),
                            reg.password.clone(),
                            reg.registry_url.clone(),
                        ) {
                            let configured_registry =
                                PullExternalImageJob::registry_host_from_url(&registry_url);
                            if matches!(
                                (&image_registry, &configured_registry),
                                (Some(image_registry), Some(configured_registry))
                                    if image_registry == configured_registry
                            ) {
                                job = job.with_registry_credentials(
                                    bollard::auth::DockerCredentials {
                                        username: Some(username),
                                        password: Some(password),
                                        serveraddress: Some(registry_url),
                                        ..Default::default()
                                    },
                                );
                            } else {
                                warn!(
                                    image_registry = ?image_registry,
                                    configured_registry = ?configured_registry,
                                    "Skipping Docker registry credentials because the image registry does not match"
                                );
                            }
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
                    self.docker_handle.clone(),
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
                Ok(Arc::new(
                    PrepareSourceBundleJob::new(
                        db_job.job_id.clone(),
                        absolute_path,
                        project.slug.clone(),
                    )
                    .with_log_service(self.log_service.clone(), db_job.log_id.clone()),
                ))
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
                // Absent on jobs queued before compose secret support; the
                // reader returns an empty map, so those deploy unchanged.
                let secrets = crate::services::sensitive_envelope::read_sealed(
                    config,
                    self.encryption_service.get(),
                    "secrets",
                )
                .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?;
                // Plain JSON, not sealed: service names are not sensitive.
                // Absent on jobs queued before scoping shipped, which reads as
                // "every secret goes to every service" — the prior behaviour.
                let secret_compose_services: std::collections::HashMap<String, Vec<String>> =
                    config
                        .get("secret_compose_services")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| WorkflowExecutionError::InvalidJobConfig(e.to_string()))?
                        .unwrap_or_default();

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

                let compose_policy =
                    temps_entities::compose_security_policies::Entity::find_by_id(project.id)
                        .one(self.db.as_ref())
                        .await
                        .map_err(|error| {
                            WorkflowError::JobExecutionFailed(format!(
                                "Failed to load Compose security policy for project {}: {error}",
                                project.id
                            ))
                        })?
                        .map(|row| row.policy)
                        .unwrap_or_default();
                // Build the executor from the handle rather than from a
                // resolved client: `ComposeExecutor` already carries the
                // "no daemon here" case (`docker_available()`), and
                // `DeployComposeJob::execute_locked` uses it to refuse with a
                // `LocalWorkloadsDisabled` failure naming the remedy.
                //
                // Resolving the daemon *here* instead would abort job
                // construction, which happens before `WorkflowExecutor`
                // exists — so the deployment would be failed by the outer
                // processor while its already-inserted `deployment_jobs` rows
                // stayed `pending` forever, with no per-job reason anywhere in
                // the UI. Constructing unconditionally keeps the refusal on
                // the job's own execution path, where the tracker records it.
                // Where the stack lives: its pinned home, or (first
                // deployment) the node its Target Nodes / Label Selectors
                // choose. See `services::compose_placement`.
                let (placement_plan, compose_host, compose_vacate_host) = self
                    .plan_compose_stack(project, environment, deployment, &compose_policy)
                    .await?;
                let compose_executor = Arc::new(
                    temps_deployer::compose::ComposeExecutor::new_with_handle(
                        self.docker_handle.clone(),
                        self.config_service.data_dir(),
                    )
                    .with_security_policy(compose_policy),
                );

                // Same startup-timeout setting as single-container deploys:
                // environment override, then project, then the default.
                let compose_ready_timeout_secs =
                    temps_entities::deployment_config::resolve_health_check_timeout_seconds(
                        environment
                            .deployment_config
                            .as_ref()
                            .and_then(|c| c.health_check_timeout_seconds)
                            .or_else(|| {
                                project
                                    .deployment_config
                                    .as_ref()
                                    .and_then(|c| c.health_check_timeout_seconds)
                            }),
                    );

                let job = crate::jobs::DeployComposeJobBuilder::new()
                    .job_id(db_job.job_id.clone())
                    .deployment_id(deployment.id)
                    .project_id(project.id)
                    .environment_id(environment.id)
                    .db(self.db.clone())
                    .compose_executor(compose_executor)
                    .compose_path(compose_path)
                    .directory(directory)
                    .compose_content(compose_content)
                    .compose_override(compose_override)
                    .excluded_services(excluded_services)
                    .relaxed_capability_services(relaxed_capability_services)
                    .unsandboxed_services(unsandboxed_services)
                    .ready_timeout_secs(compose_ready_timeout_secs)
                    .public_ports(public_ports)
                    .download_job_id(download_job_id)
                    .environment_vars(env_vars)
                    .secrets(secrets)
                    .secret_compose_services(secret_compose_services)
                    .build_args(build_args)
                    .log_id(Some(db_job.log_id.clone()))
                    .log_service(self.log_service.clone())
                    .workload_mode(environment.workload_mode)
                    .host(compose_host)
                    .vacate_host(compose_vacate_host)
                    .pin_home(!placement_plan.pinned)
                    .placement_drift(placement_plan.drift)
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

    /// Choose a worker node to build a source image that would otherwise be
    /// built by this control plane's own Docker (build location `node`).
    ///
    /// The node must produce exactly the image a local build would: the
    /// control plane's architecture and one platform -- or, for a static
    /// site, any architecture. A node labelled `temps.sh/role=builder` is
    /// preferred; otherwise the worker this deployment would be placed on
    /// builds it, so that replica needs no transfer. `Err` carries the reason
    /// the build stays on the control plane, written verbatim into the build
    /// log.
    #[allow(clippy::too_many_arguments)]
    async fn select_offload_builder(
        &self,
        deployment_id: i32,
        project_slug: &str,
        target_nodes: Option<&[i32]>,
        target_labels: Option<&serde_json::Value>,
        cross_build_platforms: &[String],
        needs_npm_credentials: bool,
        inlined_build_variables: &[String],
    ) -> Result<SelectedNodeBuilder, String> {
        // The reason becomes the build-log line explaining why the build
        // stays on the control plane.
        inlined_variables_allow_offload(inlined_build_variables)
            .map_err(|reason| reason.to_string())?;
        let static_output_only = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::JobType.eq("DeployStaticJob"))
            .one(self.db.as_ref())
            .await
            .map_err(|error| {
                format!("could not read the jobs of deployment {deployment_id}: {error}")
            })?
            .is_some();
        let control_plane_platform = if static_output_only {
            None
        } else {
            self.image_builder.ensure_platform_discovered().await
        };
        let scheduler = self.node_scheduler.get();
        let candidate = offload_build_candidate(
            static_output_only,
            needs_npm_credentials,
            cross_build_platforms,
            control_plane_platform.as_deref(),
            scheduler.is_some(),
        )?;
        let Some(scheduler) = scheduler else {
            return Err("node scheduling is not available on this server".to_string());
        };

        // A dedicated builder needs no placement: one of the required
        // platform, or of any architecture for a static site. Placement below
        // only considers workload nodes, so this is the only way a builder-only
        // node can take the build.
        let dedicated = match &candidate.platform {
            Some(platform) => scheduler
                .select_builder_node(platform)
                .await
                .map(|node| node.map(|node| (node, platform.clone()))),
            None => scheduler.select_any_builder_node().await.map(|node| {
                node.and_then(|node| {
                    let platform = node.architecture.as_deref().map(|architecture| {
                        temps_deployer::platform::canonicalize_platform(architecture)
                    })?;
                    Some((node, platform))
                })
            }),
        }
        .map_err(|error| format!("could not list build nodes: {error}"))?;
        if let Some((node, platform)) = dedicated {
            let selected = self
                .connect_build_node(deployment_id, node, platform)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(selected);
        }

        let required_platforms: Vec<String> = candidate.platform.iter().cloned().collect();
        let outcome = scheduler
            .schedule_placement(crate::services::node_scheduler::ReplicaPlacementRequest {
                replica_count: 1,
                labels: target_labels,
                target_node_ids: target_nodes,
                anti_affinity: true,
                exclude_node_ids: &[],
                image_platforms: &required_platforms,
                project_slug: Some(project_slug),
                // Only workers: the control plane is what the build is
                // moving off, so it must never win the placement.
                exclude_control_plane: true,
            })
            .await;
        let no_worker = || {
            let needed = candidate
                .platform
                .as_deref()
                .map(|platform| format!(" runs {platform}"))
                .unwrap_or_default();
            format!(
                "no active worker node{needed} for this deployment. Join one with `temps join`, \
                 or label a node `{}={}` to dedicate it to builds",
                crate::services::node_scheduler::NODE_ROLE_LABEL,
                crate::services::node_scheduler::BUILDER_NODE_ROLE,
            )
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            // An empty worker pool, with or without placement constraints.
            Err(
                crate::services::node_service::NodeError::NoCompatibleNode { .. }
                | crate::services::node_service::NodeError::LocalWorkloadsDisabled { .. },
            ) => return Err(no_worker()),
            Err(error) => return Err(format!("could not choose a worker node: {error}")),
        };
        let Some(crate::services::NodeAssignment::Remote {
            node_id: target_id,
            node_name: target_name,
            platform: target_platform,
            ..
        }) = outcome.assignments.into_iter().next()
        else {
            return Err(no_worker());
        };

        let platform = match candidate.platform {
            Some(platform) => platform,
            // A static site builds for whatever the chosen worker runs; no
            // dedicated builder exists, or it would have been picked above.
            None => target_platform
                .filter(|value| temps_deployer::platform::is_buildable_platform(value))
                .map(|value| temps_deployer::platform::canonicalize_platform(&value))
                .ok_or_else(|| {
                    format!(
                        "worker '{target_name}' (id={target_id}) has not reported a \
                         buildable architecture yet"
                    )
                })?,
        };
        let node = scheduler
            .node_service()
            .get_by_id(target_id)
            .await
            .map_err(|error| {
                format!("could not load worker '{target_name}' (id={target_id}): {error}")
            })?;
        let selected = self
            .connect_build_node(deployment_id, node, platform)
            .await
            .map_err(|error| error.to_string())?;
        Ok(selected)
    }

    /// Choose the node that builds a source image when the control plane
    /// has no Docker daemon.
    ///
    /// Placement is resolved first (one replica, with the deployment's own
    /// node and label constraints) because the architecture of the node that
    /// will run the image decides what to build. A build-only node
    /// (`temps.sh/role=builder`) of that architecture is preferred so build
    /// load stays off application hosts; without one, the placement target
    /// builds the image itself and no transfer is needed for it.
    /// The resolver jobs use to read an image back from the worker that
    /// built it. `None` only before the scheduler and encryption service are
    /// wired, in which case no worker build can have run either.
    fn builder_node_resolver(&self) -> Option<Arc<dyn BuilderNodeResolver>> {
        let scheduler = self.node_scheduler.get()?.clone();
        let encryption = self.encryption_service.get()?.clone();
        Some(Arc::new(NodeBuilderResolver {
            scheduler,
            config: self.config_service.clone(),
            encryption,
        }))
    }

    /// Routes scans that run after a deployment (the daily rescan, a manual
    /// one) to the build node holding a worker-built image. `None` before
    /// the scheduler and encryption service are wired, which is also before
    /// any worker build can exist.
    pub fn build_node_scanner_resolver(
        &self,
    ) -> Option<Arc<crate::jobs::scan_vulnerabilities::BuildNodeScannerResolver>> {
        let nodes = self.builder_node_resolver()?;
        Some(Arc::new(
            crate::jobs::scan_vulnerabilities::BuildNodeScannerResolver::new(
                self.db.clone(),
                self.image_builder.clone(),
                nodes,
                self.docker_handle.clone(),
            ),
        ))
    }

    async fn select_node_builder(
        &self,
        deployment_id: i32,
        project_slug: &str,
        target_nodes: Option<&[i32]>,
        target_labels: Option<&serde_json::Value>,
        static_output_only: bool,
    ) -> Result<Vec<SelectedNodeBuilder>, WorkflowExecutionError> {
        let scheduler = self.node_scheduler.get().ok_or_else(|| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Deployment {deployment_id} cannot select a build node: node scheduler is unavailable"
            ))
        })?;
        // Static output is architecture-independent and never runs on a
        // node, so any eligible worker may build it.
        let required_platforms = if static_output_only {
            Vec::new()
        } else {
            let platforms = scheduler
                .required_build_platforms_for_project(
                    target_labels,
                    target_nodes,
                    Some(project_slug),
                )
                .await
                .map_err(|error| {
                    WorkflowExecutionError::JobCreationFailed(format!(
                        "Deployment {deployment_id} cannot determine worker build platforms: {error}"
                    ))
                })?;
            // Inspect the entire eligible target set, not just the first
            // replica: every architecture a replica may land on needs its
            // own image, built natively on a worker of that architecture.
            if validate_worker_build_scope(deployment_id, &platforms)?
                == WorkerBuildScope::PerArchitecture
            {
                return self
                    .select_platform_builders(
                        deployment_id,
                        project_slug,
                        target_nodes,
                        target_labels,
                        &platforms,
                    )
                    .await;
            }
            platforms
        };
        let outcome = scheduler
            .schedule_placement(crate::services::node_scheduler::ReplicaPlacementRequest {
                replica_count: 1,
                labels: target_labels,
                target_node_ids: target_nodes,
                anti_affinity: true,
                exclude_node_ids: &[],
                image_platforms: &required_platforms,
                project_slug: Some(project_slug),
                exclude_control_plane: false,
            })
            .await
            .map_err(|error| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Deployment {deployment_id} cannot select a build node: {error}"
                ))
            })?;
        let Some(crate::services::NodeAssignment::Remote {
            node_id: target_id,
            node_name: target_name,
            platform: target_platform,
            ..
        }) = outcome.assignments.into_iter().next()
        else {
            return Err(WorkflowExecutionError::JobCreationFailed(format!(
                "Deployment {deployment_id} has no worker node to build on: this control plane \
                 runs no builds — join a worker with `temps join`"
            )));
        };
        let platform = target_platform
            .filter(|value| temps_deployer::platform::is_buildable_platform(value))
            .ok_or_else(|| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Worker '{target_name}' (id={target_id}) has not reported a buildable \
                     architecture for deployment {deployment_id}; wait for its next heartbeat"
                ))
            })?;

        let dedicated = scheduler
            .select_builder_node(&platform)
            .await
            .map_err(|error| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Deployment {deployment_id} cannot list build nodes: {error}"
                ))
            })?;
        let node = match dedicated {
            Some(node) => node,
            None => scheduler
                .node_service()
                .get_by_id(target_id)
                .await
                .map_err(|error| {
                    WorkflowExecutionError::JobCreationFailed(format!(
                        "Cannot load worker '{target_name}' (id={target_id}) to build \
                         deployment {deployment_id}: {error}"
                    ))
                })?,
        };
        Ok(vec![
            self.connect_build_node(deployment_id, node, platform)
                .await?,
        ])
    }

    /// Choose one worker per platform for a deployment whose target nodes
    /// span several architectures, each building its own architecture
    /// natively.
    ///
    /// For every platform a dedicated builder (`temps.sh/role=builder`) of
    /// that architecture is preferred, then a node this deployment could be
    /// placed on. Each candidate is probed over its authenticated agent
    /// connection first: an unreachable worker, or one whose agent reports
    /// another architecture than its node row, is passed over for the next
    /// one. A platform with no usable worker stops the deployment with
    /// [`WorkflowExecutionError::NoBuilderForArchitecture`] before anything
    /// is built -- there is no emulated fallback.
    async fn select_platform_builders(
        &self,
        deployment_id: i32,
        project_slug: &str,
        target_nodes: Option<&[i32]>,
        target_labels: Option<&serde_json::Value>,
        platforms: &[String],
    ) -> Result<Vec<SelectedNodeBuilder>, WorkflowExecutionError> {
        let scheduler = self.node_scheduler.get().ok_or_else(|| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Deployment {deployment_id} cannot select build nodes: node scheduler is unavailable"
            ))
        })?;
        let encryption = self.encryption_service.get().ok_or_else(|| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Deployment {deployment_id} cannot reach its build nodes: the encryption service \
                 that unseals agent tokens is unavailable"
            ))
        })?;
        // Every architecture's candidates are probed at the same time, so a
        // slow or unreachable worker of one architecture does not hold up the
        // choice for the others. The result keeps `platforms`' order.
        futures::future::try_join_all(platforms.iter().map(|platform| async move {
            let mut passed_over: Vec<String> = Vec::new();
            let mut tried: Vec<i32> = Vec::new();
            let no_builder = |reason: String| WorkflowExecutionError::NoBuilderForArchitecture {
                deployment_id,
                platform: platform.clone(),
                reason,
            };

            // Every dedicated builder of this architecture, lowest id first,
            // before any application worker: placement below never offers a
            // build-only node, so one that is skipped here is never tried.
            let mut dedicated: std::collections::VecDeque<_> = scheduler
                .select_builder_nodes(platform)
                .await
                .map_err(|error| {
                    WorkflowExecutionError::JobCreationFailed(format!(
                        "Deployment {deployment_id} cannot list {platform} build nodes: {error}"
                    ))
                })?
                .into();
            let builder = loop {
                let node = match dedicated.pop_front() {
                    Some(node) => node,
                    None => {
                        let outcome = scheduler
                            .schedule_placement(
                                crate::services::node_scheduler::ReplicaPlacementRequest {
                                    replica_count: 1,
                                    labels: target_labels,
                                    target_node_ids: target_nodes,
                                    anti_affinity: true,
                                    exclude_node_ids: &tried,
                                    image_platforms: std::slice::from_ref(platform),
                                    project_slug: Some(project_slug),
                                    exclude_control_plane: true,
                                },
                            )
                            .await;
                        let assignment = match outcome {
                            Ok(outcome) => outcome.assignments.into_iter().next(),
                            Err(error) if passed_over.is_empty() => {
                                return Err(no_builder(format!(
                                    "no eligible {platform} worker: {error}"
                                )));
                            }
                            Err(_) => None,
                        };
                        // The scheduler relaxes `exclude_node_ids` once every
                        // candidate is excluded and offers one again, so a
                        // repeat means no other worker of this platform is left.
                        let Some(crate::services::NodeAssignment::Remote { node_id, .. }) =
                            assignment.filter(|assignment| {
                                assignment
                                    .node_id()
                                    .is_some_and(|node_id| !tried.contains(&node_id))
                            })
                        else {
                            return Err(no_builder(if passed_over.is_empty() {
                                format!("no eligible {platform} worker")
                            } else {
                                passed_over.join("; ")
                            }));
                        };
                        scheduler
                            .node_service()
                            .get_by_id(node_id)
                            .await
                            .map_err(|error| {
                                WorkflowExecutionError::JobCreationFailed(format!(
                                    "Cannot load worker {node_id} to build {platform} for \
                                         deployment {deployment_id}: {error}"
                                ))
                            })?
                    }
                };
                tried.push(node.id);
                match probe_platform_builder(
                    &node,
                    platform,
                    self.config_service.as_ref(),
                    encryption.as_ref(),
                )
                .await
                {
                    Ok(remote) => break (node, remote),
                    Err(reason) => {
                        warn!(
                            deployment_id,
                            node_id = node.id,
                            node_name = %node.name,
                            platform = %platform,
                            "Passing over worker for a per-architecture build: {}",
                            reason
                        );
                        passed_over.push(reason);
                    }
                }
            };
            let (node, remote) = builder;
            info!(
                deployment_id,
                node_id = node.id,
                node_name = %node.name,
                build_only = crate::services::node_scheduler::is_build_only_node(&node),
                platform = %platform,
                "Building one architecture of a mixed-architecture deployment on node"
            );
            let remote = Arc::new(remote);
            Ok::<_, WorkflowExecutionError>(SelectedNodeBuilder {
                node_id: node.id,
                node_name: node.name.clone(),
                platform: platform.clone(),
                remote: remote.clone(),
                agent: remote,
            })
        }))
        .await
    }

    /// Open an authenticated agent client to `node` for building `platform`.
    async fn connect_build_node(
        &self,
        deployment_id: i32,
        node: temps_entities::nodes::Model,
        platform: String,
    ) -> Result<SelectedNodeBuilder, WorkflowExecutionError> {
        let build_only = crate::services::node_scheduler::is_build_only_node(&node);
        let encryption = self.encryption_service.get().ok_or_else(|| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Deployment {deployment_id} cannot decrypt the token for build node '{}' (id={})",
                node.name, node.id
            ))
        })?;
        let remote = connect_build_node(&node, self.config_service.as_ref(), encryption.as_ref())
            .await
            .map_err(|reason| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Deployment {deployment_id}: {reason}"
                ))
            })?;
        info!(
            deployment_id,
            node_id = node.id,
            node_name = %node.name,
            build_only,
            platform = %platform,
            "Building source image on node"
        );
        let remote = Arc::new(remote.with_platform(Some(platform.clone())));
        Ok(SelectedNodeBuilder {
            node_id: node.id,
            node_name: node.name.clone(),
            platform,
            remote: remote.clone(),
            agent: remote,
        })
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
                use temps_entities::deployment_containers;
                let portless = deployment_containers::Entity::find()
                    .filter(deployment_containers::Column::DeploymentId.eq(updated_deployment.id))
                    .filter(deployment_containers::Column::DeletedAt.is_null())
                    .filter(deployment_containers::Column::ContainerPort.is_null())
                    .one(self.db.as_ref())
                    .await?
                    .is_some();
                let url = if portless {
                    None
                } else if !environment.host.is_empty() {
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
                let template_provenance =
                    projects::Entity::find_by_id(updated_deployment.project_id)
                        .one(self.db.as_ref())
                        .await
                        .ok()
                        .flatten()
                        .and_then(|project| project.template_slug);
                self.telemetry().report(
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::DeployCancelled,
                    )
                    .with("trigger", "workflow")
                    .with_template_provenance(template_provenance.as_deref()),
                );
            }
            _ => {}
        }

        Ok(())
    }

    async fn teardown_deployer_for_node(
        &self,
        node_id: Option<i32>,
    ) -> Result<Arc<dyn ContainerDeployer>, WorkflowExecutionError> {
        let Some(node_id) = node_id else {
            return Ok(self.container_deployer.clone());
        };
        let (_, deployer) = self
            .remote_deployer_for_node(node_id, "removing a retained container")
            .await?;
        Ok(Arc::new(deployer))
    }

    /// The agent client for worker node `node_id`, for `purpose` (named in
    /// every error).
    async fn remote_deployer_for_node(
        &self,
        node_id: i32,
        purpose: &str,
    ) -> Result<
        (
            temps_entities::nodes::Model,
            temps_deployer::remote::RemoteNodeDeployer,
        ),
        WorkflowExecutionError,
    > {
        use temps_entities::nodes;

        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Worker node {node_id} is not registered (while {purpose})"
                ))
            })?;
        let encrypted_token = node.token_encrypted.as_ref().ok_or_else(|| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Worker node {node_id} has no agent token, so Temps cannot reach it safely (while {purpose})"
            ))
        })?;
        let encryption_service = self.encryption_service.get().ok_or_else(|| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Encryption service is unavailable, so worker node {node_id} cannot be reached (while {purpose})"
            ))
        })?;
        let token_bytes = encryption_service
            .decrypt(encrypted_token)
            .map_err(|error| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Failed to decrypt the agent token for worker node {node_id} (while {purpose}): {error}"
                ))
            })?;
        let token = String::from_utf8(token_bytes).map_err(|error| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Agent token for worker node {node_id} is not valid UTF-8 (while {purpose}): {error}"
            ))
        })?;
        let deployer = crate::cluster_ca::build_node_deployer(
            &node,
            token,
            self.config_service.as_ref(),
            encryption_service.as_ref(),
        )
        .await
        .map_err(|error| {
            WorkflowExecutionError::JobCreationFailed(format!(
                "Failed to connect to worker node {node_id} (while {purpose}): {error}"
            ))
        })?;
        Ok((node, deployer))
    }

    /// Where this deployment of a Compose environment runs, which host a
    /// pending move leaves, and whether the home still has to be pinned.
    async fn plan_compose_stack(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
        policy: &temps_entities::compose_security::ComposeSecurityPolicy,
    ) -> Result<
        (
            crate::services::compose_placement::ComposePlacementPlan,
            crate::jobs::ComposeStackHost,
            Option<crate::jobs::ComposeStackHost>,
        ),
        WorkflowExecutionError,
    > {
        let (target_nodes, target_labels) = resolve_placement_constraints(
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        );
        let plan =
            crate::services::compose_placement::ComposePlacementService::new(self.db.clone())
                .plan(
                    self.node_scheduler.get().map(Arc::as_ref),
                    project,
                    environment,
                    target_nodes.as_deref(),
                    target_labels.as_ref(),
                )
                .await
                .map_err(|source| WorkflowExecutionError::ComposePlacement {
                    deployment_id: deployment.id,
                    source,
                })?;
        let host = self
            .compose_stack_host(plan.home, policy, "deploying a Compose stack")
            .await?;
        if !host.is_control_plane() {
            self.refuse_linked_services_on_worker(project, deployment, &host)
                .await?;
        }
        let vacate_host = match plan.vacate {
            Some(old) => Some(
                self.compose_stack_host(old, policy, "moving a Compose stack off it")
                    .await?,
            ),
            None => None,
        };
        info!(
            deployment_id = deployment.id,
            environment_id = environment.id,
            home = %host.describe(),
            pinned = plan.pinned,
            moving_from = ?vacate_host.as_ref().map(crate::jobs::ComposeStackHost::describe),
            "Planned Compose stack placement"
        );
        Ok((plan, host, vacate_host))
    }

    async fn compose_stack_host(
        &self,
        node_id: Option<i32>,
        policy: &temps_entities::compose_security::ComposeSecurityPolicy,
        purpose: &str,
    ) -> Result<crate::jobs::ComposeStackHost, WorkflowExecutionError> {
        let Some(node_id) = node_id else {
            return Ok(crate::jobs::ComposeStackHost::ControlPlane);
        };
        let (node, deployer) = self.remote_deployer_for_node(node_id, purpose).await?;
        Ok(crate::jobs::ComposeStackHost::Worker {
            node_id,
            node_name: node.name,
            deployer: Arc::new(deployer),
            policy: policy.clone(),
        })
    }

    /// Linked Temps-managed services are reached by container name on the
    /// control plane's Docker network. A stack on a worker has no route to
    /// them, so it would start with connection strings that never connect.
    async fn refuse_linked_services_on_worker(
        &self,
        project: &projects::Model,
        deployment: &deployments::Model,
        host: &crate::jobs::ComposeStackHost,
    ) -> Result<(), WorkflowExecutionError> {
        use temps_entities::{external_services, project_services};
        let linked = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .find_also_related(external_services::Entity)
            .all(self.db.as_ref())
            .await?;
        if linked.is_empty() {
            return Ok(());
        }
        let services = linked
            .iter()
            .map(|(link, service)| match service {
                Some(service) => format!("'{}'", service.name),
                None => format!("service {}", link.service_id),
            })
            .collect::<Vec<_>>()
            .join(", ");
        Err(WorkflowExecutionError::ComposeLinkedServicesOnWorker {
            deployment_id: deployment.id,
            host: host.describe(),
            services,
        })
    }

    async fn teardown_registered_container(
        &self,
        container: temps_entities::deployment_containers::Model,
        retry_failed_cleanup: bool,
    ) -> Result<Option<String>, WorkflowExecutionError> {
        use temps_entities::deployment_containers;

        let container_id = container.container_id.clone();
        let node_id = container.node_id;
        let original_status = container.status.clone();
        let rotate_retry = retry_failed_cleanup
            || original_status
                .as_deref()
                .is_some_and(|status| status.starts_with("retained:"));
        // A replica an `enforce` deployment started through the workload
        // plane is retired by deleting its workload: the plane removes the
        // instance (a container or a microVM Docker cannot see). Removing
        // only the container would leave the workload running or restarted,
        // and a microVM's 404 must never count as "already removed".
        let resolved = match crate::services::plane_deployer::deployment_replica_workload(
            self.db.as_ref(),
            container.deployment_id,
            &container_id,
        )
        .await
        {
            Ok(Some(workload)) => Ok(TeardownPath::Plane(workload)),
            Ok(None) => self
                .teardown_deployer_for_node(container.node_id)
                .await
                .map(TeardownPath::Deployer),
            Err(error) => Err(WorkflowExecutionError::ReplicaRetire(error)),
        };
        let teardown = match resolved {
            Ok(teardown) => teardown,
            Err(error) if rotate_retry => {
                if let Err(rotation_error) = self
                    .rotate_retained_cleanup_retry(&container, &container_id, None)
                    .await
                {
                    return Err(WorkflowExecutionError::JobCreationFailed(format!(
                        "{error}; additionally failed to rotate retained container {container_id} for a later cleanup attempt: {rotation_error}"
                    )));
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };

        // Mark deleted before stopping to prevent the independent health poll
        // from reporting an intentional shutdown as a crash.
        // PostgreSQL stores this column at microsecond precision. Normalize the
        // claim before writing it so the failure CAS below compares exactly.
        let now = chrono::Utc::now();
        let claim_at = now
            .with_nanosecond((now.nanosecond() / 1_000) * 1_000)
            .ok_or_else(|| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Failed to create cleanup claim timestamp for container {container_id}"
                ))
            })?;
        let claim = deployment_containers::Entity::update_many()
            .col_expr(
                deployment_containers::Column::DeletedAt,
                Expr::value(Some(claim_at)),
            )
            .col_expr(
                deployment_containers::Column::Status,
                Expr::value(Some("deleted".to_string())),
            )
            .filter(cleanup_snapshot_condition(&container))
            .exec(self.db.as_ref())
            .await?;
        if claim.rows_affected == 0 {
            debug!(
                container_id = %container_id,
                container_row_id = container.id,
                "Skipped cleanup because another task already claimed the container"
            );
            return Ok(None);
        }

        let removal: Result<(), String> = match teardown {
            TeardownPath::Plane(workload) => {
                crate::services::plane_deployer::retire_replica_workload(
                    self.workload_plane.get().map(Arc::as_ref),
                    container.deployment_id,
                    &container_id,
                    workload,
                )
                .await
                .map_err(|error| error.to_string())
            }
            TeardownPath::Deployer(deployer) => {
                if let Err(error) = deployer.stop_container(&container_id).await {
                    warn!(
                        "Failed to stop container {} on node {:?}: {}",
                        container_id, container.node_id, error
                    );
                }
                match deployer.remove_container(&container_id).await {
                    Ok(()) | Err(temps_deployer::DeployerError::ContainerNotFound(_)) => Ok(()),
                    Err(error) => Err(error.to_string()),
                }
            }
        };

        match removal {
            Ok(()) => {}
            Err(error) => {
                // Never hide a still-live container from later cleanup attempts.
                // Retained failures receive a timestamped, lexically-last state,
                // which moves them behind unattempted rows and rotates retries
                // fairly inside the bounded cleanup window.
                if rotate_retry {
                    self.rotate_retained_cleanup_retry(&container, &container_id, Some(claim_at))
                        .await?;
                } else {
                    let claimed = deployment_containers::Model {
                        deleted_at: Some(claim_at),
                        status: Some("deleted".to_string()),
                        ..container
                    };
                    deployment_containers::Entity::update_many()
                        .col_expr(
                            deployment_containers::Column::DeletedAt,
                            Expr::value(None::<chrono::DateTime<chrono::Utc>>),
                        )
                        .col_expr(
                            deployment_containers::Column::Status,
                            Expr::value(original_status),
                        )
                        .filter(cleanup_snapshot_condition(&claimed))
                        .exec(self.db.as_ref())
                        .await?;
                }
                return Err(WorkflowExecutionError::JobCreationFailed(format!(
                    "Failed to remove container {container_id} from node {:?}: {error}",
                    node_id
                )));
            }
        }

        Ok(Some(container_id))
    }

    async fn rotate_retained_cleanup_retry(
        &self,
        container: &temps_entities::deployment_containers::Model,
        container_id: &str,
        claim_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), WorkflowExecutionError> {
        use temps_entities::deployment_containers;

        let expected = match claim_at {
            Some(claim_at) => deployment_containers::Model {
                deleted_at: Some(claim_at),
                status: Some("deleted".to_string()),
                ..container.clone()
            },
            None => container.clone(),
        };
        deployment_containers::Entity::update_many()
            .col_expr(
                deployment_containers::Column::DeletedAt,
                Expr::value(None::<chrono::DateTime<chrono::Utc>>),
            )
            .col_expr(
                deployment_containers::Column::Status,
                Expr::value(Some(retained_cleanup_retry_status())),
            )
            .filter(cleanup_snapshot_condition(&expected))
            .exec(self.db.as_ref())
            .await
            .map_err(|error| {
                WorkflowExecutionError::JobCreationFailed(format!(
                    "Failed to persist a later cleanup attempt for retained container {container_id}: {error}"
                ))
            })?;
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
        // environments. Failed retained candidates are handled by the direct,
        // uncapped active-row query below. "failed"/"stopped" remain excluded
        // here to avoid repeatedly scanning historical deployments.
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
                match self.teardown_registered_container(container, false).await {
                    Ok(Some(container_id)) => {
                        info!("Removed container {}", container_id);
                        if first_stopped_container_id.is_none() {
                            first_stopped_container_id = Some(container_id);
                        }
                        total_containers_cleaned += 1;
                    }
                    Ok(None) => {}
                    Err(error) => warn!("Failed to teardown previous container: {error}"),
                }
            }
        }

        // Failed candidates include legacy rows without a retained status and
        // rows left by unsuccessful failure cleanup. Ownership and deployment
        // state determine eligibility, not the container status. Keep failure
        // history intact while removing these containers after a newer success.
        // Prioritize unattempted rows, then rotate timestamped retries fairly.
        //
        // Terminal "stopped"/"cancelled" deployments are swept here too: the
        // capped scan above excludes them, so a container row a teardown
        // failed to retire would otherwise stay live forever — an orphaned
        // container on the host that nothing routes to or ever removes. This
        // query only matches live rows, so it stays bounded by leaked
        // containers rather than by deployment history.
        const MAX_RETAINED_CLEANUPS_PER_DEPLOYMENT: u64 = 20;
        const RETAINED_CLEANUP_CONCURRENCY: usize = 4;
        let retained_failed = deployment_containers::Entity::find()
            .find_also_related(deployments::Entity)
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.is_in(["failed", "stopped", "cancelled"]))
            .filter(
                Condition::any()
                    .add(deployments::Column::CreatedAt.lt(current_deployment.created_at))
                    .add(
                        Condition::all()
                            .add(deployments::Column::CreatedAt.eq(current_deployment.created_at))
                            .add(deployments::Column::Id.lt(current_deployment.id)),
                    ),
            )
            .order_by_asc(sea_orm::sea_query::SimpleExpr::Case(Box::new(
                sea_orm::sea_query::Expr::case(
                    sea_orm::sea_query::Expr::col((
                        deployment_containers::Entity,
                        deployment_containers::Column::Status,
                    ))
                    .like(format!("{RETAINED_CLEANUP_RETRY_PREFIX}%")),
                    1,
                )
                .finally(0),
            )))
            .order_by_asc(deployment_containers::Column::Status)
            .order_by_asc(deployment_containers::Column::Id)
            .limit(MAX_RETAINED_CLEANUPS_PER_DEPLOYMENT)
            .all(self.db.as_ref())
            .await?;

        if retained_failed.len() == MAX_RETAINED_CLEANUPS_PER_DEPLOYMENT as usize {
            warn!(
                "Retained-container cleanup reached the per-deployment limit of {}; unattempted candidates were prioritized and remaining rows will be retried after the next successful deployment",
                MAX_RETAINED_CLEANUPS_PER_DEPLOYMENT
            );
        }

        let cleanup_results = futures::stream::iter(
            retained_failed
                .into_iter()
                .map(|(container, _)| self.teardown_registered_container(container, true)),
        )
        .buffer_unordered(RETAINED_CLEANUP_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        for result in cleanup_results {
            match result {
                Ok(Some(container_id)) => {
                    info!("Removed retained failed container {}", container_id);
                    if first_stopped_container_id.is_none() {
                        first_stopped_container_id = Some(container_id);
                    }
                    total_containers_cleaned += 1;
                }
                Ok(None) => {}
                Err(error) => warn!("Failed to teardown retained failed container: {error}"),
            }
        }

        // Containers a drain or failover took out of routing on a worker
        // without confirming they were removed (`retired`, `failed_over`)
        // are no longer live rows, so neither query above sees them. Once
        // their deployment is superseded nothing else removes them, and one
        // on a node that came back keeps running unrouted (#1384). Each is
        // removed only after Docker's labels confirm it belongs to this
        // project, like node removal does, and recorded as removed; one that
        // cannot be confirmed yet (node offline) is left for the next
        // successful deployment. Bounded per deployment.
        total_containers_cleaned += self
            .remove_retired_worker_containers(project_id, environment_id, current_deployment)
            .await;

        if total_containers_cleaned > 0 {
            info!(
                "Cleaned up {} containers from previous deployments",
                total_containers_cleaned
            );
        }

        Ok(first_stopped_container_id)
    }

    /// See the end of [`Self::teardown_previous_deployment`]. Returns how many
    /// containers were confirmed gone. Failures are logged, never returned:
    /// the new deployment is already live.
    async fn remove_retired_worker_containers(
        &self,
        project_id: i32,
        environment_id: i32,
        current_deployment: &deployments::Model,
    ) -> usize {
        use crate::services::node_service::{
            remove_retired_container, CONTAINER_STATUS_FAILED_OVER,
            CONTAINER_STATUS_FAILED_OVER_RECOVERING, CONTAINER_STATUS_RETIRED,
        };
        use sea_orm::{JoinType, RelationTrait};
        use temps_entities::{deployment_containers, nodes};

        const MAX_RETIRED_WORKER_CLEANUPS: u64 = 20;
        // Least recently attempted first, and a failed attempt moves to the
        // back (below), so containers on one unreachable worker never keep
        // the ones on other workers from being attempted. Offline nodes are
        // skipped: nothing on them can be removed until they return.
        let retired = match deployment_containers::Entity::find()
            .find_also_related(deployments::Entity)
            .join(
                JoinType::InnerJoin,
                deployment_containers::Relation::Node.def(),
            )
            .filter(nodes::Column::Status.ne("offline"))
            .filter(deployment_containers::Column::DeletedAt.is_not_null())
            .filter(deployment_containers::Column::NodeId.is_not_null())
            .filter(deployment_containers::Column::Status.is_in([
                CONTAINER_STATUS_RETIRED,
                CONTAINER_STATUS_FAILED_OVER,
                CONTAINER_STATUS_FAILED_OVER_RECOVERING,
            ]))
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
            .order_by_asc(deployment_containers::Column::DeletedAt)
            .order_by_asc(deployment_containers::Column::Id)
            .limit(MAX_RETIRED_WORKER_CLEANUPS)
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    project_id,
                    environment_id,
                    "Could not look up retired worker containers of superseded deployments: {error}"
                );
                return 0;
            }
        };

        let mut removed = 0;
        for (container, _) in retired {
            let outcome = match self.teardown_deployer_for_node(container.node_id).await {
                Ok(deployer) => remove_retired_container(
                    self.db.as_ref(),
                    deployer.as_ref(),
                    &container,
                    project_id,
                )
                .await
                .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            match outcome {
                Ok(()) => {
                    removed += 1;
                    info!(
                        project_id,
                        deployment_id = container.deployment_id,
                        node_id = ?container.node_id,
                        container_id = %container.container_id,
                        "Removed retired worker container of a superseded deployment"
                    );
                }
                Err(error) => {
                    warn!(
                        project_id,
                        deployment_id = container.deployment_id,
                        node_id = ?container.node_id,
                        container_id = %container.container_id,
                        "Retired worker container left for a later cleanup: {error}"
                    );
                    // Move it behind the rows not yet attempted: `deleted_at`
                    // of a row out of routing is when it was last handled.
                    // Only while the row is as read, so no other outcome is
                    // overwritten.
                    let read_status = match &container.status {
                        Some(status) => deployment_containers::Column::Status.eq(status.clone()),
                        None => deployment_containers::Column::Status.is_null(),
                    };
                    if let Err(error) = deployment_containers::Entity::update_many()
                        .col_expr(
                            deployment_containers::Column::DeletedAt,
                            Expr::value(Some(chrono::Utc::now())),
                        )
                        .filter(deployment_containers::Column::Id.eq(container.id))
                        .filter(read_status)
                        .exec(self.db.as_ref())
                        .await
                    {
                        warn!(
                            container_id = %container.container_id,
                            "Could not reschedule the cleanup of a retired worker container: {error}"
                        );
                    }
                }
            }
        }
        removed
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

    #[error("Deployment {deployment_id} has no node to run on: {source}")]
    DedicatedNodesNotPinned {
        deployment_id: i32,
        #[source]
        source: crate::services::node_service::NodeError,
    },

    /// A mixed-architecture deployment has a target architecture that no
    /// reachable worker of that architecture can build. Builds are never
    /// emulated, so the deployment stops before anything is built.
    #[error(
        "Deployment {deployment_id} targets {platform} nodes, but no {platform} worker can \
         build its image: {reason}. Each architecture is built natively on a worker of that \
         architecture, never emulated: bring a {platform} worker online (or label one \
         `temps.sh/role=builder`), or restrict this environment's target nodes/labels to the \
         architectures that have a reachable worker"
    )]
    NoBuilderForArchitecture {
        deployment_id: i32,
        platform: String,
        reason: String,
    },

    /// Finding or retiring a replica the workload plane started.
    #[error(transparent)]
    ReplicaRetire(#[from] crate::services::plane_deployer::ReplicaRetireError),
    /// Where a Compose stack lives could not be decided, or its home cannot
    /// take this deployment (see `services::compose_placement`).
    #[error("Deployment {deployment_id}: {source}")]
    ComposePlacement {
        deployment_id: i32,
        #[source]
        source: crate::services::compose_placement::ComposePlacementError,
    },

    /// A Compose stack whose home is a worker links Temps-managed services,
    /// which its containers there cannot reach.
    #[error(
        "Deployment {deployment_id} would run this Docker Compose stack on {host}, but the \
         project is linked to Temps-managed services ({services}) whose connection details \
         only resolve on the control plane. Compose stacks on worker nodes cannot reach linked \
         services yet: unlink them and run the database inside the stack, or move the stack to \
         the control plane in the environment's Node scheduling settings"
    )]
    ComposeLinkedServicesOnWorker {
        deployment_id: i32,
        host: String,
        services: String,
    },
}

impl WorkflowExecutionError {
    /// Whether this failure is an outcome of the deployment itself (the
    /// user's build or app failed, the run was cancelled, a resource it
    /// targeted was deleted mid-flight) rather than a fault in Temps.
    ///
    /// Outcomes are recorded on the deployment where the user acts on them,
    /// so callers log them at WARN and keep ERROR for database failures and
    /// a workflow Temps itself could not assemble.
    pub fn is_workload_outcome(&self) -> bool {
        match self {
            Self::WorkflowFailed(error) => error.is_workload_outcome(),
            Self::DeploymentNotFound(_)
            | Self::ProjectNotFound(_)
            | Self::EnvironmentNotFound(_)
            | Self::Validation(_)
            // The operator's placement configuration, recorded on the
            // deployment as its failure reason.
            | Self::DedicatedNodesNotPinned { .. }
            // The cluster's state: a target architecture has no reachable
            // worker. Recorded on the deployment with the fix.
            | Self::NoBuilderForArchitecture { .. }
            // A Compose stack whose home links services it cannot reach.
            | Self::ComposeLinkedServicesOnWorker { .. } => true,
            // The stack's home is offline, nothing can take its first
            // deployment, or a concurrent deployment pinned it elsewhere.
            // Recorded on the deployment with the fix.
            Self::ComposePlacement { source, .. } => !matches!(
                source,
                crate::services::compose_placement::ComposePlacementError::Database { .. }
            ),
            Self::DatabaseError(_)
            | Self::NoJobsFound(_)
            | Self::MissingJobConfig(_)
            | Self::InvalidJobConfig(_)
            | Self::UnsupportedJobType(_)
            | Self::JobCreationFailed(_)
            | Self::ReplicaRetire(_) => false,
        }
    }
}

impl From<anyhow::Error> for WorkflowExecutionError {
    fn from(e: anyhow::Error) -> Self {
        WorkflowExecutionError::JobCreationFailed(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn user_caused_failures_are_workload_outcomes() {
        use super::WorkflowExecutionError;
        use temps_core::WorkflowError;

        let outcomes = [
            WorkflowExecutionError::WorkflowFailed(WorkflowError::JobValidationFailed(
                "Invalid build preset".into(),
            )),
            WorkflowExecutionError::WorkflowFailed(WorkflowError::WorkflowCancelled),
            WorkflowExecutionError::DeploymentNotFound(3),
            WorkflowExecutionError::Validation("bad port".into()),
        ];
        for error in outcomes {
            assert!(error.is_workload_outcome(), "{error}");
        }

        let faults = [
            WorkflowExecutionError::WorkflowFailed(WorkflowError::JobExecutionFailed(
                "Required job 'build' failed: exit code 1".into(),
            )),
            WorkflowExecutionError::DatabaseError(sea_orm::DbErr::Custom("down".into())),
            WorkflowExecutionError::WorkflowFailed(WorkflowError::Other("tracker".into())),
            WorkflowExecutionError::NoJobsFound(3),
            WorkflowExecutionError::UnsupportedJobType("X".into()),
        ];
        for error in faults {
            assert!(!error.is_workload_outcome(), "{error}");
        }
    }

    /// One target architecture keeps the single worker build every
    /// homogeneous cluster does today; several get one native build per
    /// architecture; none is a configuration error naming the deployment.
    #[test]
    fn worker_build_scope_groups_target_platforms() {
        use super::{validate_worker_build_scope, WorkerBuildScope};
        assert_eq!(
            validate_worker_build_scope(42, &["linux/amd64".into()]).unwrap(),
            WorkerBuildScope::SingleArchitecture
        );
        assert_eq!(
            validate_worker_build_scope(42, &["linux/amd64".into(), "linux/arm64".into()]).unwrap(),
            WorkerBuildScope::PerArchitecture
        );
        let error = validate_worker_build_scope(42, &[]).unwrap_err();
        assert!(error.to_string().contains("Deployment 42"), "{error}");
        assert!(error.to_string().contains("target nodes/labels"), "{error}");
    }

    /// The refusal names the architecture and both fixes, and is recorded as
    /// an outcome of the deployment (cluster state), not a Temps fault.
    #[test]
    fn no_builder_for_architecture_names_the_platform_and_the_fix() {
        let error = WorkflowExecutionError::NoBuilderForArchitecture {
            deployment_id: 9,
            platform: "linux/amd64".to_string(),
            reason: "worker 'w-amd' (id=3) did not answer on its agent connection".to_string(),
        };
        let message = error.to_string();
        assert!(
            message.contains("Deployment 9 targets linux/amd64 nodes"),
            "{message}"
        );
        assert!(message.contains("w-amd"), "{message}");
        assert!(message.contains("never emulated"), "{message}");
        assert!(
            message.contains("bring a linux/amd64 worker online"),
            "{message}"
        );
        assert!(message.contains("target nodes/labels"), "{message}");
        assert!(error.is_workload_outcome());
        let classified =
            crate::services::failure_classifier::classify_failure_reason(Some(&message));
        assert_eq!(
            classified.code,
            crate::services::failure_classifier::DeploymentFailureCode::ImagePlatformMismatch
        );
    }

    /// Build location `node` moves a build only when a worker can produce the
    /// same image the control plane would; anything else stays local with a
    /// reason the build log can show.
    #[test]
    fn offload_build_candidate_builds_the_control_plane_platform_on_a_worker() {
        use super::{offload_build_candidate, OffloadBuildCandidate};

        assert_eq!(
            offload_build_candidate(false, false, &[], Some("linux/amd64"), true),
            Ok(OffloadBuildCandidate {
                platform: Some("linux/amd64".to_string())
            })
        );
        // A single requested platform is still one image, built for that
        // platform rather than the control plane's.
        assert_eq!(
            offload_build_candidate(
                false,
                false,
                &["linux/arm64".into()],
                Some("linux/amd64"),
                true
            ),
            Ok(OffloadBuildCandidate {
                platform: Some("linux/arm64".to_string())
            })
        );
        assert_eq!(
            offload_build_candidate(false, false, &[], Some(" linux/amd64 "), true),
            Ok(OffloadBuildCandidate {
                platform: Some("linux/amd64".to_string())
            })
        );
    }

    /// A static site's output is read from the build node and never runs,
    /// so any worker may build it -- even when the control plane's own
    /// platform is unknown or the cluster spans several architectures.
    #[test]
    fn offload_build_candidate_lets_any_worker_build_a_static_site() {
        use super::{offload_build_candidate, OffloadBuildCandidate};

        for platform in [None, Some("linux/amd64")] {
            assert_eq!(
                offload_build_candidate(true, false, &[], platform, true),
                Ok(OffloadBuildCandidate { platform: None })
            );
        }
        assert_eq!(
            offload_build_candidate(
                true,
                false,
                &["linux/amd64".into(), "linux/arm64".into()],
                Some("linux/amd64"),
                true
            ),
            Ok(OffloadBuildCandidate { platform: None })
        );
    }

    #[test]
    fn offload_build_candidate_explains_every_reason_to_stay_local() {
        use super::offload_build_candidate;

        let multi_arch = offload_build_candidate(
            false,
            false,
            &["linux/amd64".into(), "linux/arm64".into()],
            Some("linux/amd64"),
            true,
        )
        .unwrap_err();
        assert!(multi_arch.contains("2 images"), "{multi_arch}");
        assert!(
            multi_arch.contains("linux/amd64, linux/arm64"),
            "{multi_arch}"
        );

        for static_output_only in [false, true] {
            let no_scheduler =
                offload_build_candidate(static_output_only, false, &[], Some("linux/amd64"), false)
                    .unwrap_err();
            assert!(no_scheduler.contains("node scheduling"), "{no_scheduler}");
        }

        for platform in [None, Some(""), Some("not-a-platform")] {
            let unknown = offload_build_candidate(false, false, &[], platform, true).unwrap_err();
            assert!(unknown.contains("platform is not known"), "{unknown}");
        }
    }

    /// Only a secret variable the framework inlines keeps an offloaded build
    /// on the control plane, named (never its value). The platform's public
    /// variables -- `VITE_SENTRY_DSN`, `VITE_SENTRY_TUNNEL` and the other
    /// frameworks' equivalents -- and the project's non-secret variables are
    /// sent to the worker, so they never keep a build here (issue #1344).
    #[test]
    fn inlined_build_variables_keep_offloaded_builds_local() {
        use super::{inlined_variables_allow_offload, withheld_inlined_variables};
        use std::collections::BTreeSet;

        let build_args: Vec<String> = [
            "HOST",
            "SENTRY_DSN",
            "VITE_SENTRY_DSN",
            "VITE_SENTRY_TUNNEL",
            "NEXT_PUBLIC_SENTRY_DSN",
            "PUBLIC_SENTRY_TUNNEL",
            "VITE_API_URL",
            "VITE_PRIVATE_KEY",
            "TEMPS_API_TOKEN",
            "BUILDKIT_CACHE_MOUNT_NS",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let sent: BTreeSet<String> = build_args
            .iter()
            .filter(|name| !matches!(name.as_str(), "VITE_PRIVATE_KEY" | "TEMPS_API_TOKEN"))
            .cloned()
            .collect();
        assert_eq!(
            withheld_inlined_variables(&build_args, &sent),
            vec!["VITE_PRIVATE_KEY".to_string()]
        );
        let all_public: BTreeSet<String> = sent
            .iter()
            .cloned()
            .chain(["VITE_PRIVATE_KEY".to_string()])
            .collect();
        assert_eq!(
            inlined_variables_allow_offload(&withheld_inlined_variables(&build_args, &all_public)),
            Ok(())
        );

        assert_eq!(inlined_variables_allow_offload(&[]), Ok(()));
        let one = inlined_variables_allow_offload(&["VITE_API_URL".to_string()]).unwrap_err();
        assert_eq!(one.variables, vec!["VITE_API_URL".to_string()]);
        assert!(one.to_string().contains("VITE_API_URL is inlined"), "{one}");
        assert!(one.to_string().contains("it is secret"), "{one}");
        let two = inlined_variables_allow_offload(&[
            "NEXT_PUBLIC_A".to_string(),
            "NEXT_PUBLIC_B".to_string(),
        ])
        .unwrap_err();
        assert!(
            two.to_string()
                .contains("NEXT_PUBLIC_A, NEXT_PUBLIC_B are inlined"),
            "{two}"
        );
    }

    /// Private npm credentials are only ever written into a build context on
    /// the control plane, so such a build -- static or not -- stays there
    /// with a reason instead of failing on the worker.
    #[test]
    fn offload_build_candidate_keeps_npm_credential_builds_local() {
        use super::offload_build_candidate;

        for static_output_only in [false, true] {
            let reason =
                offload_build_candidate(static_output_only, true, &[], Some("linux/amd64"), true)
                    .unwrap_err();
            assert!(reason.contains("NPM_TOKEN"), "{reason}");
            assert!(reason.contains("NPM_RC"), "{reason}");
        }
    }
    use super::*;
    use crate::services::failure_classifier::{
        legacy_failure_reason, DeploymentFailureCode, DeploymentFailureStage,
    };
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
    fn retained_cleanup_retries_sort_after_unattempted_rows_and_rotate() {
        let first = retained_cleanup_retry_status_at(
            chrono::DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
        );
        let second = retained_cleanup_retry_status_at(
            chrono::DateTime::parse_from_rfc3339("2026-09-01T10:01:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
        );

        for unattempted in [
            "retained:created",
            "retained:exited",
            "retained:failed-readiness",
            "retained:running",
            "retained:unhealthy",
        ] {
            assert!(unattempted < first.as_str());
        }
        assert!(first < second, "older retries must be selected first");
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
                // The build job's out-of-memory explanation, whose only
                // exit-code text is the ordinary 1 the build tool returned.
                "Failed to build image: The build step most likely ran out of memory: the \
                 kernel's OOM killer terminated 1 process on this host while the step ran; no \
                 other build was running; the step exited with code 1 after one of its \
                 processes was killed; host RAM 3902 MB",
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
                DeploymentFailureCode::HealthCheckTimeout,
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
        assert_eq!(
            event.properties["classifier_version"],
            FAILURE_CLASSIFIER_VERSION
        );
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

        let service_failure = deploy_failed_telemetry_event(
            Some("failed to pull image: manifest unknown"),
            Some("compose".to_string()),
            Some("docker-compose".to_string()),
            Some("keycloak".to_string()),
        );
        assert_eq!(service_failure.properties["template_source"], "bundled");
        assert_eq!(service_failure.properties["template_slug"], "keycloak");
        assert_eq!(service_failure.properties["failure_stage"], "image");
        assert_eq!(
            service_failure.properties["failure_code"],
            "base_image_pull"
        );
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
    fn symbolic_head_and_empty_commits_are_not_checked_out_as_commits() {
        assert!(!is_concrete_commit_ref(""));
        assert!(!is_concrete_commit_ref("  "));
        assert!(!is_concrete_commit_ref("HEAD"));
        assert!(!is_concrete_commit_ref("head"));
        assert!(is_concrete_commit_ref(
            "7f05d217867b2af52b0a28c6d1c91df97e1b5b39"
        ));
        assert!(is_concrete_commit_ref("7f05d21"));
    }

    #[test]
    fn go_context_canceled_is_a_failure_not_a_cancellation() {
        let docker_abort = WorkflowError::JobExecutionFailed(
            "Required job 'deploy_container' failed: Failed to start container: context canceled"
                .to_string(),
        );
        assert!(!is_workflow_cancellation(
            &docker_abort,
            &docker_abort.to_string()
        ));

        let typed = WorkflowError::WorkflowCancelled;
        assert!(is_workflow_cancellation(&typed, &typed.to_string()));

        let compose = WorkflowError::JobExecutionFailed(
            "Compose deployment was cancelled after teardown".to_string(),
        );
        assert!(!is_workflow_cancellation(&compose, &compose.to_string()));
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
                config: None,
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
                skipped_count: 0,
                skipped_paths: Vec::new(),
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
        async fn exec_in_container(
            &self,
            container_id: &str,
            _command: Vec<String>,
            _timeout: std::time::Duration,
        ) -> Result<temps_deployer::ExecResult, temps_deployer::DeployerError> {
            Err(temps_deployer::DeployerError::ExecFailed {
                container_id: container_id.to_string(),
                reason: "exec is not scripted by this test deployer".to_string(),
            })
        }

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
                docker_socket_mounted: false,
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
                block_read_bytes: None,
                block_write_bytes: None,
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
        let docker = Arc::new(DockerHandle::available(Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        )));
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
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
            docker,
        );

        // Service should be created successfully - compilation itself is the test

        Ok(())
    }

    /// Build the service under test with an explicit Docker handle, so the
    /// Dockerless (`--profile control-plane`) paths can be exercised without a
    /// daemon on the machine running the tests.
    async fn service_with_docker_handle(
        db: Arc<DbConnection>,
        docker_handle: Arc<DockerHandle>,
    ) -> Result<WorkflowExecutionService, Box<dyn std::error::Error>> {
        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);

        Ok(WorkflowExecutionService::new(
            db,
            Arc::new(queue) as Arc<dyn temps_core::JobQueue>,
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
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
            docker_handle,
        ))
    }

    fn disabled_docker_handle() -> Arc<DockerHandle> {
        Arc::new(DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            temps_core::CONTROL_PLANE_DOCKER_REASON,
        ))
    }

    /// Building a Compose job must not need a daemon.
    ///
    /// `DeployComposeJob` already refuses Dockerless hosts from inside its own
    /// `execute`, where the workflow executor records the refusal against the
    /// job row. Resolving the daemon at *construction* time instead would fail
    /// the deployment before the executor exists — leaving every planned
    /// `deployment_jobs` row `pending` forever with no reason anywhere.
    #[tokio::test]
    async fn a_compose_job_is_built_without_a_local_docker_daemon(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, deployment) = create_test_data(&db).await?;

        let compose_job = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("deploy_compose".to_string()),
            job_type: Set("DeployComposeJob".to_string()),
            name: Set("Deploy Compose".to_string()),
            status: Set(JobStatus::Pending),
            log_id: Set(format!("deployment-{}-job-deploy_compose", deployment.id)),
            job_config: Set(Some(serde_json::json!({ "compose_path": "compose.yaml" }))),
            execution_order: Set(Some(0)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let service = service_with_docker_handle(db.clone(), disabled_docker_handle()).await?;

        service
            .create_job_from_record(&project, &environment, &deployment, &compose_job)
            .await
            .expect("compose job construction must not depend on a local daemon");

        Ok(())
    }

    /// Whatever stops a workflow from being assembled, the planned job rows
    /// must not be left saying "pending" on a deployment that is over. A
    /// self-hosted operator staring at a timeline that never resolves has no
    /// way to tell a stuck deployment from a slow one.
    #[tokio::test]
    async fn planned_jobs_are_closed_out_when_the_workflow_cannot_be_built(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, _environment, deployment) = create_test_data(&db).await?;

        // `BuildStaticJob` is a planned-but-unimplemented type: constructing it
        // always fails, which is exactly the shape of a setup failure.
        let doomed = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("build_static".to_string()),
            job_type: Set("BuildStaticJob".to_string()),
            name: Set("Build Static".to_string()),
            status: Set(JobStatus::Pending),
            log_id: Set(format!("deployment-{}-job-build_static", deployment.id)),
            job_config: Set(Some(serde_json::json!({}))),
            execution_order: Set(Some(0)),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let service = service_with_docker_handle(db.clone(), disabled_docker_handle()).await?;

        let error = service
            .execute_deployment_workflow(deployment.id)
            .await
            .expect_err("an unbuildable workflow must fail the deployment");

        let row = deployment_jobs::Entity::find_by_id(doomed.id)
            .one(db.as_ref())
            .await?
            .expect("the planned job row still exists");

        assert_ne!(
            row.status,
            JobStatus::Pending,
            "a planned job must never be left pending after the deployment is over",
        );
        let reason = row
            .error_message
            .clone()
            .expect("the closed-out row must say why it never ran");
        assert!(
            reason.contains(&deployment.id.to_string()),
            "the reason must identify the deployment: {reason}",
        );
        assert!(
            reason.contains(&error.to_string()),
            "the reason must carry the underlying setup failure: {reason}",
        );

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
        let docker = Arc::new(DockerHandle::available(Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        )));
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
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
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
        let docker = Arc::new(DockerHandle::available(Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        )));
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
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
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
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
            Arc::new(DockerHandle::available(Arc::new(
                bollard::Docker::connect_with_local_defaults()?,
            ))),
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
        fail_remove_container: Option<&'static str>,
        remove_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl ContainerDeployer for AssertDeletedBeforeStopDeployer {
        async fn exec_in_container(
            &self,
            container_id: &str,
            _command: Vec<String>,
            _timeout: std::time::Duration,
        ) -> Result<temps_deployer::ExecResult, temps_deployer::DeployerError> {
            Err(temps_deployer::DeployerError::ExecFailed {
                container_id: container_id.to_string(),
                reason: "exec is not scripted by this test deployer".to_string(),
            })
        }

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
            container_id: &str,
        ) -> Result<(), temps_deployer::DeployerError> {
            self.remove_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_remove_container == Some(container_id) {
                return Err(temps_deployer::DeployerError::Other(format!(
                    "deterministic removal failure for {container_id}"
                )));
            }
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

    /// Greptile on #1395: cleanup of retired worker containers must not be
    /// starved by one unreachable worker. A failed attempt moves the row to
    /// the back (its `deleted_at` is re-stamped), and containers on offline
    /// nodes are not attempted at all.
    #[tokio::test]
    async fn test_retired_worker_cleanup_rotates_failures_and_skips_offline_nodes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::services::node_service::{
            CONTAINER_STATUS_FAILED_OVER, CONTAINER_STATUS_RETIRED,
        };
        use temps_entities::{deployment_containers, nodes};

        if !docker_available().await {
            eprintln!("Skipping retired worker cleanup test: Docker unavailable");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, environment, current_deployment) = create_test_data(&db).await?;

        let previous_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("superseded-deployment".to_string()),
            state: Set("stopped".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now() - chrono::Duration::minutes(30)),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let node = |name: &str, status: &str| nodes::ActiveModel {
            name: Set(name.to_string()),
            token_hash: Set("unused".to_string()),
            // No agent token: the node's deployer cannot be built, so every
            // attempt on it fails like an unreachable worker.
            token_encrypted: Set(None),
            address: Set("https://127.0.0.1:39999".to_string()),
            private_address: Set("127.0.0.1".to_string()),
            role: Set("worker".to_string()),
            status: Set(status.to_string()),
            labels: Set(serde_json::json!({})),
            capacity: Set(serde_json::json!({})),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        };
        let unreachable = node("unreachable-worker", "active")
            .insert(db.as_ref())
            .await?;
        let offline = node("offline-worker", "offline")
            .insert(db.as_ref())
            .await?;

        let handled_at = Utc::now() - chrono::Duration::hours(2);
        let retired =
            |container_id: &str, node_id: i32, status: &str| deployment_containers::ActiveModel {
                deployment_id: Set(previous_deployment.id),
                container_id: Set(container_id.to_string()),
                container_name: Set(container_id.to_string()),
                container_port: Set(Some(3000)),
                status: Set(Some(status.to_string())),
                node_id: Set(Some(node_id)),
                deleted_at: Set(Some(handled_at)),
                deployed_at: Set(Utc::now()),
                ..Default::default()
            };
        let on_unreachable = retired(
            "retired-unreachable",
            unreachable.id,
            CONTAINER_STATUS_RETIRED,
        )
        .insert(db.as_ref())
        .await?;
        let on_offline = retired(
            "failed-over-offline",
            offline.id,
            CONTAINER_STATUS_FAILED_OVER,
        )
        .insert(db.as_ref())
        .await?;

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let config_service = create_mock_config_service(db.clone());
        let service = WorkflowExecutionService::new(
            db.clone(),
            Arc::new(queue) as Arc<dyn temps_core::JobQueue>,
            Arc::new(MockGitProvider),
            Arc::new(MockImageBuilder { should_fail: false }),
            Arc::new(MockContainerDeployer { should_fail: false }),
            Arc::new(MockStaticDeployer),
            Arc::new(LogService::new(std::env::temp_dir())),
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>,
            Arc::new(crate::jobs::NoOpMetricAlertConfigService)
                as Arc<dyn crate::jobs::MetricAlertConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            config_service.clone(),
            Arc::new(ScreenshotService::new(config_service).await?),
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
            Arc::new(DockerHandle::available(Arc::new(
                bollard::Docker::connect_with_local_defaults()?,
            ))),
        );

        let removed = service
            .remove_retired_worker_containers(project.id, environment.id, &current_deployment)
            .await;
        assert_eq!(removed, 0, "nothing could be removed");

        let reread = |id: i32| {
            let db = db.clone();
            async move {
                deployment_containers::Entity::find_by_id(id)
                    .one(db.as_ref())
                    .await
                    .map(|row| row.expect("row exists"))
            }
        };
        let after_unreachable = reread(on_unreachable.id).await?;
        assert_eq!(
            after_unreachable.status.as_deref(),
            Some(CONTAINER_STATUS_RETIRED)
        );
        assert!(
            after_unreachable.deleted_at > Some(handled_at),
            "a failed attempt moves the row behind those not yet attempted"
        );
        let after_offline = reread(on_offline.id).await?;
        assert_eq!(
            after_offline.status.as_deref(),
            Some(CONTAINER_STATUS_FAILED_OVER)
        );
        assert_eq!(
            after_offline.deleted_at.map(|at| at.timestamp_micros()),
            Some(handled_at.timestamp_micros()),
            "containers on an offline node are not attempted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_teardown_previous_deployment_marks_deleted_before_stopping_container(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::{deployment_containers, nodes};

        if !docker_available().await {
            eprintln!("Skipping container teardown integration test: Docker unavailable");
            return Ok(());
        }
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
            container_port: Set(Some(3000)),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // A failed readiness check retains its candidate for logs. The next
        // successful deployment must retire it just like the previous healthy
        // container, otherwise repeated failures leak live Docker resources.
        let failed_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("failed-deployment".to_string()),
            state: Set("failed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now() - chrono::Duration::minutes(10)),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let retained_container = deployment_containers::ActiveModel {
            deployment_id: Set(failed_deployment.id),
            container_id: Set("failed-container-1".to_string()),
            container_name: Set("failed-container-1".to_string()),
            container_port: Set(Some(3000)),
            status: Set(Some("retained:failed-readiness".to_string())),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // A superseded deployment that was flipped to "stopped" while its
        // container removal failed left a live row behind. The capped scan
        // above never revisits "stopped" deployments, so this sweep must
        // retire it — otherwise the exited container lingers on the host
        // indefinitely.
        let stopped_deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("stopped-deployment".to_string()),
            state: Set("stopped".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now() - chrono::Duration::minutes(20)),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let leaked_container = deployment_containers::ActiveModel {
            deployment_id: Set(stopped_deployment.id),
            container_id: Set("leaked-stopped-container".to_string()),
            container_name: Set("leaked-stopped-container".to_string()),
            container_port: Set(Some(3000)),
            status: Set(Some("exited".to_string())),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // Older failed deployments and interrupted cleanup need not have the
        // retained prefix. They must still be selected by the failure sweep.
        let mut legacy_containers = Vec::new();
        for (index, status) in [None, Some("running"), Some("exited")]
            .into_iter()
            .enumerate()
        {
            let container = deployment_containers::ActiveModel {
                deployment_id: Set(failed_deployment.id),
                container_id: Set(format!("legacy-failed-{index}")),
                container_name: Set(format!("legacy-failed-{index}")),
                container_port: Set(Some(3000)),
                status: Set(status.map(str::to_string)),
                deployed_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
            legacy_containers.push(container);
        }

        // A remote retained row whose deployer cannot be constructed must move
        // into the retry rotation. Otherwise twenty unavailable workers can
        // permanently starve every newer diagnostic container.
        let unavailable_node = nodes::ActiveModel {
            name: Set("unavailable-worker".to_string()),
            token_hash: Set("unused".to_string()),
            token_encrypted: Set(None),
            address: Set("https://127.0.0.1:39999".to_string()),
            private_address: Set("127.0.0.1".to_string()),
            role: Set("worker".to_string()),
            status: Set("offline".to_string()),
            labels: Set(serde_json::json!({})),
            capacity: Set(serde_json::json!({})),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let retry_container = deployment_containers::ActiveModel {
            deployment_id: Set(failed_deployment.id),
            container_id: Set("failed-remote-container".to_string()),
            container_name: Set("failed-remote-container".to_string()),
            container_port: Set(Some(3000)),
            status: Set(Some("retained:failed-readiness".to_string())),
            node_id: Set(Some(unavailable_node.id)),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let legacy_retry_container = deployment_containers::ActiveModel {
            deployment_id: Set(failed_deployment.id),
            container_id: Set("legacy-remote-failure".to_string()),
            container_name: Set("legacy-remote-failure".to_string()),
            container_port: Set(Some(3000)),
            status: Set(None),
            node_id: Set(Some(unavailable_node.id)),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // Exercise the Docker removal error path for a legacy failed row. It
        // must become eligible for a later retry after owning the cleanup
        // claim, rather than remaining hidden behind the pre-stop marker.
        let legacy_remove_error = deployment_containers::ActiveModel {
            deployment_id: Set(failed_deployment.id),
            container_id: Set("legacy-remove-error".to_string()),
            container_name: Set("legacy-remove-error".to_string()),
            container_port: Set(Some(3000)),
            status: Set(None),
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
            container_port: Set(Some(3000)),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let (queue, _receiver) = temps_queue::BroadcastQueueService::create_broadcast_channel(100);
        let queue = Arc::new(queue) as Arc<dyn temps_core::JobQueue>;
        let git_provider = Arc::new(MockGitProvider);
        let image_builder = Arc::new(MockImageBuilder { should_fail: false });
        let remove_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let container_deployer = Arc::new(AssertDeletedBeforeStopDeployer {
            db: db.clone(),
            fail_remove_container: Some("legacy-remove-error"),
            remove_calls: remove_calls.clone(),
        });
        let static_deployer = Arc::new(MockStaticDeployer);
        let log_service = Arc::new(LogService::new(std::env::temp_dir()));
        let cron_service =
            Arc::new(crate::jobs::NoOpCronConfigService) as Arc<dyn crate::jobs::CronConfigService>;
        let config_service = create_mock_config_service(db.clone());
        let screenshot_service = Arc::new(ScreenshotService::new(config_service.clone()).await?);
        let docker = Arc::new(DockerHandle::available(Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .unwrap_or_else(|_| panic!("Failed to connect to Docker")),
        )));

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
            Arc::new(crate::jobs::DeploymentCaptureGuard::default()),
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

        let retained_refreshed = deployment_containers::Entity::find_by_id(retained_container.id)
            .one(db.as_ref())
            .await?
            .expect("retained container row still exists");
        assert!(retained_refreshed.deleted_at.is_some());
        assert_eq!(retained_refreshed.status.as_deref(), Some("deleted"));

        let leaked_refreshed = deployment_containers::Entity::find_by_id(leaked_container.id)
            .one(db.as_ref())
            .await?
            .expect("leaked container row still exists");
        assert!(
            leaked_refreshed.deleted_at.is_some(),
            "a live row under a stopped deployment must be removed by the sweep"
        );
        assert_eq!(leaked_refreshed.status.as_deref(), Some("deleted"));

        let retry_refreshed = deployment_containers::Entity::find_by_id(retry_container.id)
            .one(db.as_ref())
            .await?
            .expect("unavailable retained container row still exists");
        assert!(retry_refreshed.deleted_at.is_none());
        assert!(
            retry_refreshed
                .status
                .as_deref()
                .is_some_and(|status| status.starts_with(RETAINED_CLEANUP_RETRY_PREFIX)),
            "deployer lookup failures must rotate instead of starving cleanup: {:?}",
            retry_refreshed.status
        );

        let legacy_retry = deployment_containers::Entity::find_by_id(legacy_retry_container.id)
            .one(db.as_ref())
            .await?
            .expect("legacy retry row remains");
        assert!(legacy_retry.deleted_at.is_none());
        assert!(
            legacy_retry
                .status
                .as_deref()
                .is_some_and(|status| status.starts_with(RETAINED_CLEANUP_RETRY_PREFIX)),
            "legacy removal failures must remain eligible and rotate behind unattempted rows"
        );

        let legacy_remove_error = deployment_containers::Entity::find_by_id(legacy_remove_error.id)
            .one(db.as_ref())
            .await?
            .expect("legacy removal failure row remains");
        assert!(legacy_remove_error.deleted_at.is_none());
        assert!(
            legacy_remove_error
                .status
                .as_deref()
                .is_some_and(|status| status.starts_with(RETAINED_CLEANUP_RETRY_PREFIX)),
            "an actual legacy remove error must rotate into a later cleanup attempt"
        );

        // Model a stale task that selected the same retained row before this
        // task successfully removed it. Its later retry write must compare the
        // original snapshot and leave the winning deleted marker intact.
        let stale_snapshot = deployment_containers::ActiveModel {
            deployment_id: Set(failed_deployment.id),
            container_id: Set("stale-cleanup-race".to_string()),
            container_name: Set("stale-cleanup-race".to_string()),
            container_port: Set(Some(3000)),
            status: Set(Some("retained:failed-readiness".to_string())),
            deployed_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let first_result = service
            .teardown_registered_container(stale_snapshot.clone(), true)
            .await?;
        assert_eq!(first_result, Some(stale_snapshot.container_id.clone()));
        let calls_after_winner = remove_calls.load(std::sync::atomic::Ordering::SeqCst);
        let stale_result = service
            .teardown_registered_container(stale_snapshot.clone(), true)
            .await?;
        assert_eq!(stale_result, None);
        assert_eq!(
            remove_calls.load(std::sync::atomic::Ordering::SeqCst),
            calls_after_winner,
            "a stale snapshot that loses the claim must not call the container runtime"
        );

        let winning_row = deployment_containers::Entity::find_by_id(stale_snapshot.id)
            .one(db.as_ref())
            .await?
            .expect("winning cleanup row remains for history");
        let winning_claim = winning_row
            .deleted_at
            .expect("winning cleanup persisted its claim");
        let stale_claim = winning_claim - chrono::Duration::microseconds(1);
        service
            .rotate_retained_cleanup_retry(
                &stale_snapshot,
                &stale_snapshot.container_id,
                Some(stale_claim),
            )
            .await?;
        let race_winner = deployment_containers::Entity::find_by_id(stale_snapshot.id)
            .one(db.as_ref())
            .await?
            .expect("successfully removed row remains for history");
        assert!(
            race_winner.deleted_at.is_some(),
            "a stale retry must not resurrect a container removed by a competing cleanup"
        );
        assert_eq!(race_winner.status.as_deref(), Some("deleted"));

        for legacy in legacy_containers {
            let refreshed = deployment_containers::Entity::find_by_id(legacy.id)
                .one(db.as_ref())
                .await?
                .expect("legacy row remains for history");
            assert!(
                refreshed.deleted_at.is_some(),
                "legacy failed container {} was skipped",
                legacy.container_id
            );
        }
        let failed_after = deployments::Entity::find_by_id(failed_deployment.id)
            .one(db.as_ref())
            .await?
            .expect("failure history remains");
        assert_eq!(failed_after.state, "failed");

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
            upload_request_id: None,
            docker_socket_mounted: false,
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

    /// A worker labelled `temps.sh/role=dedicated` that answers the node
    /// scheduler's one `list_active` query.
    fn dedicated_scheduler(local_workloads_enabled: bool) -> Arc<crate::services::NodeScheduler> {
        let node = temps_entities::nodes::Model {
            architecture: Some("linux/amd64".to_string()),
            id: 7,
            name: "gpu-1".to_string(),
            token_hash: "hash_7".to_string(),
            token_encrypted: None,
            address: "https://10.0.0.7:3100".to_string(),
            private_address: "10.0.0.7".to_string(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({ "temps.sh/role": "dedicated", "gpu": "true" }),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            failover_at: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            deployments_without_resolver: None,
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node]])
            .into_connection();
        let node_service = Arc::new(crate::services::NodeService::new(Arc::new(db)));
        Arc::new(
            crate::services::NodeScheduler::new(node_service)
                .with_local_workloads_enabled(local_workloads_enabled),
        )
    }

    fn planned_job(deployment_id: i32, job_id: &str, job_type: &str) -> deployment_jobs::Model {
        deployment_jobs::Model {
            id: 0,
            deployment_id,
            job_id: job_id.to_string(),
            job_type: job_type.to_string(),
            name: job_id.to_string(),
            description: None,
            status: JobStatus::Pending,
            log_id: format!("deployment-{deployment_id}-job-{job_id}"),
            job_config: Some(serde_json::json!({})),
            outputs: None,
            dependencies: None,
            execution_order: None,
            started_at: None,
            finished_at: None,
            error_message: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn with_placement(
        target_nodes: Option<Vec<i32>>,
        target_labels: Option<serde_json::Value>,
    ) -> Option<temps_entities::deployment_config::DeploymentConfig> {
        Some(temps_entities::deployment_config::DeploymentConfig {
            target_nodes,
            target_labels,
            ..Default::default()
        })
    }

    #[test]
    fn placement_constraints_inherit_each_unset_field_from_the_project() {
        let project = with_placement(Some(vec![3]), Some(serde_json::json!({ "gpu": "true" })));
        assert_eq!(
            resolve_placement_constraints(with_placement(None, None).as_ref(), project.as_ref()),
            (Some(vec![3]), Some(serde_json::json!({ "gpu": "true" })))
        );
        assert_eq!(
            resolve_placement_constraints(None, project.as_ref()),
            (Some(vec![3]), Some(serde_json::json!({ "gpu": "true" })))
        );

        // An environment value wins field by field; an empty one is unset.
        let environment = with_placement(Some(vec![]), Some(serde_json::json!({ "zone": "a" })));
        assert_eq!(
            resolve_placement_constraints(environment.as_ref(), project.as_ref()),
            (Some(vec![3]), Some(serde_json::json!({ "zone": "a" })))
        );
        let environment = with_placement(Some(vec![9]), Some(serde_json::json!({})));
        assert_eq!(
            resolve_placement_constraints(environment.as_ref(), project.as_ref()),
            (Some(vec![9]), Some(serde_json::json!({ "gpu": "true" })))
        );

        assert_eq!(resolve_placement_constraints(None, None), (None, None));
    }

    #[test]
    fn only_container_deployments_are_checked_for_placement() {
        assert!(places_replicas(&[
            planned_job(1, "build_image", "BuildImageJob"),
            planned_job(1, "deploy_container", "DeployImageJob"),
        ]));
        assert!(places_replicas(&[planned_job(
            1,
            "deploy",
            "DeployContainerJob"
        )]));
        assert!(!places_replicas(&[
            planned_job(1, "build_image", "BuildImageJob"),
            planned_job(1, "deploy_static", "DeployStaticJob"),
        ]));
        assert!(!places_replicas(&[planned_job(
            1,
            "deploy_compose",
            "DeployComposeJob"
        )]));
    }

    /// A selector that matches only a dedicated worker cannot place a single
    /// replica, so the deployment fails while its workflow is assembled: no
    /// job starts, and the source is never downloaded or built.
    #[tokio::test]
    async fn a_deployment_only_dedicated_nodes_could_run_fails_before_any_job_runs(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (_project, environment, deployment) = create_test_data(&db).await?;
        let mut environment: environments::ActiveModel = environment.into();
        environment.deployment_config = Set(with_placement(
            None,
            Some(serde_json::json!({ "gpu": "true" })),
        ));
        environment.update(db.as_ref()).await?;

        let mut job_ids = Vec::new();
        for (order, (job_id, job_type)) in [
            ("download_repo", "DownloadRepoJob"),
            ("build_image", "BuildImageJob"),
            ("deploy_container", "DeployImageJob"),
        ]
        .into_iter()
        .enumerate()
        {
            let job = deployment_jobs::ActiveModel {
                deployment_id: Set(deployment.id),
                job_id: Set(job_id.to_string()),
                job_type: Set(job_type.to_string()),
                name: Set(job_id.to_string()),
                status: Set(JobStatus::Pending),
                log_id: Set(format!("deployment-{}-job-{job_id}", deployment.id)),
                job_config: Set(Some(serde_json::json!({}))),
                execution_order: Set(Some(order as i32)),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;
            job_ids.push(job.id);
        }

        let service = service_with_docker_handle(db.clone(), disabled_docker_handle()).await?;
        service.set_node_scheduler(dedicated_scheduler(true));

        let error = service
            .execute_deployment_workflow(deployment.id)
            .await
            .expect_err("a deployment with no placeable node must fail");
        match &error {
            WorkflowExecutionError::DedicatedNodesNotPinned {
                deployment_id,
                source:
                    crate::services::node_service::NodeError::DedicatedNodesNotPinned { excluded },
            } => {
                assert_eq!(*deployment_id, deployment.id);
                assert!(
                    excluded.contains("node 7 (gpu-1) is dedicated"),
                    "the error must name the dedicated node: {excluded}"
                );
            }
            other => panic!("expected DedicatedNodesNotPinned, got {other:?}"),
        }
        assert!(error.is_workload_outcome());
        let message = error.to_string();
        assert!(
            message.contains(&format!("Deployment {}", deployment.id)),
            "{message}"
        );
        assert!(message.contains("pin the environment to it"), "{message}");

        for job_id in job_ids {
            let row = deployment_jobs::Entity::find_by_id(job_id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| format!("job row {job_id} disappeared"))?;
            assert_eq!(
                row.status,
                JobStatus::Cancelled,
                "job {} must never start: {:?}",
                row.job_id,
                row.error_message
            );
        }

        Ok(())
    }

    /// Every placement the deploy step could satisfy passes the pre-build
    /// check, and deployments that place no replicas skip it.
    #[tokio::test]
    async fn the_pre_build_placement_check_only_rejects_unpinned_dedicated_placement(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, mut environment, deployment) = create_test_data(&db).await?;
        let container_jobs = [
            planned_job(deployment.id, "build_image", "BuildImageJob"),
            planned_job(deployment.id, "deploy_container", "DeployImageJob"),
        ];
        let static_jobs = [
            planned_job(deployment.id, "build_image", "BuildImageJob"),
            planned_job(deployment.id, "deploy_static", "DeployStaticJob"),
        ];
        let check = |local_workloads_enabled: bool,
                     environment: environments::Model,
                     jobs: &'static str| {
            let db = db.clone();
            let project = project.clone();
            let deployment = deployment.clone();
            let jobs = if jobs == "static" {
                static_jobs.to_vec()
            } else {
                container_jobs.to_vec()
            };
            async move {
                let service = service_with_docker_handle(db, disabled_docker_handle()).await?;
                service.set_node_scheduler(dedicated_scheduler(local_workloads_enabled));
                Ok::<_, Box<dyn std::error::Error>>(
                    service
                        .preflight_dedicated_placement(&project, &environment, &deployment, &jobs)
                        .await,
                )
            }
        };

        // Pinned: the dedicated node is the placement.
        environment.deployment_config = with_placement(Some(vec![7]), None);
        assert!(check(false, environment.clone(), "container")
            .await?
            .is_ok());

        // Unpinned on a control plane that runs workloads: it lands locally.
        environment.deployment_config = None;
        assert!(check(true, environment.clone(), "container").await?.is_ok());

        // Unpinned, and the control plane runs none: nowhere to go.
        assert!(matches!(
            check(false, environment.clone(), "container").await?,
            Err(WorkflowExecutionError::DedicatedNodesNotPinned { .. })
        ));

        // A static site places no replicas, whatever its selector says.
        environment.deployment_config =
            with_placement(None, Some(serde_json::json!({ "gpu": "true" })));
        assert!(check(true, environment.clone(), "static").await?.is_ok());
        assert!(matches!(
            check(true, environment, "container").await?,
            Err(WorkflowExecutionError::DedicatedNodesNotPinned { .. })
        ));

        Ok(())
    }

    /// Compose placement failures the operator acts on are recorded as
    /// deployment outcomes; a database failure is Temps' own fault.
    #[test]
    fn compose_placement_failures_are_outcomes_except_database_errors() {
        use crate::services::compose_placement::ComposePlacementError;
        let offline = WorkflowExecutionError::ComposePlacement {
            deployment_id: 12,
            source: ComposePlacementError::HomeNodeUnavailable {
                environment_id: 3,
                environment_name: "production".to_string(),
                node_id: 7,
                node_name: "worker-a".to_string(),
                state: "offline".to_string(),
            },
        };
        let message = offline.to_string();
        assert!(message.starts_with("Deployment 12: "), "{message}");
        assert!(message.contains("'worker-a' (id 7)"), "{message}");
        assert!(offline.is_workload_outcome());
        assert_eq!(
            crate::services::failure_classifier::classify_failure_reason(Some(&message)).code,
            crate::services::failure_classifier::DeploymentFailureCode::InvalidConfiguration
        );

        let database = WorkflowExecutionError::ComposePlacement {
            deployment_id: 12,
            source: ComposePlacementError::Database {
                environment_id: 3,
                operation: "reading the Compose placement",
                source: sea_orm::DbErr::Custom("connection reset".to_string()),
            },
        };
        assert!(!database.is_workload_outcome());

        let linked = WorkflowExecutionError::ComposeLinkedServicesOnWorker {
            deployment_id: 12,
            host: "worker node 'worker-a' (id 7)".to_string(),
            services: "'orders-db'".to_string(),
        };
        let message = linked.to_string();
        assert!(message.contains("'orders-db'"), "{message}");
        assert!(linked.is_workload_outcome());
        assert_eq!(
            crate::services::failure_classifier::classify_failure_reason(Some(&message)).code,
            crate::services::failure_classifier::DeploymentFailureCode::InvalidConfiguration
        );
    }

    /// A first Compose deployment pinned to a worker goes to that worker; an
    /// unconstrained one stays on the control plane; a pinned home that went
    /// offline is never replaced.
    #[tokio::test]
    async fn compose_stacks_go_to_their_home_and_never_fail_over(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return Ok(());
        }
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (project, mut environment, deployment) = create_test_data(&db).await?;
        let service = service_with_docker_handle(db.clone(), disabled_docker_handle()).await?;
        let node_service = Arc::new(crate::services::NodeService::new(db.clone()));
        service.set_node_scheduler(Arc::new(
            crate::services::NodeScheduler::new(node_service).with_local_workloads_enabled(true),
        ));
        service.set_encryption_service(Arc::new(temps_core::EncryptionService::new(
            TEST_MASTER_KEY,
        )?));
        let worker = insert_node(
            db.as_ref(),
            "worker-a",
            "http://127.0.0.1:9",
            "linux/amd64",
            serde_json::json!({}),
        )
        .await?;
        let policy = temps_entities::compose_security::ComposeSecurityPolicy::default();

        // Unconstrained: the control plane, as Compose always ran.
        let (plan, host, vacate) = service
            .plan_compose_stack(&project, &environment, &deployment, &policy)
            .await?;
        assert!(host.is_control_plane(), "{host:?}");
        assert!(!plan.pinned);
        assert!(vacate.is_none());

        // Target Nodes naming the worker: the worker.
        environment.deployment_config = with_placement(Some(vec![worker.id]), None);
        let (plan, host, _) = service
            .plan_compose_stack(&project, &environment, &deployment, &policy)
            .await?;
        assert_eq!(host.node_id(), Some(worker.id), "{host:?}");
        assert!(!plan.pinned);

        // Pinned there, then the worker goes offline: refused, never moved.
        crate::services::compose_placement::ComposePlacementService::new(db.clone())
            .pin(environment.id, Some(worker.id))
            .await?;
        let mut offline: temps_entities::nodes::ActiveModel = worker.clone().into();
        offline.status = Set("offline".to_string());
        offline.update(db.as_ref()).await?;
        environment.deployment_config = None;
        let error = service
            .plan_compose_stack(&project, &environment, &deployment, &policy)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                WorkflowExecutionError::ComposePlacement {
                    source: crate::services::compose_placement::ComposePlacementError::HomeNodeUnavailable { node_id, .. },
                    ..
                } if node_id == worker.id
            ),
            "{error:?}"
        );
        Ok(())
    }

    /// A stand-in worker agent that answers `GET /agent/health` with
    /// `platform`, for as long as the test runs.
    async fn spawn_health_agent(platform: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0_u8; 8192];
                    let _ = stream.read(&mut buf).await;
                    let body = serde_json::json!({
                        "success": true,
                        "data": { "status": "ok", "platform": platform }
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{address}")
    }

    /// An address nothing listens on: a worker that is down.
    async fn unreachable_agent() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    }

    const TEST_MASTER_KEY: &str = "01234567890123456789012345678901";

    async fn insert_worker(
        db: &DbConnection,
        name: &str,
        address: &str,
        architecture: &str,
    ) -> Result<temps_entities::nodes::Model, Box<dyn std::error::Error>> {
        insert_node(db, name, address, architecture, serde_json::json!({})).await
    }

    async fn insert_node(
        db: &DbConnection,
        name: &str,
        address: &str,
        architecture: &str,
        labels: serde_json::Value,
    ) -> Result<temps_entities::nodes::Model, Box<dyn std::error::Error>> {
        let encryption = temps_core::EncryptionService::new(TEST_MASTER_KEY)?;
        Ok(temps_entities::nodes::ActiveModel {
            name: Set(name.to_string()),
            token_hash: Set(format!("hash-{name}")),
            token_encrypted: Set(Some(encryption.encrypt(b"agent-token")?)),
            address: Set(address.to_string()),
            private_address: Set("127.0.0.1".to_string()),
            role: Set("worker".to_string()),
            status: Set("active".to_string()),
            labels: Set(labels),
            capacity: Set(serde_json::json!({})),
            architecture: Set(Some(architecture.to_string())),
            last_heartbeat: Set(Some(Utc::now())),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        }
        .insert(db)
        .await?)
    }

    /// The control-plane profile: no Docker daemon, no local workloads.
    async fn control_plane_service(
        db: Arc<DbConnection>,
    ) -> Result<WorkflowExecutionService, Box<dyn std::error::Error>> {
        let service = service_with_docker_handle(db.clone(), disabled_docker_handle()).await?;
        let node_service = Arc::new(crate::services::NodeService::new(db));
        service.set_node_scheduler(Arc::new(
            crate::services::NodeScheduler::new(node_service).with_local_workloads_enabled(false),
        ));
        service.set_encryption_service(Arc::new(temps_core::EncryptionService::new(
            TEST_MASTER_KEY,
        )?));
        Ok(service)
    }

    fn platforms_and_nodes(selected: &[SelectedNodeBuilder]) -> Vec<(String, i32)> {
        selected
            .iter()
            .map(|selected| (selected.platform.clone(), selected.node_id))
            .collect()
    }

    /// Target nodes spanning two architectures get one builder per
    /// architecture, each a worker of that architecture -- in the order the
    /// scheduler lists the platforms, so the first owns the plain tag.
    #[tokio::test]
    async fn mixed_architecture_targets_get_one_native_builder_per_platform(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let arm = insert_worker(
            &db,
            "w-arm",
            &spawn_health_agent("linux/arm64").await,
            "linux/arm64",
        )
        .await?;
        let amd = insert_worker(
            &db,
            "w-amd",
            &spawn_health_agent("linux/amd64").await,
            "linux/amd64",
        )
        .await?;
        let service = control_plane_service(db).await?;

        let selected = service
            .select_node_builder(1, "app", None, None, false)
            .await?;

        assert_eq!(
            platforms_and_nodes(&selected),
            vec![
                ("linux/amd64".to_string(), amd.id),
                ("linux/arm64".to_string(), arm.id)
            ]
        );
        Ok(())
    }

    /// A single-architecture cluster keeps exactly one build, as before.
    #[tokio::test]
    async fn single_architecture_targets_keep_one_builder() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        insert_worker(
            &db,
            "w-arm-1",
            &spawn_health_agent("linux/arm64").await,
            "linux/arm64",
        )
        .await?;
        insert_worker(
            &db,
            "w-arm-2",
            &spawn_health_agent("linux/arm64").await,
            "linux/arm64",
        )
        .await?;
        let service = control_plane_service(db).await?;

        let selected = service
            .select_node_builder(1, "app", None, None, false)
            .await?;

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].platform, "linux/arm64");
        Ok(())
    }

    /// An unreachable worker of one architecture is passed over for another
    /// worker of the same architecture.
    #[tokio::test]
    async fn an_unreachable_builder_is_replaced_by_another_of_its_architecture(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        insert_worker(
            &db,
            "w-arm",
            &spawn_health_agent("linux/arm64").await,
            "linux/arm64",
        )
        .await?;
        let dead =
            insert_worker(&db, "w-amd-down", &unreachable_agent().await, "linux/amd64").await?;
        let live = insert_worker(
            &db,
            "w-amd-up",
            &spawn_health_agent("linux/amd64").await,
            "linux/amd64",
        )
        .await?;
        let service = control_plane_service(db).await?;

        let selected = service
            .select_node_builder(1, "app", None, None, false)
            .await?;

        let amd = selected
            .iter()
            .find(|selected| selected.platform == "linux/amd64")
            .expect("an amd64 builder");
        assert_eq!(amd.node_id, live.id);
        assert_ne!(amd.node_id, dead.id);
        Ok(())
    }

    /// A target architecture with no reachable worker -- or only one whose
    /// agent reports another architecture than its node row -- stops the
    /// deployment with a typed error naming that architecture. Nothing is
    /// emulated and nothing is built for the other architecture first.
    #[tokio::test]
    async fn an_architecture_without_a_usable_worker_is_refused(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (amd_address, why) in [
            (unreachable_agent().await, "did not answer"),
            (
                spawn_health_agent("linux/arm64").await,
                "is recorded as linux/amd64 but its agent reports linux/arm64",
            ),
        ] {
            let test_db = TestDatabase::with_migrations().await?;
            let db = test_db.connection_arc();
            insert_worker(
                &db,
                "w-arm",
                &spawn_health_agent("linux/arm64").await,
                "linux/arm64",
            )
            .await?;
            insert_worker(&db, "w-amd", &amd_address, "linux/amd64").await?;
            let service = control_plane_service(db).await?;

            let error = match service
                .select_node_builder(17, "app", None, None, false)
                .await
            {
                Ok(selected) => panic!(
                    "expected a refusal, got {:?}",
                    platforms_and_nodes(&selected)
                ),
                Err(error) => error,
            };
            match &error {
                WorkflowExecutionError::NoBuilderForArchitecture {
                    deployment_id,
                    platform,
                    reason,
                } => {
                    assert_eq!(*deployment_id, 17);
                    assert_eq!(platform, "linux/amd64");
                    assert!(reason.contains("w-amd"), "{reason}");
                    assert!(reason.contains(why), "{reason}");
                }
                other => panic!("expected NoBuilderForArchitecture, got {other}"),
            }
        }
        Ok(())
    }

    /// Every dedicated builder of an architecture is tried before any
    /// application worker: with the first amd64 builder and the amd64
    /// application worker both down, the second amd64 builder still builds.
    #[tokio::test]
    async fn a_spare_dedicated_builder_replaces_an_unreachable_one(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let builder = serde_json::json!({ "temps.sh/role": "builder" });
        insert_worker(
            &db,
            "w-arm",
            &spawn_health_agent("linux/arm64").await,
            "linux/arm64",
        )
        .await?;
        insert_worker(&db, "w-amd", &unreachable_agent().await, "linux/amd64").await?;
        let first = insert_node(
            &db,
            "b-amd-1",
            &unreachable_agent().await,
            "linux/amd64",
            builder.clone(),
        )
        .await?;
        let spare = insert_node(
            &db,
            "b-amd-2",
            &spawn_health_agent("linux/amd64").await,
            "linux/amd64",
            builder,
        )
        .await?;
        assert!(first.id < spare.id);
        let service = control_plane_service(db).await?;

        let selected = service
            .select_node_builder(1, "app", None, None, false)
            .await?;

        let amd = selected
            .iter()
            .find(|selected| selected.platform == "linux/amd64")
            .expect("an amd64 builder");
        assert_eq!(amd.node_id, spare.id);
        Ok(())
    }

    /// Target nodes pin the architectures built: a deployment restricted to
    /// one architecture builds once even on a mixed cluster.
    #[tokio::test]
    async fn target_nodes_restrict_the_architectures_built(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let arm = insert_worker(
            &db,
            "w-arm",
            &spawn_health_agent("linux/arm64").await,
            "linux/arm64",
        )
        .await?;
        insert_worker(
            &db,
            "w-amd",
            &spawn_health_agent("linux/amd64").await,
            "linux/amd64",
        )
        .await?;
        let service = control_plane_service(db).await?;

        let selected = service
            .select_node_builder(1, "app", Some(&[arm.id]), None, false)
            .await?;

        assert_eq!(
            platforms_and_nodes(&selected),
            vec![("linux/arm64".to_string(), arm.id)]
        );
        Ok(())
    }

    /// Replicas of a mixed-architecture build land only on nodes whose
    /// architecture has an image, and each replica's tag and image source are
    /// the ones built for its node. With one node drained, its replica moves
    /// to a node of the other architecture and takes that architecture's
    /// image -- never the drained architecture's.
    #[tokio::test]
    async fn replicas_are_placed_and_sourced_by_architecture(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let arm = insert_worker(&db, "w-arm", "http://127.0.0.1:9", "linux/arm64").await?;
        let amd = insert_worker(&db, "w-amd", "http://127.0.0.1:9", "linux/amd64").await?;
        let scheduler = crate::services::NodeScheduler::new(Arc::new(
            crate::services::NodeService::new(db.clone()),
        ))
        .with_local_workloads_enabled(false);
        let output = crate::jobs::deploy_image::BuildImageOutput {
            image_tag: "app:latest".to_string(),
            image_id: "sha256:amd".to_string(),
            size_bytes: 1,
            build_context: ".".into(),
            dockerfile_path: "Dockerfile".into(),
            image_tags_by_platform: HashMap::from([
                ("linux/amd64".to_string(), "app:latest".to_string()),
                ("linux/arm64".to_string(), "app:latest-arm64".to_string()),
            ]),
            builder_node_id: Some(amd.id),
            builder_node_ids_by_platform: HashMap::from([
                ("linux/amd64".to_string(), amd.id),
                ("linux/arm64".to_string(), arm.id),
            ]),
            image_ids_by_platform: HashMap::new(),
        };
        let platforms: Vec<String> = vec!["linux/amd64".into(), "linux/arm64".into()];

        let placed = |exclude: Vec<i32>, replicas: u32| {
            let scheduler = &scheduler;
            let platforms = &platforms;
            async move {
                scheduler
                    .schedule_placement(crate::services::node_scheduler::ReplicaPlacementRequest {
                        replica_count: replicas,
                        labels: None,
                        target_node_ids: None,
                        anti_affinity: true,
                        exclude_node_ids: &exclude,
                        image_platforms: platforms,
                        project_slug: Some("app"),
                        exclude_control_plane: false,
                    })
                    .await
            }
        };

        let outcome = placed(vec![], 2).await?;
        let mut seen = Vec::new();
        for assignment in &outcome.assignments {
            let node_id = assignment.node_id().expect("a worker");
            let tag = output.tag_for_platform(assignment.platform());
            let source = output.builder_node_for_tag(tag);
            seen.push((node_id, tag.to_string(), source));
        }
        seen.sort();
        let mut expected = vec![
            (arm.id, "app:latest-arm64".to_string(), Some(arm.id)),
            (amd.id, "app:latest".to_string(), Some(amd.id)),
        ];
        expected.sort();
        assert_eq!(seen, expected);

        // Drain the arm64 node: its replica moves to the amd64 node, which
        // runs (and already holds) the amd64 image.
        temps_entities::nodes::Entity::update_many()
            .col_expr(
                temps_entities::nodes::Column::Status,
                sea_orm::sea_query::Expr::value("draining"),
            )
            .filter(temps_entities::nodes::Column::Id.eq(arm.id))
            .exec(db.as_ref())
            .await?;
        let outcome = placed(vec![], 1).await?;
        let moved = &outcome.assignments[0];
        assert_eq!(moved.node_id(), Some(amd.id));
        let tag = output.tag_for_platform(moved.platform());
        assert_eq!(tag, "app:latest");
        assert_eq!(output.builder_node_for_tag(tag), Some(amd.id));

        // A build that only covered arm64 can never be placed on the amd64
        // node that remains: the deployment fails instead of running the
        // wrong architecture.
        let error = scheduler
            .schedule_placement(crate::services::node_scheduler::ReplicaPlacementRequest {
                replica_count: 1,
                labels: None,
                target_node_ids: None,
                anti_affinity: true,
                exclude_node_ids: &[],
                image_platforms: &["linux/arm64".to_string()],
                project_slug: Some("app"),
                exclude_control_plane: false,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("linux/arm64"), "{error}");
        Ok(())
    }
}
