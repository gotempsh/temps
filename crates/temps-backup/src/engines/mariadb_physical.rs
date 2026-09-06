// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `MariadbPhysicalEngine`: WAL-G repository containing a physical
//! (`mariadb-backup`) base backup of an
//! external MariaDB service, implemented against `engine_v2::BackupEngine`.
//!
//! This is the **PITR** engine — the MariaDB analog of `postgres_walg`.
//! MariaDB has no turnkey continuous archiver (WAL-G drives MariaDB but does
//! not support automatic PITR for it), so PITR here is the standard,
//! MariaDB-documented approach:
//!
//!   physical base backup  +  archived binary logs  +  `mariadb-binlog` replay
//!
//! This engine owns the **base backup** half: it streams a `mariadb-backup`
//! physical snapshot to S3 and records the binlog coordinates
//! (`file`/`position`/`gtid`) at backup time into the `metadata.json`
//! companion. Those coordinates are the replay start for restore. The
//! continuous **binary-log archiving** half lives in `temps-providers`
//! (per-service background task) and ships closed binlog segments to the same
//! S3 prefix.
//!
//! ## Flow
//! 1. Load + decrypt the external-service row for the root password.
//! 2. Validate the configured S3 source.
//! 3. `docker exec wal-g backup-push` inside the running container. WAL-G runs
//!    `mariadb-backup --stream=mbstream` and uploads the stream directly to S3.
//!    No database-sized host file is created.
//! 4. Parse the binlog position from the bounded WAL-G/mariadb-backup output.
//! 5. Write `metadata.json` with the coordinates and exact backup identity.

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::dispatch::service_container_name;
use super::mariadb_exec::parse_binlog_position;
use super::postgres_walg::run_walg_exec;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "mariadb_physical";
const WALG_STREAM_CREATE_COMMAND: &str = "sh -ceu 'if command -v mariadb-backup >/dev/null 2>&1; then BK=mariadb-backup; else BK=mariabackup; fi; rm -rf /var/tmp/temps-mariadb-backup; mkdir -p /var/tmp/temps-mariadb-backup; exec \"$BK\" --backup --stream=mbstream --target-dir=/var/tmp/temps-mariadb-backup --user=root --host=127.0.0.1'";
const WALG_STREAM_RESTORE_COMMAND: &str = "mbstream -x -C /data";
const WALG_PREPARE_COMMAND: &str = "mariadb-backup --prepare --target-dir=/data";

pub struct MariadbPhysicalDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct MariadbPhysicalEngine {
    deps: Arc<MariadbPhysicalDeps>,
}

impl MariadbPhysicalEngine {
    pub fn new(deps: MariadbPhysicalDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MariadbPhysicalEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;

        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        temps_providers::continuous_archive::ensure_continuous_archive_source_pin(
            deps.db.as_ref(),
            &service,
            s3_source_id,
            "MariaDB binlog archiving",
        )
        .await
        .map_err(|e| match e {
            temps_providers::continuous_archive::ContinuousArchiveError::Mismatch(reason) => {
                BackupError::PermanentFailure { reason }
            }
            temps_providers::continuous_archive::ContinuousArchiveError::Database(reason) => {
                BackupError::Failed { reason }
            }
        })?;

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "mariadb-physical-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let bucket_path = s3_source.bucket_path.trim_matches('/');
        let service_root = format!("external_services/mariadb/{}/walg", service.name);
        let repository_key = if bucket_path.is_empty() {
            service_root
        } else {
            format!("{bucket_path}/{service_root}")
        };
        let walg_prefix = format!("s3://{}/{}", s3_source.bucket_name, repository_key);
        let list_prefix = format!("{repository_key}/");

        info!(
            backup_id,
            service_id,
            repository = %walg_prefix,
            "MariadbPhysicalEngine: starting direct WAL-G physical base backup",
        );

        let config_json = decrypt_service_config(
            &deps.encryption_service,
            service_id,
            service.config.as_deref(),
        )?;
        let root_password = root_password_from_config(service_id, &config_json)?;

        let container_name = service_container_name(&service);
        let access_key = deps
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|error| BackupError::PermanentFailure {
                reason: format!("decrypt MariaDB WAL-G access key: {error}"),
            })?;
        let secret_key = deps
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|error| BackupError::PermanentFailure {
                reason: format!("decrypt MariaDB WAL-G secret key: {error}"),
            })?;
        let session_token = v2_common::decrypt_session_token(&s3_source, &deps.encryption_service)?;
        let container_endpoint = temps_providers::externalsvc::S3Credentials {
            access_key_id: access_key.clone(),
            secret_key: secret_key.clone(),
            session_token: session_token.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        }
        .resolve_endpoint_for_container(&deps.docker, &container_name)
        .await;
        let mut env = build_walg_env(
            &walg_prefix,
            &s3_source.region,
            &access_key,
            &secret_key,
            session_token.as_deref(),
            &root_password,
            &backup_uuid,
        );
        if let Some(endpoint) = container_endpoint {
            env.push(format!(
                "AWS_ENDPOINT={}",
                if endpoint.starts_with("http") {
                    endpoint
                } else {
                    format!("http://{endpoint}")
                }
            ));
        }
        if s3_source.force_path_style.unwrap_or(true) {
            env.push("AWS_S3_FORCE_PATH_STYLE=true".into());
        }

        let exec = run_walg_exec(
            &deps.docker,
            &container_name,
            "wal-g backup-push",
            &env,
            &ctx.cancel,
        )
        .await?;
        if exec.exit_code != 0 {
            return Err(BackupError::Failed {
                reason: format!(
                    "MariaDB wal-g backup-push exited with code {}. stderr: {}",
                    exec.exit_code,
                    stderr_tail(&exec.stderr),
                ),
            });
        }
        let file_size =
            list_total_s3_size(&s3_client, &s3_source.bucket_name, &list_prefix).await?;
        if file_size <= 0 {
            return Err(BackupError::Failed {
                reason: "MariaDB WAL-G repository contains no backup bytes after backup-push"
                    .into(),
            });
        }

        // Binlog coordinates anchor PITR replay. Absence means binary logging
        // is off on the source — the base is still a valid full backup, but
        // PITR will not be possible until binlog archiving is enabled.
        let coord = parse_binlog_position(&format!("{}\n{}", exec.stdout, exec.stderr));
        match &coord {
            Some(c) => info!(
                backup_id,
                binlog_file = %c.file,
                binlog_position = %c.position,
                gtid = %c.gtid,
                "MariadbPhysicalEngine: captured binlog coordinates",
            ),
            None => warn!(
                backup_id,
                "MariadbPhysicalEngine: no binlog position in mariadb-backup output \
                 (binary logging disabled on source?) — PITR will be unavailable for this base",
            ),
        }

        let metadata_key = format!("{list_prefix}{backup_uuid}.metadata.json");
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &walg_prefix,
            file_size,
            s3_source_id,
            "wal-g-native",
            Some(json!({
                "backup_tool": "wal-g+mariadb-backup",
                "stream_format": "mbstream",
                "pitr": coord.is_some(),
                "binlog_file": coord.as_ref().map(|c| c.file.clone()).unwrap_or_default(),
                "binlog_position": coord.as_ref().map(|c| c.position.clone()).unwrap_or_default(),
                "gtid": coord.as_ref().map(|c| c.gtid.clone()).unwrap_or_default(),
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;
        v2_common::record_walg_identity(deps.db.as_ref(), backup_id, &backup_uuid).await?;

        info!(
            backup_id,
            repository = %walg_prefix,
            size_bytes = file_size,
            pitr = coord.is_some(),
            "MariadbPhysicalEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: walg_prefix,
            size_bytes: Some(file_size),
            compression: "wal-g-native".to_string(),
        })
    }
}

fn stderr_tail(stderr: &str) -> String {
    const TAIL: usize = 2000;
    let trimmed = stderr.trim();
    if trimmed.len() <= TAIL {
        return trimmed.to_string();
    }
    let start = trimmed.len() - TAIL;
    format!("…{}", &trimmed[start..])
}

fn decrypt_service_config(
    encryption: &temps_core::EncryptionService,
    service_id: i32,
    encrypted_config: Option<&str>,
) -> Result<String, BackupError> {
    let encrypted_config = encrypted_config.ok_or_else(|| BackupError::PermanentFailure {
        reason: format!("MariaDB service {service_id} has no encrypted configuration"),
    })?;
    encryption
        .decrypt_string(encrypted_config)
        .map_err(|error| BackupError::PermanentFailure {
            reason: format!("decrypt configuration for MariaDB service {service_id}: {error}"),
        })
}

fn root_password_from_config(service_id: i32, config_json: &str) -> Result<String, BackupError> {
    let params: Value =
        serde_json::from_str(config_json).map_err(|error| BackupError::PermanentFailure {
            reason: format!(
                "parse decrypted configuration for MariaDB service {service_id}: {error}"
            ),
        })?;
    params
        .get("root_password")
        .and_then(|v| v.as_str())
        .filter(|password| !password.is_empty())
        .map(str::to_string)
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!(
                "decrypted configuration for MariaDB service {service_id} has no root_password"
            ),
        })
}

fn build_walg_env(
    prefix: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
    root_password: &str,
    backup_uuid: &str,
) -> Vec<String> {
    let mut env = vec![
        format!("WALG_S3_PREFIX={prefix}"),
        format!("AWS_ACCESS_KEY_ID={access_key}"),
        format!("AWS_SECRET_ACCESS_KEY={secret_key}"),
        format!("AWS_REGION={region}"),
        format!("MYSQL_PWD={root_password}"),
        format!("MARIADB_PWD={root_password}"),
        format!("WALG_MYSQL_DATASOURCE_NAME=root:{root_password}@tcp(127.0.0.1:3306)/mysql"),
        format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
        format!("WALG_STREAM_RESTORE_COMMAND={WALG_STREAM_RESTORE_COMMAND}"),
        format!("WALG_MYSQL_BACKUP_PREPARE_COMMAND={WALG_PREPARE_COMMAND}"),
        "WALG_UPLOAD_CONCURRENCY=4".into(),
        "WALG_UPLOAD_DISK_CONCURRENCY=1".into(),
        "WALG_UPLOAD_QUEUE=2".into(),
        "WALG_TAR_SIZE_THRESHOLD=134217728".into(),
    ];
    // Only a temporary (STS-style) credential contributes an
    // `AWS_SESSION_TOKEN`; for a long-lived one the variable is absent, not
    // empty, so the container environment is unchanged.
    env.extend(temps_providers::externalsvc::aws_session_token_env(
        session_token,
    ));
    env.extend(v2_common::walg_identity_env(backup_uuid));
    env
}

async fn list_total_s3_size(
    client: &S3Client,
    bucket: &str,
    prefix: &str,
) -> Result<i64, BackupError> {
    let mut total = 0_i64;
    let mut continuation = None;
    loop {
        let mut request = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(token) = continuation {
            request = request.continuation_token(token);
        }
        let response = request.send().await.map_err(|error| BackupError::Failed {
            reason: format!("list MariaDB WAL-G repository {prefix}: {error}"),
        })?;
        for object in response.contents() {
            total = total
                .checked_add(object.size().unwrap_or(0))
                .ok_or_else(|| BackupError::Failed {
                    reason: format!("MariaDB WAL-G repository {prefix} size overflowed i64"),
                })?;
        }
        if response.is_truncated().unwrap_or(false) {
            continuation = response.next_continuation_token().map(str::to_owned);
        } else {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #149 invariant: the base-backup shell must not contain credentials.
    /// Connection auth flows through `MYSQL_PWD` in the exec env.
    #[test]
    fn walg_stream_command_contains_no_credentials() {
        // The stream command is a const with no interpolation — guard against a
        // future refactor hardcoding a password flag. (We can't assert
        // `!contains("-p")` because `mkdir -p` is a legitimate, benign use.)
        assert!(!WALG_STREAM_CREATE_COMMAND.contains("MYSQL_PWD"));
        assert!(!WALG_STREAM_CREATE_COMMAND.contains("--password"));
        // No mysql-style short password flag (`-p<value>` / `-p'...'`).
        assert!(!WALG_STREAM_CREATE_COMMAND.contains("-p'"));
        assert!(!WALG_STREAM_CREATE_COMMAND.contains("-p\""));
        assert!(WALG_STREAM_CREATE_COMMAND.contains("--user=root"));
        assert!(WALG_STREAM_CREATE_COMMAND.contains("--stream=mbstream"));
    }

    #[test]
    fn walg_env_keeps_password_out_of_commands() {
        let password = "p4ss-word";
        let env = build_walg_env(
            "s3://bucket/path",
            "auto",
            "access",
            "secret",
            None,
            password,
            "id",
        );
        let stream = env
            .iter()
            .find(|value| value.starts_with("WALG_STREAM_CREATE_COMMAND="))
            .unwrap();
        assert!(!stream.contains(password));
        assert!(env
            .iter()
            .any(|value| value == &format!("MYSQL_PWD={password}")));
        assert!(env.iter().any(|value| value.contains("root:p4ss-word@tcp")));
    }

    /// The zero-change guarantee for an operator-configured S3 source: the
    /// `mariabackup`/WAL-G container gets no `AWS_SESSION_TOKEN` at all.
    #[test]
    fn walg_env_omits_the_session_token_for_a_long_lived_credential() {
        let env = build_walg_env(
            "s3://bucket/path",
            "auto",
            "access",
            "secret",
            None,
            "password",
            "id",
        );
        assert!(!env
            .iter()
            .any(|value| value.starts_with("AWS_SESSION_TOKEN")));
    }

    #[test]
    fn walg_env_carries_a_session_token_for_a_temporary_credential() {
        let env = build_walg_env(
            "s3://bucket/path",
            "auto",
            "access",
            "secret",
            Some("sts-session-token"),
            "password",
            "id",
        );
        assert!(env
            .iter()
            .any(|value| value == "AWS_SESSION_TOKEN=sts-session-token"));
        // The other two must still be there, in the same form as before.
        assert!(env.iter().any(|value| value == "AWS_ACCESS_KEY_ID=access"));
        assert!(env
            .iter()
            .any(|value| value == "AWS_SECRET_ACCESS_KEY=secret"));
    }

    #[test]
    fn stderr_tail_truncates_long_output() {
        let long = "x".repeat(5000);
        let tail = stderr_tail(&long);
        assert!(tail.starts_with('…'));
        assert!(tail.len() < 5000);
    }

    #[test]
    fn wrong_encryption_key_is_a_contextual_permanent_failure() {
        let original =
            temps_core::EncryptionService::new_from_password("original-mariadb-config-key");
        let wrong =
            temps_core::EncryptionService::new_from_password("different-mariadb-config-key");
        let encrypted = original
            .encrypt_string(r#"{"root_password":"secret"}"#)
            .unwrap();

        let error = decrypt_service_config(&wrong, 42, Some(&encrypted)).unwrap_err();
        match error {
            BackupError::PermanentFailure { reason } => {
                assert!(reason.contains("MariaDB service 42"));
                assert!(reason.contains("decrypt configuration"));
            }
            other => panic!("expected permanent failure, got {other:?}"),
        }
    }

    #[test]
    fn invalid_decrypted_config_is_not_treated_as_an_empty_object() {
        let error = root_password_from_config(73, "not-json").unwrap_err();
        assert!(matches!(error, BackupError::PermanentFailure { .. }));
        assert!(error.to_string().contains("MariaDB service 73"));
    }
}
