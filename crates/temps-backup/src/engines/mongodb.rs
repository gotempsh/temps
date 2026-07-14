//! `MongodbEngine`: in-process MongoDB backup using `mongodump --archive --gzip`,
//! implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Load the external-service row, decrypt its config, locate the target
//!    mongo container (`temps-mongodb-<name>`).
//! 2. Prefer the container's `MONGO_INITDB_ROOT_USERNAME` /
//!    `MONGO_INITDB_ROOT_PASSWORD` env vars (root creds — full access) over
//!    the per-service config creds (which may have been provisioned with a
//!    narrower role). Verified necessary on 2026-05-14 when configured user
//!    silently emitted a 927-byte admin-only archive instead of the real
//!    100k+ docs.
//! 3. Run a one-shot `mongo` sidecar that executes
//!    `mongodump --archive --gzip` against the target container over the
//!    user-defined bridge network, capturing the archive in a host bind
//!    mount.
//! 4. Upload the resulting `.archive` to S3.
//! 5. Write the `metadata.json` companion.
//!
//! ## Why a sidecar, not exec
//!
//! Earlier versions called `docker exec` against the mongo container itself
//! and piped the archive back to the host process — that requires keeping
//! the exec attached for the entire dump and made cancellation racy. The
//! one-shot helper isolates the dump in its own container so cancellation
//! is just `docker stop`.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "mongodb";
const DUMP_FILE_SUFFIX: &str = "dump.archive";
const MONGO_SIDECAR_IMAGE: &str = "mongo:7";

pub struct MongodbDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct MongodbEngine {
    deps: Arc<MongodbDeps>,
}

impl MongodbEngine {
    pub fn new(deps: MongodbDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MongodbEngine {
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
            "mongodb-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        // ── Resolve credentials, preferring container env vars ───────────────
        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let cfg: Value = serde_json::from_str(&config_json).unwrap_or_else(|_| json!({}));
        let mut username = cfg
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut password = cfg
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let target_container = format!("temps-mongodb-{}", service.name);
        match deps
            .docker
            .inspect_container(
                &target_container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(inspect) => {
                if let Some(env_vec) = inspect.config.as_ref().and_then(|c| c.env.as_ref()) {
                    for env in env_vec {
                        if let Some(v) = env.strip_prefix("MONGO_INITDB_ROOT_USERNAME=") {
                            username = v.to_string();
                        } else if let Some(v) = env.strip_prefix("MONGO_INITDB_ROOT_PASSWORD=") {
                            password = v.to_string();
                        }
                    }
                }
            }
            Err(e) => warn!(
                backup_id,
                container = %target_container,
                error = %e,
                "MongodbEngine: could not inspect target for root creds; falling back to service config",
            ),
        }
        if username.is_empty() {
            username = "admin".to_string();
        }
        info!(
            backup_id,
            container = %target_container,
            username = %username,
            password_set = !password.is_empty(),
            "MongodbEngine: mongodump credentials resolved",
        );

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "mongodb",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );

        // ── One-shot mongodump container ─────────────────────────────────────
        let backup_dir = std::env::temp_dir().join("temps-mongo-backup");
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("failed to create tmpdir {}: {}", backup_dir.display(), e),
            })?;
        let dump_filename = format!("{}.archive", backup_uuid);
        let host_dump_path = backup_dir.join(&dump_filename);
        let container_dump_path = format!("/backup/{}", dump_filename);

        // mongodump itself writes the archive to stdout; redirect to the bind
        // mount inside the container. `--archive=/path` is the supported form
        // for writing directly to a file.
        let dump_cmd = format!(
            "mongodump --host={} --archive={} --gzip \
             -u {} -p {} --authenticationDatabase admin",
            v2_common::shell_escape(&target_container),
            v2_common::shell_escape(&container_dump_path),
            v2_common::shell_escape(&username),
            v2_common::shell_escape(&password),
        );

        super::image_pull::ensure_image_pulled_v2(MONGO_SIDECAR_IMAGE, ENGINE_KEY).await?;

        let spec = OneShotSpec {
            image: MONGO_SIDECAR_IMAGE.to_string(),
            name: format!("temps-mongodump-{}", backup_uuid),
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
                v2_common::best_effort_remove(&host_dump_path).await;
                return Err(BackupError::Failed {
                    reason: format!("mongodump one-shot failed: {}", e),
                });
            }
        };
        if result.exit_code != 0 {
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Failed {
                reason: format!(
                    "mongodump exited with code {}. stderr: {}",
                    result.exit_code,
                    result.stderr_tail.trim(),
                ),
            });
        }

        let dump_meta =
            tokio::fs::metadata(&host_dump_path)
                .await
                .map_err(|e| BackupError::Failed {
                    reason: format!("dump file missing after mongodump succeeded: {}", e),
                })?;
        if dump_meta.len() == 0 {
            v2_common::best_effort_remove(&host_dump_path).await;
            return Err(BackupError::Failed {
                reason: "mongodump produced an empty archive".into(),
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
            "application/octet-stream",
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
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = file_size,
            "MongodbEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}
