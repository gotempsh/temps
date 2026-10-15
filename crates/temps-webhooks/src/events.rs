// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Webhook event types and payload definitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// All supported webhook event types
///
/// The serialized form is the dotted name (`deployment.succeeded`): the same
/// string used to subscribe, in the `X-Webhook-Event` header and in the
/// payload's `event_type`. The legacy snake_case spelling
/// (`deployment_succeeded`) is still accepted on deserialization so stored
/// subscriptions and stored delivery payloads written before the names were
/// aligned keep parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum WebhookEventType {
    // Deployment events
    #[serde(rename = "deployment.created", alias = "deployment_created")]
    DeploymentCreated,
    #[serde(rename = "deployment.succeeded", alias = "deployment_succeeded")]
    DeploymentSucceeded,
    #[serde(rename = "deployment.failed", alias = "deployment_failed")]
    DeploymentFailed,
    #[serde(rename = "deployment.cancelled", alias = "deployment_cancelled")]
    DeploymentCancelled,
    #[serde(rename = "deployment.ready", alias = "deployment_ready")]
    DeploymentReady,

    // Project events
    #[serde(rename = "project.created", alias = "project_created")]
    ProjectCreated,
    #[serde(rename = "project.deleted", alias = "project_deleted")]
    ProjectDeleted,

    // Domain events
    #[serde(rename = "domain.created", alias = "domain_created")]
    DomainCreated,
    #[serde(rename = "domain.provisioned", alias = "domain_provisioned")]
    DomainProvisioned,

    // Backup events
    #[serde(rename = "backup.started", alias = "backup_started")]
    BackupStarted,
    #[serde(rename = "backup.completed", alias = "backup_completed")]
    BackupCompleted,
    #[serde(rename = "backup.failed", alias = "backup_failed")]
    BackupFailed,

    // Email events
    #[serde(rename = "email.delivered", alias = "email_delivered")]
    EmailDelivered,
    #[serde(rename = "email.bounced", alias = "email_bounced")]
    EmailBounced,
    #[serde(rename = "email.complained", alias = "email_complained")]
    EmailComplained,
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
            Self::BackupStarted,
            Self::BackupCompleted,
            Self::BackupFailed,
            Self::EmailDelivered,
            Self::EmailBounced,
            Self::EmailComplained,
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
            Self::BackupStarted => "backup.started",
            Self::BackupCompleted => "backup.completed",
            Self::BackupFailed => "backup.failed",
            Self::EmailDelivered => "email.delivered",
            Self::EmailBounced => "email.bounced",
            Self::EmailComplained => "email.complained",
        }
    }

    /// Parse event type from string
    #[allow(clippy::should_implement_trait)]
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
            "backup.started" | "backup_started" => Some(Self::BackupStarted),
            "backup.completed" | "backup_completed" => Some(Self::BackupCompleted),
            "backup.failed" | "backup_failed" => Some(Self::BackupFailed),
            "email.delivered" | "email_delivered" => Some(Self::EmailDelivered),
            "email.bounced" | "email_bounced" => Some(Self::EmailBounced),
            "email.complained" | "email_complained" => Some(Self::EmailComplained),
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
    Backup(BackupPayload),
    Email(EmailEventPayload),
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

/// Backup lifecycle event payload. Shared across `backup.started` /
/// `backup.completed` / `backup.failed` — `status` distinguishes which,
/// same convention as [`DeploymentPayload`]. `project_id`/`project_name`
/// are `None` when the backup's underlying service isn't linked to exactly
/// one project (unlinked, or linked to several) — a backup is scoped to a
/// service and schedule, not a project, unlike a deployment.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupPayload {
    pub backup_id: i32,
    pub engine: String,
    pub project_id: Option<i32>,
    pub project_name: Option<String>,
    pub status: String,
    pub s3_location: Option<String>,
    pub size_bytes: Option<i64>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Email event payload (bounces, complaints, deliveries)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmailEventPayload {
    pub email_id: String,
    pub event_type: String,
    pub recipient: Option<String>,
    pub bounce_type: Option<String>,
    pub complaint_type: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_serialization() {
        let event_type = WebhookEventType::DeploymentCreated;
        let json = serde_json::to_string(&event_type).unwrap();
        assert_eq!(json, "\"deployment.created\"");
    }

    #[test]
    fn serialized_event_name_matches_subscription_and_header_name_for_every_event() {
        for event_type in WebhookEventType::all() {
            let json = serde_json::to_string(&event_type).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", event_type.as_str()),
                "body event_type must equal the X-Webhook-Event / subscription name"
            );
            // from_str is the parser used for subscriptions: it must accept
            // exactly the name that is emitted.
            assert_eq!(
                WebhookEventType::from_str(event_type.as_str()),
                Some(event_type)
            );
        }
    }

    #[test]
    fn legacy_snake_case_event_names_still_deserialize() {
        for event_type in WebhookEventType::all() {
            let legacy = event_type.as_str().replace('.', "_");
            let parsed: WebhookEventType = serde_json::from_str(&format!("\"{legacy}\"")).unwrap();
            assert_eq!(parsed, event_type, "legacy name {legacy} must keep parsing");
        }
    }

    #[test]
    fn stored_legacy_event_payload_still_parses() {
        let legacy = r#"{"id":"evt-1","event_type":"deployment_succeeded","timestamp":"2025-11-24T18:38:10Z","project_id":1,"payload":{"project_id":1,"project_name":"p","slug":"p","repo_url":null}}"#;
        let event: WebhookEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(event.event_type, WebhookEventType::DeploymentSucceeded);
        let reserialized = serde_json::to_value(&event).unwrap();
        assert_eq!(reserialized["event_type"], "deployment.succeeded");
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
