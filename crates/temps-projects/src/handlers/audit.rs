// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::Result;
use serde::Serialize;
use temps_core::AuditOperation;

#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

// Add these new audit structs after the other audit structs
#[derive(Debug, Clone, Serialize)]
pub struct ProjectCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: Option<String>,
    pub automatic_deploy: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub updated_fields: ProjectUpdatedFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUpdatedFields {
    pub name: Option<String>,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: Option<String>,
    pub main_branch: Option<String>,
    pub preset: Option<String>,
    pub automatic_deploy: Option<bool>,
    /// Records that Compose routing, service selection, permissions, or the
    /// advanced override changed without persisting any secret-bearing YAML.
    pub compose_configuration_updated: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomDomainReassignmentRequestedAudit {
    pub context: AuditContext,
    pub custom_domain_id: i32,
    pub source_project_id: i32,
    pub target_project_id: i32,
    pub target_environment_id: i32,
}

impl AuditOperation for CustomDomainReassignmentRequestedAudit {
    fn operation_type(&self) -> String {
        "CUSTOM_DOMAIN_REASSIGNMENT_REQUESTED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        // AuditOperation's shared contract currently returns anyhow::Result;
        // preserve the serialization source while adding operation context.
        serde_json::to_string(self).map_err(|error| {
            anyhow::anyhow!("Failed to serialize domain reassignment request: {error}")
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomDomainReassignedAudit {
    pub context: AuditContext,
    pub custom_domain_id: i32,
    pub domain: String,
    pub source_project_id: i32,
    pub target_project_id: i32,
    pub target_environment_id: i32,
}

impl AuditOperation for CustomDomainReassignedAudit {
    fn operation_type(&self) -> String {
        "CUSTOM_DOMAIN_REASSIGNED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        // AuditOperation's shared contract currently returns anyhow::Result.
        serde_json::to_string(self)
            .map_err(|error| anyhow::anyhow!("Failed to serialize domain reassignment: {error}"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineTriggeredAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_slug: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
}

impl AuditOperation for PipelineTriggeredAudit {
    fn operation_type(&self) -> String {
        "PIPELINE_TRIGGERED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSettingsUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub updated_settings: ProjectSettingsUpdatedFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSettingsUpdatedFields {
    /// The project's persisted display name after a rename, set only when the
    /// name actually changed (the value is post-trim, so it is what reached the
    /// database — not the raw request). Audited so a project that suddenly
    /// reports under a different name in dashboards and alerts can be traced
    /// back to who renamed it.
    pub name: Option<String>,
    /// The display name immediately before this update. Set only alongside
    /// `name` — the two are recorded as a pair, because a previous name with no
    /// new name reads like a half-finished rename. Recorded because the new
    /// value alone doesn't tell an incident reviewer what the project used to be
    /// called, which is the whole question when older traces and alerts refer to
    /// it by the old name.
    pub previous_name: Option<String>,
    pub slug: Option<String>,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub performance_metrics_enabled: Option<bool>,
    /// Security-relevant Compose settings changed; values are deliberately
    /// omitted because preset_config may contain credentials.
    pub compose_configuration_updated: Option<bool>,
    /// New image-retention window, in hours. `Some(None)` records a reset back
    /// to the system default. Audited because shortening retention permanently
    /// destroys the project's ability to roll back to older deployments.
    pub image_retention_hours: Option<Option<i32>>,
    /// The project's `image_retention_hours` value immediately before this
    /// update, when `image_retention_hours` above is `Some`. `None` when the
    /// value is either not being changed or genuinely was unset. Recorded
    /// because the new value alone can't answer "how much rollback history
    /// did this just cost" during an incident review.
    pub previous_image_retention_hours: Option<i32>,
}

impl AuditOperation for ProjectCreatedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_CREATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ProjectUpdatedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_UPDATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ProjectDeletedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_DELETED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl AuditOperation for ProjectSettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_SETTINGS_UPDATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentConfigUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub updated_fields: std::collections::HashMap<String, String>,
}

impl AuditOperation for DeploymentConfigUpdatedAudit {
    fn operation_type(&self) -> String {
        "DEPLOYMENT_CONFIG_UPDATED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_settings_audit_records_change_without_secret_values() {
        let audit = ProjectSettingsUpdatedAudit {
            context: AuditContext {
                user_id: 7,
                ip_address: Some("127.0.0.1".to_string()),
                user_agent: "test-agent".to_string(),
            },
            project_id: 11,
            project_name: "example".to_string(),
            project_slug: "example".to_string(),
            updated_settings: ProjectSettingsUpdatedFields {
                name: None,
                previous_name: None,
                slug: None,
                cpu_request: None,
                cpu_limit: None,
                memory_request: None,
                memory_limit: None,
                performance_metrics_enabled: None,
                compose_configuration_updated: Some(true),
                image_retention_hours: None,
                previous_image_retention_hours: None,
            },
        };

        let serialized = AuditOperation::serialize(&audit).expect("audit should serialize");

        assert!(serialized.contains("\"compose_configuration_updated\":true"));
        assert!(!serialized.contains("composeOverride"));
        assert!(!serialized.contains("environment"));
    }
}
