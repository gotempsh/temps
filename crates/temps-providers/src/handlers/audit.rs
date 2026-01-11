use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use temps_core::{AuditContext, AuditOperation};

// Add these after the other audit structs
#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceCreatedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub name: String,
    pub service_type: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceUpdatedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub name: String,
    pub service_type: String,
    pub updated_parameters: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceDeletedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub name: String,
    pub service_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceStatusChangedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub name: String,
    pub service_type: String,
    pub new_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceProjectLinkedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub project_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceProjectUnlinkedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub project_id: i32,
}

impl AuditOperation for ExternalServiceCreatedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_CREATED".to_string()
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

impl AuditOperation for ExternalServiceUpdatedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_UPDATED".to_string()
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

impl AuditOperation for ExternalServiceDeletedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_DELETED".to_string()
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

impl AuditOperation for ExternalServiceStatusChangedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_STATUS_CHANGED".to_string()
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

impl AuditOperation for ExternalServiceProjectLinkedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PROJECT_LINKED".to_string()
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

impl AuditOperation for ExternalServiceProjectUnlinkedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PROJECT_UNLINKED".to_string()
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
