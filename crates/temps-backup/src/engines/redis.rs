//! `RedisEngine`: in-process Redis backup using `redis-cli --rdb`,
//! implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Load the external-service row, decrypt its config to find the auth
//!    password (if any), and validate the S3 source.
//! 2. Run a one-shot `redis:7-alpine` sidecar over the user-defined bridge
//!    network. The sidecar issues `redis-cli -h redis-<name> --rdb
//!    /backup/<uuid>.rdb`, then `gzip` the resulting file.
//! 3. Upload the gzipped `.rdb.gz` to S3.
//! 4. Write the `metadata.json` companion.
//!
//! ## Notes
//!
//! - `redis-cli --rdb` triggers a `SYNC` and streams the RDB snapshot
//!   over the wire. This works against any Redis ≥ 2.8 and does not
//!   require WAL-G to be installed on the target.
//! - The v1 engine optionally used WAL-G when detected on the target
//!   container. That path is dropped from v2 — the `redis-cli --rdb`
//!   path is universally available and simpler. If WAL-G support is
//!   needed later, add a `--backup-tool=walg` param branch here.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::info;
use uuid::Uuid;

use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "redis";
const DUMP_FILE_SUFFIX: &str = "dump.rdb.gz";
const REDIS_SIDECAR_IMAGE: &str = "redis:7-alpine";

pub struct RedisDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct RedisEngine {
    deps: Arc<RedisDeps>,
}

impl RedisEngine {
    pub fn new(deps: RedisDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for RedisEngine {
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
            "redis-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let cfg: Value = serde_json::from_str(&config_json).unwrap_or_else(|_| json!({}));
        let password = cfg
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let backup_uuid = Uuid::new_v4().to_string();
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "redis",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );

        let target_container = format!("redis-{}", service.name);

        // ── One-shot redis-cli --rdb container ───────────────────────────────
        let backup_dir = std::env::temp_dir().join("temps-redis-backup");
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("failed to create tmpdir {}: {}", backup_dir.display(), e),
            })?;
        let rdb_filename = format!("{}.rdb", backup_uuid);
        let host_rdb_path = backup_dir.join(&rdb_filename);
        let host_rdb_gz_path = backup_dir.join(format!("{}.rdb.gz", backup_uuid));
        let container_rdb_path = format!("/backup/{}", rdb_filename);

        let auth_args = if password.is_empty() {
            String::new()
        } else {
            format!("-a {} ", v2_common::shell_escape(&password))
        };
        let dump_cmd = format!(
            "redis-cli {}-h {} --rdb {} && gzip {}",
            auth_args,
            v2_common::shell_escape(&target_container),
            v2_common::shell_escape(&container_rdb_path),
            v2_common::shell_escape(&container_rdb_path),
        );

        super::image_pull::ensure_image_pulled_v2(REDIS_SIDECAR_IMAGE, ENGINE_KEY).await?;

        let spec = OneShotSpec {
            image: REDIS_SIDECAR_IMAGE.to_string(),
            name: format!("temps-redis-backup-{}", backup_uuid),
            engine: ENGINE_KEY,
            backup_id,
            entrypoint: vec!["sh".to_string(), "-c".to_string()],
            cmd: vec![dump_cmd],
            env: vec![],
            binds: vec![format!("{}:/backup:rw", backup_dir.display())],
            network_mode: Some(temps_core::NETWORK_NAME.to_string()),
            user: Some("root".to_string()),
        };

        let result = match run_one_shot(&deps.docker, spec, &ctx.cancel).await {
            Ok(r) => r,
            Err(OneShotError::Cancelled) => return Err(BackupError::Cancelled),
            Err(e) => {
                v2_common::best_effort_remove(&host_rdb_path).await;
                v2_common::best_effort_remove(&host_rdb_gz_path).await;
                return Err(BackupError::Failed {
                    reason: format!("redis-cli --rdb one-shot failed: {}", e),
                });
            }
        };
        if result.exit_code != 0 {
            v2_common::best_effort_remove(&host_rdb_path).await;
            v2_common::best_effort_remove(&host_rdb_gz_path).await;
            return Err(BackupError::Failed {
                reason: format!(
                    "redis-cli --rdb exited with code {}. stderr: {}",
                    result.exit_code,
                    result.stderr_tail.trim(),
                ),
            });
        }

        let dump_meta =
            tokio::fs::metadata(&host_rdb_gz_path)
                .await
                .map_err(|e| BackupError::Failed {
                    reason: format!("gzipped RDB missing after redis-cli succeeded: {}", e),
                })?;
        if dump_meta.len() == 0 {
            v2_common::best_effort_remove(&host_rdb_gz_path).await;
            return Err(BackupError::Failed {
                reason: "redis-cli produced an empty RDB".into(),
            });
        }
        let file_size = dump_meta.len() as i64;
        let host_dump_path_str = host_rdb_gz_path.to_str().unwrap_or("").to_string();

        if ctx.cancel.is_cancelled() {
            v2_common::best_effort_remove(&host_rdb_gz_path).await;
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
        v2_common::best_effort_remove(&host_rdb_gz_path).await;

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
                "backup_tool": "redis-cli-rdb",
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = file_size,
            "RedisEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}
