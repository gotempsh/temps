// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Standalone backup and restore support: WAL health, archive repointing and
//! restore fencing.

use crate::externalsvc::{ServiceConfig, ServiceType};
use crate::services::*;
use sea_orm::{ActiveModelTrait, Set};
use std::collections::BTreeSet;
use temps_entities::external_services;
use tracing::debug;

impl ExternalServiceManager {
    /// Stop every restore helper container that may still be writing into
    /// the service `service_name` (see [`crate::externalsvc::restore_helper`]).
    ///
    /// Helpers are matched by the container they restore into, which is the
    /// engine's own container name for this service and, for an imported
    /// service, the pre-existing container it runs in (`imported_container`).
    /// Both are fenced, so a helper labelled with either is found.
    ///
    /// Returns `Ok(None)` when this process has no Docker daemon: restores
    /// drive local containers, so such a process cannot have launched one.
    pub async fn fence_restore_helpers(
        &self,
        service_name: &str,
        service_type: ServiceType,
        imported_container: Option<&str>,
    ) -> Result<
        Option<crate::externalsvc::restore_helper::RestoreFenceReport>,
        crate::externalsvc::restore_helper::RestoreFenceError,
    > {
        use crate::externalsvc::restore_helper::{fence_restore_helpers, RestoreFenceError};

        let Some(docker) = self.docker.get() else {
            return Ok(None);
        };
        // An S3/Blob service may run on any managed engine, and a service
        // that is not registered yet has no recorded one, so every engine
        // that could own the name is fenced.
        let instances = self
            .candidate_instances(service_name, service_type)
            .map_err(|e| RestoreFenceError::Resolve {
                service_name: service_name.to_string(),
                service_type: service_type.to_string(),
                reason: e.to_string(),
            })?;
        let mut containers: BTreeSet<String> = instances
            .iter()
            .map(|instance| instance.get_docker_container_name())
            .collect();
        if let Some(imported) = imported_container.filter(|c| !c.trim().is_empty()) {
            containers.insert(imported.to_string());
        }

        let mut report = crate::externalsvc::restore_helper::RestoreFenceReport::default();
        for container in containers {
            let fenced = fence_restore_helpers(docker, &container).await?;
            report.stopped.extend(fenced.stopped);
            report.removed.extend(fenced.removed);
        }
        // A download the engine staged inside the service itself (not a
        // helper container) must be confirmed stopped too.
        for instance in &instances {
            instance
                .stop_staged_restore()
                .await
                .map_err(|e| RestoreFenceError::Stop {
                    target_container: instance.get_docker_container_name(),
                    helper: "staged restore download".to_string(),
                    reason: e.to_string(),
                })?;
        }
        Ok(Some(report))
    }

    /// Remove the Docker objects of a service that was provisioned but never
    /// registered (a cancelled or failed restore into a new service). Covers
    /// every engine that could have created it; objects that do not exist are
    /// skipped by the engines.
    pub async fn remove_unregistered_service(
        &self,
        name: &str,
        service_type: ServiceType,
    ) -> Result<(), ExternalServiceError> {
        // Every engine is tried: an engine that never created the service
        // fails with "no such container", which must not stop the engine that
        // did create it from being cleaned up.
        let mut removed = 0usize;
        let mut failures = Vec::new();
        for instance in self.candidate_instances(name, service_type)? {
            let container = instance.get_docker_container_name();
            match instance.remove().await {
                Ok(()) => removed += 1,
                Err(e) => failures.push(format!("{container}: {e}")),
            }
        }
        if removed == 0 && !failures.is_empty() {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "removing unregistered service '{}' failed on every engine: {}",
                    name,
                    failures.join("; ")
                ),
            });
        }
        if !failures.is_empty() {
            debug!(
                service = %name,
                "Engines that did not own the unregistered service skipped removal: {}",
                failures.join("; ")
            );
        }
        Ok(())
    }

    /// Containers and volumes that already exist under the names a service
    /// called `service_name` would own (see
    /// [`ExternalService::docker_resource_names`]).
    ///
    /// A restore into a new service calls this before provisioning: if
    /// anything is already there, it belongs to someone else (a leftover of an
    /// earlier failed run, or another process), and tearing the new service
    /// down on cancellation would delete it. Returns one `"container NAME"` /
    /// `"volume NAME"` entry per object found; an engine that cannot report
    /// its names is an error, never an empty list.
    pub async fn existing_docker_resources(
        &self,
        service_name: &str,
        service_type: ServiceType,
    ) -> Result<Vec<String>, ExternalServiceError> {
        let docker = self.require_docker()?;
        let mut existing = Vec::new();
        let mut containers: Vec<String> = Vec::new();
        let mut volumes: Vec<String> = Vec::new();
        for instance in self.candidate_instances(service_name, service_type)? {
            let names = instance.docker_resource_names().ok_or_else(|| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "service type '{}' does not report the Docker resources it owns, so \
                         existing containers or volumes for '{}' cannot be checked",
                        service_type, service_name
                    ),
                }
            })?;
            containers.extend(names.containers);
            volumes.extend(names.volumes);
        }
        for container in &containers {
            match docker
                .inspect_container(
                    container,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(_) => existing.push(format!("container {}", container)),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(e) => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "could not inspect container '{}' for service '{}': {}",
                            container, service_name, e
                        ),
                    })
                }
            }
        }
        for volume in &volumes {
            match docker.inspect_volume(volume).await {
                Ok(_) => existing.push(format!("volume {}", volume)),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(e) => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "could not inspect volume '{}' for service '{}': {}",
                            volume, service_name, e
                        ),
                    })
                }
            }
        }
        Ok(existing)
    }

    // Helper methods
    /// Why a WAL-G backup of `service_id` would restart PostgreSQL right
    /// now, or `None` when it would not. A restart is needed when a managed,
    /// local, standalone service has `archive_mode` off: turning it on
    /// recreates the container. When the setting cannot be read the answer
    /// is "it may", so the caller asks for confirmation rather than assume.
    /// Bounded: one SQL round trip to the service.
    pub async fn backup_restart_reason(
        &self,
        service_id: i32,
    ) -> Result<Option<String>, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let is_postgres = matches!(
            ServiceType::from_str(&service.service_type),
            Ok(ServiceType::Postgres)
        );
        if !is_postgres || service.topology == "cluster" || service.node_id.is_some() {
            return Ok(None);
        }
        let parameters = self.get_service_parameters(service_id).await?;
        let docker = self.require_docker()?;
        let postgres =
            crate::externalsvc::postgres::PostgresService::new(service.name.clone(), docker);
        let config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::Postgres,
            version: service.version.clone(),
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to serialize parameters of service {}: {}",
                        service_id, e
                    ),
                }
            })?,
        };
        Ok(
            match postgres.backup_requires_restart(config.clone()).await {
                Ok(false) => None,
                Ok(true) => match postgres.backup_restart_refusal(config).await {
                    Ok(Some(refusal)) => Some(format!(
                    "WAL archiving is off for PostgreSQL service '{}', and turning it on needs a \
                     new container that Temps cannot create safely, so this backup will fail \
                     without changing anything: {}",
                    service.name, refusal
                )),
                    Ok(None) | Err(_) => Some(format!(
                "WAL archiving is off for PostgreSQL service '{}'. The first WAL-G backup turns \
                 it on, which restarts PostgreSQL in a new container (usually under a minute). \
                 Temps keeps the current container until the new one is verified to hold the same \
                 data, and puts it back otherwise",
                    service.name
                )),
                },
                Err(e) => Some(format!(
                    "Temps could not read whether WAL archiving is on for PostgreSQL service '{}' \
                 ({}). If it is off, this backup restarts PostgreSQL in a new container to turn it \
                 on",
                    service.name, e
                )),
            },
        )
    }

    /// Read the Postgres WAL health snapshot from `health_metadata.postgres_wal`.
    ///
    /// Returns `Ok(None)` when the service exists but has no snapshot yet
    /// (e.g., probe hasn't run, or service isn't Postgres). Returns
    /// `ServiceNotFound` only when the row doesn't exist at all.
    pub async fn get_postgres_wal_health(
        &self,
        service_id: i32,
    ) -> Result<
        Option<crate::externalsvc::postgres_wal_health::PostgresWalHealth>,
        ExternalServiceError,
    > {
        let service = self.get_service(service_id).await?;
        let Some(metadata) = service.health_metadata else {
            return Ok(None);
        };
        let Some(snapshot) = metadata.get("postgres_wal") else {
            return Ok(None);
        };
        match serde_json::from_value(snapshot.clone()) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(e) => {
                tracing::warn!(
                    "health_metadata.postgres_wal for service {} did not parse: {}",
                    service_id,
                    e
                );
                Ok(None)
            }
        }
    }

    /// Deliberately, explicitly move a service's continuous archiving to a
    /// different S3 source.
    ///
    /// For Postgres/Timescale this physically re-points `archive_command`
    /// (not just the pin's bookkeeping columns — see
    /// `crates/temps-providers/src/externalsvc/postgres.rs`'s
    /// `force_reenable_continuous_archiving`), since WAL-G bakes its
    /// destination into the container's environment. For MariaDB there is no
    /// equivalent container-side config to rewrite: the binlog shipper
    /// (`ExternalServiceHealthMonitor::maybe_archive_mariadb_binlogs`) reads
    /// `continuous_archive_s3_source_id` fresh every tick, so updating the
    /// pin alone is sufficient to redirect the next shipment.
    ///
    /// Only ever call this on purpose, and only when you accept that data
    /// archived before this call (WAL segments, binlog segments) lives under
    /// the *old* source and will never be visible under the new one again —
    /// Cloud's Postgres mirror (or any WAL-G/MariaDB PITR restore) can no
    /// longer verify or replay it going forward. `continuous_archive_pinned_at`
    /// records the moment of the switch so `crates/temps-cloud/src/backup_mirror.rs`
    /// can tell those backups apart from ones taken after the switch, which
    /// are expected to resolve normally as archiving catches up.
    pub async fn repoint_continuous_archive_source(
        &self,
        service_id: i32,
        new_s3_source_id: i32,
    ) -> Result<external_services::Model, ExternalServiceError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let service = self.get_service(service_id).await?;
        let service_type = service.service_type.to_ascii_lowercase();
        if !matches!(
            service_type.as_str(),
            "postgres" | "postgresql" | "timescale" | "timescaledb" | "mariadb" | "mysql"
        ) {
            return Err(ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            });
        }

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(new_s3_source_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| ExternalServiceError::DatabaseError {
                reason: format!("looking up S3 source {}: {}", new_s3_source_id, e),
            })?
            .ok_or_else(|| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!("S3 source {} does not exist", new_s3_source_id),
            })?;

        // Captured *before* the physical repoint below, not after it
        // succeeds. `backup_mirror.rs` uses this timestamp as the cutoff for
        // "this backup's WAL predates the switch, so it can never appear
        // under the new prefix" -- if it were captured after the physical
        // change instead, any backup whose base snapshot started in the gap
        // between "container actually repointed" and "DB write observed"
        // would be a false positive: its WAL is correctly landing in the new
        // source already, but it would still get permanently marked
        // unsupported because its `started_at` predates that later
        // timestamp. Capturing it first makes it a safe lower bound on the
        // real switch instant instead.
        let pin_started_at = chrono::Utc::now();

        // Postgres/Timescale needs `archive_command` physically rewritten —
        // WAL-G bakes its destination into the container's environment, so
        // updating the pin alone would be a lie about where archiving
        // actually writes. MariaDB's shipper has no equivalent container
        // state to rewrite: it reads the pin fresh every tick (see
        // `ExternalServiceHealthMonitor::maybe_archive_mariadb_binlogs`), so
        // updating the pin below is the entire repoint for that engine.
        //
        // Captured before the conditional so `ArchiveSourceDesynced` can
        // produce an engine-accurate message if the DB persist fails below.
        let physical_repoint_occurred = matches!(
            service_type.as_str(),
            "postgres" | "postgresql" | "timescale" | "timescaledb"
        );
        if physical_repoint_occurred {
            let access_key = self
                .encryption_service
                .decrypt_string(&s3_source.access_key_id)
                .map_err(|e| ExternalServiceError::DecryptionFailed {
                    service_id,
                    param_name: "access_key_id".to_string(),
                    reason: e.to_string(),
                })?;
            let secret_key = self
                .encryption_service
                .decrypt_string(&s3_source.secret_key)
                .map_err(|e| ExternalServiceError::DecryptionFailed {
                    service_id,
                    param_name: "secret_key".to_string(),
                    reason: e.to_string(),
                })?;
            let session_token = s3_source
                .session_token
                .as_deref()
                .map(|token| self.encryption_service.decrypt_string(token))
                .transpose()
                .map_err(|e| ExternalServiceError::DecryptionFailed {
                    service_id,
                    param_name: "session_token".to_string(),
                    reason: e.to_string(),
                })?;

            let s3_credentials = crate::S3Credentials {
                access_key_id: access_key,
                secret_key,
                session_token,
                region: s3_source.region.clone(),
                endpoint: s3_source.endpoint.clone(),
                bucket_name: s3_source.bucket_name.clone(),
                bucket_path: s3_source.bucket_path.clone(),
                force_path_style: s3_source.force_path_style.unwrap_or(true),
            };

            // Layout must match `crates/temps-backup/src/engines/postgres_walg.rs`
            // exactly: WAL-G requires a base backup and the WAL segments covering
            // its start/end LSN under the same prefix to be restorable.
            let subpath_root = format!("external_services/postgres/{}", service.name);
            let bucket_path_clean = s3_source.bucket_path.trim_matches('/');
            let walg_prefix = if bucket_path_clean.is_empty() {
                format!(
                    "s3://{}/{}/walg",
                    s3_source.bucket_name,
                    subpath_root.trim_matches('/'),
                )
            } else {
                format!(
                    "s3://{}/{}/{}/walg",
                    s3_source.bucket_name,
                    bucket_path_clean,
                    subpath_root.trim_matches('/'),
                )
            };

            let config_json = service
                .config
                .as_deref()
                .map(|encrypted| self.encryption_service.decrypt_string(encrypted))
                .transpose()
                .map_err(|e| ExternalServiceError::DecryptionFailed {
                    service_id,
                    param_name: "config".to_string(),
                    reason: e.to_string(),
                })?
                .unwrap_or_else(|| "{}".to_string());
            let service_config = crate::externalsvc::ServiceConfig {
                name: service.name.clone(),
                service_type: crate::externalsvc::ServiceType::Postgres,
                version: None,
                parameters: serde_json::from_str(&config_json).unwrap_or(serde_json::Value::Null),
            };

            let docker = self.require_docker()?;
            let postgres =
                crate::externalsvc::postgres::PostgresService::new(service.name.clone(), docker);
            postgres
                .force_reenable_continuous_archiving(service_config, &s3_credentials, &walg_prefix)
                .await
                .map_err(|e| ExternalServiceError::DockerError {
                    id: service_id,
                    reason: format!("failed to repoint WAL archiving: {}", e),
                })?;
        }

        // The container (when Postgres/Timescale) has already been
        // physically repointed above -- WAL is now landing in
        // `new_s3_source_id` regardless of whether this persists. A single
        // transient DB hiccup right here must not leave that live change
        // unrecorded, so retry before surfacing the desync as a distinct,
        // actionable error instead of an ordinary `DatabaseError`.
        let retry = temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_millis(200))
            .with_max_delay(std::time::Duration::from_secs(2));
        let persisted = retry
            .retry(|| async {
                external_services::ActiveModel {
                    id: Set(service.id),
                    continuous_archive_s3_source_id: Set(Some(new_s3_source_id)),
                    continuous_archive_pinned_at: Set(Some(pin_started_at)),
                    ..Default::default()
                }
                .update(self.db.as_ref())
                .await
                .map_err(|e| e.to_string())
            })
            .await;

        if let Err(reason) = persisted {
            let attempts = retry.max_attempts;
            let message = if physical_repoint_occurred {
                // Postgres/Timescale: WAL-G archive_command was already
                // rewritten in the container, so archiving really is landing
                // in the new source. The DB still records the old one.
                // Genuine live desync — operator must repoint again once
                // the database is reachable.
                format!(
                    "Service {service_id} archiving now writes to S3 source \
                     {new_s3_source_id}, but the database still records the previous \
                     source because persisting the pin failed after {attempts} \
                     attempt(s): {reason}. The live WAL destination and the recorded \
                     pin are now out of sync — repoint to the same source again to \
                     reconcile, or fix the underlying database issue first."
                )
            } else {
                // MariaDB: no container-side change occurred. The shipper
                // re-reads the pin every tick, so archiving has not moved.
                // No live desync — operator just needs to retry once the
                // database is reachable.
                format!(
                    "Service {service_id}: persisting the continuous archive source \
                     pin to S3 source {new_s3_source_id} failed after {attempts} \
                     attempt(s): {reason}. The archiving source was not changed — \
                     retry to apply the change once the database issue is resolved."
                )
            };
            return Err(ExternalServiceError::ArchiveSourceDesynced {
                service_id,
                new_s3_source_id,
                attempts,
                reason,
                physical_repoint_occurred,
                message,
            });
        }

        self.get_service(service_id).await
    }

    /// Wipe `health_metadata.postgres_wal` then immediately run the WAL probe
    /// and persist the fresh snapshot. Called from `start_service` after a
    /// Postgres recreate so the UI doesn't show pre-recreate data while the
    /// background monitor catches up.
    pub(super) async fn refresh_postgres_wal_health(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        use crate::externalsvc::postgres_wal_health;

        // Reload the service row (status is now 'running'); skip if it isn't
        // actually a standalone Postgres service.
        let service = self.get_service(service_id).await?;
        if service.service_type != "postgres" || service.topology != "standalone" {
            return Ok(());
        }

        // Clear the stale snapshot first. Even if the probe below fails, the
        // UI will see "no snapshot yet" rather than wrong data.
        let cleared = clear_health_metadata_key(service.health_metadata.as_ref(), "postgres_wal");
        let mut active: external_services::ActiveModel = service.clone().into();
        active.health_metadata = Set(cleared);
        active.update(self.db.as_ref()).await?;

        // Probe fresh against the new container. Best-effort.
        let service_config = self.get_service_config(service_id).await?;
        let Some(conn_str) = postgres_wal_health::build_conn_str(&service_config.parameters) else {
            return Ok(());
        };
        let Some(snapshot) = postgres_wal_health::probe_wal_health(&conn_str).await else {
            return Ok(());
        };

        // Reload again, merge the fresh snapshot under postgres_wal.
        let service = self.get_service(service_id).await?;
        let merged =
            merge_health_metadata_key(service.health_metadata.as_ref(), "postgres_wal", &snapshot);
        let mut active: external_services::ActiveModel = service.into();
        active.health_metadata = Set(Some(merged));
        active.update(self.db.as_ref()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::ServiceType;
    use crate::services::test_support::*;
    use bollard::Docker;
    use chrono::Utc;
    use std::sync::Arc;
    use temps_entities::external_services;

    /// Minimal external_services model suitable for repoint tests. No config
    /// encryption needed: the fields read by `repoint_continuous_archive_source`
    /// before the Postgres-specific decryption branch are only `service_type`,
    /// `id`, and `name`.
    fn repoint_test_service(id: i32, service_type: &str) -> external_services::Model {
        let now = Utc::now();
        external_services::Model {
            id,
            name: format!("test-{service_type}-{id}"),
            service_type: service_type.to_string(),
            version: None,
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            slug: None,
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    fn repoint_test_s3_source(id: i32) -> temps_entities::s3_sources::Model {
        let now = Utc::now();
        temps_entities::s3_sources::Model {
            id,
            backing_service_id: None,
            name: format!("test-source-{id}"),
            bucket_name: "test-bucket".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_path: String::new(),
            access_key_id: "ciphertext-key".to_string(),
            secret_key: "ciphertext-secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn repoint_rejects_unsupported_service_type() {
        // Redis has no continuous archive mechanism; repoint must fail fast.
        let service = repoint_test_service(100, "redis");
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![service]])
                .into_connection(),
        ));

        let err = manager
            .repoint_continuous_archive_source(100, 5)
            .await
            .expect_err("redis service type must be rejected");

        assert!(
            matches!(
                err,
                ExternalServiceError::InvalidServiceType { id: 100, .. }
            ),
            "expected InvalidServiceType(100), got {err:?}"
        );
    }

    #[tokio::test]
    async fn repoint_rejects_nonexistent_s3_source() {
        // Valid service type (mariadb) but the requested S3 source ID does not exist.
        let service = repoint_test_service(101, "mariadb");
        let manager = mock_service_manager_with_db(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![service]])
                // Empty result for `s3_sources::Entity::find_by_id(999)`.
                .append_query_results([Vec::<temps_entities::s3_sources::Model>::new()])
                .into_connection(),
        ));

        let err = manager
            .repoint_continuous_archive_source(101, 999)
            .await
            .expect_err("unknown S3 source must be rejected");

        assert!(
            matches!(
                err,
                ExternalServiceError::ParameterValidationFailed {
                    service_id: 101,
                    ..
                }
            ),
            "expected ParameterValidationFailed(101), got {err:?}"
        );
    }

    /// Exercises the retry-exhausted path for MariaDB, which has no
    /// container-side physical repoint (`physical_repoint_occurred = false`).
    /// The DB persist is attempted `max_attempts` (3) times and all fail; the
    /// returned error must carry the correct attempt count and a message that
    /// does NOT imply a live desync (archiving was never redirected).
    #[tokio::test]
    async fn repoint_mariadb_desynced_error_after_all_persist_attempts_fail() {
        let service = repoint_test_service(102, "mariadb");
        let s3_source = repoint_test_s3_source(7);
        // 3 exec errors: one per retry attempt (RetryConfig::new(3)).
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![service]])
            .append_query_results([vec![s3_source]])
            .append_exec_errors([
                sea_orm::DbErr::Custom("connection refused".to_owned()),
                sea_orm::DbErr::Custom("connection refused".to_owned()),
                sea_orm::DbErr::Custom("connection refused".to_owned()),
            ])
            .into_connection();
        let manager = mock_service_manager_with_db(Arc::new(db));

        let err = manager
            .repoint_continuous_archive_source(102, 7)
            .await
            .expect_err("persist failure after all retries must be surfaced");

        match err {
            ExternalServiceError::ArchiveSourceDesynced {
                service_id: 102,
                new_s3_source_id: 7,
                attempts,
                physical_repoint_occurred: false,
                ref message,
                ..
            } => {
                assert_eq!(attempts, 3, "must report the configured retry count");
                assert!(
                    message.contains("was not changed"),
                    "MariaDB message must say the archiving source was not changed; got: {message}"
                );
                assert!(
                    !message.contains("now writes to"),
                    "MariaDB message must not imply archiving moved to the new source; got: {message}"
                );
            }
            other => panic!("expected ArchiveSourceDesynced(102, 7, false), got: {other:?}"),
        }
    }

    // --- Docker-optional / control-plane policy tests ---

    /// A process without a Docker daemon cannot have launched a restore
    /// helper, so fencing is vacuous rather than an error that would keep
    /// restores blocked forever.
    #[tokio::test]
    async fn fence_restore_helpers_without_docker_is_vacuous() {
        let manager = control_plane_manager();
        let fenced = manager
            .fence_restore_helpers("orders", ServiceType::Mongodb, None)
            .await
            .expect("no daemon is not an error");
        assert!(fenced.is_none());
    }

    /// The fence resolves the container name from the engine itself
    /// (MongoDB's is `temps-mongodb-<name>`, not `mongodb-<name>`) and also
    /// covers an imported service's real container.
    #[tokio::test]
    async fn fence_restore_helpers_uses_the_engine_and_imported_container_names() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        if crate::utils::pull_image_with_retry(&docker, "busybox:latest", None)
            .await
            .is_err()
        {
            println!("Could not pull busybox, skipping");
            return;
        }
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let service_name = format!("fence-{}", &suffix[..8]);
        let imported = format!("imported-{}", &suffix[..8]);
        let mut helper_ids = Vec::new();
        for (index, target) in [format!("temps-mongodb-{service_name}"), imported.clone()]
            .iter()
            .enumerate()
        {
            let created = docker
                .create_container(
                    Some(
                        bollard::query_parameters::CreateContainerOptionsBuilder::new()
                            .name(&format!("temps-fence-mgr-{}-{index}", &suffix[..8]))
                            .build(),
                    ),
                    bollard::models::ContainerCreateBody {
                        image: Some("busybox:latest".to_string()),
                        cmd: Some(vec!["sleep".into(), "300".into()]),
                        labels: Some(crate::externalsvc::restore_helper::restore_helper_labels(
                            target,
                        )),
                        ..Default::default()
                    },
                )
                .await;
            let Ok(created) = created else {
                println!("Could not create a helper container, skipping");
                return;
            };
            let _ = docker
                .start_container(
                    &created.id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await;
            helper_ids.push(created.id);
        }

        let manager = mock_service_manager(vec![]);
        let fenced = manager
            .fence_restore_helpers(&service_name, ServiceType::Mongodb, Some(&imported))
            .await;

        let mut left = 0;
        for id in &helper_ids {
            if docker
                .inspect_container(
                    id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .is_ok()
            {
                left += 1;
                let _ = docker
                    .remove_container(
                        id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
        }
        let report = fenced
            .expect("fence against a reachable daemon")
            .expect("a daemon is available");
        assert_eq!(report.stopped.len(), 2, "{report:?}");
        assert_eq!(left, 0, "both helpers must be removed");
    }
}
