// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Labels every instance carries (ADR-052 §5, ADR-055 §3), next to the
//! existing `sh.temps.*` labels. Logs, metrics, the metadata-egress rules
//! and the reconciler find containers by these.

use std::collections::BTreeMap;

use crate::{WorkloadKind, WorkloadRef};

/// `deployment | sandbox | service | function | job`.
pub const KIND: &str = "sh.temps.kind";
/// The `workloads.id` of the plane's object.
pub const WORKLOAD_ID: &str = "sh.temps.workload_id";
/// The workload revision the instance runs.
pub const REVISION: &str = "sh.temps.revision";
/// The id of the product object that owns the workload.
pub const OWNER_ID: &str = "sh.temps.owner_id";

/// The environment a deployment replica serves. Network policy subjects
/// `deployment/N` name environment N, not deployment N.
pub const ENVIRONMENT: &str = "sh.temps.environment";

/// The workload network policy names an instance by: its owner, except
/// that a deployment replica is named by its environment (the
/// [`ENVIRONMENT`] label). `None` for a replica without one, which policy
/// cannot name.
pub fn policy_subject(
    owner: WorkloadRef,
    labels: &BTreeMap<String, String>,
) -> Option<WorkloadRef> {
    if owner.kind != WorkloadKind::Deployment {
        return Some(owner);
    }
    labels
        .get(ENVIRONMENT)
        .and_then(|id| id.parse::<i64>().ok())
        .map(|id| WorkloadRef::new(WorkloadKind::Deployment, id))
}

/// The systemd slice the instances of `subject` run under, so their cgroup
/// names exactly one workload: `temps-sandbox-7.slice`, nested by systemd
/// as `temps.slice/temps-sandbox.slice/temps-sandbox-7.slice`.
pub fn workload_slice(subject: WorkloadRef) -> String {
    format!("temps-{}-{}.slice", subject.kind.as_str(), subject.id)
}

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

    #[test]
    fn a_replica_is_named_by_its_environment() {
        let replica = WorkloadRef::new(WorkloadKind::Deployment, 40);
        let labels = BTreeMap::from([(ENVIRONMENT.to_string(), "7".to_string())]);
        assert_eq!(
            policy_subject(replica, &labels),
            Some(WorkloadRef::new(WorkloadKind::Deployment, 7))
        );
        assert_eq!(policy_subject(replica, &BTreeMap::new()), None);
        let service = WorkloadRef::new(WorkloadKind::Service, 3);
        assert_eq!(policy_subject(service, &BTreeMap::new()), Some(service));
        assert_eq!(
            workload_slice(WorkloadRef::new(WorkloadKind::Sandbox, 7)),
            "temps-sandbox-7.slice"
        );
    }

    #[test]
    fn instance_labels_name_kind_owner_workload_and_revision() {
        let labels = instance_labels(WorkloadRef::new(WorkloadKind::Job, 7), 31, 2);
        assert_eq!(labels[KIND], "job");
        assert_eq!(labels[OWNER_ID], "7");
        assert_eq!(labels[WORKLOAD_ID], "31");
        assert_eq!(labels[REVISION], "2");
    }
}
