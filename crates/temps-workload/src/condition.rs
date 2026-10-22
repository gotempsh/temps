// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Status, conditions and events (ADR-055 §2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    Scheduled,
    ArtifactReady,
    Running,
    Ready,
    PolicyEnforced,
    LimitEnforced,
    /// A run-to-completion workload finished (successfully or not).
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

/// One fact about a workload, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    pub kind: ConditionKind,
    pub status: ConditionStatus,
    /// Machine reason, CamelCase: `SecretsUnavailable`, `NoEligibleNode`.
    pub reason: String,
    /// For a human; names nodes and numbers.
    pub message: String,
    pub last_transition_at: DateTime<Utc>,
}

/// Coarse lifecycle phase, derived from conditions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadPhase {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Terminating,
}

impl WorkloadPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkloadPhase::Pending => "pending",
            WorkloadPhase::Running => "running",
            WorkloadPhase::Succeeded => "succeeded",
            WorkloadPhase::Failed => "failed",
            WorkloadPhase::Terminating => "terminating",
        }
    }

    /// No further transitions happen without a new spec.
    pub fn is_terminal(self) -> bool {
        matches!(self, WorkloadPhase::Succeeded | WorkloadPhase::Failed)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadStatus {
    #[serde(default)]
    pub phase: WorkloadPhase,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub ready_instances: u32,
    /// Exit code of the last finished run, for run-to-completion workloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
}

impl WorkloadStatus {
    pub fn condition(&self, kind: ConditionKind) -> Option<&Condition> {
        self.conditions.iter().find(|c| c.kind == kind)
    }

    /// Set a condition. `last_transition_at` only moves when the status
    /// changes, so a repeated report is not a transition. Returns whether
    /// anything changed, so a caller writes status only when it changes.
    pub fn set_condition(
        &mut self,
        kind: ConditionKind,
        status: ConditionStatus,
        reason: impl Into<String>,
        message: impl Into<String>,
        now: DateTime<Utc>,
    ) -> bool {
        let reason = reason.into();
        let message = message.into();
        match self.conditions.iter_mut().find(|c| c.kind == kind) {
            Some(existing) => {
                if existing.status == status
                    && existing.reason == reason
                    && existing.message == message
                {
                    return false;
                }
                if existing.status != status {
                    existing.last_transition_at = now;
                }
                existing.status = status;
                existing.reason = reason;
                existing.message = message;
                true
            }
            None => {
                self.conditions.push(Condition {
                    kind,
                    status,
                    reason,
                    message,
                    last_transition_at: now,
                });
                true
            }
        }
    }
}

/// An event in a workload's history. Repeats of the same reason and
/// message increment `count` instead of adding a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadEvent {
    pub reason: String,
    pub message: String,
    pub count: u32,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn a_repeated_condition_is_not_a_change() {
        let mut status = WorkloadStatus::default();
        assert!(status.set_condition(
            ConditionKind::Scheduled,
            ConditionStatus::False,
            "NoEligibleNode",
            "0/1 nodes eligible: node-a is draining",
            at(10)
        ));
        assert!(!status.set_condition(
            ConditionKind::Scheduled,
            ConditionStatus::False,
            "NoEligibleNode",
            "0/1 nodes eligible: node-a is draining",
            at(20)
        ));
        assert_eq!(
            status
                .condition(ConditionKind::Scheduled)
                .unwrap()
                .last_transition_at,
            at(10)
        );
    }

    #[test]
    fn transition_time_moves_only_with_the_status() {
        let mut status = WorkloadStatus::default();
        status.set_condition(
            ConditionKind::Ready,
            ConditionStatus::False,
            "Starting",
            "",
            at(1),
        );
        // Same status, new message: changed, but not a transition.
        assert!(status.set_condition(
            ConditionKind::Ready,
            ConditionStatus::False,
            "ProbeFailing",
            "http :8080/health returned 503",
            at(2)
        ));
        assert_eq!(
            status
                .condition(ConditionKind::Ready)
                .unwrap()
                .last_transition_at,
            at(1)
        );
        assert!(status.set_condition(
            ConditionKind::Ready,
            ConditionStatus::True,
            "ProbePassed",
            "",
            at(3)
        ));
        assert_eq!(
            status
                .condition(ConditionKind::Ready)
                .unwrap()
                .last_transition_at,
            at(3)
        );
    }

    #[test]
    fn terminal_phases() {
        assert!(WorkloadPhase::Succeeded.is_terminal());
        assert!(WorkloadPhase::Failed.is_terminal());
        assert!(!WorkloadPhase::Running.is_terminal());
        assert_eq!(
            serde_json::to_string(&ConditionStatus::True).unwrap(),
            "\"True\""
        );
    }
}
