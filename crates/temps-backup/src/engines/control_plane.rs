//! `ControlPlaneEngine`: in-process backup of the Temps control-plane
//! PostgreSQL database, implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Validate S3 source + bucket reachability.
//! 2. Run `pg_dumpall | gzip` as a one-shot Docker container whose entrypoint
//!    is the backup command itself. The container exits when the dump exits;
//!    `auto_remove=true` reaps it.
//! 3. Upload the resulting `.sql.gz` to S3 (single-part or multipart).
//! 4. Write a `metadata.json` companion object.
//!
//! ## Retry semantics
//!
//! Each attempt allocates a fresh UUID for the dump file + S3 key, so
//! partial artefacts from a failed attempt are orphaned harmlessly rather
//! than racing the next attempt. No explicit cleanup hook is needed —
//! `auto_remove` reaps containers, and the bounded attempt count caps the
//! wasted disk/S3 cost.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use tracing::{info, warn};
use uuid::Uuid;

use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "control_plane";
const DUMP_FILE_SUFFIX: &str = "backup.sql.gz";

// ── Dependencies ─────────────────────────────────────────────────────────────

pub struct ControlPlaneDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub config_service: Arc<temps_config::ConfigService>,
}

// ── Engine ────────────────────────────────────────────────────────────────────

pub struct ControlPlaneEngine {
    deps: Arc<ControlPlaneDeps>,
}

impl ControlPlaneEngine {
    pub fn new(deps: ControlPlaneDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for ControlPlaneEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        // ── Params + S3 client ───────────────────────────────────────────────
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;
        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "control-plane-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = Uuid::new_v4().to_string();
        let s3_key =
            v2_common::build_dump_s3_key(&s3_source.bucket_path, &backup_uuid, DUMP_FILE_SUFFIX);

        info!(
            backup_id,
            s3_key = %s3_key,
            bucket = %s3_source.bucket_name,
            "ControlPlaneEngine: S3 validated, starting dump",
        );

        // ── Resolve DB connection params ─────────────────────────────────────
        let database_url = deps.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::PermanentFailure {
            reason: format!("invalid DATABASE_URL: {}", e),
        })?;
        let host = url.host_str().unwrap_or("localhost").to_string();
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/').to_string();
        let username = url.username().to_string();
        let password = urlencoding::decode(url.password().unwrap_or(""))
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Match the running server's major so pg_dumpall is version-compatible.
        let pg_tag = detect_postgres_version(&deps).await;
        let major = pg_tag.trim_start_matches("pg");
        let image_tag = format!("postgres:{}", major);
        super::image_pull::ensure_image_pulled_v2(&image_tag, ENGINE_KEY).await?;

        // ── Bind-mount + container command ───────────────────────────────────
        let backup_dir = v2_common::ensure_backup_tmpdir(&deps.config_service).await?;
        let dump_filename = format!("{}.sql.gz", backup_uuid);
        let host_dump_path = backup_dir.join(&dump_filename);
        let container_dump_path = format!("/backup/{}", dump_filename);
        let uncompressed_in_container = container_dump_path
            .strip_suffix(".gz")
            .unwrap_or(&container_dump_path)
            .to_string();

        let stderr_filename = format!("{}.stderr", backup_uuid);
        let stderr_path_in_container = format!("/backup/{}", stderr_filename);
        let host_stderr_path = backup_dir.join(&stderr_filename);

        let pg_dump_cmd = format!(
            "pg_dumpall --clean --if-exists --no-password \
             --host={} --port={} --username={} --database={} \
             2>{} > {} && gzip {}",
            v2_common::shell_escape(&host),
            v2_common::shell_escape(&port.to_string()),
            v2_common::shell_escape(&username),
            v2_common::shell_escape(&database),
            stderr_path_in_container,
            v2_common::shell_escape(&uncompressed_in_container),
            v2_common::shell_escape(&uncompressed_in_container),
        );

        let docker =
            bollard::Docker::connect_with_local_defaults().map_err(|e| BackupError::Failed {
                reason: format!("failed to connect to Docker: {}", e),
            })?;

        let spec = OneShotSpec {
            image: image_tag,
            name: format!("temps-cp-backup-{}", backup_uuid),
            engine: ENGINE_KEY,
            backup_id,
            entrypoint: vec!["sh".to_string(), "-c".to_string()],
            cmd: vec![pg_dump_cmd],
            env: vec![format!("PGPASSWORD={}", password)],
            binds: vec![format!("{}:/backup:rw", backup_dir.display())],
            // `host` mode so the container can reach 127.0.0.1:5432 where the
            // control-plane Postgres binds under `temps serve`.
            network_mode: Some("host".to_string()),
            user: Some("root".to_string()),
        };

        let result = match run_one_shot(&docker, spec, &ctx.cancel).await {
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
                    "pg_dumpall exited with code {}. file-stderr: {}{}",
                    result.exit_code,
                    String::from_utf8_lossy(&file_stderr),
                    if result.stderr_tail.trim().is_empty() {
                        String::new()
                    } else {
                        format!(". container-stderr: {}", result.stderr_tail.trim())
                    },
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

        info!(
            backup_id,
            path = %host_dump_path_str,
            size_bytes = file_size,
            "ControlPlaneEngine: pg_dumpall completed",
        );

        // ── Upload dump ──────────────────────────────────────────────────────
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

        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %s3_key,
            size_bytes = file_size,
            "ControlPlaneEngine: dump uploaded",
        );

        // ── Metadata companion ───────────────────────────────────────────────
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
            None,
        )
        .await?;
        info!(
            backup_id,
            bucket = %s3_source.bucket_name,
            key = %metadata_key,
            "ControlPlaneEngine: metadata.json written",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

// ── Local helpers ────────────────────────────────────────────────────────────

/// Detect the PostgreSQL major version via `current_setting('server_version')`.
/// Falls back to `"pg18"` if detection fails — pg_dumpall is
/// backwards-compatible so the worst case is a slightly-wrong sidecar tag.
async fn detect_postgres_version(deps: &ControlPlaneDeps) -> String {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    #[derive(FromQueryResult)]
    struct VersionRow {
        server_version: String,
    }

    let row = VersionRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT current_setting('server_version') AS server_version",
        vec![],
    ))
    .one(deps.db.as_ref())
    .await;

    match row {
        Ok(Some(r)) => {
            let major: u32 = r
                .server_version
                .split('.')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(18);
            format!("pg{}", major)
        }
        Ok(None) | Err(_) => {
            warn!("ControlPlaneEngine: could not detect PG version, defaulting to pg18");
            "pg18".to_string()
        }
    }
}
