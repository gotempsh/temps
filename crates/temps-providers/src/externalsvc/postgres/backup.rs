// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backing up managed PostgreSQL to S3 with WAL-G or pg_dumpall.

use super::*;
use crate::externalsvc::ServiceConfig;
use bollard::Docker;
use temps_entities::external_service_backups;
use tracing::{debug, error, info, warn};

/// Hard ceiling for a single backup `docker exec`. Hit this and we give up,
/// surface the captured output, and mark the backup row as failed. Six hours
/// covers very large WAL-G + pg_dumpall runs while still bounding stuck-exec
/// blast radius — without this, a hung exec would keep the row in `running`
/// indefinitely.
pub(super) const BACKUP_EXEC_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(6 * 3600);

impl PostgresService {
    /// Backup PostgreSQL data to S3 using WAL-G.
    ///
    /// Runs `wal-g backup-push` inside the running PostgreSQL container via `docker exec`.
    /// WAL-G uploads the backup directly to S3 from within the container — zero data flows
    /// through the Temps process, keeping memory usage flat regardless of database size.
    ///
    /// Before pushing a backup, this method also:
    /// 1. Writes WAL-G S3 credentials to `/var/lib/postgresql/walg.env` on the shared volume
    /// 2. Enables continuous WAL archiving via `ALTER SYSTEM SET archive_command`
    /// 3. Calls `pg_reload_conf()` so PostgreSQL picks up the change without restart
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn backup_to_s3_walg(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &crate::externalsvc::S3Credentials,
        backup: temps_entities::backups::Model,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> anyhow::Result<crate::externalsvc::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        let postgres_config = self.get_postgres_config(service_config.clone())?;
        let container_name = self.get_live_container_name(&postgres_config);

        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": self.name,
            "backup_tool": "wal-g",
        });

        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(external_service.id),
            service_name_snapshot: Set(Some(external_service.name.clone())),
            service_type_snapshot: Set(Some(external_service.service_type.clone())),
            backup_id: Set(backup.id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set("".to_string()),
            metadata: Set(metadata),
            compression_type: Set("lz4".to_string()),
            created_by: Set(0),
            ..Default::default()
        }
        .insert(pool)
        .await?;

        // Build the WAL-G S3 prefix using the STABLE subpath_root (no date component).
        let walg_s3_prefix = format!(
            "s3://{}/{}/walg",
            s3_credentials.bucket_name,
            subpath_root.trim_matches('/')
        );
        // Bucket-relative prefix used to list backup objects after success.
        let s3_list_prefix = format!("{}/walg/", subpath_root.trim_matches('/'));

        // Run the backup, then either persist success or mark failure. Any
        // `?` propagation in the inner block lands in the failure branch.
        let result = async {
            // Legacy callers need the same pre-backup policy and resource
            // verification as the v2 engine. Persist any refusal as a failed
            // child rather than completing an unrestorable base backup.
            let archiving = self
                .write_wal_archiving_config(service_config, s3_credentials, &walg_s3_prefix, true)
                .await?;
            if archiving.restarted {
                info!(
                    "PostgreSQL service '{}' was restarted to enable WAL archiving; its data was verified intact before the base backup",
                    self.name
                );
            }
            self.run_walg_backup_push(
                &container_name,
                &walg_s3_prefix,
                s3_credentials,
                &postgres_config,
            )
            .await
        }
        .await;

        match result {
            Ok(_) => {
                // Compute size by listing S3. WAL-G streams chunks; we
                // don't see them locally.
                let size_bytes = match crate::externalsvc::s3_util::list_total_size(
                    s3_client,
                    &s3_credentials.bucket_name,
                    &s3_list_prefix,
                )
                .await
                {
                    Ok(n) => Some(n),
                    Err(e) => {
                        warn!(
                            "WAL-G backup succeeded but failed to compute size for s3://{}/{}: {}",
                            s3_credentials.bucket_name, s3_list_prefix, e
                        );
                        None
                    }
                };

                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("completed".to_string());
                backup_update.finished_at = Set(Some(Utc::now()));
                backup_update.s3_location = Set(walg_s3_prefix.clone());
                backup_update.size_bytes = Set(size_bytes);
                backup_update.update(pool).await?;

                info!(
                    "PostgreSQL WAL-G backup completed successfully (prefix: {}, size: {:?})",
                    walg_s3_prefix, size_bytes
                );

                Ok(crate::externalsvc::BackupOutcome::new(
                    walg_s3_prefix,
                    size_bytes,
                ))
            }
            Err(e) => {
                let error_msg = format!("WAL-G backup failed: {}", e);
                error!("{}", error_msg);
                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("failed".to_string());
                backup_update.error_message = Set(Some(error_msg.clone()));
                backup_update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = backup_update.update(pool).await {
                    error!(
                        "Failed to mark external_service_backups row {} as failed: {}",
                        backup_record.id, update_err
                    );
                }
                Err(e)
            }
        }
    }

    /// Build the wal-g env, run `wal-g backup-push` via `docker exec`, and
    /// return the env vector so the caller can also wire up WAL archiving.
    async fn run_walg_backup_push(
        &self,
        container_name: &str,
        walg_s3_prefix: &str,
        s3_credentials: &crate::externalsvc::S3Credentials,
        postgres_config: &PostgresConfig,
    ) -> anyhow::Result<Vec<String>> {
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!("PGUSER={}", postgres_config.username),
            format!("PGPASSWORD={}", postgres_config.password),
            format!("PGDATABASE={}", postgres_config.database),
            "PGHOST=localhost".to_string(),
            format!("PGPORT={}", POSTGRES_INTERNAL_PORT),
        ];
        // Absent unless this source holds a temporary (STS-style)
        // credential, so a long-lived one produces the exact environment
        // it always did.
        walg_env.extend(s3_credentials.session_token_env());

        if let Some(resolved_endpoint) = s3_credentials
            .resolve_endpoint_for_container(&self.docker, container_name)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_credentials.force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        info!(
            "Running wal-g backup-push in container '{}' (S3 prefix: {})",
            container_name, walg_s3_prefix
        );

        crate::externalsvc::exec_util::run_exec(
            &self.docker,
            container_name,
            vec![
                "sh".into(),
                "-c".into(),
                "wal-g backup-push $PGDATA 2>&1".into(),
            ],
            Some(walg_env.clone()),
            BACKUP_EXEC_TIMEOUT,
        )
        .await?;

        Ok(walg_env)
    }

    /// Backup PostgreSQL data to S3 using pg_dump via a sidecar container.
    ///
    /// Legacy fallback for containers without WAL-G (e.g., `postgres:18-alpine`,
    /// `pgvector/pgvector:pg17`). Runs pg_dump in a sidecar container on the same
    /// Docker network, streams output through gzip to a temp file, then uploads to S3.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn backup_to_s3_pgdump(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> anyhow::Result<crate::externalsvc::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        info!("Starting PostgreSQL backup to S3 via pg_dump sidecar");

        let postgres_config = self.get_postgres_config(service_config)?;

        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": self.name,
            "backup_tool": "pg_dumpall",
        });

        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(external_service.id),
            backup_id: Set(backup.id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set("".to_string()),
            metadata: Set(metadata),
            compression_type: Set("gzip".to_string()),
            created_by: Set(0),
            ..Default::default()
        }
        .insert(pool)
        .await?;

        let outcome = self
            .run_pg_dumpall_to_s3(s3_client, s3_source, subpath, &postgres_config)
            .await;

        match outcome {
            Ok((backup_key, size_bytes)) => {
                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("completed".to_string());
                backup_update.finished_at = Set(Some(Utc::now()));
                backup_update.size_bytes = Set(Some(size_bytes));
                backup_update.s3_location = Set(backup_key.clone());
                backup_update.update(pool).await?;
                Ok(crate::externalsvc::BackupOutcome::new(
                    backup_key,
                    Some(size_bytes),
                ))
            }
            Err(e) => {
                let error_msg = format!("pg_dumpall backup failed: {}", e);
                error!("{}", error_msg);
                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("failed".to_string());
                backup_update.error_message = Set(Some(error_msg.clone()));
                backup_update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = backup_update.update(pool).await {
                    error!(
                        "Failed to mark external_service_backups row {} as failed: {}",
                        backup_record.id, update_err
                    );
                }
                // The trait returns anyhow; the typed error stays its source.
                Err(e.into())
            }
        }
    }

    /// Pull image, run `pg_dumpall | gzip` in a sidecar, copy the dump out,
    /// upload it to S3, clean up. Returns `(backup_key, size_bytes)`.
    ///
    /// All cleanup (sidecar removal, temp file deletion) is best-effort and
    /// runs regardless of which step failed — so the caller only has to
    /// decide whether to mark the DB row as completed or failed.
    async fn run_pg_dumpall_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        postgres_config: &PostgresConfig,
    ) -> Result<(String, i64), PgDumpallBackupError> {
        use chrono::Utc;

        let db_container_name = self.get_live_container_name(postgres_config);
        let sidecar_image = postgres_config.docker_image.clone();

        info!("Pulling sidecar image {} for pg_dump", sidecar_image);
        crate::utils::pull_image_with_retry(&self.docker, &sidecar_image, None)
            .await
            .map_err(|reason| PgDumpallBackupError::PullImage {
                service: self.name.clone(),
                image: sidecar_image.clone(),
                reason,
            })?;

        // This attempt's own host directory, deleted when it goes out of
        // scope on every path.
        let work_dir = tempfile::Builder::new()
            .prefix("temps-extpg-backup-")
            .tempdir()
            .map_err(|e| PgDumpallBackupError::WorkDir {
                service: self.name.clone(),
                parent: std::env::temp_dir().display().to_string(),
                reason: e.to_string(),
            })?;
        let port_str = POSTGRES_INTERNAL_PORT.to_string();

        info!(
            "Running pg_dumpall sidecar for service '{}' (host={})",
            self.name, db_container_name
        );

        let (host_backup_path, size_bytes) = run_pg_dumpall_sidecar(
            &self.docker,
            &PgDumpallSidecar {
                image: &sidecar_image,
                network: &temps_core::NETWORK_NAME,
                host: &db_container_name,
                port: &port_str,
                username: &postgres_config.username,
                password: &postgres_config.password,
                database: &postgres_config.database,
            },
            work_dir.path(),
        )
        .await
        .map_err(|source| PgDumpallBackupError::Sidecar {
            service: self.name.clone(),
            source: Box::new(source),
        })?;

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_key = format!(
            "{}/postgres_backup_{}.sql.gz",
            subpath.trim_matches('/'),
            timestamp
        );

        let body = aws_sdk_s3::primitives::ByteStream::from_path(&host_backup_path)
            .await
            .map_err(|e| PgDumpallBackupError::OpenDump {
                service: self.name.clone(),
                path: host_backup_path.display().to_string(),
                reason: e.to_string(),
            })?;

        if let Err(e) = s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(body)
            .content_type("application/x-gzip")
            .send()
            .await
        {
            return Err(PgDumpallBackupError::Upload {
                service: self.name.clone(),
                bucket: s3_source.bucket_name.clone(),
                key: backup_key.clone(),
                reason: e.to_string(),
            });
        }

        drop(work_dir);
        info!(
            "Successfully uploaded pg_dumpall backup to s3://{}/{} ({} bytes)",
            s3_source.bucket_name, backup_key, size_bytes
        );

        Ok((backup_key, size_bytes))
    }
}

/// Where a [`run_pg_dumpall_sidecar`] connects, and with what.
pub(super) struct PgDumpallSidecar<'a> {
    image: &'a str,
    /// Docker network shared with the database container.
    network: &'a str,
    host: &'a str,
    port: &'a str,
    username: &'a str,
    password: &'a str,
    database: &'a str,
}

/// Why a pg_dumpall full backup of a PostgreSQL service failed. Every
/// variant names the service, and the step and resource that failed.
#[derive(Debug, thiserror::Error)]
pub(super) enum PgDumpallBackupError {
    #[error("Failed to pull pg_dumpall sidecar image '{image}' for service '{service}': {reason}")]
    PullImage {
        service: String,
        image: String,
        reason: String,
    },

    #[error(
        "Failed to create a working directory for the pg_dumpall backup of service '{service}' \
         under '{parent}': {reason}"
    )]
    WorkDir {
        service: String,
        parent: String,
        reason: String,
    },

    #[error("pg_dumpall backup of service '{service}' failed: {source}")]
    Sidecar {
        service: String,
        /// Boxed: the sidecar error carries several paths and would make
        /// every `Result` of this type large.
        #[source]
        source: Box<PgDumpallSidecarError>,
    },

    #[error("Failed to open the pg_dumpall backup of service '{service}' at '{path}' for upload: {reason}")]
    OpenDump {
        service: String,
        path: String,
        reason: String,
    },

    #[error("Failed to upload the pg_dumpall backup of service '{service}' to s3://{bucket}/{key}: {reason}")]
    Upload {
        service: String,
        bucket: String,
        key: String,
        reason: String,
    },
}

/// Why [`run_pg_dumpall_sidecar`] produced no dump. Every variant names the
/// sidecar container and the database host it dumped.
#[derive(Debug, thiserror::Error)]
pub(super) enum PgDumpallSidecarError {
    #[error(
        "Failed to create pg_dumpall backup container '{container}' from image '{image}' for \
         database host '{host}': {reason}"
    )]
    Create {
        container: String,
        image: String,
        host: String,
        reason: String,
    },

    #[error("Failed to start pg_dumpall backup container '{container}' for database host '{host}': {reason}")]
    Start {
        container: String,
        host: String,
        reason: String,
    },

    #[error(
        "pg_dumpall of database host '{host}' failed in backup container '{container}': \
         {reason}{diagnostics}"
    )]
    Export {
        container: String,
        host: String,
        reason: String,
        /// The tail of pg_dumpall's stderr, prefixed for display, or empty.
        diagnostics: String,
    },

    #[error(
        "pg_dumpall exported database host '{host}', but copying '{container_path}' out of \
         backup container '{container}' through the Docker API into '{host_path}' on the Temps \
         host failed: {reason}. Check that the Docker daemon is reachable and that the Temps \
         host has free space in its temporary directory, then retry"
    )]
    CopyOut {
        container: String,
        host: String,
        container_path: String,
        host_path: String,
        reason: String,
    },

    #[error(
        "pg_dumpall of database host '{host}' in backup container '{container}' produced an \
         empty dump"
    )]
    EmptyDump { container: String, host: String },
}

/// Run `pg_dumpall | gzip` in a one-shot sidecar and leave the dump in
/// `host_dir`, returning its path and size. The sidecar is removed on every
/// path.
///
/// The dump is written into the sidecar's own filesystem and copied out
/// through the Docker archive API, never through a bind mount: a bind source
/// on the Temps host only reaches the container when the Docker daemon sees
/// the host's filesystem, which it does not with Docker in a VM (Colima,
/// Docker Desktop) or on a remote host. There the dump landed inside the VM
/// and the backup failed with "Failed to stat backup file" (#1387). The copy
/// streams through a bounded channel, so memory does not grow with the dump.
pub(super) async fn run_pg_dumpall_sidecar(
    docker: &Docker,
    spec: &PgDumpallSidecar<'_>,
    host_dir: &std::path::Path,
) -> Result<(std::path::PathBuf, i64), PgDumpallSidecarError> {
    use bollard::models::ContainerCreateBody as Config;
    use bollard::query_parameters::RemoveContainerOptions;

    let sidecar_name = format!("temps-pg-backup-{}", uuid::Uuid::new_v4());
    let password_env = format!("PGPASSWORD={}", spec.password);
    let backup_filename = format!("{}.sql.gz", uuid::Uuid::new_v4());
    let stderr_filename = format!("{}.stderr", uuid::Uuid::new_v4());
    let host_backup_path = host_dir.join(&backup_filename);
    let host_stderr_path = host_dir.join(&stderr_filename);
    let container_backup_path = format!("/backup/{}", backup_filename);
    let container_stderr_path = format!("/backup/{}", stderr_filename);

    let sidecar_config = Config {
        image: Some(spec.image.to_string()),
        entrypoint: Some(vec!["/bin/sleep".to_string()]),
        cmd: Some(vec!["86400".to_string()]),
        env: Some(vec![password_env.clone()]),
        user: Some("root".to_string()),
        host_config: Some(bollard::models::HostConfig {
            oom_score_adj: Some(-500),
            ..Default::default()
        }),
        networking_config: Some(bollard::models::NetworkingConfig {
            endpoints_config: Some(std::collections::HashMap::from([(
                spec.network.to_string(),
                bollard::models::EndpointSettings {
                    ..Default::default()
                },
            )])),
        }),
        ..Default::default()
    };

    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&sidecar_name)
                    .build(),
            ),
            sidecar_config,
        )
        .await
        .map_err(|e| PgDumpallSidecarError::Create {
            container: sidecar_name.clone(),
            image: spec.image.to_string(),
            host: spec.host.to_string(),
            reason: e.to_string(),
        })?;

    let outcome = async {
        docker
            .start_container(
                &sidecar_name,
                Some(bollard::query_parameters::StartContainerOptionsBuilder::new().build()),
            )
            .await
            .map_err(|e| PgDumpallSidecarError::Start {
                container: sidecar_name.clone(),
                host: spec.host.to_string(),
                reason: e.to_string(),
            })?;

        // pg_dumpall dumps the entire cluster (all DBs, roles, tablespaces);
        // `--database` is just the bootstrap connection target.
        let pg_dump_shell_cmd = format!(
            "mkdir -p /backup && pg_dumpall --clean --if-exists --no-password --host={} --port={} --username={} --database={} 2>{} | gzip > {}",
            shell_escape(spec.host),
            shell_escape(spec.port),
            shell_escape(spec.username),
            shell_escape(spec.database),
            container_stderr_path,
            container_backup_path,
        );

        let exec_result = crate::externalsvc::exec_util::run_exec(
            docker,
            &sidecar_name,
            vec!["sh".into(), "-c".into(), pg_dump_shell_cmd],
            Some(vec![password_env.clone()]),
            BACKUP_EXEC_TIMEOUT,
        )
        .await;

        // pg_dumpall's stderr, for diagnostics. Best-effort; missing is fine.
        // Only its tail is read: a failing dump can write a lot of it.
        let stderr_from_file = match crate::externalsvc::container_download::copy_file_out_of_container(
            docker,
            &sidecar_name,
            &container_stderr_path,
            &host_stderr_path,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        {
            Ok(_) => crate::externalsvc::container_download::read_file_tail(&host_stderr_path, 4_000)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };

        if let Err(e) = exec_result {
            return Err(PgDumpallSidecarError::Export {
                container: sidecar_name.clone(),
                host: spec.host.to_string(),
                reason: e.to_string(),
                diagnostics: if stderr_from_file.is_empty() {
                    String::new()
                } else {
                    format!("\npg_dumpall stderr:\n{}", stderr_from_file)
                },
            });
        }
        if !stderr_from_file.is_empty() {
            debug!("pg_dumpall stderr for host '{}': {}", spec.host, stderr_from_file);
        }

        let size_bytes = crate::externalsvc::container_download::copy_file_out_of_container(
            docker,
            &sidecar_name,
            &container_backup_path,
            &host_backup_path,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .map_err(|failure| {
            let reason = match failure {
                crate::externalsvc::container_download::CopyOutFailure::Cancelled => "cancelled".to_string(),
                crate::externalsvc::container_download::CopyOutFailure::Failed(reason) => reason,
            };
            PgDumpallSidecarError::CopyOut {
                container: sidecar_name.clone(),
                host: spec.host.to_string(),
                container_path: container_backup_path.clone(),
                host_path: host_backup_path.display().to_string(),
                reason,
            }
        })? as i64;
        if size_bytes == 0 {
            return Err(PgDumpallSidecarError::EmptyDump {
                container: sidecar_name.clone(),
                host: spec.host.to_string(),
            });
        }
        Ok((host_backup_path.clone(), size_bytes))
    }
    .await;

    let _ = docker
        .remove_container(
            &sidecar_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::query_parameters::InspectContainerOptions;
    use bollard::Docker;
    use std::time::Duration;

    /// Every pg_dumpall sidecar failure names the sidecar container and the
    /// database host, so the log line is enough to find what failed.
    #[test]
    fn pg_dumpall_sidecar_errors_name_the_container_and_host() {
        let container = || "temps-pg-backup-7".to_string();
        let host = || "postgres-orders".to_string();
        for error in [
            PgDumpallSidecarError::Create {
                container: container(),
                image: "postgres:16".to_string(),
                host: host(),
                reason: "no such image".to_string(),
            },
            PgDumpallSidecarError::Start {
                container: container(),
                host: host(),
                reason: "port in use".to_string(),
            },
            PgDumpallSidecarError::Export {
                container: container(),
                host: host(),
                reason: "exit code 1".to_string(),
                diagnostics: "\npg_dumpall stderr:\nconnection refused".to_string(),
            },
            PgDumpallSidecarError::CopyOut {
                container: container(),
                host: host(),
                container_path: "/backup/x.sql.gz".to_string(),
                host_path: "/tmp/x.sql.gz".to_string(),
                reason: "the Docker archive download failed".to_string(),
            },
            PgDumpallSidecarError::EmptyDump {
                container: container(),
                host: host(),
            },
        ] {
            let message = error.to_string();
            assert!(message.contains("temps-pg-backup-7"), "{message}");
            assert!(message.contains("postgres-orders"), "{message}");
            // The service-level error keeps the sidecar error as its source
            // and names the service.
            let backup = PgDumpallBackupError::Sidecar {
                service: "orders".to_string(),
                source: Box::new(error),
            };
            let message = backup.to_string();
            assert!(message.contains("service 'orders'"), "{message}");
            assert!(message.contains("temps-pg-backup-7"), "{message}");
            assert!(std::error::Error::source(&backup).is_some());
        }
        for error in [
            PgDumpallBackupError::PullImage {
                service: "orders".to_string(),
                image: "postgres:16".to_string(),
                reason: "denied".to_string(),
            },
            PgDumpallBackupError::WorkDir {
                service: "orders".to_string(),
                parent: "/tmp".to_string(),
                reason: "read-only".to_string(),
            },
            PgDumpallBackupError::OpenDump {
                service: "orders".to_string(),
                path: "/tmp/x.sql.gz".to_string(),
                reason: "gone".to_string(),
            },
            PgDumpallBackupError::Upload {
                service: "orders".to_string(),
                bucket: "backups".to_string(),
                key: "orders/x.sql.gz".to_string(),
                reason: "denied".to_string(),
            },
        ] {
            let message = error.to_string();
            assert!(message.contains("service 'orders'"), "{message}");
        }
    }

    /// #1387: the pg_dumpall sidecar must deliver its dump to the Temps host
    /// even when the Docker daemon does not share the host's temporary
    /// directory (Docker in a VM such as Colima or Docker Desktop, or a
    /// remote daemon). The host directory here is a fresh `tempfile`
    /// directory, which such a daemon cannot see. Skips without Docker.
    #[tokio::test]
    async fn pg_dumpall_sidecar_delivers_the_dump_without_a_shared_temp_dir() {
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, RemoveContainerOptions, StartContainerOptionsBuilder,
        };
        const IMAGE: &str = "postgres:16-bookworm";
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        if docker.inspect_image(IMAGE).await.is_err()
            && crate::utils::pull_image_with_retry(&docker, IMAGE, None)
                .await
                .is_err()
        {
            println!("{IMAGE} not available, skipping");
            return;
        }

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("temps-test-pgdumpall-db-{}", &suffix[..12]);
        // Docker's default bridge: creating a network can fail on a host
        // whose address pools are used up, and the sidecar only needs to
        // reach the database by address.
        let network = "bridge";

        let outcome = async {
            docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(&database).build()),
                    bollard::models::ContainerCreateBody {
                        image: Some(IMAGE.to_string()),
                        env: Some(vec!["POSTGRES_PASSWORD=test-password".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| format!("create database: {e}"))?;
            docker
                .start_container(&database, Some(StartContainerOptionsBuilder::new().build()))
                .await
                .map_err(|e| format!("start database: {e}"))?;
            let mut ready = false;
            for _ in 0..60 {
                let probe = crate::externalsvc::exec_util::run_exec(
                    &docker,
                    &database,
                    vec![
                        "pg_isready".into(),
                        "-h".into(),
                        "127.0.0.1".into(),
                        "-U".into(),
                        "postgres".into(),
                    ],
                    None,
                    Duration::from_secs(10),
                )
                .await;
                if probe.is_ok_and(|result| result.exit_code == 0) {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if !ready {
                return Err("database did not become ready".to_string());
            }
            let address = docker
                .inspect_container(&database, None::<InspectContainerOptions>)
                .await
                .map_err(|e| format!("inspect database: {e}"))?
                .network_settings
                .and_then(|settings| settings.networks)
                .and_then(|networks| networks.get(network).cloned())
                .and_then(|endpoint| endpoint.ip_address)
                .filter(|ip| !ip.is_empty())
                .ok_or_else(|| "database has no address on the default bridge".to_string())?;

            let host_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
            let (dump, size) = run_pg_dumpall_sidecar(
                &docker,
                &PgDumpallSidecar {
                    image: IMAGE,
                    network,
                    host: &address,
                    port: POSTGRES_INTERNAL_PORT,
                    username: "postgres",
                    password: "test-password",
                    database: "postgres",
                },
                host_dir.path(),
            )
            .await
            .map_err(|e| e.to_string())?;

            let compressed = std::fs::read(&dump).map_err(|e| e.to_string())?;
            let mut sql = String::new();
            std::io::Read::read_to_string(
                &mut flate2::read::GzDecoder::new(compressed.as_slice()),
                &mut sql,
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>((size, compressed.len(), sql))
        }
        .await;

        let _ = docker
            .remove_container(
                &database,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;

        let (size, read, sql) = outcome.expect("pg_dumpall backup through the sidecar");
        assert_eq!(size as usize, read);
        assert!(sql.contains("PostgreSQL database cluster dump"), "{sql}");
    }
}
