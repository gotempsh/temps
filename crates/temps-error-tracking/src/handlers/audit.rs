// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit types for error-tracking write operations.
//!
//! Uploading and deleting application source is a write on sensitive data
//! (customer source code stored at rest), so both operations emit an audit log.
//! Triage changes to an error group (status / assignee) are writes too and are
//! audited the same way, as are alert-rule changes, source-map uploads and
//! deletions, and the DSN credential lifecycle (create / rotate / revoke).
//!
//! Audit payloads carry identifiers only. A DSN's public key is the ingest
//! credential, so it is never recorded -- only the DSN row id, its project and
//! its environment/deployment scope.

use anyhow::Result;
use serde::Serialize;
pub use temps_core::AuditContext;
use temps_core::AuditOperation;

/// Audit event for uploading a source file (native symbolication).
#[derive(Debug, Clone, Serialize)]
pub struct SourceFileUploadedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub release: String,
    pub file_path: String,
    pub size_bytes: i64,
}

/// Audit event for deleting all source files for a release.
#[derive(Debug, Clone, Serialize)]
pub struct SourceFilesDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub release: String,
    pub deleted_count: u64,
}

/// Audit event for a triage update on an error group (status and/or assignee).
#[derive(Debug, Clone, Serialize)]
pub struct ErrorGroupUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub group_id: i32,
    /// Status the group was set to.
    pub status: String,
    /// Assignee change as requested: `None` leaves it unchanged, `Some("")`
    /// clears it, any other value sets it.
    pub assigned_to: Option<String>,
}

impl AuditOperation for ErrorGroupUpdatedAudit {
    fn operation_type(&self) -> String {
        "ERROR_GROUP_UPDATED".to_string()
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

impl AuditOperation for SourceFileUploadedAudit {
    fn operation_type(&self) -> String {
        "SOURCE_FILE_UPLOADED".to_string()
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

impl AuditOperation for SourceFilesDeletedAudit {
    fn operation_type(&self) -> String {
        "SOURCE_FILES_DELETED".to_string()
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

/// Implements [`AuditOperation`] for an audit struct whose actor is the
/// authenticated user in its `context: AuditContext` field.
macro_rules! impl_user_audit_operation {
    ($ty:ty, $operation:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $operation.to_string()
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
                serde_json::to_string(self).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize {} audit operation: {}", $operation, e)
                })
            }
        }
    };
}

// ===== Alert rules =====

/// Audit event for creating an error alert rule.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorAlertRuleCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub rule_id: i32,
    pub name: String,
    pub trigger_type: String,
    pub enabled: bool,
}
impl_user_audit_operation!(ErrorAlertRuleCreatedAudit, "ERROR_ALERT_RULE_CREATED");

/// Audit event for updating an error alert rule. Records the rule's state
/// after the update.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorAlertRuleUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub rule_id: i32,
    pub name: String,
    pub trigger_type: String,
    pub enabled: bool,
}
impl_user_audit_operation!(ErrorAlertRuleUpdatedAudit, "ERROR_ALERT_RULE_UPDATED");

/// Audit event for deleting an error alert rule.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorAlertRuleDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub rule_id: i32,
}
impl_user_audit_operation!(ErrorAlertRuleDeletedAudit, "ERROR_ALERT_RULE_DELETED");

// ===== Source maps =====

/// Audit event for uploading (or replacing) a source map for a release.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMapUploadedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub source_map_id: i32,
    pub release: String,
    pub file_path: String,
    pub dist: Option<String>,
    pub size_bytes: i64,
}
impl_user_audit_operation!(SourceMapUploadedAudit, "SOURCE_MAP_UPLOADED");

/// Audit event for deleting every source map of a release.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMapsDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub release: String,
    pub deleted_count: u64,
}
impl_user_audit_operation!(SourceMapsDeletedAudit, "SOURCE_MAPS_DELETED");

/// Audit event for deleting a single source map by id.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMapDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub source_map_id: i32,
}
impl_user_audit_operation!(SourceMapDeletedAudit, "SOURCE_MAP_DELETED");

// ===== DSN credential lifecycle =====

/// Audit event for minting a new DSN (an ingest credential).
///
/// Deliberately has no field for the public key or the DSN string.
#[derive(Debug, Clone, Serialize)]
pub struct DsnCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub dsn_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: String,
}
impl_user_audit_operation!(DsnCreatedAudit, "DSN_CREATED");

/// Audit event for rotating a DSN's key. The old and new keys are never
/// recorded.
#[derive(Debug, Clone, Serialize)]
pub struct DsnRegeneratedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub dsn_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}
impl_user_audit_operation!(DsnRegeneratedAudit, "DSN_REGENERATED");

/// Audit event for revoking (deactivating) a DSN.
#[derive(Debug, Clone, Serialize)]
pub struct DsnRevokedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub dsn_id: i32,
}
impl_user_audit_operation!(DsnRevokedAudit, "DSN_REVOKED");

// ===== sentry-cli compatible API (token-authenticated, no user) =====

/// Which credential authenticated a sentry-cli request. Only the row id is
/// recorded, never the token or key value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum SentryCliCredential {
    /// A deployment token (`deployment_tokens.id`).
    DeploymentToken(i32),
    /// A project DSN public key (`project_dsns.id`).
    Dsn(i32),
}

/// Request origin for a sentry-cli request. There is no user account behind
/// these calls, so the audit actor is `None` and the credential is recorded
/// instead.
#[derive(Debug, Clone, Serialize)]
pub struct SentryCliAuditContext {
    pub credential: SentryCliCredential,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// What a sentry-cli release call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SentryCliReleaseAction {
    /// `POST /0/organizations/{org}/releases/` or
    /// `POST /0/projects/{org}/{project}/releases/`.
    Created,
    /// `PUT /0/projects/{org}/{project}/releases/{version}/`.
    Finalized,
}

/// Audit event for a sentry-cli release create/finalize call.
#[derive(Debug, Clone, Serialize)]
pub struct SentryCliReleaseAudit {
    pub context: SentryCliAuditContext,
    pub project_id: i32,
    pub version: String,
    pub action: SentryCliReleaseAction,
}

impl AuditOperation for SentryCliReleaseAudit {
    fn operation_type(&self) -> String {
        match self.action {
            SentryCliReleaseAction::Created => "SENTRY_CLI_RELEASE_CREATED".to_string(),
            SentryCliReleaseAction::Finalized => "SENTRY_CLI_RELEASE_FINALIZED".to_string(),
        }
    }

    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize sentry-cli release audit for project {}: {}",
                self.project_id,
                e
            )
        })
    }
}

/// Audit event for a source map stored through the sentry-cli upload API.
#[derive(Debug, Clone, Serialize)]
pub struct SentryCliReleaseFileUploadedAudit {
    pub context: SentryCliAuditContext,
    pub project_id: i32,
    pub source_map_id: i32,
    pub version: String,
    pub file_path: String,
    pub dist: Option<String>,
    pub size_bytes: i64,
}

impl AuditOperation for SentryCliReleaseFileUploadedAudit {
    fn operation_type(&self) -> String {
        "SENTRY_CLI_RELEASE_FILE_UPLOADED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize sentry-cli release file audit for project {}: {}",
                self.project_id,
                e
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_group_updated_audit_serializes_ids_and_change() {
        let audit = ErrorGroupUpdatedAudit {
            context: AuditContext {
                user_id: 7,
                ip_address: Some("10.0.0.1".to_string()),
                user_agent: "test-agent".to_string(),
            },
            project_id: 3,
            group_id: 42,
            status: "resolved".to_string(),
            assigned_to: Some(String::new()),
        };

        assert_eq!(audit.operation_type(), "ERROR_GROUP_UPDATED");
        assert_eq!(AuditOperation::user_id(&audit), Some(7));
        assert_eq!(audit.ip_address().as_deref(), Some("10.0.0.1"));
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).expect("serialize"))
                .expect("valid json");
        assert_eq!(json["project_id"], 3);
        assert_eq!(json["group_id"], 42);
        assert_eq!(json["status"], "resolved");
        assert_eq!(json["assigned_to"], "");
    }

    fn ctx() -> AuditContext {
        AuditContext {
            user_id: 9,
            ip_address: Some("198.51.100.4".to_string()),
            user_agent: "audit-test".to_string(),
        }
    }

    fn payload(op: &dyn AuditOperation) -> serde_json::Value {
        serde_json::from_str(&op.serialize().expect("serialize")).expect("valid json")
    }

    #[test]
    fn alert_rule_audits_serialize_ids() {
        let created = ErrorAlertRuleCreatedAudit {
            context: ctx(),
            project_id: 3,
            rule_id: 17,
            name: "Regression detected".to_string(),
            trigger_type: "regression".to_string(),
            enabled: true,
        };
        assert_eq!(created.operation_type(), "ERROR_ALERT_RULE_CREATED");
        assert_eq!(AuditOperation::user_id(&created), Some(9));
        let json = payload(&created);
        assert_eq!(json["project_id"], 3);
        assert_eq!(json["rule_id"], 17);
        assert_eq!(json["trigger_type"], "regression");
        assert_eq!(json["context"]["ip_address"], "198.51.100.4");

        let updated = ErrorAlertRuleUpdatedAudit {
            context: ctx(),
            project_id: 3,
            rule_id: 17,
            name: "Renamed".to_string(),
            trigger_type: "regression".to_string(),
            enabled: false,
        };
        assert_eq!(updated.operation_type(), "ERROR_ALERT_RULE_UPDATED");
        let json = payload(&updated);
        assert_eq!(json["rule_id"], 17);
        assert_eq!(json["enabled"], false);

        let deleted = ErrorAlertRuleDeletedAudit {
            context: ctx(),
            project_id: 3,
            rule_id: 17,
        };
        assert_eq!(deleted.operation_type(), "ERROR_ALERT_RULE_DELETED");
        let json = payload(&deleted);
        assert_eq!(json["project_id"], 3);
        assert_eq!(json["rule_id"], 17);
    }

    #[test]
    fn source_map_audits_serialize_ids() {
        let uploaded = SourceMapUploadedAudit {
            context: ctx(),
            project_id: 3,
            source_map_id: 55,
            release: "1.2.3".to_string(),
            file_path: "~/static/app.js".to_string(),
            dist: None,
            size_bytes: 1024,
        };
        assert_eq!(uploaded.operation_type(), "SOURCE_MAP_UPLOADED");
        let json = payload(&uploaded);
        assert_eq!(json["source_map_id"], 55);
        assert_eq!(json["release"], "1.2.3");
        assert_eq!(json["size_bytes"], 1024);

        let deleted_release = SourceMapsDeletedAudit {
            context: ctx(),
            project_id: 3,
            release: "1.2.3".to_string(),
            deleted_count: 4,
        };
        assert_eq!(deleted_release.operation_type(), "SOURCE_MAPS_DELETED");
        assert_eq!(payload(&deleted_release)["deleted_count"], 4);

        let deleted_one = SourceMapDeletedAudit {
            context: ctx(),
            project_id: 3,
            source_map_id: 55,
        };
        assert_eq!(deleted_one.operation_type(), "SOURCE_MAP_DELETED");
        assert_eq!(payload(&deleted_one)["source_map_id"], 55);
    }

    /// The DSN audit structs must never grow a field that could carry the
    /// credential: assert the full key set, not just the presence of ids.
    #[test]
    fn dsn_audits_serialize_ids_and_never_the_key() {
        fn keys(json: &serde_json::Value) -> Vec<String> {
            let mut keys: Vec<String> = json.as_object().expect("object").keys().cloned().collect();
            keys.sort();
            keys
        }

        let created = DsnCreatedAudit {
            context: ctx(),
            project_id: 3,
            dsn_id: 21,
            environment_id: Some(4),
            deployment_id: None,
            name: "Project DSN".to_string(),
        };
        assert_eq!(created.operation_type(), "DSN_CREATED");
        assert_eq!(AuditOperation::user_id(&created), Some(9));
        let json = payload(&created);
        assert_eq!(json["dsn_id"], 21);
        assert_eq!(json["environment_id"], 4);
        assert_eq!(
            keys(&json),
            [
                "context",
                "deployment_id",
                "dsn_id",
                "environment_id",
                "name",
                "project_id"
            ]
        );

        let regenerated = DsnRegeneratedAudit {
            context: ctx(),
            project_id: 3,
            dsn_id: 21,
            environment_id: Some(4),
            deployment_id: None,
        };
        assert_eq!(regenerated.operation_type(), "DSN_REGENERATED");
        assert_eq!(
            keys(&payload(&regenerated)),
            [
                "context",
                "deployment_id",
                "dsn_id",
                "environment_id",
                "project_id"
            ]
        );

        let revoked = DsnRevokedAudit {
            context: ctx(),
            project_id: 3,
            dsn_id: 21,
        };
        assert_eq!(revoked.operation_type(), "DSN_REVOKED");
        assert_eq!(
            keys(&payload(&revoked)),
            ["context", "dsn_id", "project_id"]
        );
    }

    #[test]
    fn sentry_cli_audits_have_no_user_and_record_credential_id_only() {
        let context = SentryCliAuditContext {
            credential: SentryCliCredential::DeploymentToken(8),
            ip_address: Some("192.0.2.10".to_string()),
            user_agent: "sentry-cli/2".to_string(),
        };

        let created = SentryCliReleaseAudit {
            context: context.clone(),
            project_id: 3,
            version: "abc123".to_string(),
            action: SentryCliReleaseAction::Created,
        };
        assert_eq!(created.operation_type(), "SENTRY_CLI_RELEASE_CREATED");
        assert_eq!(AuditOperation::user_id(&created), None);
        assert_eq!(created.ip_address().as_deref(), Some("192.0.2.10"));
        let json = payload(&created);
        assert_eq!(json["context"]["credential"]["type"], "deployment_token");
        assert_eq!(json["context"]["credential"]["id"], 8);
        assert_eq!(json["action"], "created");

        let finalized = SentryCliReleaseAudit {
            action: SentryCliReleaseAction::Finalized,
            ..created
        };
        assert_eq!(finalized.operation_type(), "SENTRY_CLI_RELEASE_FINALIZED");

        let uploaded = SentryCliReleaseFileUploadedAudit {
            context: SentryCliAuditContext {
                credential: SentryCliCredential::Dsn(21),
                ..context
            },
            project_id: 3,
            source_map_id: 55,
            version: "abc123".to_string(),
            file_path: "~/static/app.js".to_string(),
            dist: Some("web".to_string()),
            size_bytes: 2048,
        };
        assert_eq!(
            uploaded.operation_type(),
            "SENTRY_CLI_RELEASE_FILE_UPLOADED"
        );
        assert_eq!(AuditOperation::user_id(&uploaded), None);
        let json = payload(&uploaded);
        assert_eq!(json["context"]["credential"]["type"], "dsn");
        assert_eq!(json["context"]["credential"]["id"], 21);
        assert_eq!(json["source_map_id"], 55);
        let mut credential_keys: Vec<&String> = json["context"]["credential"]
            .as_object()
            .expect("object")
            .keys()
            .collect();
        credential_keys.sort();
        assert_eq!(
            credential_keys,
            ["id", "type"],
            "only the credential row id is recorded"
        );
    }
}
