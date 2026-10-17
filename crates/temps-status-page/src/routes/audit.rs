// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::Result;
use serde::Serialize;
use temps_core::AuditOperation;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusPageMutationAction {
    MonitorCreated,
    MonitorUpdated,
    MonitorDeleted,
    IncidentCreated,
    IncidentStatusUpdated,
    IncidentUpdateVisibilityChanged,
}

impl StatusPageMutationAction {
    fn operation_type(self) -> &'static str {
        match self {
            Self::MonitorCreated => "STATUS_PAGE_MONITOR_CREATED",
            Self::MonitorUpdated => "STATUS_PAGE_MONITOR_UPDATED",
            Self::MonitorDeleted => "STATUS_PAGE_MONITOR_DELETED",
            Self::IncidentCreated => "STATUS_PAGE_INCIDENT_CREATED",
            Self::IncidentStatusUpdated => "STATUS_PAGE_INCIDENT_STATUS_UPDATED",
            Self::IncidentUpdateVisibilityChanged => {
                "STATUS_PAGE_INCIDENT_UPDATE_VISIBILITY_CHANGED"
            }
        }
    }
}

/// Durable audit payload for status-page control-plane mutations.
///
/// `actor_user_id` is optional because deployment tokens do not represent a
/// user account. The project and resource identifiers are always present so
/// operators can reconstruct which tenant-owned object changed.
#[derive(Debug, Clone, Serialize)]
pub struct StatusPageMutationAudit {
    pub actor_user_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
    pub action: StatusPageMutationAction,
    pub project_id: i32,
    pub resource_type: &'static str,
    pub resource_id: i32,
    pub environment_id: Option<i32>,
    pub monitor_id: Option<i32>,
    pub status: Option<String>,
    /// Whether the incident update written by this mutation is public, for
    /// mutations that write one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,
}

impl AuditOperation for StatusPageMutationAudit {
    fn operation_type(&self) -> String {
        self.action.operation_type().to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.actor_user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|error| anyhow::anyhow!("failed to serialize status-page audit: {error}"))
    }
}

/// Lifecycle actions on a publishable status page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicStatusPageAction {
    Created,
    Updated,
    Published,
    Unpublished,
    UrlRotated,
    Deleted,
    DomainAdded,
    DomainVerified,
    DomainVerificationAttempted,
    DomainRenewalRequested,
    DomainRemoved,
}

impl PublicStatusPageAction {
    fn operation_type(self) -> &'static str {
        match self {
            Self::Created => "STATUS_PAGE_CREATED",
            Self::Updated => "STATUS_PAGE_UPDATED",
            Self::Published => "STATUS_PAGE_PUBLISHED",
            Self::Unpublished => "STATUS_PAGE_UNPUBLISHED",
            Self::UrlRotated => "STATUS_PAGE_URL_ROTATED",
            Self::Deleted => "STATUS_PAGE_DELETED",
            Self::DomainAdded => "STATUS_PAGE_DOMAIN_ADDED",
            Self::DomainVerified => "STATUS_PAGE_DOMAIN_VERIFIED",
            Self::DomainVerificationAttempted => "STATUS_PAGE_DOMAIN_VERIFICATION_ATTEMPTED",
            Self::DomainRenewalRequested => "STATUS_PAGE_DOMAIN_RENEWAL_REQUESTED",
            Self::DomainRemoved => "STATUS_PAGE_DOMAIN_REMOVED",
        }
    }
}

/// Durable audit payload for publishable status pages. Records the exact
/// monitor selection whenever it is set, so "who exposed which monitor, when"
/// can be reconstructed. The slug is deliberately not recorded: it is the
/// public capability, and audit readers do not need it to identify the page.
#[derive(Debug, Clone, Serialize)]
pub struct PublicStatusPageAudit {
    pub actor_user_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
    pub action: PublicStatusPageAction,
    pub project_id: i32,
    pub status_page_id: i32,
    pub title: Option<String>,
    pub is_published: Option<bool>,
    /// Selected monitor ids, in display order, when the selection was set.
    pub monitor_ids: Option<Vec<i32>>,
    /// Custom hostname added, verified or removed (#1412).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Result of a verification or renewal attempt: `verified`, `renewed`,
    /// `pending` or the error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl AuditOperation for PublicStatusPageAudit {
    fn operation_type(&self) -> String {
        self.action.operation_type().to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.actor_user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|error| {
            anyhow::anyhow!(
                "failed to serialize status page {} audit: {error}",
                self.status_page_id
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_audit_serializes_actor_tenant_and_resource_identity() {
        let audit = StatusPageMutationAudit {
            actor_user_id: Some(41),
            ip_address: Some("192.0.2.10".to_string()),
            user_agent: "status-page-test".to_string(),
            action: StatusPageMutationAction::IncidentStatusUpdated,
            project_id: 7,
            resource_type: "incident",
            resource_id: 19,
            environment_id: Some(11),
            monitor_id: Some(13),
            status: Some("resolved".to_string()),
            is_public: Some(true),
        };

        assert_eq!(
            audit.operation_type(),
            "STATUS_PAGE_INCIDENT_STATUS_UPDATED"
        );
        assert_eq!(audit.user_id(), Some(41));
        assert_eq!(audit.ip_address().as_deref(), Some("192.0.2.10"));
        assert_eq!(audit.user_agent(), "status-page-test");

        let serialized = AuditOperation::serialize(&audit).expect("audit should serialize");
        let payload: serde_json::Value =
            serde_json::from_str(&serialized).expect("serialized audit should be valid JSON");
        assert_eq!(payload["project_id"], 7);
        assert_eq!(payload["resource_type"], "incident");
        assert_eq!(payload["resource_id"], 19);
        assert_eq!(payload["environment_id"], 11);
        assert_eq!(payload["monitor_id"], 13);
        assert_eq!(payload["status"], "resolved");
        assert_eq!(payload["action"], "incident_status_updated");
        assert_eq!(payload["is_public"], true);
    }

    #[test]
    fn public_status_page_audit_records_selection_without_slug() {
        let audit = PublicStatusPageAudit {
            actor_user_id: Some(5),
            ip_address: None,
            user_agent: "status-page-test".to_string(),
            action: PublicStatusPageAction::Published,
            project_id: 9,
            status_page_id: 12,
            title: Some("Public status".to_string()),
            is_published: Some(true),
            monitor_ids: Some(vec![3, 1]),
            hostname: None,
            outcome: None,
        };
        assert_eq!(audit.operation_type(), "STATUS_PAGE_PUBLISHED");
        let serialized = AuditOperation::serialize(&audit).expect("audit should serialize");
        let payload: serde_json::Value =
            serde_json::from_str(&serialized).expect("serialized audit should be valid JSON");
        assert_eq!(payload["status_page_id"], 12);
        assert_eq!(payload["monitor_ids"], serde_json::json!([3, 1]));
        assert_eq!(payload["action"], "published");
        assert!(payload.get("slug").is_none());
    }
}
