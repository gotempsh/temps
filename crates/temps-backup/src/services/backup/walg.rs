// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G for the control-plane database, locating its container, enabling WAL
//! archiving and taking base backups.

use super::*;

impl BackupService {
    /// Find the Docker container that hosts the internal database by matching the hostname
    /// from DATABASE_URL against Docker container names and network aliases.
    ///
    /// Returns `(container_id, pgdata_path)` if found.
    pub(super) async fn find_internal_db_container(&self) -> Result<(String, String), BackupError> {
        use bollard::query_parameters::ListContainersOptions;
        use bollard::Docker;

        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL: {}", e),
        })?;

        let db_host = url.host_str().unwrap_or("localhost").to_string();

        // Skip Docker discovery for local connections
        if db_host == "localhost" || db_host == "127.0.0.1" || db_host == "::1" {
            return Err(BackupError::Internal {
                message: format!(
                    "Database host '{}' is local — cannot exec into a Docker container",
                    db_host
                ),
            });
        }

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // List all running containers
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: false, // only running
                ..Default::default()
            }))
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to list Docker containers: {}", e),
            })?;

        // Find container matching the database hostname by:
        // 1. Container name (e.g., /temps-postgres matches "temps-postgres")
        // 2. Docker Compose service name in network aliases (e.g., "postgres" on compose network)
        for container in &containers {
            let container_id = container.id.as_deref().unwrap_or("");
            if container_id.is_empty() {
                continue;
            }

            // Check container names (Docker prefixes with '/')
            if let Some(names) = &container.names {
                for name in names {
                    let clean_name = name.trim_start_matches('/');
                    if clean_name == db_host {
                        return self
                            .resolve_pgdata_for_container(&docker, container_id)
                            .await;
                    }
                }
            }

            // Check network aliases (Docker Compose sets the service name as an alias)
            if let Some(network_settings) = &container.network_settings {
                if let Some(networks) = &network_settings.networks {
                    for net_config in networks.values() {
                        if let Some(aliases) = &net_config.aliases {
                            if aliases.iter().any(|a| a == &db_host) {
                                return self
                                    .resolve_pgdata_for_container(&docker, container_id)
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        Err(BackupError::Internal {
            message: format!(
                "No Docker container found for database host '{}'. \
                 Ensure the database is running in a Docker container with WAL-G installed.",
                db_host
            ),
        })
    }

    /// Resolve the PGDATA path for a container by inspecting its environment variables.
    async fn resolve_pgdata_for_container(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
    ) -> Result<(String, String), BackupError> {
        let inspect = docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to inspect container {}: {}", container_id, e),
            })?;

        // Try to find PGDATA from container environment
        let mut pgdata = String::from("/var/lib/postgresql/data");
        if let Some(config) = &inspect.config {
            if let Some(env) = &config.env {
                for var in env {
                    if let Some(val) = var.strip_prefix("PGDATA=") {
                        pgdata = val.to_string();
                        break;
                    }
                }
            }
        }

        Ok((container_id.to_string(), pgdata))
    }

    /// Perform a WAL-G backup by exec'ing into the internal database container.
    /// WAL-G uploads directly to S3 — no data flows through the Temps process.
    ///
    /// Returns `(s3_location, size_bytes)` on success. The `s3_location` is the WAL-G
    /// S3 prefix (starts with `s3://`), used by the restore logic to detect WAL-G backups.
    pub(super) async fn backup_postgres_walg(
        &self,
        s3_source: &S3Source,
        _backup_id: &str,
    ) -> Result<(String, i64), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use bollard::Docker;

        let (container_id, pgdata) = self.find_internal_db_container().await?;

        info!(
            "Starting WAL-G backup via container {} (PGDATA={})",
            container_id, pgdata
        );

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Verify WAL-G is installed in the container
        let check_exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["which", "wal-g"]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to check WAL-G in container: {}", e),
            })?;

        docker
            .start_exec(
                &check_exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to run WAL-G check: {}", e),
            })?;

        // Wait for check to complete
        loop {
            let inspect =
                docker
                    .inspect_exec(&check_exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G check exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "WAL-G is not installed in container {}. \
                                     Use the gotempsh/timescaledb-walg image.",
                                    container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Build WAL-G S3 prefix using a STABLE path (no date or backup_id).
        // WAL-G requires all backups and WAL segments to share the same prefix so that:
        // - wal-g wal-push archives WAL to {prefix}/wal_005/
        // - wal-g backup-push stores base backups in {prefix}/basebackups_005/
        // - wal-g backup-fetch LATEST finds the right backup + WAL chain
        // - wal-g delete retain works across all backups
        let walg_s3_prefix = format!(
            "s3://{}/{}/internal_db/walg",
            s3_source.bucket_name,
            s3_source.bucket_path.trim_matches('/'),
        );

        // Decrypt S3 credentials for WAL-G environment variables
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 access key: {}", e),
            })?;

        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 secret key: {}", e),
            })?;

        // `None` unless this source holds a temporary (STS-style) credential.
        let decrypted_session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            s3_source,
        )
        .map_err(|e| BackupError::Internal {
            message: format!("Failed to decrypt S3 session token: {}", e),
        })?;

        // Build environment variables for WAL-G
        let mut env_vars: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", decrypted_access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", decrypted_secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGDATA={}", pgdata),
        ];
        // Absent for a long-lived credential, so its environment is unchanged.
        env_vars.extend(temps_providers::externalsvc::aws_session_token_env(
            decrypted_session_token.as_deref(),
        ));

        // Resolve S3 endpoint for use inside the Docker container.
        // localhost/127.0.0.1 endpoints are translated to Docker-resolvable addresses.
        let s3_creds = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key.clone(),
            secret_key: decrypted_secret_key.clone(),
            session_token: decrypted_session_token.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };
        if let Some(resolved_endpoint) = s3_creds
            .resolve_endpoint_for_container(&docker, &container_id)
            .await
        {
            env_vars.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }

        if s3_source.force_path_style.unwrap_or(true) {
            env_vars.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let env_refs: Vec<&str> = env_vars.iter().map(|s| s.as_str()).collect();

        // Run wal-g backup-push
        info!("Running wal-g backup-push in container {}", container_id);

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["wal-g", "backup-push", &pgdata]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(env_refs.clone()),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create WAL-G exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start WAL-G exec: {}", e),
            })?;

        // Poll for completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "wal-g backup-push failed with exit code {} in container {}",
                                    exit_code, container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        // Calculate total backup size by listing objects under the WAL-G prefix
        let s3_client = self.create_s3_client(s3_source).await?;
        let prefix = format!(
            "{}/internal_db/walg/basebackups_005/",
            s3_source.bucket_path.trim_matches('/'),
        );

        let mut total_size: i64 = 0;
        let mut continuation_token: Option<String> = None;
        loop {
            let mut req = s3_client
                .list_objects_v2()
                .bucket(&s3_source.bucket_name)
                .prefix(&prefix);

            if let Some(token) = continuation_token.take() {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| BackupError::S3(format!("Failed to list WAL-G objects: {}", e)))?;

            for obj in resp.contents() {
                total_size += obj.size().unwrap_or(0);
            }

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        info!(
            "WAL-G backup completed: {} ({} bytes)",
            walg_s3_prefix, total_size
        );

        // Enable continuous WAL archiving for the internal database.
        // Write S3 credentials to an env file on the shared volume, then configure
        // archive_command to source it before running wal-g wal-push.
        // Failures here are logged but do NOT fail the backup.
        if let Err(e) = self
            .enable_internal_wal_archiving(&docker, &container_id, &env_vars, &pgdata)
            .await
        {
            error!(
                "Failed to enable WAL archiving for internal DB in container '{}': {}. \
                 Base backup succeeded but continuous WAL archiving is not active.",
                container_id, e
            );
        }

        Ok((walg_s3_prefix, total_size))
    }

    /// Write WAL-G credentials to an env file on the shared volume and enable
    /// continuous WAL archiving for the internal database via `ALTER SYSTEM`.
    ///
    /// Same approach as external PostgreSQL services: the env file is refreshed on
    /// every backup so credential rotations are picked up automatically.
    async fn enable_internal_wal_archiving(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
        env_vars: &[String],
        pgdata: &str,
    ) -> Result<(), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        // Determine the volume mount root (parent of PGDATA) for the env file location.
        // E.g., PGDATA=/var/lib/postgresql/data -> env file at /var/lib/postgresql/walg.env
        let volume_root = std::path::Path::new(pgdata)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/var/lib/postgresql".to_string());
        let walg_env_path = format!("{}/walg.env", volume_root);

        // Filter to only S3/WAL-G env vars (no PGDATA, no PG connection vars)
        let env_file_lines: Vec<&String> = env_vars
            .iter()
            .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
            .collect();

        // Write the env file via docker exec. `walg_env_path` is derived from
        // the container's PGDATA and must be escaped like any other value
        // reaching `sh -c` -- an unescaped path lets shell metacharacters in
        // it inject arbitrary commands into the exec.
        let escaped_walg_env_path = shell_escape(&walg_env_path);
        let write_cmd = format!(
            "printf '%s\\n' {} > {} && chmod 600 {}",
            env_file_lines
                .iter()
                .filter_map(|line| shell_export_assignment(line)
                    .map(|assignment| shell_escape(&assignment)))
                .collect::<Vec<_>>()
                .join(" "),
            escaped_walg_env_path,
            escaped_walg_env_path,
        );

        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &write_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create env file write exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start env file write exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect env file write exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(BackupError::Internal {
                        message: format!(
                            "Failed to write walg.env (exit code {:?})",
                            inspect.exit_code
                        ),
                    });
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Written WAL-G credentials to {} in container '{}'",
            walg_env_path, container_id
        );

        // Parse DATABASE_URL for psql credentials
        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL for ALTER SYSTEM: {}", e),
        })?;
        let pg_user = url.username();
        let pg_password = url.password().unwrap_or("");

        // Enable archive_command via ALTER SYSTEM + pg_reload_conf().
        // Use two separate -c flags because ALTER SYSTEM cannot run inside a
        // transaction block, and psql wraps multiple statements in a single -c
        // into a transaction.
        let archive_command = format!(". {} && wal-g wal-push %p", walg_env_path);
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        let reload_sql = "SELECT pg_reload_conf()";

        let password_env = format!("PGPASSWORD={}", pg_password);
        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(vec![
                        "psql", "-U", pg_user, "-c", &alter_sql, "-c", reload_sql,
                    ]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![&password_env]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create ALTER SYSTEM exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start ALTER SYSTEM exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect ALTER SYSTEM exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(BackupError::Internal {
                        message: format!(
                            "ALTER SYSTEM SET archive_command failed (exit code {:?})",
                            inspect.exit_code
                        ),
                    });
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Enabled continuous WAL archiving for internal DB in container '{}'",
            container_id
        );

        Ok(())
    }
}
