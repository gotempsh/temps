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
