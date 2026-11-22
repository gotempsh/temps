// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applications running without the cluster resolver (ADR-024).
//!
//! A container's nameservers are fixed when it is created, so turning cluster
//! DNS on reaches new containers only: the ones already running cannot
//! resolve `*.temps.local` until they are redeployed. This service names
//! them, so the console can list them with a redeploy action instead of
//! leaving the operator to discover it through a failed connection.
//!
//! Workers report their own list on every heartbeat (stored in
//! `nodes.deployments_without_resolver`); the control plane, which has no
//! `nodes` row, checks its local daemon here when asked. Only deployments
//! that are still their environment's current one are listed: a superseded
//! deployment's containers are about to go away and redeploying it is not
//! the fix.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use sea_orm::{DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use utoipa::ToSchema;

use temps_deployer::resolver_coverage::{ResolverCoverage, MAX_REPORTED_DEPLOYMENTS};

/// Where a stale deployment runs: the control plane or a worker.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
pub struct ResolverCoverageHost {
    /// `None` for the control plane, which has no node row.
    pub node_id: Option<i32>,
    pub node_name: String,
}

/// An application whose running containers were created without the
/// cluster resolver. Redeploying it gives the new containers the resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeploymentWithoutResolver {
    pub deployment_id: i32,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_name: String,
    /// Hosts running one of its containers without the resolver.
    pub hosts: Vec<ResolverCoverageHost>,
}

#[derive(Debug, Error)]
pub enum ResolverCoverageError {
    #[error("Failed to list deployment containers on the control plane's Docker daemon to check their nameservers: {reason}")]
    Docker { reason: String },

    #[error("Failed to look up the projects of {count} deployment(s) running without the cluster resolver: {source}")]
    Database {
        count: usize,
        #[source]
        source: sea_orm::DbErr,
    },
}

#[derive(Debug, FromQueryResult)]
struct CurrentDeploymentRow {
    deployment_id: i32,
    project_id: i32,
    project_name: String,
    project_slug: String,
    environment_id: i32,
    environment_name: String,
}

/// Deployments that are still their environment's current one, with their
/// project and environment, in one query.
const CURRENT_DEPLOYMENTS_SQL: &str = r#"
SELECT d.id AS deployment_id,
       p.id AS project_id,
       p.name AS project_name,
       p.slug AS project_slug,
       e.id AS environment_id,
       e.name AS environment_name
FROM deployments d
JOIN environments e ON e.current_deployment_id = d.id AND e.deleted_at IS NULL
JOIN projects p ON p.id = e.project_id AND p.deleted_at IS NULL
WHERE d.id = ANY($1)
ORDER BY p.name, e.name
"#;

pub struct ResolverCoverageService {
    db: Arc<DatabaseConnection>,
    docker: Arc<temps_core::DockerHandle>,
    /// The control plane's resolver address while it answers; `None` when
    /// this process runs no workloads (no slot is registered).
    resolver: Option<Arc<RwLock<Option<IpAddr>>>>,
    /// Nameservers already seen per container, so a status request inspects
    /// only containers created since the last one.
    coverage: Mutex<ResolverCoverage>,
}

impl ResolverCoverageService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: Arc<temps_core::DockerHandle>,
        resolver: Option<Arc<RwLock<Option<IpAddr>>>>,
    ) -> Self {
        Self {
            db,
            docker,
            resolver,
            coverage: Mutex::new(ResolverCoverage::new()),
        }
    }

    /// Current deployments running without the resolver on the control
    /// plane or on any of `nodes`, using each worker's last report.
    pub async fn deployments_without_resolver(
        &self,
        nodes: &[temps_entities::nodes::Model],
    ) -> Result<Vec<DeploymentWithoutResolver>, ResolverCoverageError> {
        let mut hosts: BTreeMap<i64, Vec<ResolverCoverageHost>> = BTreeMap::new();
        for id in self.control_plane_deployments().await? {
            hosts.entry(id).or_default().push(ResolverCoverageHost {
                node_id: None,
                node_name: "control plane".to_string(),
            });
        }
        for node in nodes.iter().filter(|node| node.status != "removed") {
            for id in node.deployments_without_resolver() {
                hosts.entry(id).or_default().push(ResolverCoverageHost {
                    node_id: Some(node.id),
                    node_name: node.name.clone(),
                });
            }
        }
        if hosts.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i32> = hosts
            .keys()
            .filter_map(|id| i32::try_from(*id).ok())
            .take(MAX_REPORTED_DEPLOYMENTS)
            .collect();
        let count = ids.len();
        let rows = CurrentDeploymentRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            CURRENT_DEPLOYMENTS_SQL,
            [ids.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| ResolverCoverageError::Database { count, source })?;

        Ok(join_hosts(rows, &mut hosts))
    }

    /// Deployment ids on the control plane's own daemon without its
    /// resolver. Empty while the resolver is not answering: containers
    /// created now would not get it either, so there is nothing a redeploy
    /// would fix yet.
    async fn control_plane_deployments(&self) -> Result<Vec<i64>, ResolverCoverageError> {
        let Some(resolver) = self
            .resolver
            .as_ref()
            .and_then(|slot| slot.read().ok().and_then(|address| *address))
        else {
            return Ok(Vec::new());
        };
        let Some(docker) = self.docker.cloned() else {
            return Ok(Vec::new());
        };
        self.coverage
            .lock()
            .await
            .deployments_without(&docker, resolver)
            .await
            .map_err(|error| ResolverCoverageError::Docker {
                reason: error.to_string(),
            })
    }
}

fn join_hosts(
    rows: Vec<CurrentDeploymentRow>,
    hosts: &mut BTreeMap<i64, Vec<ResolverCoverageHost>>,
) -> Vec<DeploymentWithoutResolver> {
    rows.into_iter()
        .map(|row| {
            let mut on = hosts
                .remove(&i64::from(row.deployment_id))
                .unwrap_or_default();
            on.sort();
            on.dedup();
            DeploymentWithoutResolver {
                deployment_id: row.deployment_id,
                project_id: row.project_id,
                project_name: row.project_name,
                project_slug: row.project_slug,
                environment_id: row.environment_id,
                environment_name: row.environment_name,
                hosts: on,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn row(deployment_id: i32, project: &str) -> CurrentDeploymentRow {
        CurrentDeploymentRow {
            deployment_id,
            project_id: deployment_id * 10,
            project_name: project.to_string(),
            project_slug: project.to_string(),
            environment_id: deployment_id * 100,
            environment_name: "production".to_string(),
        }
    }

    fn host(node_id: Option<i32>, name: &str) -> ResolverCoverageHost {
        ResolverCoverageHost {
            node_id,
            node_name: name.to_string(),
        }
    }

    #[test]
    fn each_application_lists_every_host_once() {
        let mut hosts = BTreeMap::from([
            (
                7,
                vec![
                    host(Some(2), "worker-b"),
                    host(None, "control plane"),
                    host(Some(2), "worker-b"),
                ],
            ),
            // Superseded: no current-deployment row comes back for it.
            (8, vec![host(Some(1), "worker-a")]),
        ]);

        let listed = join_hosts(vec![row(7, "shop")], &mut hosts);

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].project_slug, "shop");
        assert_eq!(
            listed[0].hosts,
            vec![host(None, "control plane"), host(Some(2), "worker-b")]
        );
    }

    fn node(
        id: i32,
        status: &str,
        reported: Option<serde_json::Value>,
    ) -> temps_entities::nodes::Model {
        let mut node = crate::handlers::wireguard_mesh::admin_test_support::node(
            id,
            &format!("worker-{id}"),
            "token",
        );
        node.status = status.to_string();
        node.deployments_without_resolver = reported;
        node
    }

    #[tokio::test]
    async fn nothing_reported_needs_no_query() {
        // No query results are queued: a query would fail the test.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ResolverCoverageService::new(
            Arc::new(db),
            Arc::new(temps_core::DockerHandle::disabled(
                "test",
                "no daemon in tests",
            )),
            None,
        );
        let nodes = vec![
            node(1, "active", None),
            node(2, "active", Some(serde_json::json!([]))),
            // A removed node's last report no longer describes anything.
            node(3, "removed", Some(serde_json::json!([4]))),
        ];

        let listed = service
            .deployments_without_resolver(&nodes)
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn a_failed_lookup_names_how_many_deployments_it_was_for() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let service = ResolverCoverageService::new(
            Arc::new(db),
            Arc::new(temps_core::DockerHandle::disabled(
                "test",
                "no daemon in tests",
            )),
            None,
        );
        let nodes = vec![node(1, "active", Some(serde_json::json!([4, 5])))];

        let error = service
            .deployments_without_resolver(&nodes)
            .await
            .expect_err("the lookup fails");

        assert!(matches!(
            error,
            ResolverCoverageError::Database { count: 2, .. }
        ));
    }
}
