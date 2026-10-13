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
//!    /tmp/temps-backup/<attempt>/dump.rdb` into its own filesystem, then
//!    `gzip`s it. Temps streams the `.rdb.gz` out through the Docker archive
//!    API into an attempt-scoped host directory (see `dump_capture`); no host
//!    path is bind-mounted, so Docker in a VM or on a remote host works.
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
use super::dump_capture::{capture_dump, CaptureRequest, DumpAttempt, DumpCaptureError};
use super::oneshot::OneShotSpec;
use super::postgres_walg::run_walg_exec;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "redis";
const DUMP_FILE_SUFFIX: &str = "dump.rdb.gz";
/// Database name used in user-facing dump errors.
const REDIS_TOOL: &str = "Redis";
/// Uncompressed and gzipped dump names inside the sidecar's attempt dir.
const SIDECAR_DUMP_RDB: &str = "dump.rdb";
const SIDECAR_DUMP_GZ: &str = "dump.rdb.gz";
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
        //
        // The sidecar writes into its own filesystem and the dump is streamed
        // out through the Docker archive API, so this works when Docker runs
        // in a VM or on another host whose filesystem does not include the
        // Temps host's temp dir (see `dump_capture`). Every name is scoped to
        // this attempt, so a retry never trips over a previous attempt's
        // container or files.
        let attempt = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, &backup_uuid)?;
        let spec = logical_dump_spec(&attempt, backup_id, &target_container, &password);
        let container_path = attempt.container_path(SIDECAR_DUMP_GZ);
        let host_rdb_gz_path = attempt.host_path(SIDECAR_DUMP_GZ);

        super::image_pull::ensure_image_pulled_v2(REDIS_SIDECAR_IMAGE, ENGINE_KEY).await?;

        let file_size = capture_dump(
            &deps.docker,
            CaptureRequest {
                tool: REDIS_TOOL,
                spec,
                container_path,
                host_path: host_rdb_gz_path.clone(),
                failure_log: None,
            },
            &ctx.cancel,
        )
        .await
        .inspect_err(|error| {
            warn!(
                backup_id,
                attempt = attempt.attempt_id(),
                error = %error,
                "RedisEngine: logical redis-cli fallback did not produce a dump",
            )
        })?;
        let file_size = i64::try_from(file_size).map_err(|_| BackupError::Failed {
            reason: format!("Redis dump for backup {backup_id} is larger than i64::MAX bytes"),
        })?;
        let host_dump_path_str = host_rdb_gz_path.to_string_lossy().into_owned();

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
            DumpCaptureError::upload(REDIS_TOOL, &s3_source.bucket_name, &s3_key, error)
        })?;
        // The attempt's host directory (and the dump in it) is deleted here,
        // and on every early return above, when `attempt` is dropped.
        drop(attempt);

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

/// The logical fallback sidecar: `redis-cli --rdb` into the attempt's
/// directory inside the sidecar's own filesystem, then `gzip`. No bind
/// mount: the result is read back with the Docker archive API.
fn logical_dump_spec(
    attempt: &DumpAttempt,
    backup_id: i32,
    target_container: &str,
    password: &str,
) -> OneShotSpec {
    let container_dir = attempt.container_dir();
    let container_rdb_path = attempt.container_path(SIDECAR_DUMP_RDB);
    // The password travels in `REDISCLI_AUTH`, not argv, so it never shows
    // up in `docker inspect`'s command line or `ps` inside the sidecar.
    let env = if password.is_empty() {
        vec![]
    } else {
        vec![format!("REDISCLI_AUTH={password}")]
    };
    let dump_cmd = format!(
        "mkdir -p {dir} && redis-cli -h {host} --rdb {rdb} && gzip {rdb}",
        dir = v2_common::shell_escape(&container_dir),
        host = v2_common::shell_escape(target_container),
        rdb = v2_common::shell_escape(&container_rdb_path),
    );
    OneShotSpec {
        image: REDIS_SIDECAR_IMAGE.to_string(),
        name: attempt.container_name("temps-redis-backup"),
        engine: ENGINE_KEY,
        backup_id,
        entrypoint: vec!["sh".to_string(), "-c".to_string()],
        cmd: vec![dump_cmd],
        env,
        binds: vec![],
        network_mode: Some(temps_core::NETWORK_NAME.to_string()),
        user: Some("root".to_string()),
        stderr_watch: None,
    }
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

    // ── Logical fallback spec ────────────────────────────────────────────

    #[test]
    fn logical_dump_spec_has_no_bind_mount_and_is_attempt_scoped() {
        let parent = tempfile::tempdir().expect("parent");
        let first = DumpAttempt::new_in(parent.path(), REDIS_TOOL, ENGINE_KEY, "b-uuid")
            .expect("first attempt");
        let retry = DumpAttempt::new_in(parent.path(), REDIS_TOOL, ENGINE_KEY, "b-uuid")
            .expect("retry attempt");

        let spec = logical_dump_spec(&first, 42, "redis-cache", "s3cr'et");
        let retry_spec = logical_dump_spec(&retry, 42, "redis-cache", "s3cr'et");

        assert!(
            spec.binds.is_empty(),
            "output must not depend on a host bind"
        );
        assert_ne!(spec.name, retry_spec.name, "a retry gets its own container");
        assert_ne!(spec.cmd, retry_spec.cmd, "a retry writes to its own paths");
        let cmd = &spec.cmd[0];
        assert!(
            cmd.contains(&first.container_path(SIDECAR_DUMP_RDB)),
            "{cmd}"
        );
        assert!(cmd.contains("'redis-cache'"), "{cmd}");
        assert!(
            !cmd.contains("s3cr"),
            "the password must not be on argv: {cmd}"
        );
        assert_eq!(spec.env, vec!["REDISCLI_AUTH=s3cr'et".to_string()]);
        assert_eq!(spec.backup_id, 42);

        let no_auth = logical_dump_spec(&first, 42, "redis-cache", "");
        assert!(no_auth.env.is_empty());
    }

    // ── Docker-backed tests ──────────────────────────────────────────────
    //
    // These run the real fallback sidecar against a real Redis. They skip
    // (with a message) when no Docker daemon is reachable. Run them with the
    // OS default temp dir: on macOS with Colima or Docker Desktop that dir is
    // not shared with the Docker VM, which is exactly the setup the old
    // bind-mount fallback failed on. Each test prints whether the attempt's
    // host dir turned out to be visible to Docker.

    const TEST_PASSWORD: &str = "test-pass";
    const TEST_KEY: &str = "temps:test";
    const TEST_VALUE: &str = "before-backup";

    async fn docker_or_skip(test: &str) -> Option<bollard::Docker> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                println!("Docker not available, skipping {test}: {e}");
                return None;
            }
        };
        if let Err(e) = docker.ping().await {
            println!("Docker daemon not reachable, skipping {test}: {e}");
            return None;
        }
        if let Err(e) =
            super::super::image_pull::ensure_image_pulled_v2(REDIS_SIDECAR_IMAGE, ENGINE_KEY).await
        {
            println!("Could not pull {REDIS_SIDECAR_IMAGE}, skipping {test}: {e}");
            return None;
        }
        Some(docker)
    }

    /// Run a command in a container and return its combined output.
    async fn exec_output(docker: &bollard::Docker, container: &str, cmd: &[&str]) -> String {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        use futures::StreamExt;
        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|part| part.to_string()).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("create exec");
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await.expect("start exec")
        {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&chunk.to_string());
            }
        }
        text
    }

    /// Poll `cmd` in `container` until its output contains `expected`.
    async fn wait_for_output(
        docker: &bollard::Docker,
        container: &str,
        cmd: &[&str],
        expected: &str,
    ) -> String {
        let mut last = String::new();
        for _ in 0..60 {
            last = exec_output(docker, container, cmd).await;
            if last.contains(expected) {
                return last;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        panic!(
            "{container}: `{}` never printed '{expected}'; last output: {last}",
            cmd.join(" ")
        );
    }

    async fn container_is_gone(docker: &bollard::Docker, name: &str) -> bool {
        for _ in 0..60 {
            match docker
                .inspect_container(
                    name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => return true,
                _ => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            }
        }
        false
    }

    /// Containers and the network a test created, removed on drop (also on
    /// panic) from a dedicated thread with its own runtime.
    struct DockerLeftovers {
        containers: Vec<String>,
        network: Option<String>,
    }

    impl Drop for DockerLeftovers {
        fn drop(&mut self) {
            let containers = std::mem::take(&mut self.containers);
            let network = self.network.take();
            let cleanup = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
                        return;
                    };
                    for name in containers {
                        let _ = docker
                            .remove_container(
                                &name,
                                Some(bollard::query_parameters::RemoveContainerOptions {
                                    force: true,
                                    v: true,
                                    ..Default::default()
                                }),
                            )
                            .await;
                    }
                    if let Some(network) = network {
                        let _ = docker.remove_network(&network).await;
                    }
                });
            });
            let _ = cleanup.join();
        }
    }

    /// A password-protected Redis on its own network, seeded with one key.
    struct SeededRedis {
        network: String,
        container: String,
        _leftovers: DockerLeftovers,
    }

    async fn seeded_redis(docker: &bollard::Docker, run_id: &str) -> SeededRedis {
        use bollard::models::{ContainerCreateBody, EndpointSettings, NetworkingConfig};
        let network = format!("temps-test-rdb-net-{run_id}");
        let container = format!("temps-test-rdb-redis-{run_id}");
        let leftovers = DockerLeftovers {
            containers: vec![container.clone()],
            network: Some(network.clone()),
        };
        docker
            .create_network(bollard::models::NetworkCreateRequest {
                name: network.clone(),
                ..Default::default()
            })
            .await
            .expect("create test network");
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(REDIS_SIDECAR_IMAGE.to_string()),
                    cmd: Some(
                        ["redis-server", "--requirepass", TEST_PASSWORD, "--save", ""]
                            .iter()
                            .map(|part| part.to_string())
                            .collect(),
                    ),
                    networking_config: Some(NetworkingConfig {
                        endpoints_config: Some(std::collections::HashMap::from([(
                            network.clone(),
                            EndpointSettings::default(),
                        )])),
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create test redis");
        docker
            .start_container(
                &container,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start test redis");
        wait_for_output(
            docker,
            &container,
            &[
                "redis-cli",
                "-a",
                TEST_PASSWORD,
                "SET",
                TEST_KEY,
                TEST_VALUE,
            ],
            "OK",
        )
        .await;
        SeededRedis {
            network,
            container,
            _leftovers: leftovers,
        }
    }

    /// Whether a sentinel written into `dir` on this host is visible to a
    /// container that bind-mounts `dir` -- i.e. whether the old bind-mount
    /// fallback could have worked here at all.
    async fn host_dir_is_shared_with_docker(
        docker: &bollard::Docker,
        dir: &std::path::Path,
    ) -> bool {
        let sentinel = format!("sentinel-{}", uuid::Uuid::new_v4().simple());
        std::fs::write(dir.join(&sentinel), b"x").expect("write sentinel");
        let spec = OneShotSpec {
            image: REDIS_SIDECAR_IMAGE.to_string(),
            name: format!("temps-test-rdb-probe-{}", uuid::Uuid::new_v4().simple()),
            engine: ENGINE_KEY,
            backup_id: 0,
            entrypoint: vec!["sh".to_string(), "-c".to_string()],
            cmd: vec![format!("test -f /probe/{sentinel}")],
            env: vec![],
            binds: vec![format!("{}:/probe:ro", dir.display())],
            network_mode: None,
            user: Some("root".to_string()),
            stderr_watch: None,
        };
        let shared = super::super::oneshot::run_one_shot(
            docker,
            spec,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .map(|result| result.exit_code == 0)
        .unwrap_or(false);
        let _ = std::fs::remove_file(dir.join(&sentinel));
        shared
    }

    fn gunzip(path: &std::path::Path) -> Vec<u8> {
        use std::io::Read;
        let mut rdb = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).expect("open dump"))
            .read_to_end(&mut rdb)
            .expect("gunzip dump");
        rdb
    }

    /// Load `rdb` into a fresh Redis (uploaded through the archive API, the
    /// same way the restore helper does) and read the seeded key back.
    async fn restored_value(docker: &bollard::Docker, run_id: &str, rdb: Vec<u8>) -> String {
        let name = format!("temps-test-rdb-restore-{run_id}");
        let _leftovers = DockerLeftovers {
            containers: vec![name.clone()],
            network: None,
        };
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(REDIS_SIDECAR_IMAGE.to_string()),
                    cmd: Some(
                        [
                            "redis-server",
                            "--appendonly",
                            "no",
                            "--dbfilename",
                            "dump.rdb",
                        ]
                        .iter()
                        .map(|part| part.to_string())
                        .collect(),
                    ),
                    ..Default::default()
                },
            )
            .await
            .expect("create restore target");
        let mut header = tar::Header::new_gnu();
        header.set_size(rdb.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut archive = tar::Builder::new(Vec::new());
        archive
            .append_data(&mut header, "dump.rdb", rdb.as_slice())
            .expect("tar dump");
        let archive = archive.into_inner().expect("finish tar");
        docker
            .upload_to_container(
                &name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/data".to_string(),
                    ..Default::default()
                }),
                bollard::body_full(archive.into()),
            )
            .await
            .expect("upload dump into restore target");
        docker
            .start_container(
                &name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start restore target");
        wait_for_output(docker, &name, &["redis-cli", "GET", TEST_KEY], TEST_VALUE).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn logical_fallback_round_trips_without_a_shared_temp_dir() {
        let Some(docker) =
            docker_or_skip("logical_fallback_round_trips_without_a_shared_temp_dir").await
        else {
            return;
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let redis = seeded_redis(&docker, &run_id).await;

        // The real default: the OS temp dir, whatever TMPDIR says.
        let attempt = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, &format!("roundtrip-{run_id}"))
            .expect("attempt");
        let shared = host_dir_is_shared_with_docker(&docker, attempt.host_dir()).await;
        println!(
            "attempt host dir {} is {} with the Docker daemon",
            attempt.host_dir().display(),
            if shared { "SHARED" } else { "NOT shared" }
        );

        let mut spec = logical_dump_spec(&attempt, 0, &redis.container, TEST_PASSWORD);
        spec.network_mode = Some(redis.network.clone());
        let sidecar = spec.name.clone();
        let host_path = attempt.host_path(SIDECAR_DUMP_GZ);
        let size = capture_dump(
            &docker,
            CaptureRequest {
                tool: REDIS_TOOL,
                spec,
                container_path: attempt.container_path(SIDECAR_DUMP_GZ),
                host_path: host_path.clone(),
                failure_log: None,
            },
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("capture the Redis dump through the Docker API");

        assert!(size > 0);
        assert_eq!(
            std::fs::metadata(&host_path).expect("dump on host").len(),
            size
        );
        assert!(
            container_is_gone(&docker, &sidecar).await,
            "sidecar {sidecar} was not removed"
        );
        let rdb = gunzip(&host_path);
        assert!(rdb.starts_with(b"REDIS"), "not an RDB file");

        let host_dir = attempt.host_dir().to_path_buf();
        drop(attempt);
        assert!(
            !host_dir.exists(),
            "attempt dir {} not cleaned up",
            host_dir.display()
        );

        assert!(restored_value(&docker, &run_id, rdb)
            .await
            .contains(TEST_VALUE));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retry_after_failed_attempts_uses_fresh_paths_and_succeeds() {
        let Some(docker) =
            docker_or_skip("retry_after_failed_attempts_uses_fresh_paths_and_succeeds").await
        else {
            return;
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let redis = seeded_redis(&docker, &run_id).await;
        let backup_uuid = format!("retry-{run_id}");
        let cancel = tokio_util::sync::CancellationToken::new();

        // What an older binary left behind after the bind-mount failure. It
        // must neither block the retry nor be deleted by it.
        let legacy_dir = std::env::temp_dir().join("temps-redis-backup");
        std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
        let legacy = legacy_dir.join(format!("{backup_uuid}.rdb.gz"));
        std::fs::write(&legacy, b"stale").expect("legacy leftover");

        let request = |attempt: &DumpAttempt, spec: OneShotSpec| CaptureRequest {
            tool: REDIS_TOOL,
            spec,
            container_path: attempt.container_path(SIDECAR_DUMP_GZ),
            host_path: attempt.host_path(SIDECAR_DUMP_GZ),
            failure_log: None,
        };

        // Attempt 1: Redis export fails (unreachable target).
        let first = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, &backup_uuid).expect("attempt 1");
        let mut spec = logical_dump_spec(&first, 0, "temps-test-no-such-redis", TEST_PASSWORD);
        spec.network_mode = Some(redis.network.clone());
        let first_sidecar = spec.name.clone();
        let error = capture_dump(&docker, request(&first, spec), &cancel)
            .await
            .expect_err("unreachable Redis");
        assert!(
            matches!(error, DumpCaptureError::Export { exit_code, .. } if exit_code != 0),
            "{error}"
        );
        assert!(container_is_gone(&docker, &first_sidecar).await);
        let first_dir = first.host_dir().to_path_buf();
        drop(first);
        assert!(!first_dir.exists());

        // Attempt 2: the command "succeeds" without leaving a readable dump,
        // the shape of the original bind-mount failure.
        let second = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, &backup_uuid).expect("attempt 2");
        let mut spec = logical_dump_spec(&second, 0, &redis.container, TEST_PASSWORD);
        spec.network_mode = Some(redis.network.clone());
        spec.cmd = vec!["true".to_string()];
        let second_sidecar = spec.name.clone();
        let error = capture_dump(&docker, request(&second, spec), &cancel)
            .await
            .expect_err("no dump written");
        assert!(
            matches!(error, DumpCaptureError::DumpUnreadable { .. }),
            "{error}"
        );
        assert!(error.to_string().starts_with(
            "Redis exported the backup, but Temps could not read the temporary file written by Docker."
        ));
        assert!(container_is_gone(&docker, &second_sidecar).await);
        assert!(!second.host_path(SIDECAR_DUMP_GZ).exists());
        drop(second);

        // Attempt 3: same backup, real dump. No `File exists`, no name clash.
        let third = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, &backup_uuid).expect("attempt 3");
        let mut spec = logical_dump_spec(&third, 0, &redis.container, TEST_PASSWORD);
        spec.network_mode = Some(redis.network.clone());
        let size = capture_dump(&docker, request(&third, spec), &cancel)
            .await
            .expect("retry succeeds");
        assert!(size > 0);
        assert!(gunzip(&third.host_path(SIDECAR_DUMP_GZ)).starts_with(b"REDIS"));
        drop(third);

        assert_eq!(std::fs::read(&legacy).expect("legacy kept"), b"stale");
        let _ = std::fs::remove_file(&legacy);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_the_fallback_removes_its_container() {
        let Some(docker) = docker_or_skip("cancelling_the_fallback_removes_its_container").await
        else {
            return;
        };
        let attempt = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, "cancel-test").expect("attempt");
        let mut spec = logical_dump_spec(&attempt, 0, "unused", "");
        spec.network_mode = None;
        spec.cmd = vec!["sleep 120".to_string()];
        let sidecar = spec.name.clone();
        let _leftovers = DockerLeftovers {
            containers: vec![sidecar.clone()],
            network: None,
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            trigger.cancel();
        });
        let error = capture_dump(
            &docker,
            CaptureRequest {
                tool: REDIS_TOOL,
                spec,
                container_path: attempt.container_path(SIDECAR_DUMP_GZ),
                host_path: attempt.host_path(SIDECAR_DUMP_GZ),
                failure_log: None,
            },
            &cancel,
        )
        .await
        .expect_err("cancelled");

        assert!(
            matches!(error, DumpCaptureError::Cancelled { .. }),
            "{error}"
        );
        assert!(matches!(BackupError::from(error), BackupError::Cancelled));
        assert!(
            container_is_gone(&docker, &sidecar).await,
            "cancelled sidecar {sidecar} left behind"
        );
        assert!(!attempt.host_path(SIDECAR_DUMP_GZ).exists());
    }

    /// Shared `dump_capture` behaviour exercised with the Redis sidecar
    /// image: a failing command's diagnostics file (the PostgreSQL engine's
    /// redirected `pg_dumpall` stderr) is copied out of the container and
    /// attached to the export error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failure_log_is_copied_out_and_attached_to_the_export_error() {
        let Some(docker) =
            docker_or_skip("failure_log_is_copied_out_and_attached_to_the_export_error").await
        else {
            return;
        };
        let attempt = DumpAttempt::new(REDIS_TOOL, ENGINE_KEY, "failure-log").expect("attempt");
        let mut spec = logical_dump_spec(&attempt, 0, "unused", "");
        spec.network_mode = None;
        spec.cmd = vec![format!(
            "mkdir -p {dir} && echo 'tool diagnostic: role missing' > {dir}/err.log; exit 3",
            dir = attempt.container_dir()
        )];
        let sidecar = spec.name.clone();

        let error = capture_dump(
            &docker,
            CaptureRequest {
                tool: REDIS_TOOL,
                spec,
                container_path: attempt.container_path(SIDECAR_DUMP_GZ),
                host_path: attempt.host_path(SIDECAR_DUMP_GZ),
                failure_log: Some((
                    attempt.container_path("err.log"),
                    attempt.host_path("err.log"),
                )),
            },
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect_err("command exits 3");

        assert!(
            matches!(
                &error,
                DumpCaptureError::Export { exit_code: 3, stderr, .. }
                    if stderr.contains("tool diagnostic: role missing")
            ),
            "{error}"
        );
        assert!(container_is_gone(&docker, &sidecar).await);
    }
}
