use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVariableValueRevealedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub key: String,
    pub var_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub service_id: Option<i32>,
    pub source: &'static str,
}

impl AuditOperation for EnvironmentVariableValueRevealedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_VARIABLE_VALUE_REVEALED".to_string()
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
            .map_err(|error| anyhow::anyhow!("Failed to serialize audit operation: {error}"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentSettingsUpdatedFields {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub branch: Option<String>,
    pub replicas: Option<i32>,
    pub security_updated: bool,
    /// Per-environment attack-mode override change (tri-state):
    /// `None` = unchanged, `Some(None)` = cleared (inherit project),
    /// `Some(Some(b))` = overridden.
    pub attack_mode: Option<Option<bool>>,
    /// Per-environment HTTP→HTTPS redirect override change (tri-state):
    /// `None` = unchanged, `Some(None)` = cleared (inherit the proxy default),
    /// `Some(Some(b))` = overridden.
    pub force_https: Option<Option<bool>>,
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
pub struct EnvironmentSleepStateChangedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub environment_slug: String,
    pub previous_state: &'static str,
    pub new_state: &'static str,
}

impl AuditOperation for EnvironmentSleepStateChangedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_SLEEP_STATE_CHANGED".to_string()
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
pub struct EnvironmentSubdomainUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_name: String,
    pub environment_slug: String,
    pub previous_subdomain: String,
    pub new_subdomain: String,
}

impl AuditOperation for EnvironmentSubdomainUpdatedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_SUBDOMAIN_UPDATED".to_string()
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

/// Emitted when an existing environment variable is converted into a
/// write-only secret. The transition is one-way and permanently removes the
/// value from every read path, so it gets its own audit event rather than
/// being folded into a generic "variable updated" record.
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVariablePromotedToSecretAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub var_id: i32,
    pub key: String,
    /// Environments the variable applies to after the update.
    pub environment_ids: Vec<i32>,
}

impl AuditOperation for EnvironmentVariablePromotedToSecretAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_VARIABLE_PROMOTED_TO_SECRET".to_string()
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
