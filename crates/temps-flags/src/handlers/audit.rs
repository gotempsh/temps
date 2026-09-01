// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit events for feature-flag mutations.
//!
//! A flag change is a production change with no deployment record and no code
//! review, so the audit trail is the only place it is written down. Every
//! mutation records the old and new value.

use anyhow::Result;
use serde::Serialize;
pub use temps_core::AuditContext;
use temps_core::AuditOperation;

macro_rules! impl_audit_operation {
    ($ty:ty, $op:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $op.to_string()
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
                    .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
            }
        }
    };
}

#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
    pub value_type: String,
    pub default_value: serde_json::Value,
    pub client_visible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
    pub old_default_value: serde_json::Value,
    pub new_default_value: serde_json::Value,
    pub old_client_visible: bool,
    pub new_client_visible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagArchivedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
}

/// The one that matters operationally: this is the record of who turned a
/// feature on or off in production, and what it was set to before.
#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagEnvironmentValueSetAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
    pub environment_id: i32,
    pub old_enabled: Option<bool>,
    pub new_enabled: bool,
    pub old_value: Option<serde_json::Value>,
    pub new_value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagRestoredAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
}

impl_audit_operation!(FeatureFlagCreatedAudit, "FEATURE_FLAG_CREATED");
impl_audit_operation!(FeatureFlagRestoredAudit, "FEATURE_FLAG_RESTORED");
impl_audit_operation!(FeatureFlagUpdatedAudit, "FEATURE_FLAG_UPDATED");
impl_audit_operation!(FeatureFlagArchivedAudit, "FEATURE_FLAG_ARCHIVED");
impl_audit_operation!(
    FeatureFlagEnvironmentValueSetAudit,
    "FEATURE_FLAG_ENVIRONMENT_VALUE_SET"
);
