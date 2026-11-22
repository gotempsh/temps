// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `PostgresClusterEngine`: WAL-G–based backup of a Postgres cluster
//! (pg_auto_failover topology), implemented against `engine_v2::BackupEngine`.
//!
//! Same as [`super::postgres_walg::PostgresWalgEngine`] but first asks the
//! cluster's pg_auto_failover monitor which member is the current primary
//! and runs `wal-g backup-push` against THAT container's PGDATA. Replicas are
//! never targeted directly.
//!
//! The cluster's members are placed on any node, and only the Docker daemon
//! of the node a member runs on can reach its container. The node half
//! (`temps_providers::externalsvc::postgres_cluster_walg_backup`) runs here
//! with this host's Docker for a primary on this host, and on the primary's
//! worker, by its agent, for one on a worker. Everything else, and so every
//! artifact, is the same for both.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use temps_core::ScopedS3Access;
use temps_providers::externalsvc::postgres_cluster_walg_backup::{
    cluster_walg_base_backup, ClusterWalgBaseBackup,
};
use temps_providers::externalsvc::postgres_walg_backup::{
    self as node_half, PostgresLogin, WalgBaseBackupOutcome,
};
use temps_providers::services::ExternalServiceError;
use temps_providers::worker_s3_scope::{WorkerCredentialScoper, WorkerS3Grant};
use temps_providers::ServiceMemberInfo;
use tracing::{info, warn};

use super::postgres_walg::{
    await_worker_backup, backup_error, node_backup_runs_on, worker_outcome, BACKUP_OUTCOME_WAIT,
};
use super::v2_common;
use super::walg_size::load_backup_size_bytes;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "postgres_cluster";

/// The port PostgreSQL listens on inside a member container. Each member
/// listens on its own stored port (pg_autoctl `--pgport`), not 5432.
fn member_pg_port(member: &ServiceMemberInfo) -> i32 {
    member.port.unwrap_or(5432)
}

/// Where a cluster member's backup runs: the Docker daemon that can reach
/// its container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberPlacement {
    /// On this host, with this host's Docker.
    ThisHost,
    /// On worker node `node_id`, by its agent.
    Worker { node_id: i32 },
}

/// `service_members.node_id` places each member: NULL is the control
/// plane's daemon, a node id that worker's. The service's own `node_id` says
/// nothing about where its data members run (the monitor may be here while
/// every data member is on a worker).
fn member_placement(member: &ServiceMemberInfo) -> MemberPlacement {
    match member.node_id {
        Some(node_id) => MemberPlacement::Worker { node_id },
        None => MemberPlacement::ThisHost,
    }
}

pub struct PostgresClusterDeps {
    pub db: Arc<DatabaseConnection>,
    /// Resolves the live primary through the cluster's monitor, and reaches
    /// a worker primary's node through its agent.
    pub external_service_manager: Arc<temps_providers::ExternalServiceManager>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// This host's Docker, for a primary on this host. `None` on a server
    /// without one (`--profile control-plane`), which backs up only clusters
    /// whose primary runs on a worker node.
    pub docker: Option<bollard::Docker>,
    /// Derives the S3 credential a worker primary's backup gets (ADR-102).
    pub worker_credentials: Arc<WorkerCredentialScoper>,
}

pub struct PostgresClusterEngine {
    deps: Arc<PostgresClusterDeps>,
}

impl PostgresClusterEngine {
    pub fn new(deps: PostgresClusterDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for PostgresClusterEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;
        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;

        // ── Locate the cluster primary ───────────────────────────────────────
        // Ask the monitor: `service_members.role` is `replica` for every data
        // node, so it cannot say which one is writable.
        let primary_member = deps
            .external_service_manager
            .get_cluster_primary_member(service_id)
            .await
            // No trustworthy primary right now (monitor unreachable, failover
            // in progress) can clear up on its own, so let the executor retry.
            .map_err(|e| BackupError::Failed {
                reason: format!(
                    "could not locate the primary of cluster service {}: {}",
                    service_id, e
                ),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!(
                    "service {} is not a PostgreSQL cluster; the cluster backup engine cannot back it up",
                    service_id
                ),
            })?;
        let placement = member_placement(&primary_member);
        info!(
            backup_id,
            service_id,
            container = %primary_member.container_name,
            ordinal = primary_member.ordinal,
            pg_port = member_pg_port(&primary_member),
            ?placement,
            "PostgresClusterEngine: primary located",
        );

        // ── Service + S3 source ──────────────────────────────────────────────
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        let s3_source = v2_common::load_s3_source(deps.db.as_ref(), s3_source_id).await?;
        let s3_client = v2_common::build_s3_client(
            &s3_source,
            &deps.encryption_service,
            "postgres-cluster-engine",
        )?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        // The same layout the node half derives, wherever it runs:
        // <bucket_path>/external_services/postgres/<svc>/walg
        let walg_prefix = node_half::walg_prefix(
            &s3_source.bucket_name,
            &s3_source.bucket_path,
            &service.name,
        );
        let s3_list_prefix = node_half::walg_key_prefix(&s3_source.bucket_path, &service.name);

        // ── The node half: wherever the primary runs ─────────────────────────
        let outcome = match placement {
            MemberPlacement::Worker { node_id } => {
                backup_on_worker(
                    &deps,
                    ctx,
                    &service,
                    &primary_member,
                    node_id,
                    s3_source_id,
                    &s3_source.bucket_path,
                    &backup_uuid,
                )
                .await?
            }
            MemberPlacement::ThisHost => {
                backup_on_this_host(
                    &deps,
                    ctx,
                    &service,
                    &primary_member,
                    &s3_source,
                    &backup_uuid,
                )
                .await?
            }
        };

        let size_bytes = match load_backup_size_bytes(
            &s3_client,
            &s3_source.bucket_name,
            &s3_list_prefix,
            &outcome.stdout,
            &outcome.stderr,
            &backup_uuid,
        )
        .await
        {
            Ok(size) => Some(size),
            Err(error) => {
                warn!(backup_id, error = %error, "cluster: could not read per-backup size");
                None
            }
        };

        let metadata_key = format!("{}metadata.json", s3_list_prefix);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            "",
            &walg_prefix,
            size_bytes.unwrap_or(0),
            s3_source_id,
            "lz4",
            Some(json!({
                "backup_tool": "wal-g",
                "topology": "cluster",
                "primary_container": primary_member.container_name,
                "primary_ordinal": primary_member.ordinal,
                "primary_node_id": primary_member.node_id,
                "lsn": outcome.lsn.unwrap_or_default(),
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;
        v2_common::record_walg_identity(deps.db.as_ref(), backup_id, &backup_uuid).await?;

        info!(
            backup_id,
            %walg_prefix,
            ?size_bytes,
            "PostgresClusterEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: walg_prefix,
            size_bytes,
            compression: "lz4".to_string(),
        })
    }
}

/// The node half, here, against this host's Docker.
async fn backup_on_this_host(
    deps: &PostgresClusterDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    primary: &ServiceMemberInfo,
    s3_source: &temps_entities::s3_sources::Model,
    backup_uuid: &str,
) -> Result<WalgBaseBackupOutcome, BackupError> {
    let Some(docker) = deps.docker.as_ref() else {
        return Err(BackupError::PermanentFailure {
            reason: format!(
                "The primary of cluster service {} ('{}'), member '{}', runs on this server, \
                 which has no Docker daemon to run its WAL-G backup in",
                service.id, service.name, primary.container_name
            ),
        });
    };
    let decrypt = |field: &str, value: &str| {
        deps.encryption_service
            .decrypt_string(value)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt {field} of S3 source {}: {e}", s3_source.id),
            })
    };
    let s3 = temps_providers::externalsvc::S3Credentials {
        access_key_id: decrypt("access key", &s3_source.access_key_id)?,
        secret_key: decrypt("secret key", &s3_source.secret_key)?,
        session_token: v2_common::decrypt_session_token(s3_source, &deps.encryption_service)?,
        region: s3_source.region.clone(),
        endpoint: s3_source.endpoint.clone(),
        bucket_name: s3_source.bucket_name.clone(),
        bucket_path: s3_source.bucket_path.clone(),
        force_path_style: s3_source.effective_path_style(true).force_path_style,
    };
    let config_json = deps
        .encryption_service
        .decrypt_string(service.config.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|_| "{}".to_string());
    let parameters: Value = serde_json::from_str(&config_json).unwrap_or(Value::Null);
    cluster_walg_base_backup(
        docker,
        ClusterWalgBaseBackup {
            service_id: service.id,
            service: service.name.clone(),
            container_name: primary.container_name.clone(),
            pg_port: member_pg_port(primary),
            login: PostgresLogin::from_parameters(&parameters),
            s3,
            backup_uuid: backup_uuid.to_string(),
        },
        &ctx.cancel,
    )
    .await
    .map_err(backup_error)
}

/// The node half, on the worker that runs the primary, by its agent.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per input of the worker's request; a struct would only rename them"
)]
async fn backup_on_worker(
    deps: &PostgresClusterDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    primary: &ServiceMemberInfo,
    node_id: i32,
    s3_source_id: i32,
    bucket_path: &str,
    backup_uuid: &str,
) -> Result<WalgBaseBackupOutcome, BackupError> {
    // A one-shot credential, limited to the cluster's own WAL-G repository:
    // the cluster backup writes no `walg.env` that would keep using it.
    let mint = || {
        super::worker_agent::scoped_destination(
            &deps.worker_credentials,
            deps.db.as_ref(),
            s3_source_id,
            WorkerS3Grant::job(
                service.id,
                node_id,
                node_half::walg_key_prefix(bucket_path, &service.name),
                ScopedS3Access::Write,
                "backup",
            ),
            "backup",
        )
    };
    let destination = mint().await?;
    // A node already running its maximum of backups and restores is waited
    // for (bounded, within this backup's own runtime limit) instead of
    // failing the backup; a try after a wait gets a credential minted after
    // it.
    let request = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        destination,
        mint,
        |destination| {
            deps.external_service_manager
                .walg_backup_worker_postgres_cluster(
                    service.id,
                    primary,
                    destination,
                    bucket_path.to_string(),
                    backup_uuid.to_string(),
                )
        },
        |wait| super::worker_agent::wait_for_node_slot_minting(ctx.backup_id, service.id, wait),
    );
    let result = tokio::select! {
        _ = ctx.cancel.cancelled() => {
            node_backup_runs_on(service, node_id, backup_uuid);
            return Err(BackupError::Cancelled);
        }
        result = request => result?,
    };
    let result = match result {
        Ok(result) => result,
        // The node runs the backup in a task of its own, which a dropped
        // connection or a timeout here does not end: its outcome is asked
        // for under the backup's id. Failing (and so retrying) instead would
        // start another `backup-push` of the same database beside it.
        Err(ExternalServiceError::AgentUnanswered { detail, .. }) => {
            warn!(
                service_id = service.id,
                node_id,
                backup_uuid,
                "The cluster WAL-G backup request to the primary's node went unanswered \
                 ({detail}); following the node's backup until it ends"
            );
            await_worker_backup(
                &BACKUP_OUTCOME_WAIT,
                &ctx.cancel,
                service,
                node_id,
                backup_uuid,
                || {
                    deps.external_service_manager
                        .walg_backup_outcome_worker_postgres_cluster(
                            service.id,
                            node_id,
                            backup_uuid,
                        )
                },
            )
            .await?
        }
        Err(e) => {
            return Err(BackupError::Failed {
                reason: format!(
                    "WAL-G backup of cluster service {} ('{}') on node {node_id}, where its \
                     primary '{}' runs: {e}",
                    service.id, service.name, primary.container_name
                ),
            })
        }
    };
    worker_outcome(service, node_id, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(node_id: Option<i32>) -> ServiceMemberInfo {
        ServiceMemberInfo {
            id: 2,
            role: "replica".to_string(),
            node_id,
            container_name: "postgres-orders-ha-2".to_string(),
            hostname: Some("postgres-orders-ha-2".to_string()),
            port: Some(6012),
            status: "running".to_string(),
            ordinal: 2,
            compute_ip: None,
            provisioning_step: None,
            provisioning_error: None,
            live_state: Some("primary".to_string()),
        }
    }

    /// Cluster members listen on their own stored port; dialling 5432 made
    /// every cluster backup fail with "connection refused".
    #[test]
    fn member_pg_port_uses_the_members_stored_port() {
        let mut member = member(None);
        assert_eq!(member_pg_port(&member), 6012);

        member.port = None;
        assert_eq!(member_pg_port(&member), 5432);
    }

    /// The reported failure: the primary ran on a worker, the backup
    /// exec'd into it through the control plane's Docker, which has no such
    /// container ("No such container" on every run). The member's own
    /// placement decides where its backup runs.
    #[test]
    fn a_primary_on_a_worker_is_backed_up_on_that_worker() {
        assert_eq!(
            member_placement(&member(Some(9))),
            MemberPlacement::Worker { node_id: 9 }
        );
        assert_eq!(member_placement(&member(None)), MemberPlacement::ThisHost);
    }
}
