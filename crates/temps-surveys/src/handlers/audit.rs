// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit events for survey mutations.
//!
//! Activating a survey puts a prompt in front of a project's visitors with no
//! deployment, so every change is recorded with its status and question
//! count, before and after.

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
pub struct SurveyCreatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub survey_id: i32,
    pub name: String,
    pub status: String,
    pub question_count: usize,
    pub targeting: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SurveyUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub survey_id: i32,
    pub name: String,
    pub old_status: String,
    pub new_status: String,
    pub old_question_count: usize,
    pub new_question_count: usize,
    pub old_targeting: serde_json::Value,
    pub new_targeting: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SurveyDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub survey_id: i32,
    pub name: String,
    pub status: String,
}

impl_audit_operation!(SurveyCreatedAudit, "SURVEY_CREATED");
impl_audit_operation!(SurveyUpdatedAudit, "SURVEY_UPDATED");
impl_audit_operation!(SurveyDeletedAudit, "SURVEY_DELETED");
