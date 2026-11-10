// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};
#[derive(Serialize)]
pub struct AccessAudit {
    pub context: AuditContext,
    pub project_id: Option<i32>,
    pub resource_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    pub version: i32,
    pub operation: &'static str,
}
impl AuditOperation for AccessAudit {
    fn operation_type(&self) -> String {
        self.operation.into()
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

/// The immutable parent of a child event; standalone resources have no parent.
pub(super) struct AuditResource<'a> {
    pub id: &'a str,
    pub vault_id: Option<&'a str>,
}
impl<'a> From<&'a String> for AuditResource<'a> {
    fn from(id: &'a String) -> Self {
        Self { id, vault_id: None }
    }
}
impl<'a> From<&'a str> for AuditResource<'a> {
    fn from(id: &'a str) -> Self {
        Self { id, vault_id: None }
    }
}
impl<'a> From<(&'a String, &'a String)> for AuditResource<'a> {
    fn from((id, vault_id): (&'a String, &'a String)) -> Self {
        Self {
            id,
            vault_id: Some(vault_id),
        }
    }
}
