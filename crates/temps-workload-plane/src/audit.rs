// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the plane audit-logs itself (ADR-055 §15): refusals at admission.
//! Products keep auditing the writes their users make.

use serde::Serialize;
use temps_core::AuditOperation;

use crate::error::AdmissionError;

/// The audit actor for changes the plane makes on its own.
pub const PLANE_ACTOR: &str = "temps-workload-plane";

#[derive(Debug, Clone, Serialize)]
pub struct AdmissionRefusedAudit {
    pub workload: String,
    pub owner: String,
    pub rule: String,
    pub message: String,
}

impl From<&AdmissionError> for AdmissionRefusedAudit {
    fn from(error: &AdmissionError) -> Self {
        Self {
            workload: error.workload.clone(),
            owner: error.owner.clone(),
            rule: error.rule.as_str().to_string(),
            message: error.message.clone(),
        }
    }
}

impl AuditOperation for AdmissionRefusedAudit {
    fn operation_type(&self) -> String {
        "WORKLOAD_ADMISSION_REFUSED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        None
    }

    fn user_agent(&self) -> &str {
        PLANE_ACTOR
    }

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::AdmissionRule;

    #[test]
    fn a_refusal_audit_names_the_rule_workload_and_owner() {
        let error = AdmissionError {
            rule: AdmissionRule::Quota,
            workload: "cron-3-1".into(),
            owner: "cron/3".into(),
            message: "cron/3 already has 5 unfinished workloads; the limit is 5".into(),
        };
        let audit = AdmissionRefusedAudit::from(&error);
        assert_eq!(audit.operation_type(), "WORKLOAD_ADMISSION_REFUSED");
        assert_eq!(audit.user_id(), None);
        let json = AuditOperation::serialize(&audit).expect("serialised");
        assert!(json.contains("\"rule\":\"quota\""), "{json}");
        assert!(json.contains("\"workload\":\"cron-3-1\""), "{json}");
        assert!(json.contains("\"owner\":\"cron/3\""), "{json}");
    }
}
