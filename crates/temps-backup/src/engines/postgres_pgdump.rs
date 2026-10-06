// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
//!    container at `postgres-<service_name>:5432`. The dump stays in the
//!    sidecar's own filesystem and is streamed out through the Docker archive
//!    API into an attempt-scoped host dir (see `dump_capture`).
//! 4. Upload the resulting `.sql.gz` to S3.
//! 5. Write the `metadata.json` companion.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::info;

use super::dump_capture::{capture_dump, CaptureRequest, DumpAttempt, DumpCaptureError};
use super::oneshot::OneShotSpec;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "postgres_pgdump";
const DUMP_FILE_SUFFIX: &str = "dump.sql.gz";
/// Database name used in user-facing dump errors.
const POSTGRES_TOOL: &str = "PostgreSQL";
/// File names inside the sidecar's attempt directory.
const SIDECAR_DUMP_SQL: &str = "dump.sql";
const SIDECAR_DUMP_GZ: &str = "dump.sql.gz";
const SIDECAR_STDERR: &str = "pg_dumpall.stderr";

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
        //
        // The sidecar writes into its own filesystem and the dump is streamed
        // out through the Docker archive API (see `dump_capture`): no host
        // bind mount, so Docker in a VM or on another host works, and every
        // name is scoped to this attempt so retries never collide.
        let attempt = DumpAttempt::new(POSTGRES_TOOL, ENGINE_KEY, &backup_uuid)?;
        let spec = pgdump_spec(&attempt, backup_id, &service.name, &pg);
        let host_dump_path = attempt.host_path(SIDECAR_DUMP_GZ);

        super::image_pull::ensure_image_pulled_v2(&pg.docker_image, ENGINE_KEY).await?;

        let file_size = capture_dump(
            &deps.docker,
            CaptureRequest {
                tool: POSTGRES_TOOL,
                spec,
                container_path: attempt.container_path(SIDECAR_DUMP_GZ),
                host_path: host_dump_path.clone(),
                failure_log: Some((
                    attempt.container_path(SIDECAR_STDERR),
                    attempt.host_path(SIDECAR_STDERR),
                )),
            },
            &ctx.cancel,
        )
        .await?;
        let file_size = i64::try_from(file_size).map_err(|_| BackupError::Failed {
            reason: format!(
                "pg_dumpall output for backup {backup_id} is larger than i64::MAX bytes"
            ),
        })?;
        let host_dump_path_str = host_dump_path.to_string_lossy().into_owned();

        // ── Upload ───────────────────────────────────────────────────────────
        if ctx.cancel.is_cancelled() {
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
            &ctx.cancel,
        )
        .await
        .map_err(|error| {
            DumpCaptureError::upload(POSTGRES_TOOL, &s3_source.bucket_name, &s3_key, error)
        })?;
        // Deletes this attempt's host dir (and the dump in it); every early
        // return above does the same when `attempt` drops.
        drop(attempt);

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

/// `pg_dumpall | gzip` into the attempt's directory inside the sidecar's own
/// filesystem; stderr goes to a file next to it, copied out on failure.
fn pgdump_spec(
    attempt: &DumpAttempt,
    backup_id: i32,
    service_name: &str,
    pg: &PgParams,
) -> OneShotSpec {
    let uncompressed = attempt.container_path(SIDECAR_DUMP_SQL);
    let db_container = format!("postgres-{}", service_name);
    let dump_cmd = format!(
        "mkdir -p {dir} && pg_dumpall --clean --if-exists --no-password \
         --host={host} --port=5432 --username={user} --database={db} \
         2>{stderr} > {out} && gzip {out}",
        dir = v2_common::shell_escape(&attempt.container_dir()),
        host = v2_common::shell_escape(&db_container),
        user = v2_common::shell_escape(&pg.username),
        db = v2_common::shell_escape(&pg.database),
        stderr = v2_common::shell_escape(&attempt.container_path(SIDECAR_STDERR)),
        out = v2_common::shell_escape(&uncompressed),
    );
    OneShotSpec {
        image: pg.docker_image.clone(),
        name: attempt.container_name("temps-pgdump"),
        engine: ENGINE_KEY,
        backup_id,
        entrypoint: vec!["sh".to_string(), "-c".to_string()],
        cmd: vec![dump_cmd],
        env: vec![format!("PGPASSWORD={}", pg.password)],
        binds: vec![],
        // Same user-defined bridge the target Postgres container is on so
        // `postgres-{service_name}` resolves.
        network_mode: Some(temps_core::NETWORK_NAME.to_string()),
        user: Some("root".to_string()),
        stderr_watch: None,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgdump_spec_has_no_bind_mount_and_is_attempt_scoped() {
        let parent = tempfile::tempdir().expect("parent");
        let first =
            DumpAttempt::new_in(parent.path(), POSTGRES_TOOL, ENGINE_KEY, "b-uuid").expect("first");
        let retry =
            DumpAttempt::new_in(parent.path(), POSTGRES_TOOL, ENGINE_KEY, "b-uuid").expect("retry");
        let pg = load_postgres_params(r#"{"username":"u","password":"p","database":"d"}"#);

        let spec = pgdump_spec(&first, 9, "orders", &pg);
        let retry_spec = pgdump_spec(&retry, 9, "orders", &pg);

        assert!(
            spec.binds.is_empty(),
            "output must not depend on a host bind"
        );
        assert_ne!(spec.name, retry_spec.name);
        assert!(spec.name.starts_with("temps-pgdump-b-uuid-"));
        let cmd = &spec.cmd[0];
        assert!(
            cmd.contains(&first.container_path(SIDECAR_DUMP_SQL)),
            "{cmd}"
        );
        assert!(cmd.contains(&first.container_path(SIDECAR_STDERR)), "{cmd}");
        assert!(cmd.contains("'postgres-orders'"), "{cmd}");
        assert!(!cmd.contains(&retry.container_dir()), "{cmd}");
        assert_eq!(spec.env, vec!["PGPASSWORD=p".to_string()]);
    }
}
