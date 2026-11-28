// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a stored project/environment IP access policy refuses Temps' own
//! managed uptime monitor.
//!
//! The proxy answers a refused client with a generic 403 and must keep doing
//! so: an outsider may not learn that a project is restricted. The monitor
//! knows the addresses it connects from, so it asks here instead, and this
//! evaluates the stored policy with the same matcher the gate enforces it
//! with ([`evaluate_with_set`]). It runs only on the control plane: when a
//! managed check is refused, when the monitor status is read, and when the
//! IP access settings are read or saved. Never per proxied request.

use std::net::IpAddr;

use async_trait::async_trait;
use temps_core::uptime_monitor_ip_access::{
    IpAccessMode, IpAccessPolicyScope, UptimeMonitorBlockReason, UptimeMonitorIpAccess,
    UptimeMonitorIpAccessError, UptimeMonitorIpBlock,
};
use temps_entities::project_ip_access_rules::IpAccessCidrEntry;

use super::project_ip_access_service::{aligned_policy, evaluate_with_set, ProjectIpAccessService};

/// Evaluate one stored policy against the monitor's addresses.
///
/// Mirrors the gate's compile step: an unknown mode, or a deny list with an
/// entry that no longer parses, refuses every address; an allow list skips
/// an unreadable entry. `None` means every monitor address gets through (or
/// none is known).
pub(crate) fn evaluate_monitor_access(
    project_id: i32,
    environment_id: Option<i32>,
    scope: IpAccessPolicyScope,
    stored_mode: &str,
    cidrs: Vec<IpAccessCidrEntry>,
    monitor_addresses: &[IpAddr],
) -> Option<UptimeMonitorIpBlock> {
    if monitor_addresses.is_empty() {
        return None;
    }
    let total = cidrs.len();
    let (entries, networks) = aligned_policy(cidrs);
    let unreadable = total - entries.len();
    let block = |mode, reason, blocked: &[IpAddr], matched| {
        Some(UptimeMonitorIpBlock::new(
            project_id,
            environment_id,
            scope,
            mode,
            reason,
            monitor_addresses,
            blocked,
            matched,
        ))
    };

    let mode = match IpAccessMode::parse(stored_mode) {
        None => {
            return block(
                IpAccessMode::Allow,
                UptimeMonitorBlockReason::Unreadable,
                monitor_addresses,
                Vec::new(),
            )
        }
        Some(IpAccessMode::Disabled) => return None,
        Some(IpAccessMode::Deny) if unreadable > 0 => {
            return block(
                IpAccessMode::Deny,
                UptimeMonitorBlockReason::Unreadable,
                monitor_addresses,
                Vec::new(),
            )
        }
        Some(mode) => mode,
    };

    let blocked: Vec<IpAddr> = monitor_addresses
        .iter()
        .copied()
        .filter(|ip| !evaluate_with_set(mode, &networks, *ip))
        .collect();
    if blocked.is_empty() {
        return None;
    }
    match mode {
        IpAccessMode::Allow => block(
            mode,
            UptimeMonitorBlockReason::NotAllowListed,
            &blocked,
            Vec::new(),
        ),
        IpAccessMode::Deny => {
            let mut matched: Vec<String> = Vec::new();
            for (entry, network) in entries.iter().zip(&networks) {
                if blocked.iter().any(|ip| network.contains(*ip)) && !matched.contains(&entry.cidr)
                {
                    matched.push(entry.cidr.clone());
                }
            }
            block(
                mode,
                UptimeMonitorBlockReason::DenyListed,
                &blocked,
                matched,
            )
        }
        // Returned above: a disabled policy enforces nothing.
        IpAccessMode::Disabled => None,
    }
}

#[async_trait]
impl UptimeMonitorIpAccess for ProjectIpAccessService {
    async fn monitor_block(
        &self,
        project_id: i32,
        environment_id: i32,
        monitor_addresses: &[IpAddr],
    ) -> Result<Option<UptimeMonitorIpBlock>, UptimeMonitorIpAccessError> {
        // Behind a plugin gate the saved lists are not what the proxy
        // enforces, so they cannot explain the 403.
        let Some(gate) = self.enforcing_gate() else {
            return Ok(None);
        };
        // A check result is explained only by what the proxy enforces now:
        // a saved change reaches the gate within its poll interval, and in
        // between the saved list and the enforced one disagree. An address
        // the gate admits was not refused by it, whatever is saved.
        let refused: Vec<IpAddr> = monitor_addresses
            .iter()
            .copied()
            .filter(|ip| !gate.is_allowed(project_id, environment_id, *ip))
            .collect();
        if refused.is_empty() {
            return Ok(None);
        }
        let monitor_addresses = refused.as_slice();
        let governing = self
            .governing_policy(project_id, Some(environment_id))
            .await
            .map_err(|error| UptimeMonitorIpAccessError::PolicyUnavailable {
                project_id,
                environment_id,
                reason: error.to_string(),
            })?;
        Ok(governing.and_then(|(scope, row)| {
            evaluate_monitor_access(
                project_id,
                Some(environment_id),
                scope,
                &row.mode,
                row.cidrs.0,
                monitor_addresses,
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use temps_entities::project_ip_access_rules::{self, IpAccessCidrs};

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP is valid")
    }

    fn entries(cidrs: &[&str]) -> Vec<IpAccessCidrEntry> {
        cidrs
            .iter()
            .map(|cidr| IpAccessCidrEntry {
                cidr: cidr.to_string(),
                description: None,
            })
            .collect()
    }

    fn row(
        environment_id: Option<i32>,
        mode: &str,
        cidrs: &[&str],
    ) -> project_ip_access_rules::Model {
        let now = chrono::Utc::now();
        project_ip_access_rules::Model {
            id: 1,
            project_id: 5,
            environment_id,
            mode: mode.into(),
            cidrs: IpAccessCidrs(entries(cidrs)),
            updated_by: None,
            revision: 1,
            created_at: now,
            updated_at: now,
        }
    }

    fn id_row(id: i32) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([("id", sea_orm::Value::Int(Some(id)))])
    }

    /// A private app's list: one operator address plus the tailnet ranges.
    const OPERATOR_AND_TAILNET: &[&str] =
        &["198.51.100.7/32", "100.64.0.0/10", "fd7a:115c:a1e0::/48"];

    /// A wildcard HTTP listener and a TLS listener bound to the server's
    /// public address.
    fn monitor() -> Vec<IpAddr> {
        vec![ip("127.0.0.1"), ip("203.0.113.10")]
    }

    /// A gate that, like the proxy's built-in one, does or does not decide
    /// from the saved lists, and currently refuses `refused`.
    struct Gate {
        reads_saved_lists: bool,
        refused: Vec<IpAddr>,
    }

    impl temps_core::ProjectIpGate for Gate {
        fn is_allowed(&self, _project_id: i32, _environment_id: i32, ip: IpAddr) -> bool {
            !self.refused.contains(&ip)
        }

        fn reads_project_ip_access_rules(&self) -> bool {
            self.reads_saved_lists
        }
    }

    /// The service as the proxy plugin builds it behind the built-in gate,
    /// whose enforced snapshot refuses the monitor's addresses.
    fn service(db: sea_orm::DatabaseConnection) -> ProjectIpAccessService {
        service_behind(
            db,
            Gate {
                reads_saved_lists: true,
                refused: monitor(),
            },
        )
    }

    fn service_behind(db: sea_orm::DatabaseConnection, gate: Gate) -> ProjectIpAccessService {
        ProjectIpAccessService::new(Arc::new(db)).with_monitor_addresses(monitor(), Arc::new(gate))
    }

    fn evaluate(mode: &str, cidrs: &[&str]) -> Option<UptimeMonitorIpBlock> {
        evaluate_monitor_access(
            5,
            Some(9),
            IpAccessPolicyScope::Project,
            mode,
            entries(cidrs),
            &monitor(),
        )
    }

    #[test]
    fn an_allow_list_without_the_monitor_addresses_blocks_it() {
        let block = evaluate("allow", OPERATOR_AND_TAILNET).expect("monitor is blocked");
        assert_eq!(block.reason, UptimeMonitorBlockReason::NotAllowListed);
        assert_eq!(block.mode, IpAccessMode::Allow);
        assert_eq!(block.blocked_addresses, vec!["127.0.0.1", "203.0.113.10"]);
        assert_eq!(
            block.suggested_cidrs,
            vec!["127.0.0.1/32", "203.0.113.10/32"]
        );
        assert_eq!(block.environment_id, Some(9));
        assert!(block.message.contains("127.0.0.1 and 203.0.113.10"));
    }

    #[test]
    fn allowing_loopback_alone_still_blocks_the_public_listener() {
        // The trap from the report: loopback is not where an HTTPS probe to
        // a listener bound to a public address comes from.
        let block = evaluate("allow", &["198.51.100.7/32", "127.0.0.1/32", "::1/128"])
            .expect("the TLS listener's address is still refused");
        assert_eq!(block.blocked_addresses, vec!["203.0.113.10"]);
        assert_eq!(block.suggested_cidrs, vec!["203.0.113.10/32"]);
    }

    #[test]
    fn an_allow_list_with_every_monitor_address_does_not_warn() {
        assert!(evaluate(
            "allow",
            &["198.51.100.7/32", "127.0.0.0/8", "203.0.113.10/32"]
        )
        .is_none());
    }

    #[test]
    fn a_disabled_policy_never_blocks() {
        assert!(evaluate("disabled", OPERATOR_AND_TAILNET).is_none());
        assert!(evaluate("disabled", &[]).is_none());
    }

    #[test]
    fn a_deny_list_that_does_not_cover_the_monitor_does_not_warn() {
        assert!(evaluate("deny", OPERATOR_AND_TAILNET).is_none());
        assert!(evaluate("deny", &[]).is_none());
    }

    #[test]
    fn a_deny_list_covering_the_monitor_names_the_entry_to_remove() {
        let block = evaluate("deny", &["198.51.100.0/24", "127.0.0.0/8"]).expect("blocked");
        assert_eq!(block.reason, UptimeMonitorBlockReason::DenyListed);
        assert_eq!(block.blocked_addresses, vec!["127.0.0.1"]);
        assert_eq!(block.matched_cidrs, vec!["127.0.0.0/8"]);
        assert!(block.suggested_cidrs.is_empty());
    }

    #[test]
    fn an_unreadable_policy_refuses_the_monitor_like_the_gate() {
        let unknown_mode = evaluate("block", &["127.0.0.0/8"]).expect("fails closed");
        assert_eq!(unknown_mode.reason, UptimeMonitorBlockReason::Unreadable);
        let broken_deny = evaluate("deny", &["not-a-cidr"]).expect("fails closed");
        assert_eq!(broken_deny.reason, UptimeMonitorBlockReason::Unreadable);
        assert_eq!(broken_deny.blocked_addresses.len(), 2);
    }

    #[test]
    fn no_known_monitor_address_means_no_verdict() {
        assert!(evaluate_monitor_access(
            5,
            Some(9),
            IpAccessPolicyScope::Project,
            "allow",
            entries(OPERATOR_AND_TAILNET),
            &[],
        )
        .is_none());
    }

    #[tokio::test]
    async fn an_environment_override_replaces_the_project_list() {
        // The environment's own list lets the monitor in; the project list
        // (never read) would not.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row(
                Some(9),
                "allow",
                &["127.0.0.1/32", "203.0.113.10/32"],
            )]])
            .into_connection();
        let service = service(db);
        let block = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect("policy is readable");
        assert!(block.is_none());
    }

    #[tokio::test]
    async fn an_environment_override_that_blocks_is_reported_on_the_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row(Some(9), "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service = service(db);
        let block = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect("policy is readable")
            .expect("blocked");
        assert_eq!(block.policy_scope, IpAccessPolicyScope::Environment);
        assert!(block.message.contains("this environment's IP access list"));
    }

    #[tokio::test]
    async fn an_environment_without_a_list_inherits_the_project_block() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .append_query_results(vec![vec![row(None, "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service = service(db);
        let block = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect("policy is readable")
            .expect("blocked");
        assert_eq!(block.policy_scope, IpAccessPolicyScope::Project);
        assert_eq!(block.environment_id, Some(9));
    }

    #[tokio::test]
    async fn a_database_error_is_reported_with_the_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let service = service(db);
        let error = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect_err("database error");
        let message = error.to_string();
        assert!(message.contains("environment 9 in project 5"), "{message}");
        assert!(message.contains("connection reset"), "{message}");
    }

    #[tokio::test]
    async fn the_settings_response_carries_the_block_and_a_warning() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![row(None, "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service = service(db);
        let response = service.get(5, None).await.expect("get succeeds");
        let block = response.uptime_monitor_block.expect("blocked");
        assert_eq!(block.policy_scope, IpAccessPolicyScope::Project);
        assert_eq!(block.environment_id, None);
        assert_eq!(response.warnings, vec![block.message]);
    }

    #[tokio::test]
    async fn an_inheriting_environment_reports_the_project_lists_block() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Project exists, environment exists.
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![id_row(9)]])
            // No list of its own, then the project's.
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .append_query_results(vec![vec![row(None, "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service = service(db);
        let response = service.get(5, Some(9)).await.expect("get succeeds");
        assert!(!response.configured);
        let block = response.uptime_monitor_block.expect("blocked");
        assert_eq!(block.policy_scope, IpAccessPolicyScope::Project);
    }

    #[tokio::test]
    async fn a_settings_response_without_monitor_addresses_has_no_block() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![row(None, "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));
        let response = service.get(5, None).await.expect("get succeeds");
        assert!(response.uptime_monitor_block.is_none());
        assert!(response.warnings.is_empty());
    }

    #[tokio::test]
    async fn behind_a_plugin_gate_the_saved_lists_explain_nothing() {
        // The saved list would refuse the monitor, but the proxy enforces a
        // plugin's gate instead, so a 403 is not this list's doing.
        let gate = Arc::new(Gate {
            reads_saved_lists: false,
            refused: monitor(),
        });
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![row(None, "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service =
            ProjectIpAccessService::new(Arc::new(db)).with_monitor_addresses(monitor(), gate);

        // Reads nothing: the mock's rows are the GET's below, which would
        // fail had this consumed them.
        let block = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect("nothing to read");
        assert!(block.is_none());

        let response = service.get(5, None).await.expect("get succeeds");
        assert!(response.uptime_monitor_block.is_none());
        assert!(response.warnings.is_empty());
    }

    #[tokio::test]
    async fn a_saved_list_the_proxy_does_not_enforce_yet_explains_nothing() {
        // The saved list refuses the monitor, but the gate's snapshot (from
        // before the save) still admits it, so a 403 now came from the app.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service_behind(
            db,
            Gate {
                reads_saved_lists: true,
                refused: Vec::new(),
            },
        );
        // No rows in the mock: reading the saved list would fail the test.
        let block = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect("nothing is read");
        assert!(block.is_none());
    }

    #[tokio::test]
    async fn only_the_addresses_the_proxy_refuses_are_explained() {
        // The saved list refuses both addresses; the proxy has only started
        // refusing the public one.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row(Some(9), "allow", OPERATOR_AND_TAILNET)]])
            .into_connection();
        let service = service_behind(
            db,
            Gate {
                reads_saved_lists: true,
                refused: vec![ip("203.0.113.10")],
            },
        );
        let block = service
            .monitor_block(5, 9, &monitor())
            .await
            .expect("policy is readable")
            .expect("blocked");
        assert_eq!(block.blocked_addresses, vec!["203.0.113.10"]);
        assert_eq!(block.suggested_cidrs, vec!["203.0.113.10/32"]);
    }
}
