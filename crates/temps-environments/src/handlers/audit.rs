use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentSettingsUpdatedFields {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub branch: Option<String>,
    pub replicas: Option<i32>,
    pub security_updated: bool,
}

// Add these new audit structs after the other audit structs
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentSettingsUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_name: String,
    pub environment_slug: String,
    pub updated_settings: EnvironmentSettingsUpdatedFields,
}

impl AuditOperation for EnvironmentSettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_SETTINGS_UPDATED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
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
pub struct EnvironmentDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_name: String,
    pub environment_slug: String,
}

impl AuditOperation for EnvironmentDeletedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_DELETED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
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
