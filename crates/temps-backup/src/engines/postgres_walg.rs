// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `PostgresWalgEngine`: WAL-G–based backup of an external Postgres service,
//! implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Validate the external service + S3 source and its archive pin.
//! 2. The node half
//!    (`temps_providers::externalsvc::postgres_walg_backup`): turn
//!    continuous archiving on (or refresh it), `wal-g backup-push $PGDATA`
//!    in the container, read the WAL LSN. WAL-G uploads straight to S3.
//! 3. Read the new backup's WAL-G stop-sentinel to obtain its compressed size.
//! 4. Write the `metadata.json` companion and record the WAL-G identity.
//!
//! The node half is one function. For a service on this host it runs here
//! with this host's Docker; for a service on a worker node, that node's
//! agent runs it with the node's Docker (ADR-099). Everything else, and so
//! every artifact, is the same for both.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use temps_docker_util::{
    exec_capture, DockerUtilError, ExecCaptureOptions, ExitCodePolicy, OutputLimit,
    StreamErrorPolicy,
};
use temps_providers::externalsvc::postgres_walg_backup::{
    self as node_half, ArchiveRecreate, WalgBaseBackup, WalgBaseBackupError, WalgBaseBackupOutcome,
};
use temps_providers::remote_service_client::PostgresWalgBackupResult;
use tracing::{info, warn};

use super::dispatch::service_container_name;
use super::v2_common;
use super::walg_size::load_backup_size_bytes;
use crate::execution::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "postgres_walg";

pub struct PostgresWalgDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// This host's Docker, for services on this host. `None` on a server
    /// without one (`--profile control-plane`), which backs up only
    /// services on worker nodes.
    pub docker: Option<bollard::Docker>,
    /// Reaches a worker service's node through its agent.
    pub manager: Arc<temps_providers::ExternalServiceManager>,
    /// Derives a worker's S3 credential (ADR-102).
    pub worker_credentials: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
}

pub struct PostgresWalgEngine {
    deps: Arc<PostgresWalgDeps>,
}

impl PostgresWalgEngine {
    pub fn new(deps: PostgresWalgDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for PostgresWalgEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;
        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;

        // ── Load service + S3 source ─────────────────────────────────────────
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        temps_providers::continuous_archive::ensure_continuous_archive_source_pin(
            deps.db.as_ref(),
            &service,
            s3_source_id,
            "WAL-G continuous archiving",
        )
        .await
        .map_err(|e| match e {
            temps_providers::continuous_archive::ContinuousArchiveError::Mismatch(reason) => {
                BackupError::PermanentFailure { reason }
            }
            temps_providers::continuous_archive::ContinuousArchiveError::Database {
                context,
                source,
            } => BackupError::Failed {
                reason: format!("{context}: {source}"),
            },
        })?;

        let s3_source = v2_common::load_s3_source(deps.db.as_ref(), s3_source_id).await?;
        let s3_client = v2_common::build_s3_client(
            &s3_source,
            &deps.encryption_service,
            "postgres-walg-engine",
        )?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        // WAL-G layout: <bucket_path>/external_services/postgres/<svc>/walg
        let walg_prefix = node_half::walg_prefix(
            &s3_source.bucket_name,
            &s3_source.bucket_path,
            &service.name,
        );
        let s3_list_prefix = node_half::walg_key_prefix(&s3_source.bucket_path, &service.name);

        info!(backup_id, %walg_prefix, "PostgresWalgEngine: starting wal-g backup-push");

        // A manual run records whether its caller confirmed a restart;
        // scheduled runs (no caller to ask) do not set it and may restart.
        let allow_restart = ctx
            .params
            .get("allow_restart")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let outcome = match super::workload_job::worker_node(&service) {
            Some(node_id) => {
                backup_on_worker(
                    &deps,
                    ctx,
                    &service,
                    node_id,
                    s3_source_id,
                    &s3_source.bucket_path,
                    &backup_uuid,
                    allow_restart,
                )
                .await?
            }
            None => {
                backup_on_this_host(
                    &deps,
                    ctx,
                    &service,
                    &s3_source,
                    &backup_uuid,
                    allow_restart,
                )
                .await?
            }
        };
        if outcome.restarted {
            warn!(
                backup_id,
                service_id,
                service = %service.name,
                "PostgresWalgEngine: PostgreSQL was restarted to enable WAL archiving \
                 (archive_mode was off); its data was verified intact before the base backup"
            );
        }

        // ── Compute size ─────────────────────────────────────────────────────
        let size_bytes = match load_backup_size_bytes(
            &s3_client,
            &s3_source.bucket_name,
            &s3_list_prefix,
            &outcome.stdout,
            &outcome.stderr,
            &backup_uuid,
        )
        .await
        {
            Ok(size) => Some(size),
            Err(error) => {
                warn!(backup_id, error = %error, "walg: could not read per-backup size");
                None
            }
        };

        // ── Metadata ─────────────────────────────────────────────────────────
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
                "database_restarted_to_enable_archiving": outcome.restarted,
                "lsn": outcome.lsn.unwrap_or_default(),
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;
        v2_common::record_walg_identity(deps.db.as_ref(), backup_id, &backup_uuid).await?;

        info!(
            backup_id,
            %walg_prefix,
            ?size_bytes,
            "PostgresWalgEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: walg_prefix,
            size_bytes,
            compression: "lz4".to_string(),
        })
    }
}

/// The node half, here, against this host's Docker.
async fn backup_on_this_host(
    deps: &PostgresWalgDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    s3_source: &temps_entities::s3_sources::Model,
    backup_uuid: &str,
    allow_restart: bool,
) -> Result<WalgBaseBackupOutcome, BackupError> {
    let Some(docker) = deps.docker.clone() else {
        return Err(BackupError::PermanentFailure {
            reason: format!(
                "Service {} ('{}') runs on this server, which has no Docker daemon to run its \
                 WAL-G backup in",
                service.id, service.name
            ),
        });
    };
    let decrypt = |field: &str, value: &str| {
        deps.encryption_service
            .decrypt_string(value)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt {field} of S3 source {}: {e}", s3_source.id),
            })
    };
    let s3 = temps_providers::externalsvc::S3Credentials {
        access_key_id: decrypt("access key", &s3_source.access_key_id)?,
        secret_key: decrypt("secret key", &s3_source.secret_key)?,
        session_token: v2_common::decrypt_session_token(s3_source, &deps.encryption_service)?,
        region: s3_source.region.clone(),
        endpoint: s3_source.endpoint.clone(),
        bucket_name: s3_source.bucket_name.clone(),
        bucket_path: s3_source.bucket_path.clone(),
        force_path_style: s3_source.force_path_style.unwrap_or(true),
    };
    let config_json = deps
        .encryption_service
        .decrypt_string(service.config.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|_| "{}".to_string());
    let service_config = temps_providers::externalsvc::ServiceConfig {
        name: service.name.clone(),
        service_type: temps_providers::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::from_str(&config_json).unwrap_or(Value::Null),
    };
    node_half::walg_base_backup(
        Arc::new(docker),
        WalgBaseBackup {
            service_id: service.id,
            container_name: service_container_name(service),
            service_config,
            s3,
            backup_uuid: backup_uuid.to_string(),
            allow_restart,
        },
        ArchiveRecreate::Provider,
        &ctx.cancel,
    )
    .await
    .map_err(backup_error)
}

/// The node half, on the worker that runs the service, by its agent.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per input of the worker's request; a struct would only rename them"
)]
async fn backup_on_worker(
    deps: &PostgresWalgDeps,
    ctx: &BackupContext,
    service: &temps_entities::external_services::Model,
    node_id: i32,
    s3_source_id: i32,
    bucket_path: &str,
    backup_uuid: &str,
    allow_restart: bool,
) -> Result<WalgBaseBackupOutcome, BackupError> {
    // The base backup writes this credential into `walg.env`, where
    // `archive_command` keeps using it: it is the archiving credential,
    // limited to the service's own WAL-G repository and refreshed by the
    // control plane (ADR-102).
    let mint = || {
        super::worker_agent::scoped_destination(
            &deps.worker_credentials,
            deps.db.as_ref(),
            s3_source_id,
            temps_providers::worker_s3_scope::WorkerS3Grant::archive(
                service.id,
                node_id,
                temps_providers::externalsvc::postgres_walg_backup::walg_key_prefix(
                    bucket_path,
                    &service.name,
                ),
            ),
            "backup",
        )
    };
    let destination = mint().await?;
    // A node already running its maximum of backups and restores is waited
    // for (bounded, within this backup's own runtime limit) instead of
    // failing the backup; a try after a wait gets a credential minted after
    // it.
    let request = crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        destination,
        mint,
        |destination| {
            deps.manager.walg_backup_worker_postgres(
                service.id,
                destination,
                bucket_path.to_string(),
                backup_uuid.to_string(),
                allow_restart,
            )
        },
        |wait| super::worker_agent::wait_for_node_slot_minting(ctx.backup_id, service.id, wait),
    );
    let result = tokio::select! {
        // Dropping the request ends it on the node too.
        _ = ctx.cancel.cancelled() => return Err(BackupError::Cancelled),
        result = request => result?,
    }
    .map_err(|e| BackupError::Failed {
        reason: format!(
            "WAL-G backup of service {} ('{}') on node {node_id}: {e}",
            service.id, service.name
        ),
    })?;
    worker_outcome(service, node_id, result)
}

/// A worker's answer, as the outcome or error of the same run here.
fn worker_outcome(
    service: &temps_entities::external_services::Model,
    node_id: i32,
    result: PostgresWalgBackupResult,
) -> Result<WalgBaseBackupOutcome, BackupError> {
    match result {
        PostgresWalgBackupResult::Completed {
            restarted,
            stdout,
            stderr,
            lsn,
        } => Ok(WalgBaseBackupOutcome {
            restarted,
            stdout,
            stderr,
            lsn,
        }),
        PostgresWalgBackupResult::Refused { reason } => Err(BackupError::PermanentFailure {
            reason: format!(
                "WAL-G backup of service {} ('{}') on node {node_id}: {reason}",
                service.id, service.name
            ),
        }),
        PostgresWalgBackupResult::Failed { reason } => Err(BackupError::Failed {
            reason: format!(
                "WAL-G backup of service {} ('{}') on node {node_id}: {reason}",
                service.id, service.name
            ),
        }),
    }
}

/// The node half's error as the executor's: a refusal is final, a
/// cancellation is a cancellation, the rest may succeed on a retry.
fn backup_error(error: WalgBaseBackupError) -> BackupError {
    match error {
        WalgBaseBackupError::Cancelled { .. } => BackupError::Cancelled,
        error if error.is_refusal() => BackupError::PermanentFailure {
            reason: error.to_string(),
        },
        error => BackupError::Failed {
            reason: error.to_string(),
        },
    }
}

pub(crate) struct ExecResult {
    pub(crate) exit_code: i64,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Run `sh -c <cmd>` inside `container_name` with the given env, keeping the
/// last 64 KiB of stdout and stderr. Bails early on cancel. The node half's
/// exec, for the other WAL-G engines on this host.
pub(crate) async fn run_walg_exec(
    docker: &bollard::Docker,
    container_name: &str,
    cmd: &str,
    env: &[String],
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<ExecResult, BackupError> {
    match node_half::run_walg_exec(docker, container_name, cmd, env, cancel).await {
        Ok(output) => Ok(ExecResult {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Err(WalgBaseBackupError::Exec {
            container,
            operation,
            reason,
        }) => Err(BackupError::Failed {
            reason: format!("{operation} on {container}: {reason}"),
        }),
        Err(error) => Err(backup_error(error)),
    }
}

/// Run a read-only probe and return its trimmed stdout. Stops at the first
/// stream error, and the exit code is not read: an empty or partial answer is
/// for the caller to judge. Shared by both walg engines.
pub(crate) async fn exec_stdout_probe(
    docker: &bollard::Docker,
    container_name: &str,
    cmd: &[String],
    env: &[String],
) -> Result<String, BackupError> {
    let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let output = exec_capture(
        docker,
        container_name,
        bollard::exec::CreateExecOptions {
            cmd: Some(cmd_refs),
            env: Some(env_refs),
            attach_stdout: Some(true),
            attach_stderr: Some(false),
            ..Default::default()
        },
        ExecCaptureOptions::separate(OutputLimit::Unbounded, OutputLimit::Head(0))
            .stream_errors(StreamErrorPolicy::Stop)
            .exit_code(ExitCodePolicy::NotRead),
    )
    .await
    .map_err(|error| match error {
        DockerUtilError::ExecCreate { source, .. } => BackupError::Failed {
            reason: format!("create exec for LSN: {}", source),
        },
        other => BackupError::Failed {
            reason: format!("start exec for LSN: {}", other.cause_text()),
        },
    })?;
    Ok(output.stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A running container to exec in, or `None` if no Docker daemon answers.
    /// The caller removes it with [`remove_exec_container`].
    async fn exec_container(test: &str) -> Option<(bollard::Docker, String)> {
        use bollard::query_parameters::{CreateContainerOptionsBuilder, StartContainerOptions};

        let docker = bollard::Docker::connect_with_local_defaults().ok()?;
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping {test}");
            return None;
        }
        temps_docker_util::pull_image(&docker, temps_docker_util::PullSpec::new("busybox:1.36"))
            .await
            .expect("pull the test image");
        let name = format!("temps-backup-test-{test}-{}", uuid::Uuid::new_v4().simple());
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                bollard::models::ContainerCreateBody {
                    image: Some("busybox:1.36".to_string()),
                    cmd: Some(vec!["sleep".to_string(), "300".to_string()]),
                    ..Default::default()
                },
            )
            .await
            .expect("create the test container");
        if let Err(error) = docker
            .start_container(&name, None::<StartContainerOptions>)
            .await
        {
            // The panic ends the test before its own cleanup can run.
            remove_exec_container(&docker, &name).await;
            panic!("start the test container {name}: {error}");
        }
        Some((docker, name))
    }

    async fn remove_exec_container(docker: &bollard::Docker, name: &str) {
        let _ = docker
            .remove_container(
                name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    /// A verbose `wal-g` can print gigabytes: each stream keeps its last 64 KiB,
    /// which is where a failure explains itself.
    #[tokio::test]
    async fn run_walg_exec_keeps_the_last_64k_of_each_stream() {
        let Some((docker, name)) = exec_container("walg-cap").await else {
            return;
        };
        let script = "i=0; while [ $i -lt 12000 ]; do echo out-$i; echo err-$i >&2; \
                      i=$((i+1)); done; exit 4";
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = run_walg_exec(&docker, &name, script, &[], &cancel).await;
        remove_exec_container(&docker, &name).await;

        let Ok(output) = result else {
            panic!("exec failed");
        };
        assert_eq!(output.exit_code, 4);
        assert_eq!(output.stdout.len(), 64 * 1024);
        assert!(output.stdout.ends_with("out-11999\n"));
        assert_eq!(output.stderr.len(), 64 * 1024);
        assert!(output.stderr.ends_with("err-11999\n"));
    }

    #[tokio::test]
    async fn run_walg_exec_passes_env_and_returns_on_cancel() {
        let Some((docker, name)) = exec_container("walg-cancel").await else {
            return;
        };
        let live = tokio_util::sync::CancellationToken::new();
        let echoed = run_walg_exec(
            &docker,
            &name,
            "echo \"$WALG_VALUE\"",
            &["WALG_VALUE=from-env".to_string()],
            &live,
        )
        .await;

        let cancel = tokio_util::sync::CancellationToken::new();
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            canceller.cancel();
        });
        let cancelled = run_walg_exec(&docker, &name, "sleep 120", &[], &cancel).await;
        remove_exec_container(&docker, &name).await;

        let Ok(echoed) = echoed else {
            panic!("exec failed");
        };
        assert_eq!(echoed.stdout, "from-env\n");
        assert!(matches!(cancelled, Err(BackupError::Cancelled)));
    }

    #[tokio::test]
    async fn run_walg_exec_on_a_missing_container_says_create() {
        let Some((docker, name)) = exec_container("walg-missing").await else {
            return;
        };
        remove_exec_container(&docker, &name).await;

        let cancel = tokio_util::sync::CancellationToken::new();
        match run_walg_exec(&docker, &name, "true", &[], &cancel).await {
            Err(BackupError::Failed { reason }) => {
                assert!(
                    reason.starts_with(&format!("create exec on {name}: ")),
                    "{reason}"
                );
            }
            _ => panic!("expected a Failed error for a missing container"),
        }
    }

    /// The probe answers with trimmed stdout. Stderr is not attached and a
    /// non-zero exit is not an error: the caller judges the answer.
    #[tokio::test]
    async fn exec_stdout_probe_returns_trimmed_stdout_whatever_the_exit_code() {
        let Some((docker, name)) = exec_container("walg-probe").await else {
            return;
        };
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo \" $PROBE_VALUE \"; echo noise >&2; exit 2".to_string(),
        ];
        let env = vec!["PROBE_VALUE=0/16B6C50".to_string()];
        let result = exec_stdout_probe(&docker, &name, &cmd, &env).await;
        remove_exec_container(&docker, &name).await;

        let Ok(answer) = result else {
            panic!("probe failed");
        };
        assert_eq!(answer, "0/16B6C50");
    }

    fn worker_service(node_id: i32) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 8,
            name: "orders".to_string(),
            service_type: "postgres".to_string(),
            topology: "standalone".to_string(),
            status: "running".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            node_id: Some(node_id),
            version: None,
            slug: None,
            config: None,
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    /// A worker's answer becomes the same outcome or error the run here
    /// would have produced: a refusal is final, a failure retries.
    #[test]
    fn a_worker_answer_is_the_outcome_of_the_same_run() {
        let service = worker_service(2);
        let completed = worker_outcome(
            &service,
            2,
            PostgresWalgBackupResult::Completed {
                restarted: true,
                stdout: "out".to_string(),
                stderr: "err".to_string(),
                lsn: Some("0/3000060".to_string()),
            },
        );
        assert_eq!(
            completed.ok(),
            Some(WalgBaseBackupOutcome {
                restarted: true,
                stdout: "out".to_string(),
                stderr: "err".to_string(),
                lsn: Some("0/3000060".to_string()),
            })
        );
        let refused = worker_outcome(
            &service,
            2,
            PostgresWalgBackupResult::Refused {
                reason: "not confirmed".to_string(),
            },
        )
        .expect_err("refused");
        assert!(refused.is_permanent(), "{refused:?}");
        assert!(refused.to_string().contains("on node 2"), "{refused}");
        let failed = worker_outcome(
            &service,
            2,
            PostgresWalgBackupResult::Failed {
                reason: "exit 1".to_string(),
            },
        )
        .expect_err("failed");
        assert!(!failed.is_permanent(), "{failed:?}");
    }

    #[test]
    fn only_a_refusal_of_the_node_half_is_final() {
        let not_confirmed = backup_error(WalgBaseBackupError::RestartNotConfirmed {
            service_id: 7,
            service: "app".to_string(),
        });
        assert!(not_confirmed.is_permanent());
        assert!(not_confirmed.to_string().contains("not confirmed"));
        let probe = backup_error(WalgBaseBackupError::RestartProbe {
            service_id: 7,
            service: "app".to_string(),
            reason: "connection refused".to_string(),
        });
        assert!(!probe.is_permanent());
        assert!(probe.to_string().contains("connection refused"));
        assert!(matches!(
            backup_error(WalgBaseBackupError::Cancelled {
                container: "postgres-app".to_string()
            }),
            BackupError::Cancelled
        ));
    }
}
