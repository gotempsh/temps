//! `MariadbPhysicalEngine`: physical (`mariadb-backup`) base backup of an
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
//! 3. `docker exec mariadb-backup --backup --stream=mbstream | gzip` inside the
//!    running container, streaming the gzipped stream to a host temp file.
//!    Credentials travel via `MYSQL_PWD` env — never argv (PR #149).
//! 4. Verify success via the `"completed OK!"` stderr marker (the container's
//!    `/bin/sh` is dash, which has no `pipefail`, so the pipeline exit code is
//!    gzip's, not `mariadb-backup`'s).
//! 5. Parse the binlog position from stderr.
//! 6. Upload the `.mbstream.gz` to S3 and write `metadata.json` with the coords.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use super::mariadb_exec::{exec_stream_stdout_to_file, parse_binlog_position};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "mariadb_physical";
const BASE_FILE_SUFFIX: &str = "base.mbstream.gz";

/// In-container shell that streams a physical base backup to stdout and gzips
/// it. Credentials are NOT present — `--user=root` relies on `MYSQL_PWD` from
/// the exec env (libmariadb reads it). Keep it that way (PR #149).
///
/// `--target-dir` is a scratch dir mariadb-backup needs even when streaming.
/// Success is asserted via the `"completed OK!"` stderr marker, since the
/// pipe-to-gzip masks mariadb-backup's own exit code under dash.
///
/// CRITICAL: the `| gzip` must bind to the `mariadb-backup` command, NOT a
/// trailing statement. In `sh`, `a; b; c | gzip` pipes only `c` to gzip - so
/// the scratch-dir cleanup runs *before* the backup (rm-then-mkdir) and the
/// command ends with the gzip pipeline, making stdout the gzipped mbstream.
const PHYSICAL_SHELL: &str = "if command -v mariadb-backup >/dev/null 2>&1; then BK=mariadb-backup; else BK=mariabackup; fi; \
     rm -rf /var/tmp/temps-mariadb-backup; mkdir -p /var/tmp/temps-mariadb-backup; \
     \"$BK\" --backup --stream=mbstream --target-dir=/var/tmp/temps-mariadb-backup --user=root --host=localhost | gzip";

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

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "mariadb-physical-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = Uuid::new_v4().to_string();
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "mariadb",
            &service.name,
            &backup_uuid,
            BASE_FILE_SUFFIX,
        );

        info!(
            backup_id,
            service_id,
            s3_key = %s3_key,
            "MariadbPhysicalEngine: starting physical base backup",
        );

        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let root_password = root_password_from_config(&config_json);

        let container_name = format!("mariadb-{}", service.name);
        let backup_dir = std::env::temp_dir().join("temps-mariadb-backup");
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!(
                    "failed to create backup tmpdir {}: {}",
                    backup_dir.display(),
                    e
                ),
            })?;
        let host_path = backup_dir.join(format!("{}.mbstream.gz", backup_uuid));

        // Credentials via env only (MYSQL_PWD / MARIADB_PWD) — never argv.
        let env = vec![
            format!("MYSQL_PWD={}", root_password),
            format!("MARIADB_PWD={}", root_password),
        ];

        let exec = match exec_stream_stdout_to_file(
            &deps.docker,
            &container_name,
            // PHYSICAL_SHELL already ends with `| gzip` on the backup command.
            PHYSICAL_SHELL,
            &env,
            &host_path,
            &ctx.cancel,
        )
        .await
        {
            Ok(e) => e,
            Err(BackupError::Cancelled) => {
                v2_common::best_effort_remove(&host_path).await;
                return Err(BackupError::Cancelled);
            }
            Err(e) => {
                v2_common::best_effort_remove(&host_path).await;
                return Err(e);
            }
        };

        // dash has no pipefail, so the pipeline exit code is gzip's. Assert
        // mariadb-backup success via its terminal stderr marker instead.
        if !exec.stderr.contains("completed OK!") {
            v2_common::best_effort_remove(&host_path).await;
            return Err(BackupError::Failed {
                reason: format!(
                    "mariadb-backup did not report success (no 'completed OK!'). \
                     pipeline exit={}. stderr tail: {}",
                    exec.exit_code,
                    stderr_tail(&exec.stderr),
                ),
            });
        }

        let meta = tokio::fs::metadata(&host_path)
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("base file missing at {}: {}", host_path.display(), e),
            })?;
        if meta.len() == 0 {
            v2_common::best_effort_remove(&host_path).await;
            return Err(BackupError::Failed {
                reason: "mariadb-backup produced an empty stream".into(),
            });
        }
        let file_size = meta.len() as i64;
        let host_path_str = host_path.to_str().unwrap_or("").to_string();

        // Binlog coordinates anchor PITR replay. Absence means binary logging
        // is off on the source — the base is still a valid full backup, but
        // PITR will not be possible until binlog archiving is enabled.
        let coord = parse_binlog_position(&exec.stderr);
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

        if ctx.cancel.is_cancelled() {
            v2_common::best_effort_remove(&host_path).await;
            return Err(BackupError::Cancelled);
        }
        let tags = v2_common::BackupTags::load_for_backup(&ctx.db, ctx.backup_id).await;
        v2_common::upload_file(
            &s3_client,
            &s3_source.bucket_name,
            &s3_key,
            &host_path_str,
            "application/x-gzip",
            file_size,
            Some(&tags),
        )
        .await?;
        v2_common::best_effort_remove(&host_path).await;

        let metadata_key = v2_common::derive_metadata_key(&s3_key);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &s3_key,
            file_size,
            s3_source_id,
            "gzip",
            Some(json!({
                "backup_tool": "mariadb-backup",
                "stream_format": "mbstream",
                "pitr": coord.is_some(),
                "binlog_file": coord.as_ref().map(|c| c.file.clone()).unwrap_or_default(),
                "binlog_position": coord.as_ref().map(|c| c.position.clone()).unwrap_or_default(),
                "gtid": coord.as_ref().map(|c| c.gtid.clone()).unwrap_or_default(),
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            key = %s3_key,
            size_bytes = file_size,
            pitr = coord.is_some(),
            "MariadbPhysicalEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
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

fn root_password_from_config(config_json: &str) -> String {
    let params: Value = serde_json::from_str(config_json).unwrap_or_else(|_| json!({}));
    params
        .get("root_password")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #149 invariant: the base-backup shell must not contain credentials.
    /// Connection auth flows through `MYSQL_PWD` in the exec env.
    #[test]
    fn physical_shell_contains_no_credentials() {
        // PHYSICAL_SHELL is a const with no interpolation — guard against a
        // future refactor hardcoding a password flag. (We can't assert
        // `!contains("-p")` because `mkdir -p` is a legitimate, benign use.)
        assert!(!PHYSICAL_SHELL.contains("MYSQL_PWD"));
        assert!(!PHYSICAL_SHELL.contains("--password"));
        // No mysql-style short password flag (`-p<value>` / `-p'...'`).
        assert!(!PHYSICAL_SHELL.contains("-p'"));
        assert!(!PHYSICAL_SHELL.contains("-p\""));
        assert!(PHYSICAL_SHELL.contains("--user=root"));
        assert!(PHYSICAL_SHELL.contains("--stream=mbstream"));
    }

    #[test]
    fn stderr_tail_truncates_long_output() {
        let long = "x".repeat(5000);
        let tail = stderr_tail(&long);
        assert!(tail.starts_with('…'));
        assert!(tail.len() < 5000);
    }
}
