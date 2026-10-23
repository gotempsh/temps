// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The plane's errors. Each names the workload it is about.

use temps_workload::{ConditionKind, ConditionStatus};

use crate::admission::AdmissionRule;

/// A spec refused at admission (§4): which rule, which workload, why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("admission refused workload '{workload}' of {owner} at rule '{rule}': {message}")]
pub struct AdmissionError {
    pub rule: AdmissionRule,
    /// The workload's name.
    pub workload: String,
    /// `<owner_kind>/<owner_id>`.
    pub owner: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PlaneError {
    #[error(transparent)]
    Admission(#[from] AdmissionError),

    #[error("workload {workload_id} not found")]
    NotFound { workload_id: i64 },

    /// `apply_and_wait`: the workload finished (or was deleted) without the
    /// condition becoming true.
    #[error(
        "workload {workload_id} ('{name}') finished without {condition:?} becoming True: \
         {condition:?} is {status:?}, reason {reason}: {message}"
    )]
    ConditionFailed {
        workload_id: i64,
        name: String,
        condition: ConditionKind,
        status: ConditionStatus,
        reason: String,
        message: String,
    },

    /// `apply_and_wait`: the timeout passed first. Carries the condition's
    /// last reason and message (or, when it was never set, the first
    /// condition that is `False`), so the caller can say why.
    #[error(
        "workload {workload_id} ('{name}'): {condition:?} was not True after {waited_ms} ms; \
         last reason {reason}: {message}"
    )]
    WaitTimedOut {
        workload_id: i64,
        name: String,
        condition: ConditionKind,
        waited_ms: u64,
        reason: String,
        message: String,
    },

    #[error("workload {workload_id}: stored {field} could not be decoded: {reason}")]
    Corrupt {
        workload_id: i64,
        field: &'static str,
        reason: String,
    },

    #[error("workload '{workload}': {field} could not be encoded: {reason}")]
    Encode {
        workload: String,
        field: &'static str,
        reason: String,
    },

    #[error("workload {workload_id}: the runtime driver failed to {operation}: {source}")]
    Driver {
        workload_id: i64,
        operation: &'static str,
        #[source]
        source: temps_workload::DriverError,
    },

    #[error("the reconciler of plane '{scope}' is already running; a plane runs one")]
    ReconcilerAlreadyStarted { scope: String },

    /// The plane was built without a node inventory, so it cannot place or
    /// explain anything beyond the control plane.
    #[error(
        "plane '{scope}' has no node inventory; placement for workload '{workload}' cannot be \
         computed"
    )]
    NoInventory { scope: String, workload: String },

    #[error("reading the node inventory for workload '{workload}' failed: {reason}")]
    Inventory { workload: String, reason: String },

    /// No node can take the workload: `Scheduled = False` with `reason`.
    #[error("workload '{workload}' cannot be scheduled ({reason}): {message}")]
    Unschedulable {
        workload: String,
        reason: &'static str,
        message: String,
    },

    #[error("database error while {operation} (workload {workload}): {source}")]
    Database {
        operation: &'static str,
        /// The workload id or name the operation was about, `*` for a sweep.
        workload: String,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl PlaneError {
    /// A `map_err` adapter that adds the operation and the workload.
    pub(crate) fn db(
        operation: &'static str,
        workload: impl ToString,
    ) -> impl FnOnce(sea_orm::DbErr) -> PlaneError {
        let workload = workload.to_string();
        move |source| PlaneError::Database {
            operation,
            workload,
            source,
        }
    }
}

impl From<sea_orm::DbErr> for PlaneError {
    fn from(source: sea_orm::DbErr) -> Self {
        PlaneError::Database {
            operation: "querying workloads",
            workload: "*".to_string(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_errors_name_the_rule_and_the_workload() {
        let error = AdmissionError {
            rule: AdmissionRule::SecurityProfile,
            workload: "cron-7-1700000000".into(),
            owner: "cron/7".into(),
            message: "asks for host_privileged without the ADR-045 socket grant".into(),
        };
        assert_eq!(
            error.to_string(),
            "admission refused workload 'cron-7-1700000000' of cron/7 at rule \
             'security_profile': asks for host_privileged without the ADR-045 socket grant"
        );
    }

    #[test]
    fn database_errors_carry_the_operation_and_the_workload() {
        let error = PlaneError::db("writing status", 42)(sea_orm::DbErr::Custom("boom".into()));
        let text = error.to_string();
        assert!(text.contains("writing status"), "{text}");
        assert!(text.contains("workload 42"), "{text}");
    }
}
