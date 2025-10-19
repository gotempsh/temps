use serde::Serialize;
use temps_core::AuditOperation;
use anyhow::Result;


#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

// Add this after the other audit structs
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


