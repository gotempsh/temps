// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the host layer must supply so [`crate::heartbeat`] can build a
//! [`StatusReport`](temps_cloud_protocol::StatusReport) (ADR-039).
//!
//! The host may supply additional status via the provider trait. The default
//! worker provider uses the existing local database dependency only from the
//! background reporting task; it never participates in local request handling.
//!
//! `temps_version` and `uptime_seconds` are deliberately **not** on this
//! trait: this crate already knows its own agent version
//! ([`crate::link::CloudLink::agent_version`], the same string every
//! connection's `Hello` already carries) and can time its own uptime from
//! when its reporting task started, so asking the host layer to supply either
//! would just be plumbing the same value through an extra hop.

use async_trait::async_trait;

use temps_cloud_protocol::{StatusResourceSummary, StatusSelfUpdate};

/// One point-in-time reading of everything a [`StatusReport`](temps_cloud_protocol::StatusReport)
/// needs beyond what `temps-cloud-client` already knows about itself.
///
/// Independent of the wire type on purpose: the wire type also carries
/// `instance_id`, `temps_version`, and `uptime_seconds`, none of which the
/// host layer should have to reconstruct or duplicate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub deployment_count: u32,
    pub service_count: u32,
    pub project_count: u32,
    pub worker_node_count: Option<u32>,
    /// `None` when the instance cannot compute a resource summary at all.
    pub resources: Option<StatusResourceSummary>,
    /// `None` on a host with no `SelfUpdater` registered at all (e.g. the
    /// standalone proxy process). See the wire type's own doc comment for why
    /// that is a distinct state from `enabled: false`.
    pub self_update: Option<StatusSelfUpdate>,
}

/// Supplies the parts of an instance's status report that only the
/// `temps-cli`/`temps-core` layer can compute.
///
/// Registered with [`crate::heartbeat::run`] at instance startup. Absent
/// (`None` passed to `run`) is a normal, silent state — see that function's
/// doc comment — not an error: it simply means this connection negotiates
/// `Capability::InstanceStatusReporting` but never has anything to send.
#[async_trait]
pub trait StatusProvider: Send + Sync {
    /// Take a snapshot right now.
    ///
    /// Called on every status-report tick and on every `StatusRequest` nudge,
    /// so implementations must apply their own bounded timeouts and fallback
    /// to partial data (e.g. `None` resources) rather than let a slow query
    /// stall this task's connection loop — the same "local is primary, this
    /// channel must never block anything" rule this crate applies everywhere
    /// else applies here too.
    async fn snapshot(&self) -> StatusSnapshot;
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_cloud_protocol::{
        StatusSelfUpdate, StatusSelfUpdatePhase, StatusSelfUpdateRestartMode, StatusSupervisorKind,
    };

    struct FixedProvider(StatusSnapshot);

    #[async_trait]
    impl StatusProvider for FixedProvider {
        async fn snapshot(&self) -> StatusSnapshot {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn a_provider_reports_its_fixed_snapshot() {
        let snapshot = StatusSnapshot {
            deployment_count: 3,
            service_count: 5,
            worker_node_count: None,
            project_count: 1,
            resources: None,
            self_update: Some(StatusSelfUpdate {
                enabled: true,
                supervisor: StatusSupervisorKind::Systemd,
                restart_mode: StatusSelfUpdateRestartMode::Automatic,
                blocker: None,
                blocker_reason: None,
                phase: StatusSelfUpdatePhase::Idle,
                available_update: None,
                last_attempt: None,
            }),
        };
        let provider = FixedProvider(snapshot.clone());
        assert_eq!(provider.snapshot().await, snapshot);
    }

    #[tokio::test]
    async fn the_default_snapshot_reports_nothing_computed() {
        let snapshot = StatusSnapshot::default();
        assert_eq!(snapshot.deployment_count, 0);
        assert!(snapshot.resources.is_none());
        assert!(snapshot.self_update.is_none());
    }
}

/// Reads only a bounded aggregate in the optional background reporter.
pub struct WorkerStatusProvider(pub std::sync::Arc<sea_orm::DatabaseConnection>);

fn worker_count_query() -> sea_orm::Select<temps_entities::nodes::Entity> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_entities::nodes;
    nodes::Entity::find()
        .filter(nodes::Column::Role.eq("worker"))
        .filter(nodes::Column::Status.is_in(["active", "online"]))
}

#[async_trait]
impl StatusProvider for WorkerStatusProvider {
    async fn snapshot(&self) -> StatusSnapshot {
        use sea_orm::PaginatorTrait;
        let worker_node_count = match tokio::time::timeout(
            std::time::Duration::from_millis(250),
            worker_count_query().count(self.0.as_ref()),
        )
        .await
        {
            Ok(Ok(count)) => u32::try_from(count).ok(),
            _ => None,
        };
        StatusSnapshot {
            worker_node_count,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod worker_count_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, QueryTrait};
    #[tokio::test]
    async fn count_is_reported_and_database_failure_is_unknown() {
        use sea_orm::{DbErr, MockDatabase};
        use std::{collections::BTreeMap, sync::Arc};
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[BTreeMap::from([(
                "num_items",
                sea_orm::Value::BigInt(Some(2)),
            )])]])
            .into_connection();
        assert_eq!(
            WorkerStatusProvider(Arc::new(db))
                .snapshot()
                .await
                .worker_node_count,
            Some(2)
        );
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("count unavailable".into())])
            .into_connection();
        assert_eq!(
            WorkerStatusProvider(Arc::new(db))
                .snapshot()
                .await
                .worker_node_count,
            None
        );
    }
    #[test]
    fn count_selects_only_active_workers() {
        let sql = worker_count_query()
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains("\"role\" = 'worker'"));
        assert!(sql.contains("\"status\" IN ('active', 'online')"));
    }
}
