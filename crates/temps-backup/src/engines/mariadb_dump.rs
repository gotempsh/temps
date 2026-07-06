//! `MariadbDumpEngine`: logical (`mariadb-dump`) backup of an external MariaDB
//! service, implemented against `engine_v2::BackupEngine`.
//!
//! This is the **fallback** engine (no PITR), the MariaDB analog of
//! `postgres_pgdump`. The preferred PITR path is `mariadb_physical`
//! (physical `mariadb-backup` base + binary-log archiving). Dispatch
//! (`dispatch::resolve_engine_key`) selects this engine only when the
//! physical-backup prerequisites are absent.
//!
//! ## Flow
//! 1. Load + decrypt the external-service row for the root password + image.
//! 2. Validate the configured S3 source.
//! 3. `docker exec` `mariadb-dump --databases ... --single-transaction | gzip`
//!    inside the running container, streaming the gzipped stdout to a host
//!    temp file. Credentials travel via `MYSQL_PWD` env — never argv (PR #149).
//! 4. Upload the `.sql.gz` to S3.
//! 5. Write the `metadata.json` companion.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{debug, info};
use uuid::Uuid;

use super::mariadb_exec::exec_stream_stdout_to_file;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "mariadb_dump";
const DUMP_FILE_SUFFIX: &str = "dump.sql.gz";

/// In-container shell that dumps all user databases and gzips the result.
/// Credentials are NOT present here — `-uroot` relies on `MYSQL_PWD` from the
/// exec env. Keep it that way (PR #149).
const DUMP_SHELL: &str = "if command -v mariadb-dump >/dev/null 2>&1; then dump=mariadb-dump; else dump=mysqldump; fi; \
     if command -v mariadb >/dev/null 2>&1; then client=mariadb; else client=mysql; fi; \
     dbs=$($client -N -B -uroot -e \"SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME NOT IN ('information_schema','mysql','performance_schema','sys') ORDER BY SCHEMA_NAME\"); \
     if [ -z \"$dbs\" ]; then echo '-- No user databases to dump'; exit 0; fi; \
     $dump --databases $dbs --single-transaction --quick -uroot | gzip";

pub struct MariadbDumpDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct MariadbDumpEngine {
    deps: Arc<MariadbDumpDeps>,
}

impl MariadbDumpEngine {
    pub fn new(deps: MariadbDumpDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MariadbDumpEngine {
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
            "mariadb-dump-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = Uuid::new_v4().to_string();
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "mariadb",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );

        info!(
            backup_id,
            service_id,
            s3_key = %s3_key,
            "MariadbDumpEngine: starting logical dump",
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
        let host_dump_path = backup_dir.join(format!("{}.sql.gz", backup_uuid));

        // Credentials via env only (MYSQL_PWD / MARIADB_PWD) — never argv.
        let env = vec![
            format!("MYSQL_PWD={}", root_password),
            format!("MARIADB_PWD={}", root_password),
        ];

        let result = exec_stream_stdout_to_file(
            &deps.docker,
            &container_name,
            DUMP_SHELL,
            &env,
            &host_dump_path,
            &ctx.cancel,
        )
        .await;

        let exec = match result {
            Ok(e) => e,
            Err(BackupError::Cancelled) => {
                v2_common::best_effort_remove(&host_dump_path).await;
                return Err(BackupError::Cancelled);
            }
            Err(e) => {
                v2_common::best_effort_remove(&host_dump_path).await;
                return Err(e);
            }
        };
        if exec.exit_code != 0 {
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Failed {
                reason: format!(
                    "mariadb-dump exited with code {}. stderr: {}",
                    exec.exit_code,
                    exec.stderr.trim()
                ),
            });
        }
        if !exec.stderr.trim().is_empty() {
            debug!(backup_id, "mariadb-dump stderr: {}", exec.stderr.trim());
        }

        let dump_meta =
            tokio::fs::metadata(&host_dump_path)
                .await
                .map_err(|e| BackupError::Failed {
                    reason: format!(
                        "dump file missing at {} after exit 0: {}",
                        host_dump_path.display(),
                        e
                    ),
                })?;
        if dump_meta.len() == 0 {
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Failed {
                reason: "mariadb-dump produced an empty file".into(),
            });
        }
        let file_size = dump_meta.len() as i64;
        let host_dump_path_str = host_dump_path.to_str().unwrap_or("").to_string();

        if ctx.cancel.is_cancelled() {
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Cancelled);
        }
        let tags = v2_common::BackupTags::load_for_backup(&ctx.db, ctx.backup_id).await;
        v2_common::upload_file(
            &s3_client,
            &s3_source.bucket_name,
            &s3_key,
            &host_dump_path_str,
            "application/x-gzip",
            file_size,
            Some(&tags),
        )
        .await?;
        v2_common::best_effort_remove(&host_dump_path).await;

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
                "backup_tool": "mariadb-dump",
                "pitr": false,
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            key = %s3_key,
            size_bytes = file_size,
            "MariadbDumpEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

/// Extract the root password from the decrypted service config JSON.
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

    /// PR #149 invariant: the dump shell must not contain the password.
    /// Credentials travel via the exec env (`MYSQL_PWD`), so a password
    /// containing shell metacharacters can never break out of `sh -c`.
    #[test]
    fn dump_shell_contains_no_credentials() {
        assert!(!DUMP_SHELL.contains("MYSQL_PWD"));
        assert!(!DUMP_SHELL.contains("password"));
        // Connects as root via env-provided password, no password flag.
        assert!(DUMP_SHELL.contains("-uroot"));
        assert!(!DUMP_SHELL.contains("--password"));
        assert!(!DUMP_SHELL.contains("-p'"));
        assert!(!DUMP_SHELL.contains("-p\""));
    }

    #[test]
    fn root_password_parsed_from_config() {
        assert_eq!(
            root_password_from_config(r#"{"root_password":"s3cr3t"}"#),
            "s3cr3t"
        );
        assert_eq!(root_password_from_config("{}"), "");
        assert_eq!(root_password_from_config("not json"), "");
    }
}
