// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit events for feature-flag mutations.
//!
//! A flag change is a production change with no deployment record and no code
//! review, so the audit trail is the only place it is written down. Every
//! mutation records the old and new value.

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

            fn serialize(&self) -> temps_core::AuditResult<String> {
                temps_core::serialize_audit_operation(self)
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
    pub variants: serde_json::Value,
    pub exposure_sample_rate: f64,
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
    pub old_variants: serde_json::Value,
    pub new_variants: serde_json::Value,
    pub old_exposure_sample_rate: f64,
    pub new_exposure_sample_rate: f64,
}

/// Targeting rules replaced in one environment. Records both lists in full:
/// a rule change alters who sees what in production with no deployment.
#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagRulesSetAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
    pub environment_id: i32,
    pub old_rules: Option<serde_json::Value>,
    pub new_rules: serde_json::Value,
}

/// The bucketing salt was regenerated: every rollout and split of the flag
/// was reassigned. The salt itself is never recorded.
#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagReshuffledAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
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

/// An archived flag deleted for good; its key is free again.
#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagPurgedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
    pub value_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeatureFlagRestoredAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub flag_id: i32,
    pub key: String,
}

impl_audit_operation!(FeatureFlagCreatedAudit, "FEATURE_FLAG_CREATED");
impl_audit_operation!(FeatureFlagRulesSetAudit, "FEATURE_FLAG_RULES_SET");
impl_audit_operation!(FeatureFlagReshuffledAudit, "FEATURE_FLAG_RESHUFFLED");
impl_audit_operation!(FeatureFlagRestoredAudit, "FEATURE_FLAG_RESTORED");
impl_audit_operation!(FeatureFlagUpdatedAudit, "FEATURE_FLAG_UPDATED");
impl_audit_operation!(FeatureFlagArchivedAudit, "FEATURE_FLAG_ARCHIVED");
impl_audit_operation!(FeatureFlagPurgedAudit, "FEATURE_FLAG_PURGED");
impl_audit_operation!(
    FeatureFlagEnvironmentValueSetAudit,
    "FEATURE_FLAG_ENVIRONMENT_VALUE_SET"
);
