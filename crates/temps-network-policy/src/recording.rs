// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which nodes can record the outbound connections of sandboxes that opt in.
//!
//! Every node says so about itself: a worker's agent puts an
//! [`EgressRecordingCapability`] in the report it posts on every
//! network-policy pass, and node 0 sets its own with
//! [`NetworkPolicyService::set_local_egress_recording`]. The control plane
//! keeps the last report per node in memory, like the enforcer report, so
//! it can answer for a mixed cluster without guessing from build targets.

use std::collections::HashMap;
use std::time::Instant;

use temps_workload_netpol::{EgressRecordingCapability, RecordingUnavailable};

use crate::service::{NetworkPolicyService, CONTROL_PLANE_NODE_ID, ENFORCER_REPORT_TTL};

/// A node's last word on recording.
#[derive(Debug, Clone)]
pub(crate) struct RecordingReport {
    /// `None`: the agent reports, but predates recording.
    pub capability: Option<EgressRecordingCapability>,
    pub seen: Instant,
}

/// What the control plane knows about recording on one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRecordingStatus {
    Available,
    /// The node reported that it cannot record, and why.
    Unavailable(RecordingUnavailable),
    /// The node has not reported within [`ENFORCER_REPORT_TTL`]: its agent
    /// is offline, or this control plane restarted and has not heard from
    /// it yet.
    NotReporting {
        within_secs: u64,
    },
    /// The node's agent reports, but is older than egress recording.
    AgentPredatesRecording,
}

impl NodeRecordingStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    /// A stable code for API clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable(RecordingUnavailable::UnsupportedPlatform { .. }) => {
                "unsupported_platform"
            }
            Self::Unavailable(RecordingUnavailable::ProxyNotServing) => "proxy_not_serving",
            Self::NotReporting { .. } => "node_not_reporting",
            Self::AgentPredatesRecording => "agent_predates_recording",
        }
    }

    /// Why recording is unavailable, for an operator; `None` when it is
    /// available.
    pub fn describe(&self) -> Option<String> {
        match self {
            Self::Available => None,
            Self::Unavailable(reason) => Some(reason.describe()),
            Self::NotReporting { within_secs } => Some(format!(
                "the node's agent has not reported in the last {within_secs} seconds, so \
                 whether it can record is unknown; check that `temps agent` is running on it"
            )),
            Self::AgentPredatesRecording => Some(
                "the node's agent is older than egress recording; upgrade and restart \
                 `temps agent` on the node"
                    .to_string(),
            ),
        }
    }
}

impl From<&EgressRecordingCapability> for NodeRecordingStatus {
    fn from(capability: &EgressRecordingCapability) -> Self {
        match capability {
            EgressRecordingCapability::Available => Self::Available,
            EgressRecordingCapability::Unavailable { reason } => Self::Unavailable(reason.clone()),
        }
    }
}

pub(crate) type RecordingReports = std::sync::Mutex<HashMap<i32, RecordingReport>>;

impl NetworkPolicyService {
    /// Records what `node_id`'s agent said about recording on its last pass.
    pub fn record_egress_recording(
        &self,
        node_id: i32,
        capability: Option<EgressRecordingCapability>,
    ) {
        let mut reports = self.recording.lock().unwrap_or_else(|e| e.into_inner());
        reports.insert(
            node_id,
            RecordingReport {
                capability,
                seen: Instant::now(),
            },
        );
    }

    /// What node 0 (this process) can do. It starts as
    /// [`EgressRecordingCapability::detect`]`(false)`: this process does not
    /// serve the per-node egress proxy for sandboxes yet.
    pub fn set_local_egress_recording(&self, capability: EgressRecordingCapability) {
        *self
            .local_recording
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = capability;
    }

    /// Whether `node_id` can record outbound connections now.
    pub fn egress_recording_status(&self, node_id: i32) -> NodeRecordingStatus {
        if node_id == CONTROL_PLANE_NODE_ID {
            let local = self
                .local_recording
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            return NodeRecordingStatus::from(&*local);
        }
        let reports = self.recording.lock().unwrap_or_else(|e| e.into_inner());
        match reports
            .get(&node_id)
            .filter(|r| r.seen.elapsed() <= ENFORCER_REPORT_TTL)
        {
            None => NodeRecordingStatus::NotReporting {
                within_secs: ENFORCER_REPORT_TTL.as_secs(),
            },
            Some(RecordingReport {
                capability: None, ..
            }) => NodeRecordingStatus::AgentPredatesRecording,
            Some(RecordingReport {
                capability: Some(capability),
                ..
            }) => NodeRecordingStatus::from(capability),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;

    fn service() -> NetworkPolicyService {
        NetworkPolicyService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ))
    }

    #[test]
    fn each_node_answers_for_itself() {
        let service = service();
        service.record_egress_recording(3, Some(EgressRecordingCapability::Available));
        service.record_egress_recording(
            4,
            Some(EgressRecordingCapability::for_platform("macos", true)),
        );
        service.record_egress_recording(5, None);
        assert!(service.egress_recording_status(3).is_available());
        let mac = service.egress_recording_status(4);
        assert_eq!(mac.code(), "unsupported_platform");
        assert!(mac.describe().unwrap().contains("macos"));
        assert_eq!(
            service.egress_recording_status(5),
            NodeRecordingStatus::AgentPredatesRecording
        );
        assert_eq!(
            service.egress_recording_status(6),
            NodeRecordingStatus::NotReporting {
                within_secs: ENFORCER_REPORT_TTL.as_secs()
            }
        );
    }

    #[test]
    fn node_zero_reports_what_this_process_serves() {
        let service = service();
        assert!(
            !service.egress_recording_status(0).is_available(),
            "the control plane does not serve the per-node proxy for sandboxes yet"
        );
        service.set_local_egress_recording(EgressRecordingCapability::Available);
        assert!(service.egress_recording_status(0).is_available());
    }
}
