// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Labels every instance carries (ADR-052 §5, ADR-055 §3), next to the
//! existing `sh.temps.*` labels. Logs, metrics, the metadata-egress rules
//! and the reconciler find containers by these.

use std::collections::BTreeMap;

use crate::WorkloadRef;

/// `deployment | sandbox | service | function | job`.
pub const KIND: &str = "sh.temps.kind";
/// The `workloads.id` of the plane's object.
pub const WORKLOAD_ID: &str = "sh.temps.workload_id";
/// The workload revision the instance runs.
pub const REVISION: &str = "sh.temps.revision";
/// The id of the product object that owns the workload.
pub const OWNER_ID: &str = "sh.temps.owner_id";

/// The plane's labels for one instance.
pub fn instance_labels(
    owner: WorkloadRef,
    workload_id: i64,
    revision: i64,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (KIND.to_string(), owner.kind.as_str().to_string()),
        (OWNER_ID.to_string(), owner.id.to_string()),
        (WORKLOAD_ID.to_string(), workload_id.to_string()),
        (REVISION.to_string(), revision.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkloadKind;

    #[test]
    fn instance_labels_name_kind_owner_workload_and_revision() {
        let labels = instance_labels(WorkloadRef::new(WorkloadKind::Job, 7), 31, 2);
        assert_eq!(labels[KIND], "job");
        assert_eq!(labels[OWNER_ID], "7");
        assert_eq!(labels[WORKLOAD_ID], "31");
        assert_eq!(labels[REVISION], "2");
    }
}
