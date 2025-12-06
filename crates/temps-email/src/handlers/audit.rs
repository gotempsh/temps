//! Audit types for the email service

use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

// ========================================
// Provider Audit Types
// ========================================

#[derive(Debug, Clone, Serialize)]
pub struct EmailProviderCreatedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    pub provider_type: String,
    pub region: String,
}

impl AuditOperation for EmailProviderCreatedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_PROVIDER_CREATED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailProviderDeletedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
}

impl AuditOperation for EmailProviderDeletedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_PROVIDER_DELETED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

/// Audit event for testing email provider
#[derive(Debug, Clone, Serialize)]
pub struct EmailProviderTestedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    pub recipient_email: String,
    pub success: bool,
    pub error: Option<String>,
}

impl AuditOperation for EmailProviderTestedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_PROVIDER_TESTED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

// ========================================
// Domain Audit Types
// ========================================

#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainCreatedAudit {
    pub context: AuditContext,
    pub domain_id: i32,
    pub domain: String,
    pub provider_id: i32,
}

impl AuditOperation for EmailDomainCreatedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_CREATED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainVerifiedAudit {
    pub context: AuditContext,
    pub domain_id: i32,
    pub domain: String,
    pub status: String,
}

impl AuditOperation for EmailDomainVerifiedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_VERIFIED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainDeletedAudit {
    pub context: AuditContext,
    pub domain_id: i32,
    pub domain: String,
}

impl AuditOperation for EmailDomainDeletedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_DELETED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

// ========================================
// Email Audit Types
// ========================================

#[derive(Debug, Clone, Serialize)]
pub struct EmailSentAudit {
    pub context: AuditContext,
    pub email_id: uuid::Uuid,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
}

impl AuditOperation for EmailSentAudit {
    fn operation_type(&self) -> String {
        "EMAIL_SENT".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}
