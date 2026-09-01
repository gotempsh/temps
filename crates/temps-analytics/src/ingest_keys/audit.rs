// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit operations for analytics ingest key management (ADR-040 §5).
//!
//! Minting, rotating or revoking an ingest key creates or destroys a
//! long-lived credential, so every write on the admin CRUD surface is audited.
//! That is the explicit mitigation for reusing `AnalyticsWrite` — a permission
//! that otherwise only gates data mutation — rather than inventing a new
//! permission variant.
//!
//! The `public_key` value is recorded in the audit payload on purpose: it is
//! public by construction (see [`crate::ingest_keys::types`]), and knowing
//! *which* key was rotated out is the whole point of auditing a rotation.

use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsIngestKeyCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub key_id: i32,
    pub name: String,
    /// Public by construction — not a secret. See the module docs.
    pub public_key: String,
    pub allowed_origins: Option<Vec<String>>,
    pub rate_limit_per_minute: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsIngestKeyUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub key_id: i32,
    pub name: String,
    pub allowed_origins: Option<Vec<String>>,
    pub rate_limit_per_minute: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsIngestKeyRotatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub key_id: i32,
    /// The value that stopped working. Public by construction.
    pub previous_public_key: String,
    /// The value that replaced it. Public by construction.
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsIngestKeyRevokedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub key_id: i32,
    /// The value that stopped working. Public by construction.
    pub public_key: String,
}

macro_rules! impl_audit_operation {
    ($type:ty, $op:expr) => {
        impl AuditOperation for $type {
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
                serde_json::to_string(self).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize audit operation {}: {}", $op, e)
                })
            }
        }
    };
}

impl_audit_operation!(
    AnalyticsIngestKeyCreatedAudit,
    "ANALYTICS_INGEST_KEY_CREATED"
);
impl_audit_operation!(
    AnalyticsIngestKeyUpdatedAudit,
    "ANALYTICS_INGEST_KEY_UPDATED"
);
impl_audit_operation!(
    AnalyticsIngestKeyRotatedAudit,
    "ANALYTICS_INGEST_KEY_ROTATED"
);
impl_audit_operation!(
    AnalyticsIngestKeyRevokedAudit,
    "ANALYTICS_INGEST_KEY_REVOKED"
);

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> AuditContext {
        AuditContext {
            user_id: 5,
            ip_address: Some("203.0.113.7".to_string()),
            user_agent: "test-agent".to_string(),
        }
    }

    #[test]
    fn operation_types_are_distinct_and_namespaced() {
        let created = AnalyticsIngestKeyCreatedAudit {
            context: context(),
            project_id: 1,
            environment_id: Some(2),
            key_id: 3,
            name: "key".into(),
            public_key: "pa_abc".into(),
            allowed_origins: None,
            rate_limit_per_minute: Some(600),
        };
        let updated = AnalyticsIngestKeyUpdatedAudit {
            context: context(),
            project_id: 1,
            key_id: 3,
            name: "key".into(),
            allowed_origins: None,
            rate_limit_per_minute: None,
        };
        let rotated = AnalyticsIngestKeyRotatedAudit {
            context: context(),
            project_id: 1,
            key_id: 3,
            previous_public_key: "pa_old".into(),
            public_key: "pa_new".into(),
        };
        let revoked = AnalyticsIngestKeyRevokedAudit {
            context: context(),
            project_id: 1,
            key_id: 3,
            public_key: "pa_abc".into(),
        };

        let types = [
            created.operation_type(),
            updated.operation_type(),
            rotated.operation_type(),
            revoked.operation_type(),
        ];
        for t in &types {
            assert!(t.starts_with("ANALYTICS_INGEST_KEY_"), "{t}");
        }
        let unique: std::collections::HashSet<_> = types.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn serializes_with_actor_context() {
        let revoked = AnalyticsIngestKeyRevokedAudit {
            context: context(),
            project_id: 11,
            key_id: 22,
            public_key: "pa_abc".into(),
        };
        assert_eq!(revoked.user_id(), Some(5));
        assert_eq!(revoked.ip_address().as_deref(), Some("203.0.113.7"));
        assert_eq!(revoked.user_agent(), "test-agent");

        let payload = AuditOperation::serialize(&revoked).expect("audit payload should serialize");
        assert!(payload.contains("\"project_id\":11"), "{payload}");
        assert!(payload.contains("\"key_id\":22"), "{payload}");
    }
}
