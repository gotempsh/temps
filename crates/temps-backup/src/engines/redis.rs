// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `RedisEngine`: direct-to-S3 WAL-G stream backups for managed Redis
//! images, with a logical `redis-cli --rdb` fallback for arbitrary OSS images.
//!
//! ## Flow
//!
//! 1. Load the external-service row, decrypt its config to find the auth
//!    password (if any), and validate the S3 source.
//! 2. When the service image contains WAL-G, execute `wal-g backup-push`
//!    inside Redis. WAL-G streams `redis-cli --rdb -` directly to S3 without
//!    a database-sized host file. This is the Cloud-verifiable path.
//! 3. Otherwise run a one-shot `redis:7-alpine` sidecar over the user-defined bridge
//!    network. The sidecar issues `redis-cli -h redis-<name> --rdb
//!    /backup/<uuid>.rdb`, then `gzip` the resulting file.
//! 4. Upload the gzipped `.rdb.gz` to S3.
//! 5. Write the `metadata.json` companion.
//!
//! ## Notes
//!
//! - `redis-cli --rdb` triggers a `SYNC` and streams the RDB snapshot
//!   over the wire. This works against any Redis ≥ 2.8 and does not
//!   require WAL-G to be installed on the target.
//! - The logical fallback remains usable in OSS but cannot provide the exact
//!   immutable WAL-G identity required by managed Cloud restore verification.

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

const ENGINE_KEY: &str = "redis";
const DUMP_FILE_SUFFIX: &str = "dump.rdb.gz";
const REDIS_SIDECAR_IMAGE: &str =
    "redis:7.4.10-alpine@sha256:e7723ff73d963f5cc6d9c4643ea3d989527a402a319239054e9472a7fb9219a2";
// `redis-cli --rdb -` negotiates Redis diskless replication and writes the
// valid RDB followed by Redis's 40-byte hexadecimal EOF marker. The standalone
// `redis-check-rdb` command accepts that trailing marker, but Redis rejects the
// same bytes when the snapshot is used as an AOF base file during restore.
// `head -c -40` keeps only a 40-byte tail buffer, so this remains a true stream
// from Redis to WAL-G without creating a database-sized local file.
const WALG_STREAM_CREATE_COMMAND: &str = "bash -c 'error=$(mktemp); redis-cli --rdb - 2>$error | head -c -40; statuses=(\"${PIPESTATUS[@]}\"); code=${statuses[0]}; if [ ${statuses[1]} -ne 0 ]; then code=${statuses[1]}; fi; cat $error >&2; if [ $code -ne 0 ] && grep -q \"Fail to fsync.*Invalid argument\" $error; then code=0; fi; rm -f $error; exit $code'";
const WALG_STREAM_RESTORE_COMMAND: &str = "cat > /data/dump.rdb";

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

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let target_container = service_container_name(&service);
        if container_has_walg(&deps.docker, &target_container).await {
            return run_walg_backup(
                &deps,
                ctx,
                &service,
                &s3_source,
                &s3_client,
                &target_container,
                &password,
                &backup_uuid,
            )
            .await;
        }
        warn!(
            backup_id,
            container = %target_container,
            "RedisEngine: WAL-G is unavailable; using logical redis-cli fallback. This backup remains usable in OSS but is not eligible for managed Cloud restore verification",
        );

        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "redis",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );

        // ── One-shot redis-cli --rdb fallback container ──────────────────────
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

#[allow(clippy::too_many_arguments)]
async fn run_walg_backup(
    deps: &RedisDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    s3_client: &aws_sdk_s3::Client,
    container_name: &str,
    password: &str,
    backup_uuid: &str,
) -> Result<BackupOutcome, BackupError> {
    let bucket_path = s3_source.bucket_path.trim_matches('/');
    let service_root = format!("external_services/redis/{}/walg", service.name);
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
            reason: format!("decrypt Redis WAL-G access key: {error}"),
        })?;
    let secret_key = deps
        .encryption_service
        .decrypt_string(&s3_source.secret_key)
        .map_err(|error| BackupError::PermanentFailure {
            reason: format!("decrypt Redis WAL-G secret key: {error}"),
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
    let mut env = vec![
        format!("WALG_S3_PREFIX={walg_prefix}"),
        format!("AWS_ACCESS_KEY_ID={access_key}"),
        format!("AWS_SECRET_ACCESS_KEY={secret_key}"),
        format!("AWS_REGION={}", s3_source.region),
        format!("REDISCLI_AUTH={password}"),
        format!("WALG_STREAM_CREATE_COMMAND={WALG_STREAM_CREATE_COMMAND}"),
        format!("WALG_STREAM_RESTORE_COMMAND={WALG_STREAM_RESTORE_COMMAND}"),
    ];
    // Absent unless this source holds a temporary credential.
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
        "RedisEngine: starting direct WAL-G stream backup",
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
                "Redis wal-g backup-push exited with code {}. stderr: {}",
                exec.exit_code,
                bounded_tail(&exec.stderr),
            ),
        });
    }

    let file_size = list_total_s3_size(s3_client, &s3_source.bucket_name, &list_prefix).await?;
    if file_size <= 0 {
        return Err(BackupError::Failed {
            reason: format!("Redis WAL-G repository {walg_prefix} contains no backup bytes"),
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
            "backup_tool": "wal-g+redis-rdb-stream",
            "service": { "id": service.id, "name": service.name },
        })),
    )
    .await?;
    v2_common::record_walg_identity(deps.db.as_ref(), ctx.backup_id, backup_uuid).await?;

    info!(
        backup_id = ctx.backup_id,
        repository = %walg_prefix,
        size_bytes = file_size,
        "RedisEngine: WAL-G stream backup complete",
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
            reason: format!("list Redis WAL-G repository {prefix}: {error}"),
        })?;
        for object in response.contents() {
            total = total
                .checked_add(object.size().unwrap_or(0))
                .ok_or_else(|| BackupError::Failed {
                    reason: format!("Redis WAL-G repository {prefix} size overflowed i64"),
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
    fn redis_sidecar_image_is_release_and_digest_pinned() {
        assert!(REDIS_SIDECAR_IMAGE.contains("redis:7.4.10-alpine@"));
        assert!(REDIS_SIDECAR_IMAGE
            .contains("sha256:e7723ff73d963f5cc6d9c4643ea3d989527a402a319239054e9472a7fb9219a2"));
    }

    #[test]
    fn walg_stream_commands_match_cloud_restore_contract() {
        assert!(WALG_STREAM_CREATE_COMMAND.contains("redis-cli --rdb -"));
        assert!(WALG_STREAM_CREATE_COMMAND.contains("head -c -40"));
        assert!(WALG_STREAM_CREATE_COMMAND.contains("PIPESTATUS"));
        assert_eq!(WALG_STREAM_RESTORE_COMMAND, "cat > /data/dump.rdb");
    }

    #[test]
    fn bounded_tail_preserves_utf8_boundaries() {
        let value = format!("{}END", "é".repeat(1_100));
        let tail = bounded_tail(&value);
        assert!(tail.ends_with("END"));
        assert!(tail.starts_with('…'));
    }
}
