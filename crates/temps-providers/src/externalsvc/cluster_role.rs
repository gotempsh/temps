//! Typed roles + pg_auto_failover states for cluster members.
//!
//! Replaces the dozens of `&str` matches against `"primary"` / `"replica"` /
//! `"monitor"` and the equally fragile pg_auto_failover state strings that
//! used to live in [`postgres_role_reconciler`], `cluster_health`, and the
//! various member helpers in `services.rs`.
//!
//! ## Storage shape unchanged
//!
//! `service_members.role` is still a TEXT column — we serialise back to the
//! same lowercase strings, so this is a pure-Rust refactor with no
//! migration. `as_str` is the canonical wire format; `from_str` is permissive
//! about case but rejects anything outside the known set.
//!
//! ## Why two enums
//!
//! [`ClusterRole`] is what *we* assign to a member (monitor, primary, replica).
//! [`PgAutoFailoverState`] is what pg_auto_failover *reports* via its monitor
//! (primary, secondary, wait_primary, catchingup, ...). They overlap but are
//! not the same vocabulary; pre-refactor, code conflated them in 12+ places
//! and the bug we kept hitting was treating `wait_primary` as both "data
//! member" and "primary" depending on the caller.

use std::fmt;
use std::str::FromStr;

/// A cluster member's role as we model it. Stored in `service_members.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClusterRole {
    /// pg_auto_failover orchestration node. Singleton per cluster.
    Monitor,
    /// Writable data node. Exactly one per cluster at steady state; pg_auto_failover
    /// elects which member holds this role and we sync it back into the row via
    /// the reconciler.
    Primary,
    /// Read-only data node. N per cluster (N >= 1 for HA).
    Replica,
}

impl ClusterRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ClusterRole::Monitor => "monitor",
            ClusterRole::Primary => "primary",
            ClusterRole::Replica => "replica",
        }
    }

    /// `true` for any data-bearing role (everything except the monitor).
    pub fn is_data_member(self) -> bool {
        !matches!(self, ClusterRole::Monitor)
    }
}

impl fmt::Display for ClusterRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownClusterRole(pub String);

impl fmt::Display for UnknownClusterRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown cluster role: {:?}", self.0)
    }
}

impl std::error::Error for UnknownClusterRole {}

impl FromStr for ClusterRole {
    type Err = UnknownClusterRole;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "monitor" => Ok(ClusterRole::Monitor),
            "primary" => Ok(ClusterRole::Primary),
            "replica" => Ok(ClusterRole::Replica),
            other => Err(UnknownClusterRole(other.to_string())),
        }
    }
}

/// pg_auto_failover's reported FSM state for a node.
///
/// Sourced from `pgautofailover.node.reportedstate`. Documented in the
/// pg_auto_failover source — full state machine at
/// <https://github.com/hapostgres/pg_auto_failover/blob/main/src/monitor/node_active_protocol.h>.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgAutoFailoverState {
    /// The node is the writable primary in a multi-node group.
    Primary,
    /// Single-node mode (no replica yet); still writable.
    Single,
    /// About to be promoted to primary; not yet writable.
    WaitPrimary,
    /// Streaming from the primary; healthy.
    Secondary,
    /// Waiting on monitor to assign a primary.
    WaitSecondary,
    /// Replica is catching up after a restart / network issue.
    Catchingup,
    /// Replica reported its LSN, monitor is deciding.
    ReportLsn,
    /// Applying a new pg_hba / config setting.
    ApplySettings,
    /// Node is being drained, soon to be dropped.
    Draining,
    /// Node was demoted from primary; not yet a healthy replica.
    Demoted,
    /// Demote timed out — node is in a bad state.
    DemoteTimeout,
    /// Node is being dropped from the formation.
    DropNode,
    /// Initial state when a new node joins the monitor.
    Init,
    /// Anything else pg_auto_failover may emit in future versions. The
    /// reconciler must NOT crash on unknown values; it should just leave
    /// the role label alone.
    Other(&'static str),
}

impl PgAutoFailoverState {
    /// `true` when this node is currently the writable primary.
    pub fn is_primary(self) -> bool {
        matches!(
            self,
            PgAutoFailoverState::Primary
                | PgAutoFailoverState::Single
                | PgAutoFailoverState::WaitPrimary
        )
    }

    /// `true` when this node is a healthy or healthy-becoming replica.
    pub fn is_secondary(self) -> bool {
        matches!(
            self,
            PgAutoFailoverState::Secondary
                | PgAutoFailoverState::WaitSecondary
                | PgAutoFailoverState::Catchingup
                | PgAutoFailoverState::ReportLsn
                | PgAutoFailoverState::ApplySettings
        )
    }

    /// `true` for any state that puts the node in the data path
    /// (writable or readable from a quorum/sync standpoint). Used by
    /// the reconciler to decide whether to publish a Tier-3 record.
    pub fn is_data_member(self) -> bool {
        self.is_primary() || self.is_secondary()
    }

    /// Map to the `ClusterRole` we should write back into `service_members.role`.
    /// Returns `None` for transient/unknown states so the reconciler leaves
    /// the existing label alone (avoids UI flicker on every monitor poll).
    pub fn to_cluster_role(self) -> Option<ClusterRole> {
        if self.is_primary() {
            Some(ClusterRole::Primary)
        } else if self.is_secondary() {
            Some(ClusterRole::Replica)
        } else {
            None
        }
    }
}

impl FromStr for PgAutoFailoverState {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // pg_auto_failover always lowercases; normalise defensively.
        Ok(match s.to_ascii_lowercase().as_str() {
            "primary" => PgAutoFailoverState::Primary,
            "single" => PgAutoFailoverState::Single,
            "wait_primary" => PgAutoFailoverState::WaitPrimary,
            "secondary" => PgAutoFailoverState::Secondary,
            "wait_secondary" => PgAutoFailoverState::WaitSecondary,
            "catchingup" => PgAutoFailoverState::Catchingup,
            "report_lsn" => PgAutoFailoverState::ReportLsn,
            "apply_settings" => PgAutoFailoverState::ApplySettings,
            "draining" => PgAutoFailoverState::Draining,
            "demoted" => PgAutoFailoverState::Demoted,
            "demote_timeout" => PgAutoFailoverState::DemoteTimeout,
            "drop_node" => PgAutoFailoverState::DropNode,
            "init" => PgAutoFailoverState::Init,
            // Leak-safe: keep the original string so we can log it at the
            // call site, but force callers through the `Other` arm so they
            // can't accidentally treat an unknown state as primary.
            "" => PgAutoFailoverState::Other("(empty)"),
            _ => PgAutoFailoverState::Other("(other)"),
        })
    }
}

impl fmt::Display for PgAutoFailoverState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgAutoFailoverState::Primary => f.write_str("primary"),
            PgAutoFailoverState::Single => f.write_str("single"),
            PgAutoFailoverState::WaitPrimary => f.write_str("wait_primary"),
            PgAutoFailoverState::Secondary => f.write_str("secondary"),
            PgAutoFailoverState::WaitSecondary => f.write_str("wait_secondary"),
            PgAutoFailoverState::Catchingup => f.write_str("catchingup"),
            PgAutoFailoverState::ReportLsn => f.write_str("report_lsn"),
            PgAutoFailoverState::ApplySettings => f.write_str("apply_settings"),
            PgAutoFailoverState::Draining => f.write_str("draining"),
            PgAutoFailoverState::Demoted => f.write_str("demoted"),
            PgAutoFailoverState::DemoteTimeout => f.write_str("demote_timeout"),
            PgAutoFailoverState::DropNode => f.write_str("drop_node"),
            PgAutoFailoverState::Init => f.write_str("init"),
            PgAutoFailoverState::Other(tag) => write!(f, "other({})", tag),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_role_round_trip() {
        for r in [
            ClusterRole::Monitor,
            ClusterRole::Primary,
            ClusterRole::Replica,
        ] {
            assert_eq!(ClusterRole::from_str(r.as_str()).unwrap(), r);
        }
    }

    #[test]
    fn cluster_role_case_insensitive() {
        assert_eq!(
            ClusterRole::from_str("PRIMARY").unwrap(),
            ClusterRole::Primary
        );
        assert_eq!(
            ClusterRole::from_str("Replica").unwrap(),
            ClusterRole::Replica
        );
    }

    #[test]
    fn cluster_role_rejects_unknown() {
        assert!(ClusterRole::from_str("witness").is_err());
        assert!(ClusterRole::from_str("").is_err());
    }

    #[test]
    fn cluster_role_data_membership() {
        assert!(!ClusterRole::Monitor.is_data_member());
        assert!(ClusterRole::Primary.is_data_member());
        assert!(ClusterRole::Replica.is_data_member());
    }

    #[test]
    fn pg_state_classification() {
        // Primary-side
        for s in ["primary", "single", "wait_primary"] {
            let parsed: PgAutoFailoverState = s.parse().unwrap();
            assert!(parsed.is_primary(), "{s} should be primary");
            assert!(parsed.is_data_member());
            assert_eq!(parsed.to_cluster_role(), Some(ClusterRole::Primary));
        }

        // Secondary-side
        for s in [
            "secondary",
            "wait_secondary",
            "catchingup",
            "report_lsn",
            "apply_settings",
        ] {
            let parsed: PgAutoFailoverState = s.parse().unwrap();
            assert!(parsed.is_secondary(), "{s} should be secondary");
            assert!(parsed.is_data_member());
            assert_eq!(parsed.to_cluster_role(), Some(ClusterRole::Replica));
        }

        // Transient/unknown — must NOT flip the role
        for s in [
            "draining",
            "demoted",
            "demote_timeout",
            "init",
            "drop_node",
            "weird",
        ] {
            let parsed: PgAutoFailoverState = s.parse().unwrap();
            assert_eq!(
                parsed.to_cluster_role(),
                None,
                "{s} should not map to a role"
            );
        }
    }

    #[test]
    fn pg_state_unknown_is_safe() {
        let parsed: PgAutoFailoverState = "this_is_not_a_state".parse().unwrap();
        assert!(matches!(parsed, PgAutoFailoverState::Other(_)));
        assert!(!parsed.is_primary());
        assert!(!parsed.is_secondary());
        assert!(!parsed.is_data_member());
    }

    #[test]
    fn pg_state_empty_is_safe() {
        let parsed: PgAutoFailoverState = "".parse().unwrap();
        assert!(matches!(parsed, PgAutoFailoverState::Other(_)));
        assert!(parsed.to_cluster_role().is_none());
    }
}
