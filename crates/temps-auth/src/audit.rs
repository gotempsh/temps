use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditOperation, AuditContext};

// Login audit
#[derive(Debug, Clone, Serialize)]
pub struct LoginAudit {
    pub context: AuditContext,
    pub success: bool,
    pub login_method: String,
}

// User management audits
#[derive(Debug, Clone, Serialize)]
pub struct UserCreatedAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub assigned_roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserUpdatedAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub new_values: UpdatedFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdatedFields {
    pub email: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserDeletedAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub email: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserRestoredAudit {
    pub context: AuditContext,
    pub target_user_id: i32,
    pub username: String,
    pub email: String,
    pub name: String,
}

// Role management audits
#[derive(Debug, Clone, Serialize)]
pub struct RoleAssignedAudit {
    pub context: AuditContext,
    pub username: String,
    pub target_user_id: i32,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleRemovedAudit {
    pub context: AuditContext,
    pub username: String,
    pub target_user_id: i32,
    pub role: String,
}

// MFA audits
#[derive(Debug, Clone, Serialize)]
pub struct MfaEnabledAudit {
    pub context: AuditContext,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MfaDisabledAudit {
    pub context: AuditContext,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MfaVerifiedAudit {
    pub context: AuditContext,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogoutAudit {
    pub context: AuditContext,
    pub username: String,
}

// Password reset audit
#[derive(Debug, Clone, Serialize)]
pub struct PasswordResetAudit {
    pub context: AuditContext,
    pub username: String,
}

// Email verification audit
#[derive(Debug, Clone, Serialize)]
pub struct EmailVerifiedAudit {
    pub context: AuditContext,
    pub username: String,
    pub email: String,
}

// Implement AuditOperation for each struct
impl AuditOperation for LoginAudit {
    fn operation_type(&self) -> String {
        format!("LOGIN_{}", if self.success { "SUCCESS" } else { "FAILURE" })
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

impl AuditOperation for UserCreatedAudit {
    fn operation_type(&self) -> String {
        "USER_CREATED".to_string()
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

impl AuditOperation for RoleAssignedAudit {
    fn operation_type(&self) -> String {
        "ROLE_ASSIGNED".to_string()
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

impl AuditOperation for RoleRemovedAudit {
    fn operation_type(&self) -> String {
        "ROLE_REMOVED".to_string()
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

impl AuditOperation for UserUpdatedAudit {
    fn operation_type(&self) -> String {
        "USER_UPDATED".to_string()
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

impl AuditOperation for UserDeletedAudit {
    fn operation_type(&self) -> String {
        "USER_DELETED".to_string()
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

impl AuditOperation for UserRestoredAudit {
    fn operation_type(&self) -> String {
        "USER_RESTORED".to_string()
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

impl AuditOperation for MfaEnabledAudit {
    fn operation_type(&self) -> String {
        "MFA_ENABLED".to_string()
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

impl AuditOperation for MfaDisabledAudit {
    fn operation_type(&self) -> String {
        "MFA_DISABLED".to_string()
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

impl AuditOperation for MfaVerifiedAudit {
    fn operation_type(&self) -> String {
        "MFA_VERIFIED".to_string()
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

impl AuditOperation for LogoutAudit {
    fn operation_type(&self) -> String {
        "USER_LOGOUT".to_string()
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

impl AuditOperation for PasswordResetAudit {
    fn operation_type(&self) -> String {
        "PASSWORD_RESET".to_string()
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

impl AuditOperation for EmailVerifiedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_VERIFIED".to_string()
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
