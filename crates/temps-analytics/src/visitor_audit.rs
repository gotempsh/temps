// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit operation for visitor enrichment writes.
//!
//! A deployment token can now write `custom_data` onto its own project's
//! visitors, so every write that actually changes a visitor is audited. Only
//! the *names* of the keys that were set are recorded, never their values:
//! enrichment routinely carries personal data (a user's name or email), and the
//! audit trail must not become a second copy of it.

use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct VisitorEnrichedAudit {
    pub context: AuditContext,
    /// `"deployment_token"` or `"user"` (session, API key or CLI token).
    pub actor_kind: &'static str,
    /// Set when the actor is a deployment token. A token has no `users` row,
    /// so its identity lives here rather than in `context.user_id`.
    pub deployment_token_id: Option<i32>,
    pub deployment_token_name: Option<String>,
    /// The token's project; `None` for callers that are not project-scoped.
    pub project_id: Option<i32>,
    pub visitor_row_id: Option<i32>,
    /// Names of the top-level `custom_data` keys that were written.
    pub custom_data_keys: Vec<String>,
}

impl AuditOperation for VisitorEnrichedAudit {
    fn operation_type(&self) -> String {
        "VISITOR_ENRICHED".to_string()
    }

    /// `None` for deployment tokens: `context.user_id` is `0` for them and no
    /// such user exists, so recording it would violate the audit foreign key.
    fn user_id(&self) -> Option<i32> {
        (self.actor_kind == "user").then_some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize VISITOR_ENRICHED audit: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit(actor_kind: &'static str) -> VisitorEnrichedAudit {
        VisitorEnrichedAudit {
            context: AuditContext {
                user_id: if actor_kind == "user" { 7 } else { 0 },
                ip_address: Some("203.0.113.9".to_string()),
                user_agent: "test".to_string(),
            },
            actor_kind,
            deployment_token_id: (actor_kind == "deployment_token").then_some(3),
            deployment_token_name: (actor_kind == "deployment_token").then(|| "app".to_string()),
            project_id: Some(5),
            visitor_row_id: Some(11),
            custom_data_keys: vec!["email".to_string()],
        }
    }

    #[test]
    fn deployment_token_actor_has_no_user_id() {
        assert_eq!(audit("deployment_token").user_id(), None);
        assert_eq!(audit("user").user_id(), Some(7));
    }

    #[test]
    fn payload_names_keys_but_carries_no_values() {
        let json = AuditOperation::serialize(&audit("deployment_token")).unwrap();
        assert!(json.contains("\"custom_data_keys\":[\"email\"]"));
        assert_eq!(audit("user").operation_type(), "VISITOR_ENRICHED");
    }
}
