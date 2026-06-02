use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GitPushEventJob {
    pub owner: String,
    pub repo: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit: String,
    pub project_id: i32,
    /// True when this event came from an explicit user action (the
    /// "Deploy" button or `trigger_pipeline` API call), false when it
    /// came from a git provider webhook. Manual triggers bypass the
    /// `environments.automatic_deploy` gate — the user already opted in
    /// by clicking; webhook events still honour the opt-out.
    ///
    /// `#[serde(default)]` so in-flight jobs queued by older versions
    /// (no field) deserialize as webhook-driven, which preserves the
    /// pre-existing gate behaviour for them.
    #[serde(default)]
    pub manual_trigger: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UpdateRepoFrameworkJob {
    pub repo_id: i32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProvisionCertificateJob {
    pub domain: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RenewCertificateJob {
    pub domain: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GenerateCustomCertificateJob {
    pub domain_id: i32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CalculateRepositoryPresetJob {
    pub repository_id: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronInvocationErrorData {
    pub project_id: i32,
    pub environment_id: i32,
    pub cron_job_id: i32,
    pub cron_job_name: String,
    pub error_message: String,
    pub timestamp: UtcDateTime,
    pub schedule: String,
    pub last_successful_run: Option<UtcDateTime>,
}

/// Job for when a project is created
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCreatedJob {
    pub project_id: i32,
    pub project_name: String,
}

/// Job for when a project is updated
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectUpdatedJob {
    pub project_id: i32,
    pub project_name: String,
}

/// Job for when a project is deleted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDeletedJob {
    pub project_id: i32,
    pub project_name: String,
}

/// Job for when an environment is created
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentCreatedJob {
    pub environment_id: i32,
    pub environment_name: String,
    pub project_id: i32,
    pub subdomain: String,
}

/// Job for when an environment is deleted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentDeletedJob {
    pub environment_id: i32,
    pub environment_name: String,
    pub project_id: i32,
}

/// Job for when a monitor is created
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorCreatedJob {
    pub monitor_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub monitor_name: String,
}

/// Job for when a deployment is created
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentCreatedJob {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub commit_sha: Option<String>,
    pub branch: Option<String>,
}

/// Job for when a deployment succeeds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentSucceededJob {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub commit_sha: Option<String>,
    pub url: Option<String>,
    /// Health check path from .temps.yaml, used to update the monitor's check_path
    #[serde(default)]
    pub health_check_path: Option<String>,
}

/// Job for when a deployment fails
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentFailedJob {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub error_message: Option<String>,
}

/// Job for when a deployment is cancelled
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentCancelledJob {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
}

/// Job for when a deployment is ready (container is running and healthy)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentReadyJob {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub url: Option<String>,
}

/// Job for when a domain is created
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainCreatedJob {
    pub domain_id: i32,
    pub project_id: i32,
    pub domain_name: String,
}

/// Job for when a domain certificate is provisioned
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainProvisionedJob {
    pub domain_id: i32,
    pub project_id: i32,
    pub domain_name: String,
}

/// Job for when a vulnerability scan is completed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnerabilityScanCompletedJob {
    pub scan_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub total_vulnerabilities: i32,
    pub critical_count: i32,
    pub high_count: i32,
    pub medium_count: i32,
    pub low_count: i32,
    pub status: String,
}

/// Job for when a status check is completed (for outage detection)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusCheckCompletedJob {
    pub monitor_id: i32,
    pub status: String,
    pub error_message: Option<String>,
}

/// Job for when an alarm is fired (container restart, outage, high resource usage, etc.)
///
/// `environment_id` and `deployment_id` are `Option` because service-scoped
/// (database) alarms have no environment or deployment context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlarmFiredJob {
    pub alarm_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub alarm_type: String,
    pub severity: String,
    pub title: String,
}

/// Job for triggering an autopilot run (AI-powered error fix)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutopilotTriggerJob {
    pub project_id: i32,
    pub trigger_type: String,
    pub trigger_source_id: Option<i32>,
    pub trigger_source_type: Option<String>,
    pub error_group_id: Option<i32>,
}

/// Job for when an alarm is resolved
///
/// `environment_id` and `deployment_id` are `Option` because service-scoped
/// (database) alarms have no environment or deployment context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlarmResolvedJob {
    pub alarm_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub alarm_type: String,
    pub title: String,
}

/// Job emitted by route table listeners after successfully reloading routes.
/// Used by the deployment pipeline to confirm the proxy has picked up the new
/// routing configuration before marking a deployment as completed and tearing
/// down previous containers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTableUpdatedJob {
    /// The environment whose `current_deployment_id` change triggered this reload
    pub environment_id: Option<i32>,
    /// The deployment that the environment now points to
    pub deployment_id: Option<i32>,
    /// Total number of routes loaded in this reload
    pub route_count: usize,
}

/// In-process request to reload the proxy's route table.
///
/// This is the *request* counterpart to [`RouteTableUpdatedJob`] (the
/// confirmation). It is published on the shared broadcast queue by the
/// deployment pipeline immediately after writing `environments.current_deployment_id`,
/// and consumed in-process by the route-reload subscriber, which calls
/// `load_routes()` and then publishes `RouteTableUpdated`.
///
/// Unlike the PostgreSQL `LISTEN/NOTIFY` path (which is the only route-reload
/// trigger for *remote* worker nodes and can silently drop a notification if
/// its long-lived listener connection dies), this in-process path cannot lose
/// the signal between deployments: it rides the same tokio broadcast channel
/// the deploy job already uses, with no database connection in the critical
/// path. The NOTIFY path is retained for cross-node propagation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForceRouteReloadJob {
    /// The environment whose route changed (used to match the confirmation).
    pub environment_id: Option<i32>,
    /// The deployment the environment now points to (used to match the confirmation).
    pub deployment_id: Option<i32>,
}

/// Trigger event for a backup. Published by the HTTP handler or the cron
/// scheduler immediately after inserting the `backups` row. The backup
/// processor consumes this event and runs the engine in a one-shot
/// container.
///
/// The processor reads everything it needs from the `backups` row + the
/// engine-specific JSON params; this struct only carries the routing
/// info needed to dispatch to the right engine and apply the right
/// wall-clock limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRequestedJob {
    /// FK to `backups.id`. The processor uses this as the dedup key
    /// (one in-flight task per backup_id) and as the audit log key.
    pub backup_id: i32,
    /// Engine key (`"control_plane"`, `"postgres_pgdump"`, `"redis"`,
    /// `"mongodb"`, `"s3_mirror"`, `"postgres_walg"`, `"postgres_cluster"`).
    /// Must match a registered `BackupEngine::engine()` in the processor.
    pub engine: String,
    /// Engine-specific parameters (e.g. `{"service_id": 28, "s3_source_id": 2}`).
    pub params: serde_json::Value,
    /// Wall-clock timeout for this backup. The processor wraps the
    /// container's exit in `tokio::time::timeout` with this duration.
    pub max_runtime_secs: i64,
}

/// Result event published by the backup processor after a successful run.
/// The schedule_runs aggregator listens for this to update the parent
/// `schedule_runs.finished_at` once every sibling reaches a terminal state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupCompletedJob {
    pub backup_id: i32,
    pub engine: String,
    /// S3 URL or object key where the backup data lives.
    pub s3_location: String,
    pub size_bytes: Option<i64>,
}

/// Result event published by the backup processor after a failed run.
/// Carries the captured stderr tail so notification handlers can include
/// it in alerts without re-querying the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFailedJob {
    pub backup_id: i32,
    pub engine: String,
    pub error_message: String,
}

/// Cancel request from the HTTP cancel handler. The processor looks the
/// `backup_id` up in its in-memory cancel-token map and fires the token,
/// which signals the in-flight container to stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupCancelRequestedJob {
    pub backup_id: i32,
}

/// Core job enum containing all possible job types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Job {
    UpdateRepoFramework(UpdateRepoFrameworkJob),
    RenewCertificate(RenewCertificateJob),
    GenerateCustomCertificate(GenerateCustomCertificateJob),
    CustomDomainAdded(String),
    CustomDomainRemoved(String),
    CustomRouteAdded(String),
    CustomRouteRemoved(String),
    ProvisionCertificate(ProvisionCertificateJob),
    CalculateRepositoryPreset(CalculateRepositoryPresetJob),
    GitPushEvent(GitPushEventJob),
    CronInvocationError(CronInvocationErrorData),
    ProjectCreated(ProjectCreatedJob),
    ProjectUpdated(ProjectUpdatedJob),
    ProjectDeleted(ProjectDeletedJob),
    EnvironmentCreated(EnvironmentCreatedJob),
    EnvironmentDeleted(EnvironmentDeletedJob),
    MonitorCreated(MonitorCreatedJob),
    // Deployment events
    DeploymentCreated(DeploymentCreatedJob),
    DeploymentSucceeded(DeploymentSucceededJob),
    DeploymentFailed(DeploymentFailedJob),
    DeploymentCancelled(DeploymentCancelledJob),
    DeploymentReady(DeploymentReadyJob),
    // Domain events
    DomainCreated(DomainCreatedJob),
    DomainProvisioned(DomainProvisionedJob),
    // Vulnerability scan events
    VulnerabilityScanCompleted(VulnerabilityScanCompletedJob),
    // Status check events
    StatusCheckCompleted(StatusCheckCompletedJob),
    // Route table events
    RouteTableUpdated(RouteTableUpdatedJob),
    /// In-process request to reload the proxy route table (see [`ForceRouteReloadJob`]).
    ForceRouteReload(ForceRouteReloadJob),
    // Alarm events
    AlarmFired(AlarmFiredJob),
    AlarmResolved(AlarmResolvedJob),
    // Autopilot events
    AutopilotTrigger(AutopilotTriggerJob),
    // Backup events — the trigger flows through the same JobQueue as
    // deployments: the HTTP handler / cron tick publishes BackupRequested,
    // the `BackupJobProcessor` consumes it and runs the engine in a
    // one-shot container, then publishes BackupCompleted or BackupFailed.
    BackupRequested(BackupRequestedJob),
    BackupCompleted(BackupCompletedJob),
    BackupFailed(BackupFailedJob),
    BackupCancelRequested(BackupCancelRequestedJob),
    /// Scheduled hourly to prune raw service_metrics rows older than the
    /// configured `retention_raw_days` window. Continuous aggregates
    /// (hourly/daily rollups) have their own TimescaleDB retention policies
    /// and do not need to be pruned here.
    PruneMetrics,
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Job::UpdateRepoFramework(job) => write!(
                f,
                "UpdateRepoFramework(repo_id: {})",
                job.repo_id
            ),
            Job::RenewCertificate(job) => {
                write!(f, "RenewCertificate(domain_id: {})", job.domain)
            }
            Job::GenerateCustomCertificate(job) => {
                write!(f, "GenerateCustomCertificate(domain_id: {})", job.domain_id)
            }
            Job::CustomDomainAdded(domain) => write!(f, "CustomDomainAdded({})", domain),
            Job::CustomDomainRemoved(domain) => write!(f, "CustomDomainRemoved({})", domain),
            Job::CustomRouteAdded(domain) => write!(f, "CustomRouteAdded({})", domain),
            Job::CustomRouteRemoved(domain) => write!(f, "CustomRouteRemoved({})", domain),
            Job::ProvisionCertificate(job) => write!(f, "ProvisionCertificate({})", job.domain),
            Job::CalculateRepositoryPreset(job) => write!(f, "CalculateRepositoryPreset(repository_id: {})", job.repository_id),
            Job::GitPushEvent(job) => write!(f, "GitPushEvent(project_id: {}, owner: {}, repo: {}, branch: {:?}, tag: {:?}, commit: {})", job.project_id, job.owner, job.repo, job.branch, job.tag, job.commit),
            Job::CronInvocationError(job) => write!(f, "CronInvocationError(cron_id: {}, env: {}, error: {})", job.cron_job_id, job.environment_id, job.error_message),
            Job::ProjectCreated(job) => write!(f, "ProjectCreated(id: {}, name: {})", job.project_id, job.project_name),
            Job::ProjectUpdated(job) => write!(f, "ProjectUpdated(id: {}, name: {})", job.project_id, job.project_name),
            Job::ProjectDeleted(job) => write!(f, "ProjectDeleted(id: {}, name: {})", job.project_id, job.project_name),
            Job::EnvironmentCreated(job) => write!(f, "EnvironmentCreated(id: {}, name: {}, project: {})", job.environment_id, job.environment_name, job.project_id),
            Job::EnvironmentDeleted(job) => write!(f, "EnvironmentDeleted(id: {}, name: {}, project: {})", job.environment_id, job.environment_name, job.project_id),
            Job::MonitorCreated(job) => write!(f, "MonitorCreated(id: {}, name: {}, env: {}, project: {})", job.monitor_id, job.monitor_name, job.environment_id, job.project_id),
            Job::DeploymentCreated(job) => write!(f, "DeploymentCreated(id: {}, env: {}, project: {})", job.deployment_id, job.environment_id, job.project_id),
            Job::DeploymentSucceeded(job) => write!(f, "DeploymentSucceeded(id: {}, env: {}, project: {})", job.deployment_id, job.environment_id, job.project_id),
            Job::DeploymentFailed(job) => write!(f, "DeploymentFailed(id: {}, env: {}, project: {}, error: {:?})", job.deployment_id, job.environment_id, job.project_id, job.error_message),
            Job::DeploymentCancelled(job) => write!(f, "DeploymentCancelled(id: {}, env: {}, project: {})", job.deployment_id, job.environment_id, job.project_id),
            Job::DeploymentReady(job) => write!(f, "DeploymentReady(id: {}, env: {}, project: {}, url: {:?})", job.deployment_id, job.environment_id, job.project_id, job.url),
            Job::DomainCreated(job) => write!(f, "DomainCreated(id: {}, name: {}, project: {})", job.domain_id, job.domain_name, job.project_id),
            Job::DomainProvisioned(job) => write!(f, "DomainProvisioned(id: {}, name: {}, project: {})", job.domain_id, job.domain_name, job.project_id),
            Job::VulnerabilityScanCompleted(job) => write!(f, "VulnerabilityScanCompleted(id: {}, project: {}, env: {:?}, total: {}, critical: {}, high: {})", job.scan_id, job.project_id, job.environment_id, job.total_vulnerabilities, job.critical_count, job.high_count),
            Job::StatusCheckCompleted(job) => write!(f, "StatusCheckCompleted(monitor: {}, status: {})", job.monitor_id, job.status),
            Job::RouteTableUpdated(job) => write!(f, "RouteTableUpdated(env: {:?}, deployment: {:?}, routes: {})", job.environment_id, job.deployment_id, job.route_count),
            Job::ForceRouteReload(job) => write!(f, "ForceRouteReload(env: {:?}, deployment: {:?})", job.environment_id, job.deployment_id),
            Job::AlarmFired(job) => write!(f, "AlarmFired(id: {}, project: {}, type: {}, severity: {})", job.alarm_id, job.project_id, job.alarm_type, job.severity),
            Job::AlarmResolved(job) => write!(f, "AlarmResolved(id: {}, project: {}, type: {})", job.alarm_id, job.project_id, job.alarm_type),
            Job::AutopilotTrigger(job) => write!(f, "AutopilotTrigger(project: {}, type: {}, source: {:?})", job.project_id, job.trigger_type, job.trigger_source_id),
            Job::BackupRequested(job) => write!(f, "BackupRequested(backup: {}, engine: {})", job.backup_id, job.engine),
            Job::BackupCompleted(job) => write!(f, "BackupCompleted(backup: {}, engine: {}, size: {:?})", job.backup_id, job.engine, job.size_bytes),
            Job::BackupFailed(job) => write!(f, "BackupFailed(backup: {}, engine: {})", job.backup_id, job.engine),
            Job::BackupCancelRequested(job) => write!(f, "BackupCancelRequested(backup: {})", job.backup_id),
            Job::PruneMetrics => write!(f, "PruneMetrics"),
        }
    }
}

// Core queue abstraction - temps-queue implements this
use async_trait::async_trait;
use thiserror::Error;

use crate::UtcDateTime;

#[derive(Error, Debug)]
pub enum QueueError {
    #[error("Failed to send job: {0}")]
    SendError(String),
    #[error("Failed to receive job: {0}")]
    ReceiveError(String),
    #[error("Queue channel closed")]
    ChannelClosed,
    #[error("Invalid job data: {0}")]
    InvalidData(String),
}

/// Core trait for job queue operations
#[async_trait]
pub trait JobQueue: Send + Sync {
    /// Send a job to the queue
    async fn send(&self, job: Job) -> Result<(), QueueError>;

    /// Create a new receiver for jobs
    fn subscribe(&self) -> Box<dyn JobReceiver>;
}

/// Core trait for receiving jobs
#[async_trait]
pub trait JobReceiver: Send {
    /// Receive the next job
    async fn recv(&mut self) -> Result<Job, QueueError>;
}
