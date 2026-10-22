// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workload identity (ADR-052 §1).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What a workload is. Each kind has its own defaults (security profile,
/// egress profile, scheduling strategy) and its own product that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadKind {
    /// A project environment's revision.
    Deployment,
    /// A sandbox.
    Sandbox,
    /// A managed service (database, cache, object store).
    Service,
    /// A function (phase 10).
    Function,
    /// A run-to-completion job: a cron run, a backup.
    Job,
}

impl WorkloadKind {
    pub const ALL: [WorkloadKind; 5] = [
        WorkloadKind::Deployment,
        WorkloadKind::Sandbox,
        WorkloadKind::Service,
        WorkloadKind::Function,
        WorkloadKind::Job,
    ];

    /// The value stored in the database and in the `sh.temps.kind` label.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "deployment",
            WorkloadKind::Sandbox => "sandbox",
            WorkloadKind::Service => "service",
            WorkloadKind::Function => "function",
            WorkloadKind::Job => "job",
        }
    }
}

impl fmt::Display for WorkloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unknown workload kind string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown workload kind '{value}'; expected one of deployment, sandbox, service, function, job"
)]
pub struct UnknownWorkloadKind {
    pub value: String,
}

impl FromStr for WorkloadKind {
    type Err = UnknownWorkloadKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        WorkloadKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or_else(|| UnknownWorkloadKind {
                value: value.to_string(),
            })
    }
}

/// The identity network rules, names and labels attach to: a kind and the
/// id of the product object that owns the workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkloadRef {
    pub kind: WorkloadKind,
    pub id: i64,
}

impl WorkloadRef {
    pub fn new(kind: WorkloadKind, id: i64) -> Self {
        Self { kind, id }
    }
}

impl fmt::Display for WorkloadRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.id)
    }
}

/// A workload reference string that is not `<kind>/<id>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidWorkloadRef {
    #[error("workload reference '{value}' is not of the form <kind>/<id>")]
    Shape { value: String },
    #[error("workload reference '{value}': {source}")]
    Kind {
        value: String,
        #[source]
        source: UnknownWorkloadKind,
    },
    #[error("workload reference '{value}' has a non-numeric id")]
    Id { value: String },
}

impl FromStr for WorkloadRef {
    type Err = InvalidWorkloadRef;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, id) = value
            .split_once('/')
            .ok_or_else(|| InvalidWorkloadRef::Shape {
                value: value.to_string(),
            })?;
        let kind = kind.parse().map_err(|source| InvalidWorkloadRef::Kind {
            value: value.to_string(),
            source,
        })?;
        let id = id.parse().map_err(|_| InvalidWorkloadRef::Id {
            value: value.to_string(),
        })?;
        Ok(Self { kind, id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_through_their_label_value() {
        for kind in WorkloadKind::ALL {
            assert_eq!(kind.as_str().parse::<WorkloadKind>(), Ok(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert!("pod".parse::<WorkloadKind>().is_err());
    }

    #[test]
    fn references_round_trip_and_reject_malformed_input() {
        let r = WorkloadRef::new(WorkloadKind::Sandbox, 42);
        assert_eq!(r.to_string(), "sandbox/42");
        assert_eq!("sandbox/42".parse::<WorkloadRef>(), Ok(r));
        assert!(matches!(
            "sandbox".parse::<WorkloadRef>(),
            Err(InvalidWorkloadRef::Shape { .. })
        ));
        assert!(matches!(
            "pod/1".parse::<WorkloadRef>(),
            Err(InvalidWorkloadRef::Kind { .. })
        ));
        assert!(matches!(
            "job/x".parse::<WorkloadRef>(),
            Err(InvalidWorkloadRef::Id { .. })
        ));
    }

    #[test]
    fn references_order_by_kind_then_id() {
        let mut refs = [
            WorkloadRef::new(WorkloadKind::Sandbox, 1),
            WorkloadRef::new(WorkloadKind::Deployment, 9),
            WorkloadRef::new(WorkloadKind::Deployment, 2),
        ];
        refs.sort();
        assert_eq!(
            refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["deployment/2", "deployment/9", "sandbox/1"]
        );
    }
}
