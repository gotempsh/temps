//! `PostgresClusterEngine`: WAL-G–based backup of a Postgres cluster
//! (pg_auto_failover topology), implemented against `engine_v2::BackupEngine`.
//!
//! Same as [`PostgresWalgEngine`] but first locates the cluster's primary
//! member in `service_members` and runs `wal-g backup-push` against THAT
//! container's PGDATA. Replicas are never targeted directly.

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use futures::StreamExt;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{json, Value};
use tracing::{error, info, warn};

use super::ring_buffer::RingBuffer;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "postgres_cluster";

pub struct PostgresClusterDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct PostgresClusterEngine {
    deps: Arc<PostgresClusterDeps>,
}

impl PostgresClusterEngine {
    pub fn new(deps: PostgresClusterDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for PostgresClusterEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;
        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;

        // ── Locate the cluster primary ───────────────────────────────────────
        let primary_member = temps_entities::service_members::Entity::find()
            .filter(temps_entities::service_members::Column::ServiceId.eq(service_id))
            .filter(temps_entities::service_members::Column::Role.eq("primary"))
            .filter(temps_entities::service_members::Column::Status.eq("running"))
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error finding primary for service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!(
                    "no running primary found for cluster service {}",
                    service_id
                ),
            })?;
        let primary_container = primary_member.container_name.clone();
        info!(
            backup_id,
            service_id,
            container = %primary_container,
            ordinal = primary_member.ordinal,
            "PostgresClusterEngine: primary located",
        );

        // ── Service + S3 source ──────────────────────────────────────────────
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        let s3_source = v2_common::load_s3_source(deps.db.as_ref(), s3_source_id).await?;
        let s3_client = v2_common::build_s3_client(
            &s3_source,
            &deps.encryption_service,
            "postgres-cluster-engine",
        )?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let subpath_root = format!("external_services/postgres/{}", service.name);
        let bucket_path_clean = s3_source.bucket_path.trim_matches('/');
        let walg_prefix = if bucket_path_clean.is_empty() {
            format!(
                "s3://{}/{}/walg",
                s3_source.bucket_name,
                subpath_root.trim_matches('/'),
            )
        } else {
            format!(
                "s3://{}/{}/{}/walg",
                s3_source.bucket_name,
                bucket_path_clean,
                subpath_root.trim_matches('/'),
            )
        };
        let s3_list_prefix = if bucket_path_clean.is_empty() {
            format!("{}/walg/", subpath_root.trim_matches('/'))
        } else {
            format!(
                "{}/{}/walg/",
                bucket_path_clean,
                subpath_root.trim_matches('/'),
            )
        };

        // ── Decrypt creds + run wal-g ────────────────────────────────────────
        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let pg = load_postgres_params(&config_json);

        let access_key = deps
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt access key: {}", e),
            })?;
        let secret_key = deps
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt secret key: {}", e),
            })?;

        let container_endpoint = temps_providers::externalsvc::S3Credentials {
            access_key_id: access_key.clone(),
            secret_key: secret_key.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        }
        .resolve_endpoint_for_container(&deps.docker, &primary_container)
        .await;

        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_prefix),
            format!("AWS_ACCESS_KEY_ID={}", access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGUSER={}", pg.username),
            format!("PGPASSWORD={}", pg.password),
            format!("PGDATABASE={}", pg.database),
            "PGHOST=localhost".to_string(),
            "PGPORT=5432".to_string(),
            "WALG_UPLOAD_CONCURRENCY=4".to_string(),
            "WALG_UPLOAD_DISK_CONCURRENCY=1".to_string(),
            "WALG_UPLOAD_QUEUE=2".to_string(),
            "WALG_TAR_SIZE_THRESHOLD=134217728".to_string(),
        ];
        walg_env.extend(v2_common::walg_identity_env(&backup_uuid));
        if let Some(ep) = container_endpoint {
            let url = if ep.starts_with("http") {
                ep
            } else {
                format!("http://{}", ep)
            };
            walg_env.push(format!("AWS_ENDPOINT={}", url));
        }
        if s3_source.force_path_style.unwrap_or(true) {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let exec_result = run_walg_exec(
            &deps.docker,
            &primary_container,
            "wal-g backup-push $PGDATA",
            &walg_env,
            &ctx.cancel,
        )
        .await?;
        if exec_result.exit_code != 0 {
            return Err(BackupError::Failed {
                reason: format!(
                    "wal-g backup-push (cluster) exited with code {}. stderr: {}. stdout: {}",
                    exec_result.exit_code,
                    exec_result.stderr.trim(),
                    exec_result.stdout.trim(),
                ),
            });
        }
        if !exec_result.stderr.trim().is_empty() {
            info!(
                backup_id,
                container = %primary_container,
                "wal-g (cluster) stderr (warnings): {}",
                exec_result.stderr.trim(),
            );
        }

        let size_bytes =
            match list_total_s3_size(&s3_client, &s3_source.bucket_name, &s3_list_prefix).await {
                Ok(n) => Some(n),
                Err(e) => {
                    warn!(backup_id, error = %e, "cluster: could not compute size");
                    None
                }
            };
        let lsn = query_current_wal_lsn(&deps.docker, &primary_container, &pg)
            .await
            .unwrap_or_else(|e| {
                warn!(backup_id, error = %e, "cluster: could not query LSN");
                String::new()
            });

        let metadata_key = format!("{}metadata.json", s3_list_prefix);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            "",
            &walg_prefix,
            size_bytes.unwrap_or(0),
            s3_source_id,
            "lz4",
            Some(json!({
                "backup_tool": "wal-g",
                "topology": "cluster",
                "primary_container": primary_container,
                "primary_ordinal": primary_member.ordinal,
                "lsn": lsn,
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;
        v2_common::record_walg_identity(deps.db.as_ref(), backup_id, &backup_uuid).await?;

        info!(
            backup_id,
            %walg_prefix,
            ?size_bytes,
            "PostgresClusterEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: walg_prefix,
            size_bytes,
            compression: "lz4".to_string(),
        })
    }
}

// ── Local helpers (mirrors of postgres_walg.rs) ──────────────────────────────

struct PgParams {
    username: String,
    password: String,
    database: String,
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
            .or_else(|| params.get("db_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("postgres")
            .to_string(),
    }
}

struct ExecResult {
    exit_code: i64,
    stdout: String,
    stderr: String,
}

async fn run_walg_exec(
    docker: &bollard::Docker,
    container_name: &str,
    cmd: &str,
    env: &[String],
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<ExecResult, BackupError> {
    let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let exec = docker
        .create_exec(
            container_name,
            bollard::exec::CreateExecOptions {
                cmd: Some(vec!["sh", "-c", cmd]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                env: Some(env_refs),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("create exec on {}: {}", container_name, e),
        })?;
    let stream_result = docker
        .start_exec(
            &exec.id,
            Some(bollard::exec::StartExecOptions {
                detach: false,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("start exec on {}: {}", container_name, e),
        })?;

    let mut stdout = RingBuffer::with_capacity(64 * 1024);
    let mut stderr = RingBuffer::with_capacity(64 * 1024);

    if let StartExecResults::Attached { mut output, .. } = stream_result {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(BackupError::Cancelled);
                }
                item = output.next() => {
                    match item {
                        Some(Ok(LogOutput::StdOut { message })) => stdout.append(&message),
                        Some(Ok(LogOutput::StdErr { message })) => stderr.append(&message),
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            error!(container = container_name, "exec stream error: {}", e);
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
    }

    let inspect = docker
        .inspect_exec(&exec.id)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("inspect exec: {}", e),
        })?;
    let exit_code = inspect.exit_code.unwrap_or(-1);
    Ok(ExecResult {
        exit_code,
        stdout: stdout.into_string_lossy(),
        stderr: stderr.into_string_lossy(),
    })
}

/// Build the (cmd, env) pair for the `pg_current_wal_lsn()` probe.
///
/// Critically: **credentials never appear in `cmd`**. They go through env
/// (`PGUSER`, `PGPASSWORD`, `PGDATABASE`) so a password containing
/// `'; rm -rf /; #` can't break out of the shell. Tests below assert this
/// invariant — do not regress it.
fn build_lsn_exec_args(pg: &PgParams) -> (Vec<String>, Vec<String>) {
    let cmd = vec![
        "psql".to_string(),
        "-t".to_string(),
        "-c".to_string(),
        "SELECT pg_current_wal_lsn()".to_string(),
    ];
    let env = vec![
        format!("PGUSER={}", pg.username),
        format!("PGPASSWORD={}", pg.password),
        format!("PGDATABASE={}", pg.database),
    ];
    (cmd, env)
}

async fn query_current_wal_lsn(
    docker: &bollard::Docker,
    container_name: &str,
    pg: &PgParams,
) -> Result<String, BackupError> {
    let (cmd_owned, env_owned) = build_lsn_exec_args(pg);
    let cmd_refs: Vec<&str> = cmd_owned.iter().map(|s| s.as_str()).collect();
    let env_refs: Vec<&str> = env_owned.iter().map(|s| s.as_str()).collect();
    let exec = docker
        .create_exec(
            container_name,
            bollard::exec::CreateExecOptions {
                cmd: Some(cmd_refs),
                env: Some(env_refs),
                attach_stdout: Some(true),
                attach_stderr: Some(false),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("create exec for LSN: {}", e),
        })?;
    let output = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("start exec for LSN: {}", e),
        })?;
    let mut result = String::new();
    if let StartExecResults::Attached { mut output, .. } = output {
        while let Some(Ok(msg)) = output.next().await {
            if let LogOutput::StdOut { message } = msg {
                result.push_str(&String::from_utf8_lossy(&message));
            }
        }
    }
    Ok(result.trim().to_string())
}

async fn list_total_s3_size(
    client: &S3Client,
    bucket: &str,
    prefix: &str,
) -> Result<i64, BackupError> {
    let mut total: i64 = 0;
    let mut continuation: Option<String> = None;
    loop {
        let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(tok) = continuation {
            req = req.continuation_token(tok);
        }
        let resp = req.send().await.map_err(|e| BackupError::Failed {
            reason: format!("list objects: {}", e),
        })?;
        for obj in resp.contents() {
            total += obj.size().unwrap_or(0);
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
        } else {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the 0.1.0 hardening pass. See the matching
    /// tests in postgres_walg.rs for the full rationale — both files
    /// shared the same shell-injection vector and both share the same
    /// fix (credentials via env, never in the cmd vector).
    #[test]
    fn build_lsn_exec_args_keeps_credentials_out_of_cmd() {
        let pg = PgParams {
            username: "alice".to_string(),
            password: "p4ss'; rm -rf /; #".to_string(),
            database: "production".to_string(),
        };

        let (cmd, _env) = build_lsn_exec_args(&pg);

        for arg in &cmd {
            assert!(!arg.contains("alice"), "username leaked: {}", arg);
            assert!(!arg.contains("p4ss"), "password leaked: {}", arg);
            assert!(!arg.contains("production"), "database leaked: {}", arg);
        }
        // No `sh` wrapper — that wrapper plus shell-interpolated creds
        // was the vulnerable shape. (`-c` is fine here: it's `psql -c
        // <query>`, NOT `sh -c <shellstring>`.)
        assert!(!cmd.iter().any(|a| a == "sh"));
        assert!(!cmd.iter().any(|a| a == "bash"));
        assert_eq!(cmd.first().map(|s| s.as_str()), Some("psql"));
    }

    #[test]
    fn build_lsn_exec_args_passes_credentials_via_env_verbatim() {
        let pg = PgParams {
            username: "alice".to_string(),
            password: "p4ss'; rm -rf /; #".to_string(),
            database: "production".to_string(),
        };

        let (_cmd, env) = build_lsn_exec_args(&pg);
        assert!(env.contains(&"PGUSER=alice".to_string()));
        assert!(env.contains(&"PGPASSWORD=p4ss'; rm -rf /; #".to_string()));
        assert!(env.contains(&"PGDATABASE=production".to_string()));
        assert_eq!(env.len(), 3);
    }
}
