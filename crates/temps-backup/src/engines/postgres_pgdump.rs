//! `PostgresPgDumpEngine`: in-process `pg_dump`-based backup of an external
//! Postgres service, implemented against `engine_v2::BackupEngine`.
//!
//! Used as the fallback when WAL-G is not available on the target service.
//! For WAL-G see `postgres_walg.rs`; for clustered targets see
//! `postgres_cluster.rs`.
//!
//! ## Flow
//!
//! 1. Load + decrypt the external-service row to recover Postgres connection
//!    params + the sidecar image tag the user configured.
//! 2. Validate the configured S3 source.
//! 3. Run `pg_dumpall | gzip` in a one-shot sidecar container attached to
//!    the temps-app bridge network so it can reach the target Postgres
//!    container at `postgres-<service_name>:5432`.
//! 4. Upload the resulting `.sql.gz` to S3.
//! 5. Write the `metadata.json` companion.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::info;

use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "postgres_pgdump";
const DUMP_FILE_SUFFIX: &str = "dump.sql.gz";

pub struct PostgresPgDumpDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct PostgresPgDumpEngine {
    deps: Arc<PostgresPgDumpDeps>,
}

impl PostgresPgDumpEngine {
    pub fn new(deps: PostgresPgDumpDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for PostgresPgDumpEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        // ── Params + service + S3 source ─────────────────────────────────────
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
            "postgres-pgdump-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "postgres",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );

        info!(
            backup_id,
            service_id,
            s3_key = %s3_key,
            bucket = %s3_source.bucket_name,
            "PostgresPgDumpEngine: starting dump",
        );

        // ── Decode service config ────────────────────────────────────────────
        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let pg = load_postgres_params(&config_json);

        // ── One-shot pg_dumpall container ────────────────────────────────────
        let backup_dir = std::env::temp_dir().join("temps-extpg-backup");
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!(
                    "failed to create backup tmpdir {}: {}",
                    backup_dir.display(),
                    e
                ),
            })?;
        let dump_filename = format!("{}.sql.gz", backup_uuid);
        let host_dump_path = backup_dir.join(&dump_filename);
        let container_dump_path = format!("/backup/{}", dump_filename);
        let uncompressed = container_dump_path
            .strip_suffix(".gz")
            .unwrap_or(&container_dump_path)
            .to_string();
        let stderr_filename = format!("{}.stderr", backup_uuid);
        let stderr_in_container = format!("/backup/{}", stderr_filename);
        let host_stderr_path = backup_dir.join(&stderr_filename);

        let db_container = format!("postgres-{}", service.name);
        let dump_cmd = format!(
            "pg_dumpall --clean --if-exists --no-password \
             --host={} --port=5432 --username={} --database={} \
             2>{} > {} && gzip {}",
            v2_common::shell_escape(&db_container),
            v2_common::shell_escape(&pg.username),
            v2_common::shell_escape(&pg.database),
            stderr_in_container,
            v2_common::shell_escape(&uncompressed),
            v2_common::shell_escape(&uncompressed),
        );

        super::image_pull::ensure_image_pulled_v2(&pg.docker_image, ENGINE_KEY).await?;

        let spec = OneShotSpec {
            image: pg.docker_image.clone(),
            name: format!("temps-pgdump-{}", backup_uuid),
            engine: ENGINE_KEY,
            backup_id,
            entrypoint: vec!["sh".to_string(), "-c".to_string()],
            cmd: vec![dump_cmd],
            env: vec![format!("PGPASSWORD={}", pg.password)],
            binds: vec![format!("{}:/backup:rw", backup_dir.display())],
            // Same user-defined bridge the target Postgres container is on so
            // `postgres-{service_name}` resolves.
            network_mode: Some(temps_core::NETWORK_NAME.to_string()),
            user: Some("root".to_string()),
        };

        let result = match run_one_shot(&deps.docker, spec, &ctx.cancel).await {
            Ok(r) => r,
            Err(OneShotError::Cancelled) => return Err(BackupError::Cancelled),
            Err(e) => {
                v2_common::best_effort_remove(&host_dump_path).await;
                v2_common::best_effort_remove(&host_stderr_path).await;
                return Err(BackupError::Failed {
                    reason: format!("pg_dumpall one-shot failed: {}", e),
                });
            }
        };
        if result.exit_code != 0 {
            let file_stderr = tokio::fs::read(&host_stderr_path).await.unwrap_or_default();
            v2_common::best_effort_remove(&host_stderr_path).await;
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Failed {
                reason: format!(
                    "pg_dumpall exited with code {}. file-stderr: {}. container-stderr: {}",
                    result.exit_code,
                    String::from_utf8_lossy(&file_stderr),
                    result.stderr_tail.trim(),
                ),
            });
        }
        v2_common::best_effort_remove(&host_stderr_path).await;

        let dump_meta =
            tokio::fs::metadata(&host_dump_path)
                .await
                .map_err(|e| BackupError::Failed {
                    reason: format!(
                        "dump file not found at {} after pg_dumpall exited 0: {}",
                        host_dump_path.display(),
                        e
                    ),
                })?;
        if dump_meta.len() == 0 {
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Failed {
                reason: "pg_dumpall produced an empty file".into(),
            });
        }
        let file_size = dump_meta.len() as i64;
        let host_dump_path_str = host_dump_path.to_str().unwrap_or("").to_string();

        // ── Upload ───────────────────────────────────────────────────────────
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

        // ── Metadata ─────────────────────────────────────────────────────────
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
                "service": {
                    "id": service_id,
                    "name": service.name,
                },
            })),
        )
        .await?;

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = file_size,
            "PostgresPgDumpEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

// ── Local helpers ────────────────────────────────────────────────────────────

struct PgParams {
    username: String,
    password: String,
    database: String,
    docker_image: String,
}

fn load_postgres_params(config_json: &str) -> PgParams {
    let params: Value = serde_json::from_str(config_json).unwrap_or_else(|_| json!({}));
    PgParams {
        username: params
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("postgres")
            .to_string(),
        password: params
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        database: params
            .get("database")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("db_name").and_then(|v| v.as_str()))
            .unwrap_or("postgres")
            .to_string(),
        docker_image: params
            .get("docker_image")
            .and_then(|v| v.as_str())
            .unwrap_or("gotempsh/postgres-walg:18-bookworm")
            .to_string(),
    }
}
