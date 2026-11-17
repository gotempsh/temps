// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure helpers that interpret pg_auto_failover state, cluster lifecycle
//! ordering and claims, and cluster auth versions.

use super::*;
use crate::externalsvc::{HealthProbeStatus, PgAutoFailoverState};
use chrono::Utc;
use std::collections::HashMap;
use temps_entities::{external_services, service_members};

/// Whether a live pg_auto_failover state identifies a node that accepts writes.
///
/// Keep connection planning, backups, deletion guards, and DNS reconciliation
/// on the typed state model. In particular, `wait_primary` is writable: it is
/// the stable state of a promoted node that currently has no standby attached.
pub(super) fn live_state_is_writable_primary(state: Option<&str>) -> bool {
    state
        .and_then(|state| state.parse::<PgAutoFailoverState>().ok())
        .is_some_and(PgAutoFailoverState::is_primary)
}

/// Return the monitor identity of the sole healthy, recently reporting writer.
///
/// pg_auto_failover retains a stopped node's last `reported_state`, so a stale
/// unhealthy `primary` can coexist with the promoted healthy `wait_primary`.
/// Selecting solely by state and member order can therefore route connections
/// or backups to the dead node. Ambiguous reports fail closed.
pub(super) fn healthy_writable_primary_nodename(health: &ClusterHealthReport) -> Option<&str> {
    if health.monitor_error.is_some() {
        return None;
    }
    let mut candidates = health.members.iter().filter(|member| {
        member.health == 1
            && member.seconds_since_report < 30
            && live_state_is_writable_primary(Some(&member.reported_state))
    });
    let candidate = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    Some(&candidate.nodename)
}

/// Which cluster member a `pg_stat_replication` row on the primary
/// describes, by nodename.
///
/// pg_auto_failover names each standby's walreceiver
/// `pgautofailover_standby_<nodeid>`, which identifies the member exactly.
/// The standby's `client_addr` is only a fallback, and only when exactly one
/// member registered that host: members sharing a worker report the same
/// `nodehost`, and NAT can rewrite the source address entirely.
///
/// `members` holds `(nodeid, nodename, nodehost)` from the monitor.
pub(super) fn replication_row_member(
    application_name: &str,
    client_host: Option<&str>,
    members: &[(i64, String, String)],
) -> Option<String> {
    let by_application_name = application_name
        .strip_prefix("pgautofailover_standby_")
        .and_then(|id| id.parse::<i64>().ok())
        .and_then(|id| members.iter().find(|(node_id, _, _)| *node_id == id));
    if let Some((_, name, _)) = by_application_name {
        return Some(name.clone());
    }

    let host = client_host?;
    let mut same_host = members.iter().filter(|(_, _, nodehost)| nodehost == host);
    match (same_host.next(), same_host.next()) {
        (Some((_, name, _)), None) => Some(name.clone()),
        _ => None,
    }
}

/// Match monitor-reported primary identity to exactly one persisted member.
///
/// The monitor is authoritative for transient role state, but it is not an
/// authority for credential destinations. Duplicate, missing, stopped, or
/// non-data matches fail closed by returning `None`.
pub(super) fn trusted_primary_member<'a>(
    members: &'a [ServiceMemberInfo],
    monitor_nodename: &str,
) -> Option<&'a ServiceMemberInfo> {
    let mut matches = members.iter().filter(|member| {
        member.is_data_member()
            && member.status == "running"
            && member.container_name == monitor_nodename
    });
    let member = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(member)
}

/// One-line summary of what the monitor reports for each data node, for the
/// error raised when no trustworthy primary can be picked from it: e.g.
/// `orders-1=primary (unhealthy, 45s since report), orders-2=secondary`.
pub(super) fn describe_monitor_states(health: &ClusterHealthReport) -> String {
    if health.members.is_empty() {
        return "no registered data nodes".to_string();
    }
    health
        .members
        .iter()
        .map(|member| {
            let mut notes = Vec::new();
            if member.health != 1 {
                notes.push("unhealthy".to_string());
            }
            if member.seconds_since_report >= 30 {
                notes.push(format!("{}s since report", member.seconds_since_report));
            }
            if notes.is_empty() {
                format!("{}={}", member.nodename, member.reported_state)
            } else {
                format!(
                    "{}={} ({})",
                    member.nodename,
                    member.reported_state,
                    notes.join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse a raw role string (TEXT column / spec) into the typed enum.
/// Returns `None` for unknown values; callers should use the
/// classification helpers below for `is_monitor()` / `is_data_member()`
/// semantics, not direct equality.
pub(super) fn role_from_str(s: &str) -> Option<crate::ClusterRole> {
    use std::str::FromStr;
    crate::ClusterRole::from_str(s).ok()
}

/// pg_auto_failover node states, grouped by what they mean for an application.
///
/// These are `reportedstate` values from `pgautofailover.node` on the monitor,
/// not our own roles — `service_members.role` is static config, while the FSM
/// state is the runtime truth about whether anyone can serve a write.
pub(crate) mod cluster_states {
    /// States in which a node accepts writes.
    ///
    /// `wait_primary` and `single` belong here even though neither is named
    /// "primary": pg_auto_failover clears `synchronous_standby_names` in those
    /// states precisely so writes keep flowing while there is no standby. A
    /// cluster sitting in `wait_primary` is unprotected, not down, and warning
    /// that writes will fail there would be wrong.
    pub const WRITABLE: &[&str] = &["primary", "wait_primary", "single", "apply_settings"];

    /// States a node passes through during a failover.
    ///
    /// While any node reports one of these, an election is underway and the
    /// absence of a writer is expected for a few seconds — so it is reported as
    /// a failover in progress rather than a stuck cluster.
    pub const TRANSITIONAL: &[&str] = &[
        "prepare_promotion",
        "stop_replication",
        "demoted",
        "demote_timeout",
        "draining",
        "prepare_maintenance",
        "wait_maintenance",
    ];
}

/// One data node as the monitor sees it.
#[derive(Debug, Clone)]
pub(crate) struct ClusterNodeState {
    pub name: String,
    /// `reportedstate` — what the node last told the monitor it was doing.
    pub state: String,
    /// Monitor's own health check: -1 not yet checked, 0 failing, 1 responding.
    pub health: i32,
}

impl ClusterNodeState {
    /// Whether this node can serve a write *right now*.
    ///
    /// Requires both a writable FSM state and a health check that isn't
    /// actively failing. `health == 0` alone disqualifies it: when every node
    /// dies at once the monitor cannot promote anything, so it leaves the old
    /// `reportedstate` in place and a dead primary keeps reporting `primary`.
    /// `-1` (not yet checked) is not treated as failure — that would false-
    /// alarm on a freshly registered node.
    fn is_writable(&self) -> bool {
        cluster_states::WRITABLE.contains(&self.state.as_str()) && self.health != 0
    }

    fn label(&self) -> String {
        if self.health == 0 {
            format!("{}={} (unreachable)", self.name, self.state)
        } else {
            format!("{}={}", self.name, self.state)
        }
    }
}

/// Turn the monitor's per-node states into a health verdict.
///
/// Split out from `probe_cluster` so the classification is testable without a
/// live monitor — it is the part that decides what an operator is told.
pub(crate) fn classify_cluster_states(
    service_id: i32,
    states: &[ClusterNodeState],
) -> (HealthProbeStatus, Option<String>) {
    const HEALTHY: &[&str] = &["primary", "single", "secondary"];

    let listed =
        |sel: &[ClusterNodeState]| sel.iter().map(|n| n.label()).collect::<Vec<_>>().join(", ");

    let unhealthy: Vec<String> = states
        .iter()
        .filter(|n| !HEALTHY.contains(&n.state.as_str()) || n.health == 0)
        .map(|n| n.label())
        .collect();

    let has_writer = states.iter().any(|n| n.is_writable());

    // No node is accepting writes. This is what actually breaks an
    // application, and it is NOT the same as "no node reports `primary`":
    // `wait_primary` and `single` are writable, so treating those as
    // leaderless would cry wolf on a cluster that is merely unprotected.
    if !has_writer {
        let failing_over = states
            .iter()
            .any(|n| cluster_states::TRANSITIONAL.contains(&n.state.as_str()));

        // A failover in flight passes through `prepare_promotion` /
        // `stop_replication` / `demoted` for a few seconds. Saying "no leader,
        // go fix it" there would flap on every normal failover.
        let message = if failing_over {
            format!(
                "Failover in progress — no node is accepting writes right now. \
                 Node states: {}. This normally clears within seconds; if it \
                 persists, promote a member explicitly.",
                listed(states)
            )
        } else {
            format!(
                "Cluster has no leader — writes will fail. No node is in a writable state \
                 ({}), so the monitor has not elected a primary. Node states: {}. \
                 Recover by promoting a running member \
                 (POST /external-services/{}/members/{{member_id}}/promote). If no member \
                 is running, start or retry the members first — promotion needs a running \
                 container.",
                cluster_states::WRITABLE.join("/"),
                listed(states),
                service_id
            )
        };
        return (HealthProbeStatus::Degraded, Some(message));
    }

    if unhealthy.is_empty() {
        return (HealthProbeStatus::Operational, None);
    }

    // Writable, but something is off. Call out the case where writes work yet
    // there is no standby at all: the next failure is not survivable, which is
    // a materially different warning from "a replica is catching up".
    let unprotected = !states
        .iter()
        .any(|n| n.state == "secondary" && n.health != 0);
    let detail = format!(
        "{}/{} data node(s) not in a healthy state: {}",
        unhealthy.len(),
        states.len(),
        unhealthy.join(", ")
    );
    let message = if unprotected {
        format!(
            "Writes are being accepted, but the cluster has no healthy standby — a failure \
             now would take it down with no node to fail over to. {detail}"
        )
    } else {
        detail
    };

    (HealthProbeStatus::Degraded, Some(message))
}

pub(crate) fn is_role_monitor(s: &str) -> bool {
    role_from_str(s) == Some(crate::ClusterRole::Monitor)
}

/// Cluster parameter recording how far the cluster's authentication has
/// converged (absent on clusters created before SCRAM auth).
pub(super) const CLUSTER_AUTH_VERSION_KEY: &str = "_cluster_scram_auth_version";

/// Every member authenticates pg_auto_failover's infrastructure roles
/// (`autoctl_node`, `pgautofailover_replicator`) with SCRAM. This is what a
/// new member needs from the existing ones.
pub(super) const CLUSTER_INFRA_AUTH_VERSION: u64 = 1;

/// Current version: additionally, every data node has decided its
/// application catch-all rules (`scram-sha-256` once no login role stores an
/// MD5 hash). Clusters marked 1 converged before that decision ran during the
/// upgrade, so they only re-run the data-node step instead of waiting for
/// each member to restart.
pub(super) const CLUSTER_AUTH_VERSION: u64 = 2;

pub(super) fn cluster_auth_version(parameters: &HashMap<String, serde_json::Value>) -> u64 {
    parameters
        .get(CLUSTER_AUTH_VERSION_KEY)
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// The upgrade steps that bring a cluster at `version` to
/// [`CLUSTER_AUTH_VERSION`], in the order they must run.
pub(super) fn cluster_auth_upgrade_steps(
    version: u64,
) -> &'static [crate::externalsvc::postgres_cluster::AuthUpgradeStep] {
    use crate::externalsvc::postgres_cluster::AuthUpgradeStep;
    if version >= CLUSTER_AUTH_VERSION {
        &[]
    } else if version >= CLUSTER_INFRA_AUTH_VERSION {
        // Re-applies the (idempotent) replicator rules, then the
        // application-rule decision.
        &[AuthUpgradeStep::NodeEnforce]
    } else {
        &AuthUpgradeStep::ORDER
    }
}

pub(super) fn is_role_primary(s: &str) -> bool {
    role_from_str(s) == Some(crate::ClusterRole::Primary)
}

/// Direction of a whole-cluster Stop or Start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClusterLifecycleAction {
    Stop,
    Start,
}

impl ClusterLifecycleAction {
    pub(super) fn verb(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Start => "start",
        }
    }

    /// `service_members.status` of a member this action succeeded on.
    pub(super) fn member_status(self) -> &'static str {
        match self {
            Self::Stop => "stopped",
            Self::Start => "running",
        }
    }

    /// Cluster `status` while this action holds it (see `claim_cluster`).
    pub(super) fn claim_status(self) -> &'static str {
        match self {
            Self::Stop => "stopping",
            Self::Start => "starting",
        }
    }
}

/// What a cluster Stop/Start did to the members it reached.
#[derive(Debug, Default)]
pub(super) struct ClusterLifecycleReport {
    pub(super) done: Vec<String>,
    pub(super) failures: Vec<String>,
    pub(super) not_attempted: Vec<String>,
}

/// The members a cluster Stop/Start acts on, in the order it acts on them.
///
/// Stop, when the monitor named the live `primary`: standbys, then the
/// primary, then the monitor. The monitor stays up while the data nodes go
/// down, and once no standby is running it has no candidate to promote, so
/// stopping the primary cannot trigger a failover.
///
/// Stop, when the live primary is unknown (the monitor could not be
/// queried): the monitor first, then the data members. Stored roles cannot
/// stand in for the live primary (the role reconciler records every data
/// member as `replica`), and promotion needs the monitor, so with it down no
/// order of the data members can cause a failover. If the monitor cannot be
/// stopped, Stop halts before touching any data member.
///
/// Start: the monitor first so every keeper can reach it, then the data
/// members (a stored `primary` first).
///
/// A member whose provisioning failed is still stopped (it may have a
/// running container the monitor could promote) but never started.
pub(super) fn cluster_lifecycle_order<'a>(
    members: &'a [service_members::Model],
    primary: Option<&str>,
    action: ClusterLifecycleAction,
) -> Vec<&'a service_members::Model> {
    // Lower tiers go first.
    let tier = |member: &service_members::Model| -> u8 {
        let monitor = is_role_monitor(&member.role);
        match (action, primary) {
            (ClusterLifecycleAction::Stop, Some(_)) if monitor => 2,
            (ClusterLifecycleAction::Stop, Some(primary)) => {
                u8::from(primary == member.container_name)
            }
            (ClusterLifecycleAction::Stop, None) => u8::from(!monitor),
            (ClusterLifecycleAction::Start, _) if monitor => 0,
            (ClusterLifecycleAction::Start, _) => {
                if is_role_primary(&member.role) {
                    1
                } else {
                    2
                }
            }
        }
    };
    let mut ordered: Vec<_> = members
        .iter()
        .filter(|member| action == ClusterLifecycleAction::Stop || member.status != "failed")
        .collect();
    ordered.sort_by_key(|m| (tier(m), m.ordinal));
    ordered
}

/// How long a `stopping`/`starting` claim may go without progress before
/// another Stop or Start may take it over. The claim is refreshed after
/// every member and one member's agent call is bounded at 300s, so only a
/// claim whose process died gets this old.
pub(super) const CLUSTER_CLAIM_STALE_AFTER: chrono::Duration = chrono::Duration::minutes(15);

/// Whether `action` may claim a cluster whose status is `status`, last
/// changed at `updated_at`.
pub(super) fn cluster_claim_decision(
    service_id: i32,
    status: &str,
    updated_at: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
    action: ClusterLifecycleAction,
) -> Result<(), ExternalServiceError> {
    let not_settled = |remedy| ExternalServiceError::ClusterNotSettled {
        service_id,
        action: action.verb(),
        status: status.to_string(),
        remedy,
    };
    match (status, action) {
        ("creating", _) => Err(not_settled("Wait until provisioning finishes, then retry")),
        ("failed", ClusterLifecycleAction::Start) => Err(not_settled(
            "Its provisioning failed; use Retry to provision it again instead of starting it",
        )),
        ("stopping" | "starting", _) if now - updated_at < CLUSTER_CLAIM_STALE_AFTER => {
            Err(ExternalServiceError::ClusterBusy {
                service_id,
                action: action.verb(),
            })
        }
        _ => Ok(()),
    }
}

/// The error for a cluster Stop/Start that did not reach every member:
/// each failed member, what was done, and what was left alone.
pub(super) fn cluster_lifecycle_failure(
    service: &external_services::Model,
    action: ClusterLifecycleAction,
    report: &ClusterLifecycleReport,
) -> ExternalServiceError {
    let list = |names: &[String]| {
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    };
    let mut reason = format!(
        "cluster '{}': could not {} {}. Members {} so far: {}",
        service.name,
        action.verb(),
        report.failures.join("; "),
        action.member_status(),
        list(&report.done)
    );
    if !report.not_attempted.is_empty() {
        reason.push_str(&format!(
            ". Left running so no standby is promoted: {}",
            list(&report.not_attempted)
        ));
    }
    reason.push_str(". Data volumes were not touched; fix the member above and retry");
    match action {
        ClusterLifecycleAction::Stop => ExternalServiceError::StopFailed {
            id: service.id,
            reason,
        },
        ClusterLifecycleAction::Start => ExternalServiceError::StartFailed {
            id: service.id,
            reason,
        },
    }
}

/// The status a cluster returns to when a lifecycle action does not
/// complete: the one it had before. A claim taken over from a process that
/// died has no meaningful "before", and its members may be running.
pub(super) fn cluster_status_before_claim(previous: &str) -> &str {
    match previous {
        "stopping" | "starting" => "running",
        other => other,
    }
}

/// The status to record for `member` once `action` succeeded on it, or
/// `None` to keep its row as is. A member whose provisioning failed stays
/// `failed`: stopping its leftover container does not make it startable,
/// and the next Start must keep skipping it.
pub(super) fn recorded_member_status(
    member: &service_members::Model,
    action: ClusterLifecycleAction,
) -> Option<&'static str> {
    (member.status != "failed").then(|| action.member_status())
}

/// Treat members a failed cluster left `pending` or `creating` as failed.
/// No provisioning task runs for a cluster whose provisioning failed (Retry
/// takes the lifecycle lock before starting one), so such a member is not
/// "still being provisioned": refusing a Stop because of it would leave the
/// cluster's leftover containers impossible to stop.
///
/// Returns the ids of the members it changed, whose rows must say so too.
pub(super) fn settle_members_of_failed_cluster(members: &mut [service_members::Model]) -> Vec<i32> {
    let mut abandoned = Vec::new();
    for member in members {
        if !cluster_member_is_settled(&member.status) {
            member.status = "failed".to_string();
            abandoned.push(member.id);
        }
    }
    abandoned
}

/// Member states Stop/Start can act on. Anything else (`pending`,
/// `creating`) is still being provisioned by a background task that would
/// race the action.
pub(super) fn cluster_member_is_settled(status: &str) -> bool {
    matches!(status, "running" | "stopped" | "failed")
}

/// What a remote member's observed container state already decides for
/// `action`: `Some(Ok(()))` when there is nothing left to do, `Some(Err)`
/// when it cannot be done, `None` when the agent must act.
///
/// Agents report Docker's "already stopped/started" and "no such
/// container" as plain errors, so the state is checked rather than the
/// error: a retried Stop must pass members an earlier attempt already
/// stopped instead of halting on them again.
pub(super) fn remote_member_outcome(
    member: &service_members::Model,
    action: ClusterLifecycleAction,
    status: &crate::remote_service_client::RemoteServiceStatus,
) -> Option<Result<(), String>> {
    let exists = status.container_id.is_some();
    match action {
        ClusterLifecycleAction::Stop
            if !status.running && (exists || member.status == "failed") =>
        {
            Some(Ok(()))
        }
        ClusterLifecycleAction::Start if status.running => Some(Ok(())),
        _ if !exists => Some(Err(format!(
            "its container '{}' does not exist on that node",
            member.container_name
        ))),
        _ => None,
    }
}

/// `true` for any role that holds data — primary, replica, or any
/// future data role we add. Matches the historical `role != "monitor"`
/// check exactly: unknown roles are treated as data members.
pub(super) fn is_role_data_member(s: &str) -> bool {
    role_from_str(s).map(|r| r.is_data_member()).unwrap_or(true)
}

/// Phases of the async `add_cluster_member` task. The strings here are
/// what gets written to `service_members.provisioning_step`; the
/// frontend renders them as a checklist.
///
/// Ordering: each step starts when the previous one finishes, so a
/// member at `provisioning_container` has already passed `validating`
/// and `inserting_row`. `done` and `failed` are terminal.
pub mod member_provisioning_step {
    pub const VALIDATING: &str = "validating";
    pub const RESOLVING_MONITOR: &str = "resolving_monitor";
    pub const INSERTING_ROW: &str = "inserting_row";
    pub const PROVISIONING_CONTAINER: &str = "provisioning_container";
    /// Initial cluster creation only: the monitor's container is up and the
    /// data nodes wait until it is ready to accept their registration.
    pub const WAITING_FOR_HEALTH: &str = "waiting_for_health";
    pub const REGISTERING_DNS: &str = "registering_dns";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";

    /// Ordered list, used by the frontend timeline.
    pub const ORDER: &[&str] = &[
        VALIDATING,
        RESOLVING_MONITOR,
        INSERTING_ROW,
        PROVISIONING_CONTAINER,
        WAITING_FOR_HEALTH,
        REGISTERING_DNS,
        DONE,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::HealthProbeStatus;
    use crate::services::test_support::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use temps_entities::service_members;

    #[test]
    fn cluster_auth_version_reads_the_marker_and_defaults_to_legacy() {
        let mut parameters = HashMap::new();
        assert_eq!(cluster_auth_version(&parameters), 0);
        parameters.insert(CLUSTER_AUTH_VERSION_KEY.to_string(), serde_json::json!("1"));
        assert_eq!(cluster_auth_version(&parameters), 0, "only integers count");
        parameters.insert(CLUSTER_AUTH_VERSION_KEY.to_string(), serde_json::json!(1));
        assert_eq!(cluster_auth_version(&parameters), 1);
    }

    #[test]
    fn cluster_auth_upgrade_steps_rerun_only_the_data_node_step_after_infra_convergence() {
        use crate::externalsvc::postgres_cluster::AuthUpgradeStep;
        assert_eq!(cluster_auth_upgrade_steps(0), &AuthUpgradeStep::ORDER);
        assert_eq!(
            cluster_auth_upgrade_steps(CLUSTER_INFRA_AUTH_VERSION),
            &[AuthUpgradeStep::NodeEnforce]
        );
        assert!(cluster_auth_upgrade_steps(CLUSTER_AUTH_VERSION).is_empty());
        assert!(cluster_auth_upgrade_steps(CLUSTER_AUTH_VERSION + 1).is_empty());
        // The data-node step is the one that decides the application rules,
        // and the full upgrade includes it, so both paths end at the same
        // version.
        assert!(AuthUpgradeStep::ORDER.contains(&AuthUpgradeStep::NodeEnforce));
        assert!(!AuthUpgradeStep::NodeEnforce.targets_monitor());
    }

    /// Node states with an explicit monitor health value.
    fn st_h(triples: &[(&str, &str, i32)]) -> Vec<ClusterNodeState> {
        triples
            .iter()
            .map(|(n, s, h)| ClusterNodeState {
                name: n.to_string(),
                state: s.to_string(),
                health: *h,
            })
            .collect()
    }

    #[test]
    fn monitor_primary_identity_cannot_authorize_an_unstored_endpoint() {
        let members = vec![
            service_member_info(1, "cluster-node-1", "node", "running"),
            service_member_info(2, "cluster-node-2", "node", "running"),
            service_member_info(3, "cluster-monitor", "monitor", "running"),
        ];

        let selected = trusted_primary_member(&members, "cluster-node-1")
            .expect("a unique persisted running data member should be selected");
        assert_eq!(selected.id, 1);

        // A forged monitor row can supply any nodehost/nodeport, but only its
        // nodename crosses this boundary. An identity not persisted for this
        // service cannot become a credential destination.
        assert!(trusted_primary_member(&members, "attacker.example").is_none());
        assert!(trusted_primary_member(&members, "cluster-monitor").is_none());

        let stopped = vec![service_member_info(4, "cluster-node-4", "node", "stopped")];
        assert!(trusted_primary_member(&stopped, "cluster-node-4").is_none());

        let duplicates = vec![
            service_member_info(5, "cluster-node-5", "node", "running"),
            service_member_info(6, "cluster-node-5", "node", "running"),
        ];
        assert!(trusted_primary_member(&duplicates, "cluster-node-5").is_none());
    }

    /// Regression for deployment environment resolution and cluster backups:
    /// both paths used to hand-match only `primary | single`, so an application
    /// deployment could fail with "no running primary data node" during the
    /// normal writable `wait_primary` state even though cluster health passed.
    #[test]
    fn writable_primary_live_state_includes_wait_primary() {
        for state in ["primary", "single", "wait_primary"] {
            assert!(
                live_state_is_writable_primary(Some(state)),
                "{state} must be accepted as a writable primary"
            );
        }

        for state in ["secondary", "catchingup", "demoted", "unknown"] {
            assert!(
                !live_state_is_writable_primary(Some(state)),
                "{state} must not be accepted as a writable primary"
            );
        }
        assert!(!live_state_is_writable_primary(None));
    }

    /// A dead node keeps its last reported `primary` state in the monitor.
    /// Selection must ignore that stale row, choose the healthy promoted
    /// `wait_primary`, and fail closed if two live writers are ever reported.
    #[test]
    fn healthy_primary_selection_ignores_stale_rows_and_rejects_ambiguity() {
        let mut unhealthy_primary = cluster_member_health("orders-1", "primary");
        unhealthy_primary.health = 0;
        unhealthy_primary.seconds_since_report = 1;
        let promoted = cluster_member_health("orders-2", "wait_primary");
        let unhealthy_report = ClusterHealthReport {
            configured_standbys: 0,
            checked_at: chrono::Utc::now(),
            monitor_response_ms: 5,
            monitor_error: None,
            members: vec![unhealthy_primary, promoted.clone()],
        };
        assert_eq!(
            healthy_writable_primary_nodename(&unhealthy_report),
            Some("orders-2")
        );

        let mut stale_primary = cluster_member_health("orders-1", "primary");
        stale_primary.health = 1;
        stale_primary.seconds_since_report = 30;
        let stale_report = ClusterHealthReport {
            configured_standbys: 0,
            checked_at: chrono::Utc::now(),
            monitor_response_ms: 5,
            monitor_error: None,
            members: vec![stale_primary, promoted],
        };
        assert_eq!(
            healthy_writable_primary_nodename(&stale_report),
            Some("orders-2")
        );

        let ambiguous = ClusterHealthReport {
            configured_standbys: 0,
            checked_at: chrono::Utc::now(),
            monitor_response_ms: 5,
            monitor_error: None,
            members: vec![
                cluster_member_health("orders-1", "primary"),
                cluster_member_health("orders-2", "wait_primary"),
            ],
        };
        assert!(healthy_writable_primary_nodename(&ambiguous).is_none());

        let unreachable = ClusterHealthReport {
            configured_standbys: 0,
            checked_at: chrono::Utc::now(),
            monitor_response_ms: 0,
            monitor_error: Some("monitor unavailable".to_string()),
            members: vec![cluster_member_health("orders-2", "wait_primary")],
        };
        assert!(healthy_writable_primary_nodename(&unreachable).is_none());
    }

    /// When no primary can be trusted, the error says what the monitor saw,
    /// so a failed backup explains itself instead of claiming the cluster
    /// simply has no primary.
    #[test]
    fn monitor_state_summary_names_each_node_and_why_it_was_not_trusted() {
        let mut stale_primary = cluster_member_health("orders-1", "primary");
        stale_primary.health = 0;
        stale_primary.seconds_since_report = 45;
        let report = health_report(vec![
            stale_primary,
            cluster_member_health("orders-2", "secondary"),
        ]);
        assert_eq!(
            describe_monitor_states(&report),
            "orders-1=primary (unhealthy, 45s since report), orders-2=secondary"
        );
        assert_eq!(
            describe_monitor_states(&health_report(vec![])),
            "no registered data nodes"
        );
    }

    /// The total-outage case, and the reason `health` is read at all: when
    /// every node dies at once the monitor has nothing to promote, so it never
    /// demotes anyone and `reportedstate` still says primary/secondary. Judging
    /// on state alone reported a dead cluster as Operational.
    #[test]
    fn test_all_nodes_unreachable_is_leaderless_despite_stale_primary_state() {
        let (status, msg) = classify_cluster_states(
            2,
            &st_h(&[("node-1", "primary", 0), ("node-2", "secondary", 0)]),
        );
        let msg = msg.expect("a dead cluster must not be silent");

        assert_eq!(status, HealthProbeStatus::Degraded);
        assert!(msg.contains("no leader"), "got: {msg}");
        assert!(msg.contains("writes will fail"), "got: {msg}");
        // The operator needs to see it is a reachability problem, not an
        // election problem — the state still reads "primary".
        assert!(msg.contains("node-1=primary (unreachable)"), "got: {msg}");
    }

    /// A primary the monitor has not yet health-checked (-1) must not be
    /// treated as dead, or every freshly registered cluster would alarm.
    #[test]
    fn test_unchecked_health_is_not_treated_as_failure() {
        let (status, msg) = classify_cluster_states(
            1,
            &st_h(&[("node-1", "primary", -1), ("node-2", "secondary", -1)]),
        );
        assert_eq!(status, HealthProbeStatus::Operational, "got: {msg:?}");
    }

    /// A live primary with a dead standby still serves writes — that is the
    /// unprotected warning, not the leaderless one.
    #[test]
    fn test_dead_standby_leaves_a_working_primary() {
        let (status, msg) = classify_cluster_states(
            1,
            &st_h(&[("node-1", "primary", 1), ("node-2", "secondary", 0)]),
        );
        let msg = msg.expect("degraded");

        assert_eq!(status, HealthProbeStatus::Degraded);
        assert!(!msg.contains("writes will fail"), "got: {msg}");
        assert!(msg.contains("no healthy standby"), "got: {msg}");
        assert!(msg.contains("node-2=secondary (unreachable)"), "got: {msg}");
    }

    /// The condition that actually breaks an application: nothing can accept a
    /// write. The operator has to be told that plainly, and told how to get out
    /// of it — the promote endpoint is the only self-service recovery.
    #[test]
    fn test_no_writable_node_warns_about_writes_and_names_the_recovery() {
        let (status, msg) = classify_cluster_states(
            7,
            &st(&[("node-1", "catchingup"), ("node-2", "wait_standby")]),
        );
        let msg = msg.expect("must explain itself");

        assert_eq!(status, HealthProbeStatus::Degraded);
        assert!(msg.contains("no leader"), "got: {msg}");
        assert!(msg.contains("writes will fail"), "got: {msg}");
        // Actionable: names the endpoint and the service it applies to.
        assert!(msg.contains("/external-services/7/members/"), "got: {msg}");
        assert!(msg.contains("promote"), "got: {msg}");
        // And still lists the states, so the operator can see why.
        assert!(msg.contains("node-1=catchingup"), "got: {msg}");
    }

    /// `wait_primary` accepts writes — pg_auto_failover clears
    /// `synchronous_standby_names` there so the cluster keeps serving without a
    /// standby. Warning "writes will fail" would be flatly wrong, and this is
    /// the exact state a half-built cluster sits in.
    #[test]
    fn test_wait_primary_is_not_reported_as_leaderless() {
        let (status, msg) = classify_cluster_states(
            1,
            &st(&[("node-1", "wait_primary"), ("node-2", "wait_standby")]),
        );
        let msg = msg.expect("still degraded — no standby");

        assert_eq!(status, HealthProbeStatus::Degraded);
        assert!(!msg.contains("writes will fail"), "got: {msg}");
        assert!(!msg.contains("no leader"), "got: {msg}");
        // It gets the milder, accurate warning instead.
        assert!(msg.contains("Writes are being accepted"), "got: {msg}");
        assert!(msg.contains("no healthy standby"), "got: {msg}");
    }

    /// `single` is a one-node cluster: writable, and legitimately has no
    /// standby.
    #[test]
    fn test_single_node_is_writable() {
        let (status, msg) = classify_cluster_states(1, &st(&[("node-1", "single")]));
        assert_eq!(status, HealthProbeStatus::Operational);
        assert!(msg.is_none(), "got: {msg:?}");
    }

    /// A failover passes through these states for a few seconds. Reporting a
    /// stuck cluster there would flap on every normal promotion.
    #[test]
    fn test_failover_in_flight_is_not_reported_as_stuck() {
        for transient in ["prepare_promotion", "stop_replication", "demoted"] {
            let (status, msg) =
                classify_cluster_states(1, &st(&[("node-1", transient), ("node-2", "catchingup")]));
            let msg = msg.expect("should say something");

            assert_eq!(status, HealthProbeStatus::Degraded);
            assert!(
                msg.contains("Failover in progress"),
                "{transient} should read as a failover, got: {msg}"
            );
            assert!(
                !msg.contains("writes will fail"),
                "{transient} must not be reported as permanently broken, got: {msg}"
            );
        }
    }

    /// A healthy pair stays quiet — no warning fatigue.
    #[test]
    fn test_primary_plus_secondary_is_operational() {
        let (status, msg) =
            classify_cluster_states(1, &st(&[("node-1", "primary"), ("node-2", "secondary")]));
        assert_eq!(status, HealthProbeStatus::Operational);
        assert!(msg.is_none());
    }

    /// A primary with a replica still catching up is degraded, but it has a
    /// standby — so it must NOT get the "no standby" wording.
    #[test]
    fn test_catching_up_replica_is_degraded_but_not_unprotected() {
        let (_, msg) =
            classify_cluster_states(1, &st(&[("node-1", "primary"), ("node-2", "catchingup")]));
        let msg = msg.expect("degraded");
        assert!(msg.contains("node-2=catchingup"), "got: {msg}");
        assert!(!msg.contains("writes will fail"), "got: {msg}");
    }

    // ── Cluster member placement ────────────────────────────────────────

    fn lifecycle_names(members: Vec<&service_members::Model>) -> Vec<&str> {
        members
            .into_iter()
            .map(|member| member.container_name.as_str())
            .collect()
    }

    /// Regression for #1353: with the live primary known, a cluster is
    /// stopped standbys first, primary next and monitor last (so no standby
    /// is left for the monitor to promote); it is started monitor first.
    #[test]
    fn cluster_lifecycle_order_stops_standbys_first_and_starts_the_monitor_first() {
        let members = vec![
            cluster_member(0, "monitor", "running"),
            cluster_member(1, "primary", "running"),
            cluster_member(2, "replica", "running"),
            cluster_member(3, "replica", "failed"),
        ];
        assert_eq!(
            lifecycle_names(cluster_lifecycle_order(
                &members,
                Some("postgres-ha-1"),
                ClusterLifecycleAction::Stop
            )),
            [
                "postgres-ha-2",
                "postgres-ha-3",
                "postgres-ha-1",
                "postgres-ha-monitor"
            ]
        );
        // A failed member may still run a container, so it is stopped, but
        // it is never started.
        assert_eq!(
            lifecycle_names(cluster_lifecycle_order(
                &members,
                None,
                ClusterLifecycleAction::Start
            )),
            ["postgres-ha-monitor", "postgres-ha-1", "postgres-ha-2"]
        );
        // After a failover the monitor's live primary wins over the stored
        // role, so the old primary (now a standby) is stopped first.
        assert_eq!(
            lifecycle_names(cluster_lifecycle_order(
                &members,
                Some("postgres-ha-2"),
                ClusterLifecycleAction::Stop
            )),
            [
                "postgres-ha-1",
                "postgres-ha-3",
                "postgres-ha-2",
                "postgres-ha-monitor"
            ]
        );
    }

    /// Greptile on #1366: when the monitor cannot name the live primary,
    /// stored roles cannot stand in for it (data members are all stored as
    /// `replica`), so Stop takes the monitor down first: without it no
    /// standby can be promoted, whatever order the data members follow.
    #[test]
    fn cluster_stop_without_a_known_primary_stops_the_monitor_first() {
        let members = vec![
            cluster_member(1, "replica", "running"),
            cluster_member(0, "monitor", "running"),
            cluster_member(2, "replica", "running"),
            cluster_member(3, "primary", "running"),
        ];
        assert_eq!(
            lifecycle_names(cluster_lifecycle_order(
                &members,
                None,
                ClusterLifecycleAction::Stop
            )),
            [
                "postgres-ha-monitor",
                "postgres-ha-1",
                "postgres-ha-2",
                "postgres-ha-3"
            ]
        );
    }

    /// Greptile on #1366: a claim whose process died is taken over once it
    /// has made no progress for `CLUSTER_CLAIM_STALE_AFTER`; a live one is
    /// busy. Settled clusters can be claimed by either action.
    #[test]
    fn cluster_claims_expire_only_when_stale() {
        let now = Utc::now();
        let fresh = now - chrono::Duration::minutes(1);
        let stale = now - CLUSTER_CLAIM_STALE_AFTER - chrono::Duration::seconds(1);
        let stop = ClusterLifecycleAction::Stop;
        let start = ClusterLifecycleAction::Start;
        assert!(matches!(
            cluster_claim_decision(7, "stopping", fresh, now, start),
            Err(ExternalServiceError::ClusterBusy { .. })
        ));
        assert!(cluster_claim_decision(7, "stopping", stale, now, start).is_ok());
        assert!(cluster_claim_decision(7, "starting", stale, now, stop).is_ok());
        for status in ["running", "stopped"] {
            assert!(cluster_claim_decision(7, status, fresh, now, stop).is_ok());
            assert!(cluster_claim_decision(7, status, fresh, now, start).is_ok());
        }
        assert!(cluster_claim_decision(7, "failed", fresh, now, stop).is_ok());

        assert_eq!(cluster_status_before_claim("stopped"), "stopped");
        assert_eq!(cluster_status_before_claim("failed"), "failed");
        assert_eq!(cluster_status_before_claim("stopping"), "running");
    }

    /// A cluster whose provisioning failed can still be stopped: members it
    /// left `pending`/`creating` are failed, not being provisioned, and keep
    /// that status.
    #[test]
    fn members_of_a_failed_cluster_are_settled_as_failed() {
        let mut members = vec![
            cluster_member(0, "monitor", "running"),
            cluster_member(1, "primary", "pending"),
            cluster_member(2, "replica", "creating"),
            cluster_member(3, "replica", "stopped"),
        ];
        let abandoned = settle_members_of_failed_cluster(&mut members);
        assert_eq!(abandoned, [members[1].id, members[2].id]);
        let statuses: Vec<&str> = members.iter().map(|m| m.status.as_str()).collect();
        assert_eq!(statuses, ["running", "failed", "failed", "stopped"]);
        assert!(members.iter().all(|m| cluster_member_is_settled(&m.status)));
        assert_eq!(
            recorded_member_status(&members[1], ClusterLifecycleAction::Stop),
            None
        );
    }

    /// Greptile on #1366: stopping a member whose provisioning failed keeps
    /// it `failed`, so the next Start still skips it.
    #[test]
    fn a_failed_member_stays_failed_after_a_lifecycle_action() {
        for action in [ClusterLifecycleAction::Stop, ClusterLifecycleAction::Start] {
            assert_eq!(
                recorded_member_status(&cluster_member(3, "replica", "failed"), action),
                None
            );
        }
        assert_eq!(
            recorded_member_status(
                &cluster_member(1, "primary", "running"),
                ClusterLifecycleAction::Stop
            ),
            Some("stopped")
        );
        assert_eq!(
            recorded_member_status(
                &cluster_member(1, "primary", "stopped"),
                ClusterLifecycleAction::Start
            ),
            Some("running")
        );
        let stopped_failed = cluster_member(3, "replica", "failed");
        assert!(cluster_lifecycle_order(
            std::slice::from_ref(&stopped_failed),
            None,
            ClusterLifecycleAction::Start
        )
        .is_empty());
    }

    /// Greptile on #1366: a retried Stop/Start passes remote members an
    /// earlier attempt already handled, judged by the container's state
    /// because agents report "already stopped/started" as errors.
    #[test]
    fn remote_member_outcome_is_judged_by_container_state() {
        use crate::remote_service_client::RemoteServiceStatus;
        let observed = |exists: bool, running: bool| RemoteServiceStatus {
            container_name: "postgres-ha-2".to_string(),
            container_id: exists.then(|| "abc123".to_string()),
            running,
            health: None,
        };
        let healthy = cluster_member(2, "replica", "running");
        let failed = cluster_member(2, "replica", "failed");
        let stop = ClusterLifecycleAction::Stop;
        let start = ClusterLifecycleAction::Start;

        // Already in the wanted state: nothing to do.
        assert_eq!(
            remote_member_outcome(&healthy, stop, &observed(true, false)),
            Some(Ok(()))
        );
        assert_eq!(
            remote_member_outcome(&healthy, start, &observed(true, true)),
            Some(Ok(()))
        );
        // The agent must act.
        assert_eq!(
            remote_member_outcome(&healthy, stop, &observed(true, true)),
            None
        );
        assert_eq!(
            remote_member_outcome(&healthy, start, &observed(true, false)),
            None
        );
        // A missing container: fine to "stop" for a failed member only.
        assert_eq!(
            remote_member_outcome(&failed, stop, &observed(false, false)),
            Some(Ok(()))
        );
        for (member, action) in [(&healthy, stop), (&healthy, start), (&failed, start)] {
            let outcome = remote_member_outcome(member, action, &observed(false, false));
            assert!(
                matches!(&outcome, Some(Err(reason)) if reason.contains("does not exist")),
                "{action:?}: {outcome:?}"
            );
        }
    }

    fn replication_keys() -> Vec<(i64, String, String)> {
        // Two data members on the same worker share a nodehost.
        vec![
            (1, "postgres-ha-pg-1".to_string(), "10.52.0.21".to_string()),
            (2, "postgres-ha-pg-2".to_string(), "10.52.0.21".to_string()),
            (3, "postgres-ha-pg-3".to_string(), "10.52.0.22".to_string()),
        ]
    }

    /// Regression: replication state was joined by `client_addr`, so members
    /// sharing a worker (or behind NAT) never showed sync state or lag.
    #[test]
    fn replication_row_matches_standby_by_application_name() {
        let keys = replication_keys();
        // The standby's source address is the worker's Docker gateway, which
        // matches no member at all.
        assert_eq!(
            replication_row_member("pgautofailover_standby_2", Some("10.99.0.1"), &keys),
            Some("postgres-ha-pg-2".to_string())
        );
        // Co-located members are told apart by node id, not by host.
        assert_eq!(
            replication_row_member("pgautofailover_standby_1", Some("10.52.0.21"), &keys),
            Some("postgres-ha-pg-1".to_string())
        );
    }

    #[test]
    fn replication_row_falls_back_to_a_unique_client_address_only() {
        let keys = replication_keys();
        assert_eq!(
            replication_row_member("walreceiver", Some("10.52.0.22"), &keys),
            Some("postgres-ha-pg-3".to_string())
        );
        // Ambiguous: two members registered this host.
        assert_eq!(
            replication_row_member("walreceiver", Some("10.52.0.21"), &keys),
            None
        );
        assert_eq!(
            replication_row_member("pgautofailover_standby_9", None, &keys),
            None
        );
        assert_eq!(
            replication_row_member("pgautofailover_standby_x", None, &keys),
            None
        );
    }
}
