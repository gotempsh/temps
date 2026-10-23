// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ExternalServiceManager`'s side of ADR-055 Phase 4: the per-database
//! workload mode, the pending changes a recreate would apply, the recreate
//! itself, and the shadow pass. See [`crate::workload`].

use std::sync::LazyLock;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
use temps_entities::external_services;
use tokio::sync::Notify;
use tracing::{info, warn};

use super::ExternalServiceManager;
use crate::remote_service_client::ServiceWorkloadCreateRequest;
use crate::workload::registry::{self, EnforcedDatabase};
use crate::workload::runtime::{self, ContainerSnapshot};
use crate::workload::shadow::{self, SnapshotSource};
use crate::workload::{
    changes_fingerprint, DatabaseIdentity, DatabaseProcess, DatabaseWorkloadPlan, NodeAttachment,
    PendingChanges, ServiceWorkloadError, ShadowPassReport, WorkloadChange, WorkloadSupport,
};

/// The manager's containers: local Docker, or the node's agent.
struct ManagerSnapshots<'a>(&'a ExternalServiceManager);

#[async_trait::async_trait]
impl SnapshotSource for ManagerSnapshots<'_> {
    async fn snapshot(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<ContainerSnapshot, ServiceWorkloadError> {
        self.0.database_snapshot(identity).await
    }
}

/// Databases one shadow pass compares at most; the rest wait for the next.
pub const SHADOW_BATCH: u64 = 100;
/// How often the shadow pass runs when nothing wakes it earlier.
pub const SHADOW_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Bound on reading one database's container (local Docker or an agent).
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Wakes the shadow loop when a database is moved into `shadow`.
static SHADOW_WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// A database's mode and whether it can leave `legacy`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct WorkloadModeStatus {
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    pub mode: ServiceWorkloadMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    pub support: WorkloadSupport,
}

/// The result of a mode change.
#[derive(Debug, Clone)]
pub struct WorkloadModeChange {
    pub previous: ServiceWorkloadMode,
    pub status: WorkloadModeStatus,
    /// What a recreate would change, when the new mode is not `legacy`.
    pub pending: Option<PendingChanges>,
}

/// The result of asking for a recreate.
#[derive(Debug, Clone)]
pub enum RecreateOutcome {
    /// Nothing was changed: these are the changes to confirm, by passing
    /// their fingerprint back.
    NeedsConfirmation(PendingChanges),
    /// The container was replaced. `applied` is what was confirmed;
    /// `remaining` is what differs afterwards (empty when it worked).
    Recreated {
        applied: PendingChanges,
        remaining: PendingChanges,
    },
}

impl ExternalServiceManager {
    /// Why `service` stays on its engine's own path, or its identity.
    fn database_identity(
        service: &external_services::Model,
    ) -> Result<DatabaseIdentity, WorkloadSupport> {
        shadow::database_identity(service)
    }

    fn workload_status(service: &external_services::Model) -> WorkloadModeStatus {
        WorkloadModeStatus {
            service_id: service.id,
            service_name: service.name.clone(),
            service_type: service.service_type.clone(),
            mode: service.workload_mode,
            node_id: service.node_id,
            support: match Self::database_identity(service) {
                Ok(_) => WorkloadSupport::yes(),
                Err(support) => support,
            },
        }
    }

    async fn workload_service(
        &self,
        service_id: i32,
    ) -> Result<external_services::Model, ServiceWorkloadError> {
        external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ServiceWorkloadError::NotFound { service_id })
    }

    /// Load which control-plane databases are in `enforce` mode, so their
    /// engines create from the spec. Run once at startup.
    pub async fn load_workload_registry(&self) -> Result<usize, ServiceWorkloadError> {
        let rows = external_services::Entity::find()
            .filter(external_services::Column::WorkloadMode.eq(ServiceWorkloadMode::Enforce))
            .filter(external_services::Column::NodeId.is_null())
            .all(self.db.as_ref())
            .await?;
        let entries: Vec<(String, EnforcedDatabase)> = rows
            .iter()
            .filter_map(|row| Self::database_identity(row).ok())
            .map(|identity| {
                (
                    identity.container_name.clone(),
                    EnforcedDatabase {
                        service_id: identity.service_id,
                        service_name: identity.service_name,
                        engine: identity.engine,
                    },
                )
            })
            .collect();
        let count = entries.len();
        registry::replace_all(entries);
        info!(
            enforced = count,
            "Loaded control-plane databases created from their workload spec"
        );
        Ok(count)
    }

    pub async fn workload_mode_status(
        &self,
        service_id: i32,
    ) -> Result<WorkloadModeStatus, ServiceWorkloadError> {
        let service = self.workload_service(service_id).await?;
        Ok(Self::workload_status(&service))
    }

    /// Move a database to `mode`. Nothing running changes: `enforce` takes
    /// effect the next time the container is created or recreated.
    pub async fn set_workload_mode(
        &self,
        service_id: i32,
        mode: ServiceWorkloadMode,
    ) -> Result<WorkloadModeChange, ServiceWorkloadError> {
        let service = self.workload_service(service_id).await?;
        let previous = service.workload_mode;
        let identity = match Self::database_identity(&service) {
            Ok(identity) => Some(identity),
            Err(_) if mode == ServiceWorkloadMode::Legacy => None,
            Err(support) => {
                return Err(ServiceWorkloadError::Unsupported {
                    service_id,
                    name: service.name.clone(),
                    service_type: service.service_type.clone(),
                    mode: mode.to_string(),
                    reason: support.reason.unwrap_or_default(),
                })
            }
        };

        let mut update: external_services::ActiveModel = service.clone().into();
        update.workload_mode = Set(mode);
        update.updated_at = Set(Utc::now());
        let updated = update.update(self.db.as_ref()).await?;

        if updated.node_id.is_none() {
            match (&identity, mode) {
                (Some(identity), ServiceWorkloadMode::Enforce) => registry::set_enforced(
                    &identity.container_name,
                    EnforcedDatabase {
                        service_id,
                        service_name: identity.service_name.clone(),
                        engine: identity.engine,
                    },
                ),
                _ => registry::clear_service(service_id),
            }
        }
        if mode == ServiceWorkloadMode::Shadow {
            SHADOW_WAKE.notify_one();
        }
        info!(
            service_id,
            service = %updated.name,
            previous_mode = %previous,
            workload_mode = %mode,
            "Changed a database's workload mode"
        );

        let pending = if mode == ServiceWorkloadMode::Legacy {
            None
        } else {
            match self.pending_workload_changes(service_id).await {
                Ok(pending) => Some(pending),
                Err(error) => {
                    warn!(
                        service_id,
                        error = %error,
                        "Workload mode changed, but its pending changes could not be computed"
                    );
                    None
                }
            }
        };
        Ok(WorkloadModeChange {
            previous,
            status: Self::workload_status(&updated),
            pending,
        })
    }

    /// The database's container, wherever it runs.
    async fn database_snapshot(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<ContainerSnapshot, ServiceWorkloadError> {
        let read = async {
            match identity.node_id {
                None => {
                    let docker = self
                        .require_docker()
                        .map_err(ServiceWorkloadError::service(identity.service_id))?;
                    runtime::snapshot(&docker, &identity.container_name).await
                }
                Some(node_id) => {
                    let client = self
                        .get_remote_client(node_id)
                        .await
                        .map_err(ServiceWorkloadError::service(identity.service_id))?;
                    client.service_snapshot(&identity.container_name).await
                }
            }
        };
        tokio::time::timeout(SNAPSHOT_TIMEOUT, read)
            .await
            .map_err(|_| ServiceWorkloadError::Docker {
                operation: "inspect",
                container: identity.container_name.clone(),
                reason: format!("no answer within {}s", SNAPSHOT_TIMEOUT.as_secs()),
            })?
    }

    /// Where the running container differs from its spec. `None`: there is
    /// no container.
    async fn compare_database(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<Option<Vec<temps_workload_docker::ShapeDifference>>, ServiceWorkloadError> {
        let snapshot = self.database_snapshot(identity).await?;
        shadow::differences(identity, &snapshot)
    }

    /// What recreating the database's container from its spec would change.
    pub async fn pending_workload_changes(
        &self,
        service_id: i32,
    ) -> Result<PendingChanges, ServiceWorkloadError> {
        let service = self.workload_service(service_id).await?;
        let (identity, support) = match Self::database_identity(&service) {
            Ok(identity) => (Some(identity), WorkloadSupport::yes()),
            Err(support) => (None, support),
        };
        let (container_found, changes) = match &identity {
            Some(identity) => match self.compare_database(identity).await? {
                Some(differences) => (true, differences.iter().map(WorkloadChange::from).collect()),
                None => (false, Vec::new()),
            },
            None => (false, Vec::new()),
        };
        Ok(PendingChanges {
            service_id,
            service_name: service.name.clone(),
            mode: service.workload_mode,
            node_id: service.node_id,
            container_name: identity.map(|i| i.container_name),
            support,
            container_found,
            fingerprint: changes_fingerprint(&changes),
            changes,
            computed_at: Utc::now(),
        })
    }

    /// Recreate an `enforce`-mode database's container from its spec.
    ///
    /// Without `confirmed_fingerprint`, or with one that no longer matches,
    /// nothing changes and the pending changes are returned to confirm.
    /// Data is protected by the recreate guard: the replacement mounts the
    /// same volume, the original is kept until the replacement shows the
    /// same data, and is put back otherwise.
    pub async fn recreate_from_workload_spec(
        &self,
        service_id: i32,
        confirmed_fingerprint: Option<&str>,
    ) -> Result<RecreateOutcome, ServiceWorkloadError> {
        let service = self.workload_service(service_id).await?;
        if service.workload_mode != ServiceWorkloadMode::Enforce {
            return Err(ServiceWorkloadError::NotEnforced {
                service_id,
                name: service.name.clone(),
                mode: service.workload_mode.to_string(),
            });
        }
        let identity = Self::database_identity(&service).map_err(|support| {
            ServiceWorkloadError::Unsupported {
                service_id,
                name: service.name.clone(),
                service_type: service.service_type.clone(),
                mode: ServiceWorkloadMode::Enforce.to_string(),
                reason: support.reason.unwrap_or_default(),
            }
        })?;
        let pending = self.pending_workload_changes(service_id).await?;
        match confirmed_fingerprint {
            None => return Ok(RecreateOutcome::NeedsConfirmation(pending)),
            Some(confirmed) if confirmed != pending.fingerprint => {
                return Err(ServiceWorkloadError::StaleConfirmation {
                    service_id,
                    confirmed: confirmed.to_string(),
                    current: pending.fingerprint,
                })
            }
            Some(_) => {}
        }

        let snapshot = self.database_snapshot(&identity).await?;
        let (Some((plan, process)), Some(inspect)) = (
            shadow::plan_of_snapshot(&identity, &snapshot)?,
            snapshot.inspect.as_ref(),
        ) else {
            return Err(ServiceWorkloadError::ContainerMissing {
                service_id,
                container: identity.container_name.clone(),
                node: identity
                    .node_id
                    .map(|n| format!("node {n}"))
                    .unwrap_or_else(|| "the control plane".to_string()),
            });
        };
        let secrets = process.secret_values();
        match identity.node_id {
            None => {
                let docker = self
                    .require_docker()
                    .map_err(ServiceWorkloadError::service(service_id))?;
                let host_ip = inspect
                    .host_config
                    .as_ref()
                    .and_then(|h| h.port_bindings.as_ref())
                    .and_then(|b| b.values().flatten().flatten().next())
                    .and_then(|b| b.host_ip.clone())
                    .unwrap_or_else(|| "127.0.0.1".to_string());
                let node = NodeAttachment {
                    host_ip,
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                    dns: None,
                };
                runtime::recreate_from_plan(
                    &docker,
                    &plan,
                    &identity.service_name,
                    inspect,
                    &secrets,
                    &node,
                    |_| async {},
                )
                .await?;
            }
            Some(node_id) => {
                let client = self
                    .get_remote_client(node_id)
                    .await
                    .map_err(ServiceWorkloadError::service(service_id))?;
                client
                    .create_service_workload(&ServiceWorkloadCreateRequest {
                        service_name: identity.service_name.clone(),
                        plan: plan.clone(),
                        secrets,
                        network: Some(temps_core::NETWORK_NAME.to_string()),
                        replace_existing: true,
                    })
                    .await?;
            }
        }
        // A recreated container gets a new address; republish its record.
        if let Err(error) = self.register_standalone_service_dns(service_id).await {
            warn!(
                service_id,
                error = %error,
                "Recreated a database from its workload spec, but its internal DNS record could \
                 not be republished"
            );
        }
        let remaining = self.pending_workload_changes(service_id).await?;
        Ok(RecreateOutcome::Recreated {
            applied: pending,
            remaining,
        })
    }

    /// Compare every `shadow`-mode database's container with its spec and
    /// log the differences. Changes nothing. Bounded: at most
    /// [`SHADOW_BATCH`] databases, one at a time, each with a timeout.
    pub async fn run_workload_shadow_pass(&self) -> Result<ShadowPassReport, ServiceWorkloadError> {
        let rows = external_services::Entity::find()
            .filter(external_services::Column::WorkloadMode.eq(ServiceWorkloadMode::Shadow))
            .order_by_asc(external_services::Column::Id)
            .limit(SHADOW_BATCH + 1)
            .all(self.db.as_ref())
            .await?;
        let truncated = rows.len() as u64 > SHADOW_BATCH;
        let batch = &rows[..rows.len().min(SHADOW_BATCH as usize)];
        let report = shadow::shadow_pass(batch, truncated, &ManagerSnapshots(self)).await;
        if report.truncated {
            warn!(
                batch = SHADOW_BATCH,
                "More databases are in shadow mode than one pass compares; the rest are compared \
                 in a later pass"
            );
        }
        Ok(report)
    }

    /// Run the shadow pass now, every [`SHADOW_INTERVAL`], and whenever a
    /// database is moved into `shadow`.
    pub fn spawn_workload_shadow_loop(self: &std::sync::Arc<Self>) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                match manager.run_workload_shadow_pass().await {
                    Ok(report) if report.compared.is_empty() && report.skipped.is_empty() => {}
                    Ok(report) => info!(
                        compared = report.compared.len(),
                        differing = report
                            .compared
                            .iter()
                            .filter(|r| !r.differences.is_empty())
                            .count(),
                        skipped = report.skipped.len(),
                        "Workload shadow pass finished"
                    ),
                    Err(error) => warn!(error = %error, "Workload shadow pass failed"),
                }
                tokio::select! {
                    _ = tokio::time::sleep(SHADOW_INTERVAL) => {}
                    _ = SHADOW_WAKE.notified() => {}
                }
            }
        })
    }

    /// Create a worker database from its spec (enforce mode), instead of
    /// the agent's own request.
    pub(super) async fn create_remote_from_workload_spec(
        &self,
        service: &external_services::Model,
        client: &crate::remote_service_client::RemoteServiceClient,
        create_params: &crate::remote_service_client::RemoteServiceCreateParams,
    ) -> Result<crate::remote_service_client::RemoteServiceCreateResponse, ServiceWorkloadError>
    {
        let identity = Self::database_identity(service).map_err(|support| {
            ServiceWorkloadError::Unsupported {
                service_id: service.id,
                name: service.name.clone(),
                service_type: service.service_type.clone(),
                mode: ServiceWorkloadMode::Enforce.to_string(),
                reason: support.reason.unwrap_or_default(),
            }
        })?;
        let identity = DatabaseIdentity {
            // The agent request's name is the one the container gets.
            container_name: create_params.name.clone(),
            ..identity
        };
        let process = DatabaseProcess::from_remote_params(identity.engine, create_params);
        let plan = DatabaseWorkloadPlan::admit(&identity, &process)?;
        client
            .create_service_workload(&ServiceWorkloadCreateRequest {
                service_name: identity.service_name.clone(),
                plan,
                secrets: process.secret_values(),
                network: create_params.network.clone(),
                replace_existing: false,
            })
            .await
    }
}
