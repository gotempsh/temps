// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::Result;
use serde::Serialize;
use temps_core::AuditOperation;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusPageMutationAction {
    MonitorCreated,
    MonitorDeleted,
    IncidentCreated,
    IncidentStatusUpdated,
}

impl StatusPageMutationAction {
    fn operation_type(self) -> &'static str {
        match self {
            Self::MonitorCreated => "STATUS_PAGE_MONITOR_CREATED",
            Self::MonitorDeleted => "STATUS_PAGE_MONITOR_DELETED",
            Self::IncidentCreated => "STATUS_PAGE_INCIDENT_CREATED",
            Self::IncidentStatusUpdated => "STATUS_PAGE_INCIDENT_STATUS_UPDATED",
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
    }
}
