// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `MongodbEngine`: direct-to-S3 WAL-G stream backups for managed MongoDB
//! images, with a logical `mongodump` fallback for arbitrary OSS images.
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
//! 3. When the service image contains WAL-G, execute `wal-g backup-push`
//!    inside the service container. WAL-G streams `mongodump --archive`
//!    directly to S3 without a database-sized host file.
//! 4. Otherwise run a one-shot `mongo` sidecar that executes
//!    `mongodump --archive --gzip` against the target container over the
//!    user-defined bridge network, capturing the archive in a host bind
//!    mount.
//! 5. Upload the fallback `.archive` to S3 and write its metadata companion.
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

use super::dispatch::{container_has_walg, service_container_name};
use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::postgres_walg::run_walg_exec;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "mongodb";
const DUMP_FILE_SUFFIX: &str = "dump.archive";
const MONGO_SIDECAR_IMAGE: &str =
    "mongo:7.0.39-jammy@sha256:04582c3a144d088f841c446abfc19f79adcefa8bd00ad4a7fb18e27b9585c5d6";
const WALG_STREAM_CREATE_COMMAND: &str = "mongodump --archive --uri=\"$MONGODB_URI\"";
const WALG_STREAM_RESTORE_COMMAND: &str = "mongorestore --archive --drop --uri=\"$MONGODB_URI\"";

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

        let target_container = service_container_name(&service);
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
        if container_has_walg(&deps.docker, &target_container).await {
            return run_walg_backup(
                &deps,
                ctx,
                &service,
                &s3_source,
                &s3_client,
                &target_container,
                &username,
                &password,
                &backup_uuid,
            )
            .await;
        }
        warn!(
            backup_id,
            container = %target_container,
            "MongodbEngine: WAL-G is unavailable; using logical mongodump fallback. This backup remains usable in OSS but is not eligible for managed Cloud restore verification",
        );

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

#[allow(clippy::too_many_arguments)]
async fn run_walg_backup(
    deps: &MongodbDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    s3_client: &aws_sdk_s3::Client,
    container_name: &str,
    username: &str,
    password: &str,
    backup_uuid: &str,
) -> Result<BackupOutcome, BackupError> {
    let bucket_path = s3_source.bucket_path.trim_matches('/');
    let service_root = format!("external_services/mongodb/{}/walg", service.name);
    let repository_key = if bucket_path.is_empty() {
        service_root
    } else {
        format!("{bucket_path}/{service_root}")
    };
    let walg_prefix = format!("s3://{}/{}", s3_source.bucket_name, repository_key);
    let list_prefix = format!("{repository_key}/");

    let access_key = deps
        .encryption_service
        .decrypt_string(&s3_source.access_key_id)
        .map_err(|error| BackupError::PermanentFailure {
            reason: format!("decrypt MongoDB WAL-G access key: {error}"),
        })?;
    let secret_key = deps
        .encryption_service
        .decrypt_string(&s3_source.secret_key)
        .map_err(|error| BackupError::PermanentFailure {
            reason: format!("decrypt MongoDB WAL-G secret key: {error}"),
        })?;
    let session_token = v2_common::decrypt_session_token(s3_source, &deps.encryption_service)?;
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
    .resolve_endpoint_for_container(&deps.docker, container_name)
    .await;
    let mongodb_uri = format!(
        "mongodb://{}:{}@127.0.0.1:27017/?authSource=admin",
        urlencoding::encode(username),
        urlencoding::encode(password),
    );
    let mut env = vec![
        format!("WALG_S3_PREFIX={walg_prefix}"),
        format!("AWS_ACCESS_KEY_ID={access_key}"),
        format!("AWS_SECRET_ACCESS_KEY={secret_key}"),
        format!("AWS_REGION={}", s3_source.region),
        format!("MONGODB_URI={mongodb_uri}"),
        format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
        format!("WALG_STREAM_RESTORE_COMMAND={WALG_STREAM_RESTORE_COMMAND}"),
    ];
    // Present only for a temporary credential; a long-lived one contributes
    // nothing here and the container's environment is byte-for-byte unchanged.
    env.extend(temps_providers::externalsvc::aws_session_token_env(
        session_token.as_deref(),
    ));
    env.extend(v2_common::walg_identity_env(backup_uuid));
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

    info!(
        backup_id = ctx.backup_id,
        repository = %walg_prefix,
        "MongodbEngine: starting direct WAL-G stream backup",
    );
    let exec = run_walg_exec(
        &deps.docker,
        container_name,
        "wal-g backup-push",
        &env,
        &ctx.cancel,
    )
    .await?;
    if exec.exit_code != 0 {
        return Err(BackupError::Failed {
            reason: format!(
                "MongoDB wal-g backup-push exited with code {}. stderr: {}",
                exec.exit_code,
                bounded_tail(&exec.stderr),
            ),
        });
    }

    let file_size = list_total_s3_size(s3_client, &s3_source.bucket_name, &list_prefix).await?;
    if file_size <= 0 {
        return Err(BackupError::Failed {
            reason: format!("MongoDB WAL-G repository {walg_prefix} contains no backup bytes"),
        });
    }
    let metadata_key = format!("{list_prefix}{backup_uuid}.metadata.json");
    v2_common::write_metadata_companion(
        s3_client,
        &s3_source.bucket_name,
        &metadata_key,
        ENGINE_KEY,
        backup_uuid,
        &walg_prefix,
        file_size,
        s3_source.id,
        "wal-g-native",
        Some(json!({
            "backup_tool": "wal-g+mongodump-stream",
            "service": { "id": service.id, "name": service.name },
        })),
    )
    .await?;
    v2_common::record_walg_identity(deps.db.as_ref(), ctx.backup_id, backup_uuid).await?;

    info!(
        backup_id = ctx.backup_id,
        repository = %walg_prefix,
        size_bytes = file_size,
        "MongodbEngine: WAL-G stream backup complete",
    );
    Ok(BackupOutcome {
        location: walg_prefix,
        size_bytes: Some(file_size),
        compression: "wal-g-native".to_string(),
    })
}

fn bounded_tail(value: &str) -> String {
    const MAX_BYTES: usize = 2_000;
    let trimmed = value.trim();
    if trimmed.len() <= MAX_BYTES {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - MAX_BYTES;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

async fn list_total_s3_size(
    client: &aws_sdk_s3::Client,
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
            reason: format!("list MongoDB WAL-G repository {prefix}: {error}"),
        })?;
        for object in response.contents() {
            total = total
                .checked_add(object.size().unwrap_or(0))
                .ok_or_else(|| BackupError::Failed {
                    reason: format!("MongoDB WAL-G repository {prefix} size overflowed i64"),
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

    #[test]
    fn mongo_sidecar_image_is_release_and_digest_pinned() {
        assert!(MONGO_SIDECAR_IMAGE.contains("mongo:7.0.39-jammy@"));
        assert!(MONGO_SIDECAR_IMAGE
            .contains("sha256:04582c3a144d088f841c446abfc19f79adcefa8bd00ad4a7fb18e27b9585c5d6"));
    }

    #[test]
    fn walg_stream_commands_use_ephemeral_uri_environment() {
        assert!(WALG_STREAM_CREATE_COMMAND.contains("$MONGODB_URI"));
        assert!(WALG_STREAM_RESTORE_COMMAND.contains("$MONGODB_URI"));
        assert!(!WALG_STREAM_CREATE_COMMAND.contains("mongodb://"));
        assert!(!WALG_STREAM_RESTORE_COMMAND.contains("mongodb://"));
    }

    #[test]
    fn bounded_tail_preserves_utf8_boundaries() {
        let value = format!("{}END", "é".repeat(1_100));
        let tail = bounded_tail(&value);
        assert!(tail.ends_with("END"));
        assert!(tail.starts_with('…'));
    }
}
