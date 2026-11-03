// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The shadow comparison (ADR-055 Phase 4): for each database, read its
//! running container, compute the spec it would be created from, and list
//! where the two differ. It never changes anything.
//!
//! Where a container is read from is a [`SnapshotSource`]: the control
//! plane's Docker for a database without a node, the node's agent
//! otherwise. `ExternalServiceManager` provides the real one; the pass
//! itself only needs rows and a source.

use async_trait::async_trait;
use temps_entities::external_services;
use temps_workload_docker::{shape_diff, ShapeDifference};
use tracing::{info, warn};

use crate::workload::runtime::ContainerSnapshot;
use crate::workload::spec::{
    expected_shape, observed_shape, DatabaseEngine, DatabaseIdentity, DatabaseProcess,
    DatabaseWorkloadPlan,
};
use crate::workload::{
    ServiceWorkloadError, ShadowPassReport, ShadowResult, ShadowSkip, WorkloadSupport,
};

/// Reads a database's container wherever it runs.
#[async_trait]
pub trait SnapshotSource: Send + Sync {
    async fn snapshot(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<ContainerSnapshot, ServiceWorkloadError>;
}

/// The database a row describes, or why it stays on its engine's own path.
pub fn database_identity(
    service: &external_services::Model,
) -> Result<DatabaseIdentity, WorkloadSupport> {
    let engine = DatabaseEngine::from_service_type(&service.service_type).ok_or_else(|| {
        WorkloadSupport::no(format!(
            "service type '{}' is not on the workload plane yet; only postgres, redis, \
             mongodb and mariadb databases are",
            service.service_type
        ))
    })?;
    if service.topology != "standalone" {
        return Err(WorkloadSupport::no(format!(
            "topology '{}': clustered databases keep their own create path",
            service.topology
        )));
    }
    if let Some(container) = &service.container_name {
        return Err(WorkloadSupport::no(format!(
            "the service runs the imported container '{container}', which Temps never \
             recreates"
        )));
    }
    Ok(DatabaseIdentity {
        service_id: service.id,
        service_name: service.name.clone(),
        engine,
        node_id: service.node_id,
        container_name: engine.container_name(&service.name),
    })
}

/// The plan of an existing container: its own process, the spec's policy.
/// `None`: there is no container.
pub fn plan_of_snapshot(
    identity: &DatabaseIdentity,
    snapshot: &ContainerSnapshot,
) -> Result<Option<(DatabaseWorkloadPlan, DatabaseProcess)>, ServiceWorkloadError> {
    let Some(inspect) = snapshot.inspect.as_ref().filter(|_| snapshot.found) else {
        return Ok(None);
    };
    let process = DatabaseProcess::from_inspect(
        identity.engine,
        inspect,
        &snapshot.image_env,
        snapshot.image_cmd.as_deref(),
    )
    .map_err(|reason| ServiceWorkloadError::Process {
        service_id: identity.service_id,
        container: identity.container_name.clone(),
        reason,
    })?;
    let plan = DatabaseWorkloadPlan::admit(identity, &process)?;
    Ok(Some((plan, process)))
}

/// Where a container differs from the spec it would be created from.
pub fn differences(
    identity: &DatabaseIdentity,
    snapshot: &ContainerSnapshot,
) -> Result<Option<Vec<ShapeDifference>>, ServiceWorkloadError> {
    let Some((plan, _)) = plan_of_snapshot(identity, snapshot)? else {
        return Ok(None);
    };
    let Some(inspect) = snapshot.inspect.as_ref() else {
        return Ok(None);
    };
    let expected = expected_shape(&plan)?;
    let actual = observed_shape(&plan, inspect);
    Ok(Some(shape_diff(&expected, &actual)))
}

/// Compare every row's container with its spec, one at a time, and log
/// each difference at `warn` (a match at `info`). `truncated` says more
/// rows exist than were passed.
pub async fn shadow_pass(
    rows: &[external_services::Model],
    truncated: bool,
    source: &dyn SnapshotSource,
) -> ShadowPassReport {
    let mut report = ShadowPassReport {
        truncated,
        ..Default::default()
    };
    for row in rows {
        let skip = |reason: String| ShadowSkip {
            service_id: row.id,
            service_name: row.name.clone(),
            reason,
        };
        let identity = match database_identity(row) {
            Ok(identity) => identity,
            Err(support) => {
                report
                    .skipped
                    .push(skip(support.reason.unwrap_or_default()));
                continue;
            }
        };
        let compared = match source.snapshot(&identity).await {
            Ok(snapshot) => differences(&identity, &snapshot),
            Err(error) => Err(error),
        };
        match compared {
            Ok(Some(differences)) => {
                if differences.is_empty() {
                    info!(
                        service_id = row.id,
                        container = %identity.container_name,
                        node_id = ?row.node_id,
                        workload_mode = "shadow",
                        "Database container matches its workload spec"
                    );
                }
                for difference in &differences {
                    warn!(
                        service_id = row.id,
                        container = %identity.container_name,
                        node_id = ?row.node_id,
                        workload_mode = "shadow",
                        field = %difference.field,
                        expected = %difference.expected,
                        actual = %difference.actual,
                        "Database container differs from its workload spec; a recreate in \
                         enforce mode would change it"
                    );
                }
                report.compared.push(ShadowResult {
                    service_id: row.id,
                    service_name: row.name.clone(),
                    node_id: row.node_id,
                    container_name: identity.container_name.clone(),
                    differences,
                });
            }
            Ok(None) => report.skipped.push(skip(format!(
                "container '{}' does not exist",
                identity.container_name
            ))),
            Err(error) => {
                warn!(
                    service_id = row.id,
                    container = %identity.container_name,
                    workload_mode = "shadow",
                    error = %error,
                    "Could not compare a database container with its workload spec"
                );
                report.skipped.push(skip(error.to_string()));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Missing(Mutex<Vec<String>>);

    #[async_trait]
    impl SnapshotSource for Missing {
        async fn snapshot(
            &self,
            identity: &DatabaseIdentity,
        ) -> Result<ContainerSnapshot, ServiceWorkloadError> {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(identity.container_name.clone());
            if identity.service_id == 3 {
                return Err(ServiceWorkloadError::AgentUnsupported {
                    node: "worker-1".into(),
                    operation: "read a database container snapshot",
                });
            }
            Ok(ContainerSnapshot::default())
        }
    }

    fn row(id: i32, service_type: &str, node_id: Option<i32>) -> external_services::Model {
        let mut model = crate::externalsvc::test_utils::create_mock_external_service(
            format!("db{id}"),
            service_type,
            "1",
        );
        model.id = id;
        model.node_id = node_id;
        model
    }

    #[tokio::test]
    async fn unsupported_missing_and_unreadable_databases_are_skipped_with_a_reason() {
        let mut clustered = row(4, "postgres", None);
        clustered.topology = "ha".into();
        let rows = vec![
            row(1, "kv", None),
            row(2, "redis", None),
            row(3, "redis", Some(9)),
            clustered,
        ];
        let source = Missing(Mutex::new(Vec::new()));
        let report = shadow_pass(&rows, true, &source).await;
        assert!(report.compared.is_empty());
        assert!(report.truncated);
        let reasons: HashMap<i32, String> = report
            .skipped
            .iter()
            .map(|s| (s.service_id, s.reason.clone()))
            .collect();
        assert!(reasons[&1].contains("'kv'"), "{reasons:?}");
        assert!(reasons[&2].contains("does not exist"), "{reasons:?}");
        assert!(reasons[&3].contains("upgrade it"), "{reasons:?}");
        assert!(reasons[&4].contains("clustered"), "{reasons:?}");
        // Unsupported rows are never read.
        assert_eq!(
            *source.0.lock().unwrap(),
            vec!["redis-db2".to_string(), "redis-db3".to_string()]
        );
    }
}
