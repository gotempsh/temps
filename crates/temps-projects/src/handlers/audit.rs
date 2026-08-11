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
    pub slug: Option<String>,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub performance_metrics_enabled: Option<bool>,
    /// Security-relevant Compose settings changed; values are deliberately
    /// omitted because preset_config may contain credentials.
    pub compose_configuration_updated: Option<bool>,
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
                slug: None,
                cpu_request: None,
                cpu_limit: None,
                memory_request: None,
                memory_limit: None,
                performance_metrics_enabled: None,
                compose_configuration_updated: Some(true),
            },
        };

        let serialized = AuditOperation::serialize(&audit).expect("audit should serialize");

        assert!(serialized.contains("\"compose_configuration_updated\":true"));
        assert!(!serialized.contains("composeOverride"));
        assert!(!serialized.contains("environment"));
    }
}
