// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Record that health changed without copying command arguments into audit data.
    pub health_updated: bool,
    /// Per-environment attack-mode override change (tri-state):
    /// `None` = unchanged, `Some(None)` = cleared (inherit project),
    /// `Some(Some(b))` = overridden.
    pub attack_mode: Option<Option<bool>>,
    /// Per-environment HTTP→HTTPS redirect override change (tri-state):
    /// `None` = unchanged, `Some(None)` = cleared (inherit the proxy default),
    /// `Some(Some(b))` = overridden.
    pub force_https: Option<Option<bool>>,
    /// Per-environment build location override change (tri-state):
    /// `None` = unchanged, `Some(None)` = cleared (inherit the project),
    /// `Some(Some(location))` = overridden.
    pub build_location: Option<Option<temps_entities::deployment_config::BuildLocation>>,
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
/// secret. The transition is one-way and permanently removes the value from
/// list responses, so it gets its own audit event rather than being folded
/// into a generic "variable updated" record.
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

/// Emitted when a project environment variable is created or updated.
///
/// Never carries the value, old or new: `value_rotated` only records whether a
/// new value was supplied. A promotion to secret additionally emits
/// [`EnvironmentVariablePromotedToSecretAudit`].
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVariableWrittenAudit {
    pub context: AuditContext,
    /// `ENVIRONMENT_VARIABLE_CREATED` or `ENVIRONMENT_VARIABLE_UPDATED`.
    #[serde(skip)]
    pub operation: &'static str,
    pub project_id: i32,
    pub var_id: i32,
    pub key: String,
    /// Environments the variable applies to afterwards.
    pub environment_ids: Vec<i32>,
    pub include_in_preview: bool,
    pub is_secret: bool,
    /// Fields an update changed (`key`, `value`, `environments`,
    /// `include_in_preview`, `is_secret`); empty for a create.
    pub changed_fields: Vec<&'static str>,
}

impl AuditOperation for EnvironmentVariableWrittenAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
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

/// Emitted when a project environment variable is deleted. Never carries the
/// value.
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVariableDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub var_id: i32,
    pub key: String,
    pub is_secret: bool,
    /// Environments the variable applied to before it was removed.
    pub environment_ids: Vec<i32>,
}

impl AuditOperation for EnvironmentVariableDeletedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_VARIABLE_DELETED".to_string()
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

/// Emitted when a project secret is created or updated. Never carries the
/// value: `value_rotated` only records whether a new value was supplied.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSecretWrittenAudit {
    pub context: AuditContext,
    /// `PROJECT_SECRET_CREATED` or `PROJECT_SECRET_UPDATED`.
    #[serde(skip)]
    pub operation: &'static str,
    pub project_id: i32,
    pub secret_id: i32,
    pub key: String,
    pub value_rotated: bool,
    /// Environments the secret applies to afterwards; empty means all.
    pub environment_ids: Vec<i32>,
    pub compose_services: Vec<String>,
    pub include_in_preview: bool,
}

impl AuditOperation for ProjectSecretWrittenAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
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
pub struct ProjectSecretDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub secret_id: i32,
    pub key: String,
}

impl AuditOperation for ProjectSecretDeletedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_SECRET_DELETED".to_string()
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

/// An environment's ingress mode or Temps login requirement changed
/// (ADR-055 §11).
#[derive(Debug, Clone, Serialize)]
pub struct IngressSettingsUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub previous_mode: &'static str,
    pub mode: &'static str,
    pub previous_require_login: bool,
    pub require_login: bool,
}

impl AuditOperation for IngressSettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "ENVIRONMENT_INGRESS_SETTINGS_UPDATED".to_string()
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

/// A user was issued a Temps login grant for an environment's host. The
/// grant token itself is never recorded.
#[derive(Debug, Clone, Serialize)]
pub struct IngressLoginGrantIssuedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub grant_id: i64,
    pub host: String,
    pub permission: String,
}

impl AuditOperation for IngressLoginGrantIssuedAudit {
    fn operation_type(&self) -> String {
        "INGRESS_LOGIN_GRANT_ISSUED".to_string()
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

/// A user was issued a Temps login grant for a sandbox preview host. The
/// grant token itself is never recorded.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxPreviewLoginGrantIssuedAudit {
    pub context: AuditContext,
    /// The sandbox's public id (`sbx_...`).
    pub sandbox_id: String,
    pub project_id: Option<i32>,
    pub grant_id: i64,
    pub host: String,
    pub permission: String,
}

impl AuditOperation for SandboxPreviewLoginGrantIssuedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_PREVIEW_LOGIN_GRANT_ISSUED".to_string()
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
        serde_json::to_string(self).map_err(|error| {
            anyhow::anyhow!(
                "Failed to serialize preview login grant audit for sandbox {}: {error}",
                self.sandbox_id
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_audit_events_never_include_a_value_field() {
        let context = AuditContext {
            user_id: 1,
            ip_address: None,
            user_agent: "test".into(),
        };
        let written = ProjectSecretWrittenAudit {
            context: context.clone(),
            operation: "PROJECT_SECRET_UPDATED",
            project_id: 10,
            secret_id: 5,
            key: "TLS_CERT".into(),
            value_rotated: true,
            environment_ids: vec![3],
            compose_services: vec![],
            include_in_preview: false,
        };
        assert_eq!(written.operation_type(), "PROJECT_SECRET_UPDATED");
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&written).unwrap()).unwrap();
        assert_eq!(json["secret_id"], 5);
        assert_eq!(json["value_rotated"], true);
        assert!(json.get("value").is_none());
        assert!(json.get("operation").is_none());
        let deleted = ProjectSecretDeletedAudit {
            context,
            project_id: 10,
            secret_id: 5,
            key: "TLS_CERT".into(),
        };
        assert_eq!(deleted.operation_type(), "PROJECT_SECRET_DELETED");
    }

    #[test]
    fn environment_variable_write_audits_never_include_a_value_field() {
        let context = AuditContext {
            user_id: 1,
            ip_address: None,
            user_agent: "test".into(),
        };
        let written = EnvironmentVariableWrittenAudit {
            context: context.clone(),
            operation: "ENVIRONMENT_VARIABLE_UPDATED",
            project_id: 10,
            var_id: 7,
            key: "DATABASE_URL".into(),
            environment_ids: vec![3, 4],
            include_in_preview: true,
            is_secret: false,
            changed_fields: vec!["value", "environments"],
        };
        assert_eq!(written.operation_type(), "ENVIRONMENT_VARIABLE_UPDATED");
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&written).unwrap()).unwrap();
        assert_eq!(json["var_id"], 7);
        assert_eq!(json["key"], "DATABASE_URL");
        assert_eq!(json["changed_fields"][0], "value");
        assert!(json.get("value").is_none());
        assert!(json.get("operation").is_none());

        let deleted = EnvironmentVariableDeletedAudit {
            context,
            project_id: 10,
            var_id: 7,
            key: "DATABASE_URL".into(),
            is_secret: true,
            environment_ids: vec![3],
        };
        assert_eq!(deleted.operation_type(), "ENVIRONMENT_VARIABLE_DELETED");
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&deleted).unwrap()).unwrap();
        assert_eq!(json["var_id"], 7);
        assert!(json.get("value").is_none());
    }
}
