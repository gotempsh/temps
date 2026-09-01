// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::Result;
use serde::Serialize;
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
    pub updated_parameter_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceParameterRevealedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub parameter_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceEnvironmentVariableRevealedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub project_id: i32,
    pub variable_name: String,
}

/// Bulk reveal of a service's basic (non-docker, non-runtime) environment
/// variables in plaintext, before the service is linked to any project —
/// e.g. so the new-project wizard can offer "fill from service" for a
/// connection-string variable like `DATABASE_URL`. Project-agnostic by
/// necessity (no project exists yet), unlike
/// [`ExternalServiceEnvironmentVariableRevealedAudit`].
#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceEnvironmentVariablesRevealedAudit {
    pub context: AuditContext,
    pub service_id: i32,
}

/// Live connection credentials were issued for a service in an environment,
/// and the per-tenant database was provisioned if it did not exist.
///
/// Separate from [`ExternalServiceEnvironmentVariableRevealedAudit`] because
/// it is a stronger event: that one reveals a single named variable a human
/// asked for, this one hands out the whole connection — including the
/// password — and creates a database as a side effect. The variable *names*
/// are recorded so an auditor can see what was handed over; the values
/// obviously are not.
#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceRuntimeCredentialsIssuedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    /// Names only, sorted. Never the values.
    pub variable_names: Vec<String>,
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

/// Operator changed whether the AI assistant may read this service's row data.
///
/// Audited because it widens what a third party (the configured AI provider)
/// can see: rows may contain password hashes, tokens and personal data. Both
/// directions are recorded so the trail shows when access was opened *and*
/// closed.
#[derive(Debug, Clone, Serialize)]
pub struct AiDataAccessChangedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub enabled: bool,
}

impl AuditOperation for AiDataAccessChangedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_AI_DATA_ACCESS_CHANGED".to_string()
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

/// The AI assistant actually read row data from a service.
///
/// The `ai_data_access` toggle is audited, but the toggle only records that an
/// operator opened the door — not what went through it. The whole reason the
/// opt-in exists is that rows can carry password hashes, tokens and personal
/// data, and that enabling it sends them to a third-party model provider. After
/// a suspected prompt injection the operator's first question is "what did the
/// model see?", and without this record there is nothing to answer it with.
///
/// Deliberately records only stable IDs and fixed categories. Service names,
/// container paths and entity names are omitted because key-value/object
/// backends routinely store emails, tokens and filenames in those identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiBackendCategory {
    Relational,
    Document,
    KeyValue,
    ObjectStore,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiEntityCategory {
    Table,
    Collection,
    Key,
    Object,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiRowsReadAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub backend_category: AiBackendCategory,
    pub entity_category: AiEntityCategory,
    pub returned_rows: usize,
    pub truncated: bool,
    /// Bounded structural category only; literal filter values are never
    /// persisted because they commonly contain emails, tokens, and other PII.
    pub filter_shape: Option<String>,
}

impl AuditOperation for AiRowsReadAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_AI_ROWS_READ".to_string()
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

#[derive(Debug, Clone, Serialize)]
pub struct ServiceHealthChecked {
    pub context: AuditContext,
    pub service_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceClusterMemberAddedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub member_id: i32,
    pub role: String,
    pub ordinal: i32,
    pub node_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceClusterMemberRemovedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub member_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceClusterMemberPromotedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub service_name: String,
    pub member_id: i32,
    pub container_name: String,
}

impl AuditOperation for ExternalServiceCreatedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_CREATED".to_string()
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

impl AuditOperation for ExternalServiceUpdatedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_UPDATED".to_string()
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

impl AuditOperation for ExternalServiceParameterRevealedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PARAMETER_REVEALED".to_string()
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

impl AuditOperation for ExternalServiceEnvironmentVariableRevealedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_ENVIRONMENT_VARIABLE_REVEALED".to_string()
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

impl AuditOperation for ExternalServiceEnvironmentVariablesRevealedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_ENVIRONMENT_VARIABLES_REVEALED".to_string()
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

impl AuditOperation for ExternalServiceRuntimeCredentialsIssuedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_RUNTIME_CREDENTIALS_ISSUED".to_string()
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

impl AuditOperation for ExternalServiceDeletedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_DELETED".to_string()
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

impl AuditOperation for ExternalServiceStatusChangedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_STATUS_CHANGED".to_string()
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

impl AuditOperation for ExternalServiceProjectLinkedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PROJECT_LINKED".to_string()
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

impl AuditOperation for ExternalServiceProjectUnlinkedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PROJECT_UNLINKED".to_string()
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

impl AuditOperation for ServiceHealthChecked {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_HEALTH_CHECK_TRIGGERED".to_string()
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

impl AuditOperation for ExternalServiceClusterMemberAddedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_CLUSTER_MEMBER_ADDED".to_string()
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

impl AuditOperation for ExternalServiceClusterMemberRemovedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_CLUSTER_MEMBER_REMOVED".to_string()
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

impl AuditOperation for ExternalServiceClusterMemberPromotedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_CLUSTER_MEMBER_PROMOTED".to_string()
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
    fn ai_row_read_audit_never_serializes_filter_literals() {
        let audit = AiRowsReadAudit {
            context: AuditContext {
                user_id: 42,
                ip_address: None,
                user_agent: "test-agent".to_string(),
            },
            service_id: 7,
            backend_category: AiBackendCategory::Relational,
            entity_category: AiEntityCategory::Table,
            returned_rows: 1,
            truncated: false,
            filter_shape: Some("sql_where".to_string()),
        };

        let serialized = AuditOperation::serialize(&audit).expect("audit serializes");
        assert!(serialized.contains("sql_where"));
        assert!(!serialized.contains("alice@example.com"));
        assert!(!serialized.contains("tok_live_secret"));
    }

    #[test]
    fn ai_row_read_audit_omits_secret_bearing_identifiers() {
        // These are the raw values available at the handler boundary. The
        // audit type has no fields capable of accepting them.
        let service_name = "service-alice@example.com";
        let container_path = "bucket/tok_live_secret";
        let entity = "session:alice@example.com:tok_live_secret";
        let audit = AiRowsReadAudit {
            context: AuditContext {
                user_id: 42,
                ip_address: None,
                user_agent: "test-agent".to_string(),
            },
            service_id: 7,
            backend_category: AiBackendCategory::KeyValue,
            entity_category: AiEntityCategory::Key,
            returned_rows: 1,
            truncated: false,
            filter_shape: Some("structured_object".to_string()),
        };
        let serialized = AuditOperation::serialize(&audit).expect("audit serializes");

        for secret_identifier in [service_name, container_path, entity] {
            assert!(!serialized.contains(secret_identifier));
        }
        assert!(serialized.contains("key_value"));
        assert!(serialized.contains("entity_category"));
    }
}
