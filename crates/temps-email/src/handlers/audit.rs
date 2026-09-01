// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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
pub struct EmailProviderUpdatedAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    pub provider_type: String,
    /// Names of fields that changed (e.g. ["name", "region", "credentials"]).
    /// Listing fields — not values — keeps the audit log free of secret material.
    pub changed_fields: Vec<String>,
}

impl AuditOperation for EmailProviderUpdatedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_PROVIDER_UPDATED".to_string()
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

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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

/// Audit event for running the AWS-side event-tracking setup
#[derive(Debug, Clone, Serialize)]
pub struct EmailProviderTrackingSetupAudit {
    pub context: AuditContext,
    pub provider_id: i32,
    pub name: String,
    pub topic_arn: String,
    pub webhook_url: String,
}

impl AuditOperation for EmailProviderTrackingSetupAudit {
    fn operation_type(&self) -> String {
        "EMAIL_PROVIDER_TRACKING_SETUP".to_string()
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

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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

#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainProjectAuthorizedAudit {
    pub context: AuditContext,
    pub correlation_id: uuid::Uuid,
    pub domain_id: i32,
    pub project_id: i32,
    pub success: bool,
}

/// Durable intent record written before changing a project/domain grant.
///
/// The corresponding result event is still emitted after the mutation, but
/// requiring this event first guarantees that a privileged change is never
/// performed without an attributable audit record.
#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainProjectChangeRequestedAudit {
    pub context: AuditContext,
    pub correlation_id: uuid::Uuid,
    pub domain_id: i32,
    pub project_id: i32,
    pub action: String,
}

impl AuditOperation for EmailDomainProjectChangeRequestedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_PROJECT_CHANGE_REQUESTED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

impl AuditOperation for EmailDomainProjectAuthorizedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_PROJECT_AUTHORIZED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainProjectRevokedAudit {
    pub context: AuditContext,
    pub correlation_id: uuid::Uuid,
    pub domain_id: i32,
    pub project_id: i32,
    pub success: bool,
}

impl AuditOperation for EmailDomainProjectRevokedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_PROJECT_REVOKED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

impl AuditOperation for EmailDomainDeletedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_DOMAIN_DELETED".to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

// ========================================
// Email Audit Types
// ========================================

/// Durable intent record written before any provider call is allowed.
#[derive(Debug, Clone, Serialize)]
pub struct EmailSendRequestedAudit {
    pub user_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
    pub deployment_principal: Option<DeploymentEmailPrincipal>,
    pub correlation_id: uuid::Uuid,
    pub sender_domain: String,
    pub recipient_count: usize,
    pub request_fingerprint: String,
}

impl AuditOperation for EmailSendRequestedAudit {
    fn operation_type(&self) -> String {
        "EMAIL_SEND_REQUESTED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailSentAudit {
    pub user_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
    pub deployment_principal: Option<DeploymentEmailPrincipal>,
    pub correlation_id: uuid::Uuid,
    pub email_id: uuid::Uuid,
    pub sender_domain: String,
    pub recipient_count: usize,
    pub request_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentEmailPrincipal {
    pub token_id: i32,
    pub token_name: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

impl AuditOperation for EmailSentAudit {
    fn operation_type(&self) -> String {
        // This records the result returned for a request. A replay may produce
        // another completion event, but never another provider delivery event.
        "EMAIL_SEND_REQUEST_COMPLETED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> AuditContext {
        AuditContext {
            user_id: 7,
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: "audit-test".to_string(),
        }
    }

    #[test]
    fn project_sender_grants_and_revocations_are_distinct_attributable_events() {
        let correlation_id = uuid::Uuid::new_v4();
        let requested = EmailDomainProjectChangeRequestedAudit {
            context: context(),
            correlation_id,
            domain_id: 2,
            project_id: 70,
            action: "authorize".to_string(),
        };
        let authorized = EmailDomainProjectAuthorizedAudit {
            context: context(),
            correlation_id,
            domain_id: 2,
            project_id: 70,
            success: true,
        };
        let revoked = EmailDomainProjectRevokedAudit {
            context: context(),
            correlation_id,
            domain_id: 2,
            project_id: 70,
            success: false,
        };

        assert_eq!(
            authorized.operation_type(),
            "EMAIL_DOMAIN_PROJECT_AUTHORIZED"
        );
        assert_eq!(
            requested.operation_type(),
            "EMAIL_DOMAIN_PROJECT_CHANGE_REQUESTED"
        );
        assert_eq!(revoked.operation_type(), "EMAIL_DOMAIN_PROJECT_REVOKED");
        let authorized_json = AuditOperation::serialize(&authorized).unwrap();
        let revoked_json = AuditOperation::serialize(&revoked).unwrap();
        for serialized in [&authorized_json, &revoked_json] {
            assert!(serialized.contains("\"domain_id\":2"));
            assert!(serialized.contains("\"project_id\":70"));
            assert!(serialized.contains("\"user_id\":7"));
            assert!(serialized.contains("127.0.0.1"));
        }
        assert!(authorized_json.contains("\"success\":true"));
        assert!(revoked_json.contains("\"success\":false"));
    }

    #[test]
    fn deployment_email_audit_uses_machine_principal_without_fake_user() {
        let audit = EmailSentAudit {
            user_id: None,
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: "deployment-agent".to_string(),
            deployment_principal: Some(DeploymentEmailPrincipal {
                token_id: 9,
                token_name: "production-mailer".to_string(),
                project_id: 70,
                environment_id: Some(4),
                deployment_id: Some(12),
            }),
            correlation_id: uuid::Uuid::new_v4(),
            email_id: uuid::Uuid::nil(),
            sender_domain: "example.test".to_string(),
            recipient_count: 1,
            request_fingerprint: "sha256-fixture".to_string(),
        };

        assert_eq!(AuditOperation::user_id(&audit), None);
        assert_eq!(
            AuditOperation::operation_type(&audit),
            "EMAIL_SEND_REQUEST_COMPLETED"
        );
        let serialized = AuditOperation::serialize(&audit).unwrap();
        assert!(serialized.contains("\"user_id\":null"));
        assert!(serialized.contains("\"token_id\":9"));
        assert!(serialized.contains("\"project_id\":70"));
        assert!(!serialized.contains("\"user_id\":0"));
        assert!(!serialized.contains("recipient@example.test"));
        assert!(!serialized.contains("Status update"));
    }
}
