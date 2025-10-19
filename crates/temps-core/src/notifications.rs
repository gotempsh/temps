use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMessage {
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub html_body: Option<String>,
    pub from: Option<String>,
    pub reply_to: Option<String>,
}

#[async_trait]
pub trait NotificationService: Send + Sync {
    async fn send_email(&self, message: EmailMessage) -> Result<(), NotificationError>;
    async fn send_notification(
        &self,
        notification: NotificationData,
    ) -> Result<(), NotificationError>;
    async fn is_configured(&self) -> Result<bool, NotificationError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationData {
    pub id: String,
    pub title: String,
    pub message: String,
    pub notification_type: NotificationType,
    pub priority: NotificationPriority,
    pub severity: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub metadata: std::collections::HashMap<String, String>,
    pub bypass_throttling: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationType {
    Info,
    Warning,
    Error,
    Alert,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationPriority {
    Low,
    Normal,
    High,
    Critical,
}

impl Default for NotificationData {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            message: String::new(),
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            bypass_throttling: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentFailureData {
    pub service_name: String,
    pub deployment_id: String,
    pub error_message: String,
    pub environment: String,
    pub commit_sha: String,
    pub branch: String,
    pub pipeline_id: Option<String>,
    pub committer_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildErrorData {
    pub service_name: String,
    pub build_id: String,
    pub error_message: String,
    pub stage: String,
    pub commit_sha: String,
    pub branch: String,
    pub pipeline_id: Option<String>,
    pub committer_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeErrorData {
    pub service_name: String,
    pub error_message: String,
    pub error_type: String,
    pub stack_trace: Option<String>,
    pub container_id: Option<String>,
    pub pod_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslExpirationData {
    pub domain: String,
    pub days_until_expiry: i32,
    pub issuer: Option<String>,
    pub current_certificate_id: Option<String>,
    pub expiry_date: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsChangeData {
    pub domain: String,
    pub change_type: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub record_type: String,
    pub detected_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BackupFailureData {
    pub schedule_id: i32,
    pub schedule_name: String,
    pub backup_type: String,
    pub error: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3ConnectionIssueData {
    pub service_name: String,
    pub error_message: String,
    pub operation_type: String,
    pub bucket_name: Option<String>,
    pub region: Option<String>,
    pub retry_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDowntimeData {
    pub service_name: String,
    pub duration_seconds: i64,
    pub last_healthy_at: chrono::DateTime<Utc>,
    pub error_message: Option<String>,
    pub affected_endpoints: Vec<String>,
    pub container_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancerIssueData {
    pub service_name: String,
    pub issue_type: String,
    pub error_message: String,
    pub affected_backends: Vec<String>,
    pub health_check_failures: Option<i32>,
    pub last_healthy_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSuccessData {
    pub schedule_id: Option<i32>,
    pub schedule_name: Option<String>,
    pub backup_id: String,
    pub backup_type: String,
    pub size_bytes: i32,
    pub s3_location: String,
    pub timestamp: DateTime<Utc>,
    pub external_services: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentDownData {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub project_name: String,
    pub environment_name: String,
    pub error_message: String,
    pub detected_at: DateTime<Utc>,
    pub host: String,
    pub port: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronInvocationErrorData {
    pub project_id: i32,
    pub environment_id: i32,
    pub cron_job_id: i32,
    pub cron_job_name: String,
    pub error_message: String,
    pub timestamp: DateTime<Utc>,
    pub schedule: String,
    pub last_successful_run: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotFailureData {
    pub url: String,
    pub error_message: String,
    pub screenshot_service_url: String,
    pub response_status: Option<u16>,
    pub timestamp: DateTime<Utc>,
    pub project_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    #[error("Failed to send notification: {0}")]
    SendError(String),

    #[error("Invalid recipient: {0}")]
    InvalidRecipient(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Configuration error: {0}")]
    ConfigurationError(String),

    #[error("Internal error: {0}")]
    InternalError(String),
}

pub type DynNotificationService = Arc<dyn NotificationService>;
