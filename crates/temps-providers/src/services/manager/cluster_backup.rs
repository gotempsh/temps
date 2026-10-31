// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G backup and restore of PostgreSQL clusters.

use crate::services::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use temps_entities::{external_service_backups, external_services, service_members};
use tracing::{debug, info, warn};

/// Build the env-file lines that WAL-G needs for both `backup-push`
/// and `wal-push`. Same shape used by the standalone postgres path —
/// kept here so the cluster path produces an identical file (any
/// post-failover archiver that sources it works the same way).
fn build_walg_env(
    creds: &crate::S3Credentials,
    walg_s3_prefix: &str,
    resolved_endpoint: Option<&str>,
) -> Result<Vec<String>, String> {
    fn export(name: &str, value: &str) -> Result<String, String> {
        if value.contains(['\r', '\n']) {
            return Err(format!("{name} must not contain CR or LF characters"));
        }
        Ok(format!(
            "export {name}={}",
            crate::externalsvc::postgres::shell_escape(value)
        ))
    }

    let mut env = vec![
        export("WALG_S3_PREFIX", walg_s3_prefix)?,
        export("AWS_ACCESS_KEY_ID", &creds.access_key_id)?,
        export("AWS_SECRET_ACCESS_KEY", &creds.secret_key)?,
        export("AWS_REGION", &creds.region)?,
        // Pin the WAL segment compression to lz4 — fast, low CPU,
        // matches the standalone path. Operators who want zstd can
        // override via service parameters in a follow-up.
        "export WALG_COMPRESSION_METHOD='lz4'".to_string(),
    ];
    // Only for a temporary (STS-style) credential. A long-lived
    // operator-configured credential emits no AWS_SESSION_TOKEN at all —
    // exporting an empty one would be signed and rejected. The empty-string
    // filter is what makes that true for `Some("")` as well, matching
    // `aws_session_token_env` and `mc_host_credential`.
    if let Some(session_token) = creds
        .session_token
        .as_deref()
        .filter(|token| !token.is_empty())
    {
        env.push(export("AWS_SESSION_TOKEN", session_token)?);
    }
    if let Some(endpoint) = resolved_endpoint {
        env.push(export("AWS_ENDPOINT", endpoint)?);
    }
    if creds.force_path_style {
        env.push("export AWS_S3_FORCE_PATH_STYLE='true'".to_string());
    }
    Ok(env)
}

// ---------------------------------------------------------------------------
// Runtime + stats DTOs (response shapes for the runtime/stats endpoints).
//
// These surface raw container state so the UI can warn about restarts and
// OOM kills, plus live CPU/memory usage. Cluster services return a list
// of members; standalone services return a single entry with role="standalone".
// ---------------------------------------------------------------------------

impl ExternalServiceManager {
    /// Run a WAL-G basebackup against a Postgres HA cluster.
    ///
    /// Routes the backup to the **current primary** (resolved from
    /// `service_members` — kept fresh by the role reconciler). Writes
    /// the WAL-G env file to **every running data member** so failover
    /// doesn't break continuous WAL archiving — the new primary picks
    /// up the same env file and `archive_command` (which lives in
    /// `postgresql.auto.conf`, replicated through streaming).
    ///
    /// Writes an `external_service_backups` row tied to `backup_id`,
    /// transitioning it through `running` → `completed`/`failed` so
    /// the standard backup listing/restore flow works for clusters
    /// without further special-casing.
    ///
    /// Returns the WAL-G S3 prefix on success — same shape as the
    /// standalone postgres path so the rest of `temps-backup` doesn't
    /// have to special-case clusters.
    ///
    /// Designed to be called from `BackupService::backup_external_service`
    /// when `service.topology == "cluster"`.
    pub async fn backup_postgres_cluster(
        &self,
        service: &external_services::Model,
        s3_credentials: &crate::S3Credentials,
        subpath_root: &str,
        backup_id: i32,
    ) -> Result<crate::externalsvc::BackupOutcome, ExternalServiceError> {
        info!(
            service_id = service.id,
            service_name = %service.name,
            "Starting WAL-G basebackup for cluster"
        );

        if service.topology != "cluster" || service.service_type != "postgres" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: format!(
                    "backup_postgres_cluster requires topology='cluster' and service_type='postgres' (got {}/{})",
                    service.topology, service.service_type,
                ),
            });
        }

        let members = self.get_service_members(service.id).await?;
        let health = self.cluster_health(service).await;
        // Resolve the sole healthy, fresh writer back through persisted member
        // identity. Monitor state is authoritative for role, but never for a
        // credential destination.
        let primary = healthy_writable_primary_nodename(&health)
            .and_then(|nodename| trusted_primary_member(&members, nodename))
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: "Cannot run backup: cluster has no unique healthy, recently reporting primary (monitor unreachable, election incomplete, or primary state ambiguous)".to_string(),
            })?;

        // Write the external_service_backups row up front so the UI's
        // backup listing reflects an in-progress backup. Updated to
        // completed/failed at the end.
        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": service.name,
            "topology": "cluster",
            "backup_tool": "wal-g",
            "primary_member_id": primary.id,
            "primary_container": primary.container_name,
        });
        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(service.id),
            backup_id: Set(backup_id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set(String::new()),
            metadata: Set(metadata),
            compression_type: Set("lz4".to_string()),
            created_by: Set(0),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        // Single stable WAL-G prefix per cluster. WAL-G needs every
        // basebackup + WAL segment under the same prefix so
        // backup-fetch + wal-fetch can find each other.
        let walg_prefix = format!(
            "s3://{}/{}/walg",
            s3_credentials.bucket_name,
            subpath_root.trim_matches('/'),
        );

        // Resolve the S3 endpoint relative to the primary's container.
        // Important for self-hosted MinIO setups where the endpoint
        // looks like `localhost:9000` from the host but needs to be
        // a Docker-routable address from inside the container.
        let resolved_endpoint = if primary.node_id.is_none() {
            let docker = self.require_docker()?;
            s3_credentials
                .resolve_endpoint_for_container(&docker, &primary.container_name)
                .await
        } else {
            // Remote primary — we can't introspect the worker's docker
            // from here. Use the configured endpoint as-is; the agent
            // sees the same network the user supplied.
            s3_credentials.endpoint.clone()
        };

        let walg_env = build_walg_env(s3_credentials, &walg_prefix, resolved_endpoint.as_deref())
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
            service_id: service.id,
            reason: format!("Invalid WAL-G S3 configuration: {reason}"),
        })?;

        // Write walg.env to every running data member. Cheap (kilobytes
        // per file) and means failover doesn't lose archiving — the
        // new primary already has the credentials. This also covers
        // our case where ALTER SYSTEM is replicated via the data
        // directory (postgresql.auto.conf) but the env file isn't.
        for m in members
            .iter()
            .filter(|m| !is_role_monitor(&m.role) && m.status == "running")
        {
            if let Err(e) = self.write_walg_env_file(m, &walg_env).await {
                // Don't fail the backup over an env file on a non-primary;
                // the primary's the one that matters now. Failover would
                // lose archiving on this node, but the next backup will
                // re-write it.
                warn!(
                    service_id = service.id,
                    member_id = m.id,
                    node_id = ?m.node_id,
                    error = %e,
                    "Failed to write walg.env to cluster member; continuing"
                );
            }
        }

        // Run the basebackup against the primary.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            // Source the env file so wal-g picks up the credentials.
            // Same script the standalone enable_wal_archiving uses for
            // archive_command — keeps backup and archive pointing at
            // the same prefix.
            ". /var/lib/postgresql/walg.env && wal-g backup-push /var/lib/postgresql/pgdata"
                .to_string(),
        ];

        info!(
            service_id = service.id,
            primary_container = %primary.container_name,
            primary_node_id = ?primary.node_id,
            walg_prefix,
            "Running wal-g backup-push on primary"
        );

        let (exit_code, stdout, stderr) =
            self.exec_in_member(primary, cmd, Some("postgres")).await?;

        if exit_code != 0 {
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            let err_msg = format!(
                "wal-g backup-push failed on '{}' (exit {}): {}",
                primary.container_name,
                exit_code,
                detail.trim()
            );
            // Mark the row failed before returning so the UI shows it.
            let mut update: external_service_backups::ActiveModel = backup_record.into();
            update.state = Set("failed".to_string());
            update.error_message = Set(Some(err_msg.clone()));
            update.finished_at = Set(Some(Utc::now()));
            let _ = update.update(self.db.as_ref()).await;
            return Err(ExternalServiceError::InternalError { reason: err_msg });
        }

        info!(
            service_id = service.id,
            walg_prefix, "wal-g basebackup completed; enabling continuous WAL archiving"
        );

        // Enable archive_command via ALTER SYSTEM. Idempotent — if it's
        // already set to the same value, postgres just rewrites the
        // line. The setting lives in postgresql.auto.conf which IS
        // streamed to replicas, so a future failover doesn't need this
        // step repeated.
        if let Err(e) = self.enable_cluster_wal_archiving(primary, service).await {
            // Don't fail the backup — the basebackup is on S3. WAL
            // archiving will be off until the next backup retries it.
            warn!(
                service_id = service.id,
                error = %e,
                "Basebackup succeeded but enabling continuous WAL archiving failed"
            );
        }

        // Compute size by listing the WAL-G prefix in S3.
        let s3_list_prefix = format!("{}/walg/", subpath_root.trim_matches('/'));
        let s3_client = s3_credentials.build_s3_client().await;
        let size_bytes = match crate::externalsvc::s3_util::list_total_size(
            &s3_client,
            &s3_credentials.bucket_name,
            &s3_list_prefix,
        )
        .await
        {
            Ok(n) => Some(n),
            Err(e) => {
                warn!(
                    service_id = service.id,
                    error = %e,
                    "Cluster backup succeeded but failed to compute size from S3"
                );
                None
            }
        };

        // Success — mark the row completed with the prefix and size.
        let mut update: external_service_backups::ActiveModel = backup_record.into();
        update.state = Set("completed".to_string());
        update.s3_location = Set(walg_prefix.clone());
        update.finished_at = Set(Some(Utc::now()));
        update.size_bytes = Set(size_bytes);
        if let Err(e) = update.update(self.db.as_ref()).await {
            warn!(
                service_id = service.id,
                error = %e,
                "Backup succeeded but failed to mark external_service_backups row as completed"
            );
        }

        Ok(crate::externalsvc::BackupOutcome::new(
            walg_prefix,
            size_bytes,
        ))
    }

    /// Write `/var/lib/postgresql/walg.env` to a single cluster member.
    /// The file is sourced by both `archive_command` (every WAL
    /// segment) and `backup-push` (basebackups), so both paths use
    /// identical credentials without needing them in the postgres
    /// process environment (which would leak into pg_dump output).
    async fn write_walg_env_file(
        &self,
        member: &ServiceMemberInfo,
        env_lines: &[String],
    ) -> Result<(), ExternalServiceError> {
        // chmod 0600 — credentials. Owned by postgres because that's the
        // user the archiver + backup commands run as.
        let env_body = env_lines.join("\n");
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "umask 077 && cat > /var/lib/postgresql/walg.env <<'WALG_ENV_EOF'\n{}\nWALG_ENV_EOF\n\
                 chown postgres:postgres /var/lib/postgresql/walg.env && \
                 chmod 0600 /var/lib/postgresql/walg.env",
                env_body
            ),
        ];

        // Run as root because the file may not exist yet and chown
        // requires it. The file ends up owned by postgres regardless.
        let (exit_code, _stdout, stderr) = self.exec_in_member(member, cmd, None).await?;
        if exit_code != 0 {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to write walg.env on '{}' (exit {}): {}",
                    member.container_name,
                    exit_code,
                    stderr.trim()
                ),
            });
        }
        Ok(())
    }

    /// Run `ALTER SYSTEM SET archive_command` on the cluster's primary.
    /// `postgresql.auto.conf` is part of pgdata and gets streamed to
    /// replicas, so this only needs to run once per cluster (not per
    /// failover). Re-running is harmless.
    async fn enable_cluster_wal_archiving(
        &self,
        primary: &ServiceMemberInfo,
        service: &external_services::Model,
    ) -> Result<(), ExternalServiceError> {
        // Pull the app-user credentials so psql can authenticate. We
        // keep them in cluster parameters under `username` / `password`.
        let parameters = self.get_service_parameters(service.id).await?;
        let username = parameters
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("postgres")
            .to_string();
        let password = parameters
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let database = parameters
            .get("database")
            .and_then(|v| v.as_str())
            .unwrap_or("postgres")
            .to_string();

        // Source the env file before running wal-g — same shape as the
        // standalone path. Single-quote the archive_command string so
        // the SQL parser sees the literal; the shell still expands $p
        // because postgres treats %p as its own placeholder.
        let archive_command = ". /var/lib/postgresql/walg.env && wal-g wal-push %p";
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        // Need archive_mode too — defaults to off. archive_mode is a
        // POSTMASTER setting (requires restart). pg_auto_failover
        // tolerates a server restart cleanly, so we set it and rely on
        // the next pg_autoctl-driven restart to pick it up. Until
        // restart, archive_command runs but archiver isn't enabled —
        // which means WAL accumulates locally without being shipped.
        // Acceptable for the first basebackup; operators can manually
        // restart the primary to start streaming, or wait for the next
        // pg_auto_failover-initiated restart.
        let psql_cmd = vec![
            "psql".to_string(),
            "-U".to_string(),
            username,
            "-d".to_string(),
            database,
            "-c".to_string(),
            alter_sql,
            "-c".to_string(),
            "ALTER SYSTEM SET archive_mode = 'on'".to_string(),
            "-c".to_string(),
            "ALTER SYSTEM SET wal_level = 'replica'".to_string(),
            "-c".to_string(),
            "SELECT pg_reload_conf()".to_string(),
        ];

        // psql needs PGPASSWORD; pass it through env, NOT the command
        // line, so it doesn't show up in `ps`.
        let mut envs = std::collections::HashMap::new();
        envs.insert("PGPASSWORD".to_string(), password);

        let (exit_code, _stdout, stderr) = self
            .exec_in_member_with_env(primary, psql_cmd, Some("postgres"), envs)
            .await?;
        if exit_code != 0 {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "ALTER SYSTEM SET archive_command failed (exit {}): {}",
                    exit_code,
                    stderr.trim()
                ),
            });
        }
        Ok(())
    }

    /// Run a command inside a cluster member's container, regardless of
    /// whether it's local (control-plane bollard) or remote (agent).
    /// Returns `(exit_code, stdout, stderr)`. Same signature as the
    /// existing `exec_in_local_container` so callers can reuse error
    /// handling.
    async fn exec_in_member(
        &self,
        member: &ServiceMemberInfo,
        cmd: Vec<String>,
        user: Option<&str>,
    ) -> Result<(i64, String, String), ExternalServiceError> {
        self.exec_in_member_with_env(member, cmd, user, std::collections::HashMap::new())
            .await
    }

    pub(super) async fn exec_in_member_with_env(
        &self,
        member: &ServiceMemberInfo,
        cmd: Vec<String>,
        user: Option<&str>,
        env: std::collections::HashMap<String, String>,
    ) -> Result<(i64, String, String), ExternalServiceError> {
        if let Some(node_id) = member.node_id {
            let client = self.get_remote_client(node_id).await?;
            let result = client
                .exec_in_service(crate::remote_service_client::RemoteExecParams {
                    container_name: member.container_name.clone(),
                    command: cmd,
                    environment: env,
                    user: user.map(|s| s.to_string()),
                    detach: false,
                })
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Remote exec failed on member '{}' (node {}): {}",
                        member.container_name, node_id, e
                    ),
                })?;
            Ok((result.exit_code, result.stdout, result.stderr))
        } else {
            // Local path — augment exec_in_local_container with env
            // support. Until we extend that helper, fall back to a
            // bollard call here.
            use bollard::exec::{CreateExecOptions, StartExecOptions};
            use futures::StreamExt;

            let docker = self.require_docker()?;
            let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let env_strings: Vec<String> =
                env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            let env_refs: Option<Vec<&str>> = if env_strings.is_empty() {
                None
            } else {
                Some(env_strings.iter().map(|s| s.as_str()).collect())
            };

            let exec = docker
                .create_exec(
                    &member.container_name,
                    CreateExecOptions {
                        cmd: Some(cmd_refs),
                        env: env_refs,
                        user,
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!(
                        "Failed to create exec in '{}': {}",
                        member.container_name, e
                    ),
                })?;

            let output = docker
                .start_exec(
                    &exec.id,
                    Some(StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!("Failed to start exec in '{}': {}", member.container_name, e),
                })?;

            let mut stdout = String::new();
            let mut stderr = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
                while let Some(chunk) = output.next().await {
                    match chunk {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(other) => stdout.push_str(&other.to_string()),
                        Err(e) => {
                            return Err(ExternalServiceError::DockerError {
                                id: 0,
                                reason: format!("Exec stream error: {}", e),
                            });
                        }
                    }
                }
            }

            let inspect = docker.inspect_exec(&exec.id).await.map_err(|e| {
                ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!("Failed to inspect exec result: {}", e),
                }
            })?;
            let exit_code = inspect.exit_code.unwrap_or(-1);
            Ok((exit_code, stdout, stderr))
        }
    }

    /// Restore a Postgres HA cluster from a WAL-G backup.
    ///
    /// **In-place destructive** restore: every existing data node is
    /// torn down (containers + volumes + DNS + service_members rows).
    /// The same monitor + member topology is rebuilt with the primary's
    /// pgdata pre-seeded from S3. Replicas come up via the standard
    /// pg_auto_failover basebackup-from-primary path.
    ///
    /// MVP scope:
    ///   * Single-host clusters only (every member's `node_id IS NULL`).
    ///     Multi-host needs the agent to spin up the pre-seeding helper
    ///     container on the right worker — wired in a follow-up.
    ///   * Plain restore-to-latest (no point-in-time target). The
    ///     `recovery_target` argument is reserved but ignored today.
    ///
    /// Caller flow (e.g. `BackupService::restore_external_service`):
    ///   1. Look up the backup's S3 source + walg_prefix.
    ///   2. Call this with `(service, walg_prefix, s3_credentials)`.
    ///   3. Returns once the cluster is back at `status='running'`
    ///      and the primary has fully recovered.
    pub async fn restore_postgres_cluster(
        &self,
        service: &external_services::Model,
        walg_s3_prefix: &str,
        s3_credentials: &crate::S3Credentials,
        target_user_data: Option<&str>,
    ) -> Result<(), ExternalServiceError> {
        info!(
            service_id = service.id,
            service_name = %service.name,
            walg_s3_prefix,
            "Starting in-place restore of Postgres HA cluster"
        );

        if service.topology != "cluster" || service.service_type != "postgres" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: format!(
                    "restore_postgres_cluster requires topology='cluster' and service_type='postgres' (got {}/{})",
                    service.topology, service.service_type,
                ),
            });
        }

        // Snapshot the current member topology before tearing it down.
        // We rebuild with the same names/ordinals/node assignments so
        // downstream consumers (DNS reconciler, app conn strings) see
        // continuity across the restore.
        let members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service.id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;
        if members.is_empty() {
            return Err(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: "Cannot restore: cluster has no members on record".to_string(),
            });
        }

        // MVP gate: refuse multi-host. The pre-seeding helper has to
        // run on the same Docker daemon as the primary's volume; that
        // works for local members via bollard, but remote members
        // need an agent-side "create + run helper container" RPC we
        // don't have yet.
        if members.iter().any(|m| m.node_id.is_some()) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: "MVP: cluster restore is single-host only (no remote members yet). \
                         Move all members to the control plane or wait for the multi-host \
                         restore path."
                    .to_string(),
            });
        }

        // Find the primary in the snapshot — the node that *currently*
        // holds the writable copy of the data. We can't trust
        // `service_members.role` here (it's `replica` for every data
        // node post-rework); ask pg_auto_failover instead.
        let original_primary = self
            .find_live_primary_member(service, &members)
            .await?
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: "Cannot restore: cluster has no primary on record".to_string(),
            })?;
        let primary_container_name = original_primary.container_name.clone();
        let primary_volume_name = format!("{}_data", primary_container_name);

        // Reconstruct the member spec list (role + node_id) so we can
        // re-run initialize_cluster after teardown. Filter the monitor
        // out — initialize_cluster expects you to pass the monitor as
        // its own member spec, which is fine.
        let member_specs: Vec<ClusterMemberRequest> = members
            .iter()
            .map(|m| ClusterMemberRequest {
                role: m.role.clone(),
                node_id: m.node_id,
            })
            .collect();

        // ---- Phase 1: tear down everything pg_auto_failover-managed ----
        info!(
            service_id = service.id,
            "Restore phase 1: tearing down current cluster members"
        );
        self.stop_role_reconciler(service.id).await;

        for m in &members {
            // Drop DNS first so consumers see NXDOMAIN instead of a
            // stale IP for the duration of the rebuild.
            let _ = self
                .dns_registry
                .delete_by_owner(temps_dns::InternalOwnerKind::ServiceMember, m.id as i64)
                .await;

            // Stop + remove the container. Best-effort; container may
            // have died on its own already. If this process has no local
            // Docker daemon (control-plane profile), there is nothing to
            // clean up locally — that's expected, not a failure.
            if let Some(docker) = self.docker.get() {
                let _ = docker
                    .remove_container(
                        &m.container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;

                // Remove the data volume too — full reset. The primary's
                // volume gets recreated below with restored pgdata; the
                // monitor and replicas get fresh ones.
                let volume_name = format!("{}_data", m.container_name);
                let _ = docker
                    .remove_volume(
                        &volume_name,
                        None::<bollard::query_parameters::RemoveVolumeOptions>,
                    )
                    .await;
            } else {
                debug!(
                    member_id = m.id,
                    container_name = %m.container_name,
                    "No local Docker daemon in this process; skipping local teardown for cluster member"
                );
            }
        }

        // Drop role/VIP records (Tier 3) once.
        let _ = self
            .dns_registry
            .delete_by_owner(temps_dns::InternalOwnerKind::ServiceRole, service.id as i64)
            .await;

        // Drop the service_members rows. We keep the external_services
        // row in place so the URL/credentials/UI bookmarks survive the
        // restore.
        service_members::Entity::delete_many()
            .filter(service_members::Column::ServiceId.eq(service.id))
            .exec(self.db.as_ref())
            .await?;

        // Mark the parent service back to creating so the UI shows
        // progress + retry_cluster won't be confused if this aborts.
        let mut svc_update: external_services::ActiveModel = service.clone().into();
        svc_update.status = Set("creating".to_string());
        svc_update.updated_at = Set(Utc::now());
        let _ = svc_update.update(self.db.as_ref()).await;

        // ---- Phase 2: pre-seed the primary's pgdata from S3 ----
        info!(
            service_id = service.id,
            walg_s3_prefix,
            primary_volume = %primary_volume_name,
            "Restore phase 2: pre-seeding primary pgdata via wal-g backup-fetch"
        );
        if let Err(e) = self
            .preseed_primary_pgdata(
                service,
                &primary_volume_name,
                walg_s3_prefix,
                s3_credentials,
                target_user_data,
            )
            .await
        {
            // Pre-seed failed — leave the service in `creating` so the
            // operator can retry, but surface the real reason.
            return Err(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: format!("Pre-seed of primary pgdata failed: {}", e),
            });
        }

        // ---- Phase 3: rebuild cluster on top of the restored data ----
        info!(
            service_id = service.id,
            "Restore phase 3: rebuilding cluster on top of restored pgdata"
        );
        // Wrap in Arc::new(self.clone())? No — we already are &Arc<Self>
        // for the reconciler. initialize_cluster takes &self, that's
        // fine. The primary's container will start, see existing
        // pgdata, postgres will recover-from-WAL up to consistency,
        // then pg_autoctl create will register it as the new primary.
        // Replicas pull a fresh basebackup from the new primary as
        // part of their own pg_autoctl create.
        if let Err(e) = self.initialize_cluster(service.id, &member_specs).await {
            return Err(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: format!("Cluster rebuild after restore failed: {}", e),
            });
        }

        info!(
            service_id = service.id,
            "Cluster restore complete; service is back at status='running'"
        );
        Ok(())
    }

    /// Run a one-shot helper container that fetches `wal-g backup-fetch
    /// LATEST` into the named volume that the new primary will attach.
    /// Also writes `recovery.signal` + `restore_command` so the primary
    /// container's first postgres boot replays WAL up to consistency
    /// before pg_autoctl takes over.
    async fn preseed_primary_pgdata(
        &self,
        service: &external_services::Model,
        primary_volume_name: &str,
        walg_s3_prefix: &str,
        s3_credentials: &crate::S3Credentials,
        target_user_data: Option<&str>,
    ) -> Result<(), ExternalServiceError> {
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::CreateContainerOptionsBuilder;
        use futures::StreamExt;

        // Provisioning a new helper container needs a real Docker daemon;
        // this is a hard requirement, not a best-effort path.
        let docker = self.docker.require()?;

        // Make sure the volume exists. Docker is happy to (re)create
        // it; this also covers the case where teardown removed it.
        let _ = docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(primary_volume_name.to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: service.id,
                reason: format!(
                    "Failed to create primary volume '{}': {}",
                    primary_volume_name, e
                ),
            })?;

        // Resolve S3 endpoint relative to the helper. Helper runs on
        // the same host as the future primary, so endpoint resolution
        // can use the same temps-overlay heuristics. There's no live
        // primary container to inspect yet, so probe the postgres-ha
        // image's network membership instead — actually we don't have
        // a container at all, so just pass the endpoint through; the
        // resolve helper bails to None for non-localhost endpoints
        // anyway.
        let resolved_endpoint = s3_credentials.endpoint.clone();
        let mut walg_env =
            build_walg_env(s3_credentials, walg_s3_prefix, resolved_endpoint.as_deref()).map_err(
                |reason| ExternalServiceError::ParameterValidationFailed {
                    service_id: service.id,
                    reason: format!("Invalid WAL-G S3 configuration: {reason}"),
                },
            )?;
        if let Some(target_user_data) = target_user_data {
            walg_env.push(format!(
                "export WALG_FETCH_TARGET_USER_DATA={}",
                crate::externalsvc::postgres::shell_escape(target_user_data)
            ));
        }

        // The helper script:
        //   1. Fetch the latest WAL-G basebackup into pgdata.
        //   2. Drop a recovery.signal so postgres enters recovery mode
        //      on first boot.
        //   3. Write postgresql.auto.conf with restore_command so
        //      postgres can pull WAL segments from S3 to roll forward
        //      to consistency. Disable archive_mode/archive_command so
        //      the recovering primary doesn't re-push WAL into the
        //      source's prefix mid-recovery.
        //   4. chown to postgres:999 (postgres user uid in the
        //      official image) so postgres can read its own data.
        let env_lines = walg_env.join("\n");
        let script = format!(
            r#"set -eu
PGDATA=/var/lib/postgresql/pgdata
mkdir -p "$PGDATA"
chown -R postgres:postgres /var/lib/postgresql

# Stash the env file the recovery + future archiver will source.
umask 077
cat > /var/lib/postgresql/walg-restore.env <<'WALG_RESTORE_EOF'
{env_lines}
WALG_RESTORE_EOF
chown postgres:postgres /var/lib/postgresql/walg-restore.env
chmod 0600 /var/lib/postgresql/walg-restore.env

echo "[restore] Fetching selected WAL-G basebackup into $PGDATA..."
if grep -q '^export WALG_FETCH_TARGET_USER_DATA=' /var/lib/postgresql/walg-restore.env; then
  gosu postgres sh -c '. /var/lib/postgresql/walg-restore.env && wal-g backup-fetch "$PGDATA" --target-user-data "$WALG_FETCH_TARGET_USER_DATA"'
else
  gosu postgres sh -c '. /var/lib/postgresql/walg-restore.env && wal-g backup-fetch "$PGDATA" LATEST'
fi

echo "[restore] Writing recovery.signal + restore_command"
touch "$PGDATA/recovery.signal"
chown postgres:postgres "$PGDATA/recovery.signal"

cat > "$PGDATA/postgresql.auto.conf" <<'PG_AUTO_EOF'
# Written by Temps cluster restore. Overwrites any source-side settings.
restore_command = '. /var/lib/postgresql/walg-restore.env && wal-g wal-fetch %f %p'
recovery_target = 'immediate'
recovery_target_action = 'promote'
archive_mode = 'off'
archive_command = '/bin/true'
PG_AUTO_EOF
chown postgres:postgres "$PGDATA/postgresql.auto.conf"
chmod 0600 "$PGDATA/postgresql.auto.conf"

echo "[restore] Pre-seed complete"
"#,
            env_lines = env_lines,
        );

        let helper_name = format!(
            "temps-restore-helper-{}-{}",
            service.id,
            Utc::now().timestamp()
        );
        let helper_config = ContainerCreateBody {
            // postgres-ha has both wal-g and gosu, so no extra image
            // shopping. Pin to the same -walg-bundled tag the cluster
            // uses (DEFAULT_CLUSTER_IMAGE) — both wal-g binary and the
            // image version need to match the primary's pgdata layout.
            image: Some(crate::externalsvc::postgres_cluster::DEFAULT_CLUSTER_IMAGE.to_string()),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), script]),
            host_config: Some(HostConfig {
                binds: Some(vec![format!("{}:/var/lib/postgresql", primary_volume_name)]),
                ..Default::default()
            }),
            // Run as root so the chown calls land — the helper drops
            // to postgres internally for the wal-g call.
            user: Some("root".to_string()),
            // Keyed like a standalone Postgres container so startup
            // reconciliation of an interrupted restore can fence it.
            labels: Some(crate::externalsvc::restore_helper::restore_helper_labels(
                &format!("postgres-{}", service.name),
            )),
            ..Default::default()
        };

        let helper = docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                helper_config,
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: service.id,
                reason: format!("Failed to create restore helper container: {}", e),
            })?;

        // Pull the image first if it's missing (debug builds skip web,
        // but they don't pre-pull our images either).
        if let Err(e) = docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
        {
            // Clean up the half-created helper before bubbling out.
            let _ = docker
                .remove_container(
                    &helper.id,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: false,
                        ..Default::default()
                    }),
                )
                .await;
            return Err(ExternalServiceError::DockerError {
                id: service.id,
                reason: format!("Failed to start restore helper container: {}", e),
            });
        }

        // Wait for the helper to finish.
        let wait_result = docker
            .wait_container(
                &helper.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .next()
            .await;

        // Capture logs before removing — useful for surfacing the real
        // reason a wal-g fetch failed.
        let logs = docker
            .logs(
                &helper.id,
                Some(bollard::query_parameters::LogsOptions {
                    stdout: true,
                    stderr: true,
                    tail: "200".to_string(),
                    ..Default::default()
                }),
            )
            .map(|chunk| match chunk {
                Ok(c) => c.to_string(),
                Err(e) => format!("[log read error: {}]", e),
            })
            .collect::<Vec<_>>()
            .await
            .join("");

        let _ = docker
            .remove_container(
                &helper.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await;

        match wait_result {
            Some(Ok(resp)) if resp.status_code == 0 => {
                info!(
                    service_id = service.id,
                    "Restore helper completed successfully"
                );
                Ok(())
            }
            Some(Ok(resp)) => Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Restore helper exited with status {}.\nLast log lines:\n{}",
                    resp.status_code,
                    logs.trim_end()
                ),
            }),
            Some(Err(e)) => Err(ExternalServiceError::DockerError {
                id: service.id,
                reason: format!("Restore helper wait failed: {}", e),
            }),
            None => Err(ExternalServiceError::InternalError {
                reason: "Restore helper finished but no status code was returned".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_s3_credentials() -> crate::S3Credentials {
        crate::S3Credentials {
            access_key_id: "key'quoted".to_string(),
            secret_key: "secret'quoted".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: Some("https://s3.example.test".to_string()),
            bucket_name: "backups".to_string(),
            bucket_path: "tenant".to_string(),
            force_path_style: true,
        }
    }

    #[test]
    fn walg_env_file_values_are_posix_escaped() {
        let env = build_walg_env(
            &test_s3_credentials(),
            "s3://backups/repo'quoted",
            Some("https://s3.example.test/path'quoted"),
        )
        .expect("single quotes should be safely escaped");
        assert!(env
            .iter()
            .any(|line| line == "export AWS_ACCESS_KEY_ID='key'\\''quoted'"));
        assert!(env
            .iter()
            .any(|line| line == "export WALG_S3_PREFIX='s3://backups/repo'\\''quoted'"));
    }

    #[test]
    fn walg_env_file_rejects_line_break_injection() {
        let mut credentials = test_s3_credentials();
        credentials.secret_key = "secret\nWALG_RESTORE_EOF\nid".to_string();
        let error = build_walg_env(&credentials, "s3://backups/repo", None)
            .expect_err("line breaks must be rejected before heredoc interpolation");
        assert!(error.contains("AWS_SECRET_ACCESS_KEY"));
    }

    /// A long-lived, operator-configured credential must produce no
    /// `AWS_SESSION_TOKEN` export whatsoever — not an empty one, which the
    /// AWS SDKs would sign and the provider would then reject.
    #[test]
    fn walg_env_file_omits_the_session_token_for_a_long_lived_credential() {
        let env = build_walg_env(&test_s3_credentials(), "s3://backups/repo", None)
            .expect("long-lived credentials still build an env file");
        assert!(!env.iter().any(|line| line.contains("AWS_SESSION_TOKEN")));
    }

    #[test]
    fn walg_env_file_exports_and_escapes_a_session_token() {
        let mut credentials = test_s3_credentials();
        credentials.session_token = Some("token'quoted".to_string());
        let env = build_walg_env(&credentials, "s3://backups/repo", None)
            .expect("a session token is escaped like every other value");
        assert!(env
            .iter()
            .any(|line| line == "export AWS_SESSION_TOKEN='token'\\''quoted'"));
    }

    /// Parity with `aws_session_token_env` and `mc_host_credential`, which
    /// already filter this: `export AWS_SESSION_TOKEN=''` is worse than no
    /// export at all, because WAL-G signs the empty token and the provider
    /// rejects every request.
    #[test]
    fn walg_env_file_omits_an_empty_session_token() {
        let mut credentials = test_s3_credentials();
        credentials.session_token = Some(String::new());
        let env = build_walg_env(&credentials, "s3://backups/repo", None)
            .expect("an empty session token still builds an env file");
        assert!(
            !env.iter().any(|line| line.contains("AWS_SESSION_TOKEN")),
            "an empty session token must be absent, never exported as ''"
        );
    }

    #[test]
    fn walg_env_file_rejects_line_break_injection_through_the_session_token() {
        let mut credentials = test_s3_credentials();
        credentials.session_token = Some("token\nWALG_RESTORE_EOF\nid".to_string());
        let error = build_walg_env(&credentials, "s3://backups/repo", None)
            .expect_err("line breaks must be rejected before heredoc interpolation");
        assert!(error.contains("AWS_SESSION_TOKEN"));
    }

    // ── Container stats helpers ──────────────────────────────────────────────
}
