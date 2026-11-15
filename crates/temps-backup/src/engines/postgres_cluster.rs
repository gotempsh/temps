// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `PostgresClusterEngine`: WAL-G–based backup of a Postgres cluster
//! (pg_auto_failover topology), implemented against `engine_v2::BackupEngine`.
//!
//! Same as [`PostgresWalgEngine`] but first asks the cluster's
//! pg_auto_failover monitor which member is the current primary and runs
//! `wal-g backup-push` against THAT container's PGDATA. Replicas are never
//! targeted directly.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::postgres_walg::{exec_stdout_probe, run_walg_exec};
use super::v2_common;
use super::walg_size::load_backup_size_bytes;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "postgres_cluster";

/// Where every cluster data node keeps its data. The member containers run
/// pg_autoctl with this path, which is NOT the image's `PGDATA` env
/// (`/var/lib/postgresql/<major>/docker`), so `$PGDATA` cannot be trusted.
const CLUSTER_PGDATA: &str = "/var/lib/postgresql/pgdata";

/// The port PostgreSQL listens on inside a member container. Each member
/// listens on its own stored port (pg_autoctl `--pgport`), not 5432.
fn member_pg_port(member: &temps_providers::ServiceMemberInfo) -> i32 {
    member.port.unwrap_or(5432)
}

pub struct PostgresClusterDeps {
    pub db: Arc<DatabaseConnection>,
    /// Resolves the live primary through the cluster's monitor.
    pub external_service_manager: Arc<temps_providers::ExternalServiceManager>,
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
        // Ask the monitor: `service_members.role` is `replica` for every data
        // node, so it cannot say which one is writable.
        let primary_member = deps
            .external_service_manager
            .get_cluster_primary_member(service_id)
            .await
            // No trustworthy primary right now (monitor unreachable, failover
            // in progress) can clear up on its own, so let the executor retry.
            .map_err(|e| BackupError::Failed {
                reason: format!(
                    "could not locate the primary of cluster service {}: {}",
                    service_id, e
                ),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!(
                    "service {} is not a PostgreSQL cluster; the cluster backup engine cannot back it up",
                    service_id
                ),
            })?;
        let primary_container = primary_member.container_name.clone();
        let pg_port = member_pg_port(&primary_member);
        info!(
            backup_id,
            service_id,
            container = %primary_container,
            ordinal = primary_member.ordinal,
            pg_port,
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

        let session_token = v2_common::decrypt_session_token(&s3_source, &deps.encryption_service)?;

        let container_endpoint = temps_providers::externalsvc::S3Credentials {
            access_key_id: access_key.clone(),
            secret_key: secret_key.clone(),
            session_token: session_token.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.effective_path_style(true).force_path_style,
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
            format!("PGPORT={}", pg_port),
            format!("PGDATA={}", CLUSTER_PGDATA),
            "WALG_UPLOAD_CONCURRENCY=4".to_string(),
            "WALG_UPLOAD_DISK_CONCURRENCY=1".to_string(),
            "WALG_UPLOAD_QUEUE=2".to_string(),
            "WALG_TAR_SIZE_THRESHOLD=134217728".to_string(),
        ];
        // Absent unless this source holds a temporary credential.
        walg_env.extend(temps_providers::externalsvc::aws_session_token_env(
            session_token.as_deref(),
        ));
        walg_env.extend(v2_common::walg_identity_env(&backup_uuid));
        if let Some(ep) = container_endpoint {
            let url = temps_core::s3_endpoint::endpoint_url(&ep);
            walg_env.push(format!("AWS_ENDPOINT={}", url));
        }
        if s3_source.effective_path_style(true).force_path_style {
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

        let size_bytes = match load_backup_size_bytes(
            &s3_client,
            &s3_source.bucket_name,
            &s3_list_prefix,
            &exec_result.stdout,
            &exec_result.stderr,
            &backup_uuid,
        )
        .await
        {
            Ok(size) => Some(size),
            Err(error) => {
                warn!(backup_id, error = %error, "cluster: could not read per-backup size");
                None
            }
        };
        let lsn = query_current_wal_lsn(&deps.docker, &primary_container, &pg, pg_port)
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

/// Build the (cmd, env) pair for the `pg_current_wal_lsn()` probe.
///
/// Critically: **credentials never appear in `cmd`**. They go through env
/// (`PGUSER`, `PGPASSWORD`, `PGDATABASE`) so a password containing
/// `'; rm -rf /; #` can't break out of the shell. Tests below assert this
/// invariant — do not regress it.
fn build_lsn_exec_args(pg: &PgParams, pg_port: i32) -> (Vec<String>, Vec<String>) {
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
        format!("PGPORT={}", pg_port),
    ];
    (cmd, env)
}

async fn query_current_wal_lsn(
    docker: &bollard::Docker,
    container_name: &str,
    pg: &PgParams,
    pg_port: i32,
) -> Result<String, BackupError> {
    let (cmd, env) = build_lsn_exec_args(pg, pg_port);
    exec_stdout_probe(docker, container_name, &cmd, &env).await
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

        let (cmd, _env) = build_lsn_exec_args(&pg, 6012);

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

        let (_cmd, env) = build_lsn_exec_args(&pg, 6012);
        assert!(env.contains(&"PGUSER=alice".to_string()));
        assert!(env.contains(&"PGPASSWORD=p4ss'; rm -rf /; #".to_string()));
        assert!(env.contains(&"PGDATABASE=production".to_string()));
        assert!(env.contains(&"PGPORT=6012".to_string()));
        assert_eq!(env.len(), 4);
    }

    /// Cluster members listen on their own stored port; dialling 5432 made
    /// every cluster backup fail with "connection refused".
    #[test]
    fn member_pg_port_uses_the_members_stored_port() {
        let mut member = temps_providers::ServiceMemberInfo {
            id: 2,
            role: "replica".to_string(),
            node_id: None,
            container_name: "orders-ha-2".to_string(),
            hostname: Some("orders-ha-2".to_string()),
            port: Some(6012),
            status: "running".to_string(),
            ordinal: 2,
            compute_ip: None,
            provisioning_step: None,
            provisioning_error: None,
            live_state: Some("primary".to_string()),
        };
        assert_eq!(member_pg_port(&member), 6012);

        member.port = None;
        assert_eq!(member_pg_port(&member), 5432);
    }
}
