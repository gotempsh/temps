//! Webhook event types and payload definitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// All supported webhook event types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEventType {
    // Deployment events
    DeploymentCreated,
    DeploymentSucceeded,
    DeploymentFailed,
    DeploymentCancelled,
    DeploymentReady,

    // Project events
    ProjectCreated,
    ProjectDeleted,

    // Domain events
    DomainCreated,
    DomainProvisioned,
}

impl WebhookEventType {
    /// Returns all available event types
    pub fn all() -> Vec<Self> {
        vec![
            Self::DeploymentCreated,
            Self::DeploymentSucceeded,
            Self::DeploymentFailed,
            Self::DeploymentCancelled,
            Self::DeploymentReady,
            Self::ProjectCreated,
            Self::ProjectDeleted,
            Self::DomainCreated,
            Self::DomainProvisioned,
        ]
    }

    /// Returns the string representation of the event type
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeploymentCreated => "deployment.created",
            Self::DeploymentSucceeded => "deployment.succeeded",
            Self::DeploymentFailed => "deployment.failed",
            Self::DeploymentCancelled => "deployment.cancelled",
            Self::DeploymentReady => "deployment.ready",
            Self::ProjectCreated => "project.created",
            Self::ProjectDeleted => "project.deleted",
            Self::DomainCreated => "domain.created",
            Self::DomainProvisioned => "domain.provisioned",
        }
    }

    /// Parse event type from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "deployment.created" | "deployment_created" => Some(Self::DeploymentCreated),
            "deployment.succeeded" | "deployment_succeeded" => Some(Self::DeploymentSucceeded),
            "deployment.failed" | "deployment_failed" => Some(Self::DeploymentFailed),
            "deployment.cancelled" | "deployment_cancelled" => Some(Self::DeploymentCancelled),
            "deployment.ready" | "deployment_ready" => Some(Self::DeploymentReady),
            "project.created" | "project_created" => Some(Self::ProjectCreated),
            "project.deleted" | "project_deleted" => Some(Self::ProjectDeleted),
            "domain.created" | "domain_created" => Some(Self::DomainCreated),
            "domain.provisioned" | "domain_provisioned" => Some(Self::DomainProvisioned),
            _ => None,
        }
    }
}

impl std::fmt::Display for WebhookEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Base webhook event structure
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WebhookEvent {
    /// Unique event ID
    pub id: String,
    /// Event type
    pub event_type: WebhookEventType,
    /// Timestamp when event occurred
    pub timestamp: DateTime<Utc>,
    /// Project ID (if applicable)
    pub project_id: Option<i32>,
    /// Event payload
    pub payload: WebhookPayload,
}

impl WebhookEvent {
    /// Create a new webhook event
    pub fn new(
        event_type: WebhookEventType,
        project_id: Option<i32>,
        payload: WebhookPayload,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            event_type,
            timestamp: Utc::now(),
            project_id,
            payload,
        }
    }
}

/// Webhook payload variants for different event types
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum WebhookPayload {
    Deployment(DeploymentPayload),
    Project(ProjectPayload),
    Domain(DomainPayload),
}

/// Deployment event payload
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeploymentPayload {
    pub deployment_id: i32,
    pub project_id: i32,
    pub project_name: String,
    pub environment: String,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,
    pub url: Option<String>,
    pub status: String,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Project event payload
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectPayload {
    pub project_id: i32,
    pub project_name: String,
    pub slug: String,
    pub repo_url: Option<String>,
}

/// Domain event payload
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DomainPayload {
    pub domain_id: i32,
    pub domain_name: String,
    pub project_id: i32,
    pub project_name: String,
    pub is_primary: bool,
    pub ssl_status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_serialization() {
        let event_type = WebhookEventType::DeploymentCreated;
        let json = serde_json::to_string(&event_type).unwrap();
        assert_eq!(json, "\"deployment_created\"");
    }

    #[test]
    fn test_event_type_from_str() {
        assert_eq!(
            WebhookEventType::from_str("deployment.created"),
            Some(WebhookEventType::DeploymentCreated)
        );
        assert_eq!(
            WebhookEventType::from_str("deployment_created"),
            Some(WebhookEventType::DeploymentCreated)
        );
        assert_eq!(WebhookEventType::from_str("invalid"), None);
    }

    #[test]
    fn test_event_type_display() {
        assert_eq!(
            WebhookEventType::DeploymentSucceeded.to_string(),
            "deployment.succeeded"
        );
    }

    #[test]
    fn test_webhook_event_creation() {
        let payload = WebhookPayload::Project(ProjectPayload {
            project_id: 1,
            project_name: "test-project".to_string(),
            slug: "test-project".to_string(),
            repo_url: Some("https://github.com/test/repo".to_string()),
        });

        let event = WebhookEvent::new(WebhookEventType::ProjectCreated, Some(1), payload);

        assert!(!event.id.is_empty());
        assert_eq!(event.event_type, WebhookEventType::ProjectCreated);
        assert_eq!(event.project_id, Some(1));
    }
}
