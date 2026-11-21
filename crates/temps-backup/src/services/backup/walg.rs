// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G for the control-plane database, locating its container, enabling WAL
//! archiving and taking base backups.

use super::internal_archive::BaseBackupSteps;
use super::*;

impl BackupService {
    /// Find the Docker container that hosts the internal database by matching the host
    /// from DATABASE_URL against Docker container names, network aliases and network
    /// addresses.
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

        // Find the container the database host names: by container name
        // (`/temps-postgres` matches "temps-postgres"), by network alias (the
        // Compose service name), or by an address it has on one of its
        // networks (the bundled Compose files connect by a static IP). A
        // match is only a candidate: it must also be the server Temps is
        // connected to, since every archiving change goes through that
        // connection. A candidate that is another server, or that cannot be
        // checked, is skipped and named in the error if none qualifies.
        let mut rejected = Vec::new();
        for container in &containers {
            let container_id = container.id.as_deref().unwrap_or("");
            if container_id.is_empty() || !container_serves_host(container, &db_host) {
                continue;
            }
            match self
                .verify_container_is_connected_database(&docker, container_id)
                .await
            {
                Ok(()) => {
                    return self
                        .resolve_pgdata_for_container(&docker, container_id)
                        .await
                }
                Err(error) => {
                    debug!(
                        container_id = %container_id,
                        db_host = %db_host,
                        "Container matching the database host is not the connected database: {}",
                        error
                    );
                    rejected.push(error.to_string());
                }
            }
        }

        if !rejected.is_empty() {
            return Err(BackupError::Internal {
                message: format!(
                    "No Docker container matching database host '{}' is the database Temps is \
                     connected to: {}",
                    db_host,
                    rejected.join("; ")
                ),
            });
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

        ensure_walg_installed(&docker, &container_id).await?;

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
            force_path_style: s3_source.effective_path_style(true).force_path_style,
        };
        if let Some(resolved_endpoint) = s3_creds
            .resolve_endpoint_for_container(&docker, &container_id)
            .await
        {
            env_vars.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }

        if s3_source.effective_path_style(true).force_path_style {
            env_vars.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let env_refs: Vec<&str> = env_vars.iter().map(|s| s.as_str()).collect();

        // Point archive_command at WAL-G before the base backup starts. Until
        // now it may be the placeholder that discards WAL
        // (`reconcile_internal_wal_archiving`), and a base backup is only
        // restorable together with the WAL written while it ran, so that WAL
        // must reach the archive. Without archiving the base backup would be
        // unrestorable, so the error ends the WAL-G attempt and the caller
        // falls back to pg_dump. The settings are read first so a failure
        // can put back what was there. `archive_then_push` owns that order.
        let archive_before = self.read_archive_settings().await?;
        let archiving_command = internal_walg_archive_command(&pgdata);
        let walg_env_path = internal_walg_env_path(&pgdata);
        if archive_before.archive_command == archiving_command {
            let archiving_to = run_walg_env_script(
                &docker,
                &container_id,
                walg_env_prefix_script(&walg_env_path),
                "read the WAL-G destination from",
            )
            .await?;
            if let Some(reason) = archive_before.walg_destination_change_blocker(
                &archiving_command,
                Some(&archiving_to),
                &walg_s3_prefix,
            ) {
                return Err(BackupError::Internal {
                    message: format!(
                        "Not taking a WAL-G base backup of the control-plane database in \
                         container '{}': {}",
                        container_id, reason
                    ),
                });
            }
        }
        // Marks the walg.env this attempt writes, so a failure puts back the
        // previous one exactly when this attempt replaced it, including when
        // Docker loses the result of the write.
        let attempt = format!("# temps-walg-attempt {}", Uuid::new_v4());
        let write_env =
            self.write_internal_walg_env(&docker, &container_id, &env_vars, &pgdata, &attempt);
        let restore_env = async {
            run_walg_env_script(
                &docker,
                &container_id,
                walg_env_restore_script(&walg_env_path, &attempt),
                "put back the previous",
            )
            .await
            .map(|_| ())
        };
        let discard_previous_env = async {
            run_walg_env_script(
                &docker,
                &container_id,
                format!("rm -f {}", shell_escape(&format!("{walg_env_path}.prev"))),
                "delete the copy of the previous",
            )
            .await
            .map(|_| ())
        };

        // A push that fails from inside the container (an endpoint or bucket
        // only the Temps process can reach, a credential wal-g rejects) would
        // otherwise leave archive_command failing on every segment, so
        // PostgreSQL keeps all WAL again.
        let push = async {
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
            Ok::<(), BackupError>(())
        };
        self.archive_then_push(
            &archive_before,
            &archiving_command,
            &container_id,
            BaseBackupSteps {
                write_env,
                push,
                restore_env,
                discard_previous_env,
            },
        )
        .await?;

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

        Ok((walg_s3_prefix, total_size))
    }

    /// Write WAL-G credentials to an env file on the shared volume, which the
    /// internal database's archive_command sources.
    ///
    /// Same approach as external PostgreSQL services: the env file is refreshed on
    /// every backup so credential rotations are picked up automatically. The
    /// file it replaces is kept as `walg.env.prev`, and the new one starts with
    /// the `attempt` line, so a failed backup can put the old one back.
    async fn write_internal_walg_env(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
        env_vars: &[String],
        pgdata: &str,
        attempt: &str,
    ) -> Result<(), BackupError> {
        let walg_env_path = internal_walg_env_path(pgdata);

        // The credentials reach the shell through the exec's environment,
        // never its command line, which any process in the container can read.
        let contents = format!(
            "{}={}",
            WALG_ENV_CONTENTS_VAR,
            walg_env_file_contents(env_vars, attempt)
        );
        temps_providers::externalsvc::exec_util::run_exec_as(
            docker,
            container_id,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                walg_env_replace_script(&walg_env_path),
            ],
            Some(vec![contents]),
            std::time::Duration::from_secs(30),
            Some("postgres"),
        )
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to write {} in container '{}': {}",
                walg_env_path, container_id, e
            ),
        })?;

        info!(
            "Written WAL-G credentials to {} in container '{}'",
            walg_env_path, container_id
        );
        Ok(())
    }
}

/// Fail unless `wal-g` is on the PATH of `container_id`.
async fn ensure_walg_installed(
    docker: &bollard::Docker,
    container_id: &str,
) -> Result<(), BackupError> {
    use bollard::exec::{CreateExecOptions, StartExecOptions};

    let check_exec = docker
        .create_exec(
            container_id,
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
    Ok(())
}

/// Where the internal database's WAL-G credentials are written: next to
/// PGDATA, e.g. `/var/lib/postgresql/walg.env` for
/// `PGDATA=/var/lib/postgresql/data`.
fn internal_walg_env_path(pgdata: &str) -> String {
    let volume_root = std::path::Path::new(pgdata)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/var/lib/postgresql".to_string());
    format!("{}/walg.env", volume_root)
}

/// The exec environment variable that carries walg.env's contents to the
/// script writing it.
const WALG_ENV_CONTENTS_VAR: &str = "TEMPS_WALG_ENV";

/// walg.env's contents: the `attempt` comment line, then an `export` line for
/// each WAL-G and S3 variable (no PGDATA, no PostgreSQL connection variables).
fn walg_env_file_contents(env_vars: &[String], attempt: &str) -> String {
    std::iter::once(attempt.to_string())
        .chain(
            env_vars
                .iter()
                .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
                .filter_map(|line| shell_export_assignment(line)),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shell that replaces walg.env at `path` with the contents of
/// [`WALG_ENV_CONTENTS_VAR`], readable only by its owner, and keeps the file it
/// replaces as `<path>.prev`. It succeeds only once both are in place, and
/// otherwise leaves walg.env as it was: the new file is written aside and
/// renamed over it. `path` is derived from the container's PGDATA and is
/// escaped like any other value reaching `sh -c`.
fn walg_env_replace_script(path: &str) -> String {
    let env = shell_escape(path);
    let prev = shell_escape(&format!("{path}.prev"));
    let tmp = shell_escape(&format!("{path}.tmp"));
    format!(
        "rm -f {prev} {tmp} && {{ [ ! -e {env} ] || cp -p {env} {prev}; }} && umask 077 && \
         printf '%s\\n' \"${WALG_ENV_CONTENTS_VAR}\" > {tmp} && chmod 600 {tmp} && \
         mv -f {tmp} {env}"
    )
}

/// Shell that prints the `WALG_S3_PREFIX` walg.env at `path` archives to, or
/// nothing when there is no walg.env. It reads the file the way
/// archive_command does, by sourcing it, in a subshell that prints nothing
/// else.
fn walg_env_prefix_script(path: &str) -> String {
    let env = shell_escape(path);
    format!("[ -e {env} ] || exit 0; ( . {env} >/dev/null 2>&1; printf '%s' \"$WALG_S3_PREFIX\" )")
}

/// Shell that undoes [`walg_env_replace_script`] if walg.env is the one the
/// `attempt` wrote: the kept file goes back, or, when there was none, the new
/// walg.env is removed. Any other walg.env is left as it is.
fn walg_env_restore_script(path: &str, attempt: &str) -> String {
    let env = shell_escape(path);
    let prev = shell_escape(&format!("{path}.prev"));
    let attempt = shell_escape(attempt);
    format!(
        "if [ -e {env} ] && grep -qxF {attempt} {env}; then \
         if [ -e {prev} ]; then mv -f {prev} {env}; else rm -f {env}; fi; fi"
    )
}

/// Run a walg.env maintenance `script` as the postgres user and return what
/// it printed; `action` says what it does to walg.env, for the error.
async fn run_walg_env_script(
    docker: &bollard::Docker,
    container_id: &str,
    script: String,
    action: &str,
) -> Result<String, BackupError> {
    temps_providers::externalsvc::exec_util::run_exec_as(
        docker,
        container_id,
        vec!["sh".to_string(), "-c".to_string(), script],
        None,
        std::time::Duration::from_secs(30),
        Some("postgres"),
    )
    .await
    .map(|result| result.output.trim().to_string())
    .map_err(|e| BackupError::Internal {
        message: format!(
            "Failed to {} walg.env in container '{}': {}",
            action, container_id, e
        ),
    })
}

/// The `archive_command` that ships the internal database's WAL with WAL-G.
pub(super) fn internal_walg_archive_command(pgdata: &str) -> String {
    format!(". {} && wal-g wal-push %p", internal_walg_env_path(pgdata))
}

/// Whether `container` is the one `db_host` (the DATABASE_URL host) names: by
/// container name, by a network alias, or by an address it has on one of its
/// networks.
fn container_serves_host(container: &bollard::models::ContainerSummary, db_host: &str) -> bool {
    // `Url::host_str` keeps the brackets around an IPv6 literal.
    let host = db_host.trim_start_matches('[').trim_end_matches(']');
    let host_ip = host.parse::<std::net::IpAddr>().ok();
    let same_address = |address: &Option<String>| match (address.as_deref(), host_ip) {
        (Some(address), Some(host_ip)) => address.parse::<std::net::IpAddr>().ok() == Some(host_ip),
        _ => false,
    };

    // Docker prefixes container names with '/'.
    let by_name = container
        .names
        .iter()
        .flatten()
        .any(|name| name.trim_start_matches('/') == host);
    let by_network = container
        .network_settings
        .iter()
        .filter_map(|settings| settings.networks.as_ref())
        .flat_map(|networks| networks.values())
        .any(|endpoint| {
            endpoint.aliases.iter().flatten().any(|alias| alias == host)
                || same_address(&endpoint.ip_address)
                || same_address(&endpoint.global_ipv6_address)
        });
    by_name || by_network
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummary, ContainerSummaryNetworkSettings, EndpointSettings};
    use std::collections::HashMap;

    fn container(name: &str, aliases: &[&str], ipv4: &str, ipv6: &str) -> ContainerSummary {
        let endpoint = EndpointSettings {
            aliases: Some(aliases.iter().map(|a| a.to_string()).collect()),
            ip_address: Some(ipv4.to_string()),
            global_ipv6_address: Some(ipv6.to_string()),
            ..Default::default()
        };
        ContainerSummary {
            names: Some(vec![format!("/{name}")]),
            network_settings: Some(ContainerSummaryNetworkSettings {
                networks: Some(HashMap::from([("temps-network".to_string(), endpoint)])),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn database_container_is_found_by_its_static_network_address() {
        // The bundled Compose files connect to the database by IP.
        let db = container("temps-postgres", &["postgres"], "198.18.255.11", "");
        assert!(container_serves_host(&db, "198.18.255.11"));
        assert!(!container_serves_host(&db, "198.18.255.12"));
    }

    #[test]
    fn database_container_is_found_by_name_alias_or_ipv6_address() {
        let db = container("temps-postgres", &["postgres"], "", "fd00::11");
        assert!(container_serves_host(&db, "temps-postgres"));
        assert!(container_serves_host(&db, "postgres"));
        assert!(container_serves_host(&db, "[fd00::11]"));
        assert!(!container_serves_host(&db, "db.example.internal"));
    }

    #[test]
    fn a_container_with_no_address_matches_no_ip_host() {
        // Docker reports an empty address for a container not yet on a network.
        let db = container("worker", &[], "", "");
        assert!(!container_serves_host(&db, "198.18.255.11"));
        assert!(!container_serves_host(&db, ""));
    }

    /// Run a walg.env script with `sh`, as the container would, with `contents`
    /// in the variable the replace script reads.
    fn run_script(script: &str, contents: &str) -> bool {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env(WALG_ENV_CONTENTS_VAR, contents)
            .status()
            .expect("sh runs")
            .success()
    }

    const ATTEMPT: &str = "# temps-walg-attempt 1b4e28ba-2fa1-11d2-883f-0016d3cca427";
    const WORKING: &str = "export WALG_S3_PREFIX='s3://working'\n";

    /// walg.env as an attempt writes it.
    fn new_walg_env() -> String {
        format!("{ATTEMPT}\nexport WALG_S3_PREFIX='s3://new'")
    }

    #[test]
    fn walg_env_credentials_stay_out_of_the_command_line() {
        let env_vars = [
            "WALG_S3_PREFIX=s3://backups/internal_db/walg".to_string(),
            "AWS_SECRET_ACCESS_KEY=s3cr3t'key".to_string(),
            "PGDATA=/home/postgres/pgdata/pgroot/data".to_string(),
        ];

        let contents = walg_env_file_contents(&env_vars, ATTEMPT);
        let path = "/home/postgres/pgdata/pgroot/walg.env";

        assert_eq!(
            contents,
            format!(
                "{ATTEMPT}\n\
                 export WALG_S3_PREFIX='s3://backups/internal_db/walg'\n\
                 export AWS_SECRET_ACCESS_KEY='s3cr3t'\\''key'"
            )
        );
        for script in [
            walg_env_replace_script(path),
            walg_env_restore_script(path, ATTEMPT),
        ] {
            assert!(!script.contains("s3cr3t"), "{script}");
            assert!(!script.contains("WALG_S3_PREFIX"), "{script}");
        }
    }

    #[test]
    fn a_replaced_walg_env_can_be_put_back() {
        let dir = tempfile::tempdir().unwrap();
        // A space and a quote, which the scripts must escape.
        let path = dir.path().join("pg data's").join("walg.env");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let path_str = path.to_str().unwrap();
        let prev = format!("{path_str}.prev");
        std::fs::write(&path, WORKING).unwrap();

        assert!(run_script(
            &walg_env_replace_script(path_str),
            &new_walg_env()
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", new_walg_env())
        );
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), WORKING);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        assert!(run_script(&walg_env_restore_script(path_str, ATTEMPT), ""));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), WORKING);
        assert!(!std::path::Path::new(&prev).exists());
    }

    /// What [`walg_env_prefix_script`] prints for walg.env at `path`.
    fn read_prefix(path: &str) -> String {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(walg_env_prefix_script(path))
            .output()
            .expect("sh runs");
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn the_walg_destination_is_read_back_from_walg_env() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pg data's").join("walg.env");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let path_str = path.to_str().unwrap();

        // No walg.env yet: nothing archives with WAL-G.
        assert_eq!(read_prefix(path_str), "");

        let env_vars = [
            "WALG_S3_PREFIX=s3://backups/it's/internal_db/walg".to_string(),
            "AWS_SECRET_ACCESS_KEY=s3cr3t".to_string(),
        ];
        assert!(run_script(
            &walg_env_replace_script(path_str),
            &walg_env_file_contents(&env_vars, ATTEMPT)
        ));
        assert_eq!(read_prefix(path_str), "s3://backups/it's/internal_db/walg");
    }

    #[test]
    fn putting_back_a_first_walg_env_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("walg.env");
        let path_str = path.to_str().unwrap();

        assert!(run_script(
            &walg_env_replace_script(path_str),
            &new_walg_env()
        ));
        assert!(path.exists());
        assert!(run_script(&walg_env_restore_script(path_str, ATTEMPT), ""));
        assert!(!path.exists());
    }

    #[test]
    fn putting_back_leaves_a_walg_env_the_attempt_did_not_write() {
        // The attempt's write never happened (or its result was lost before
        // it ran): a copy from an earlier attempt must not replace the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("walg.env");
        let path_str = path.to_str().unwrap();
        std::fs::write(&path, WORKING).unwrap();
        std::fs::write(
            dir.path().join("walg.env.prev"),
            "export WALG_S3_PREFIX='s3://stale'\n",
        )
        .unwrap();

        assert!(run_script(&walg_env_restore_script(path_str, ATTEMPT), ""));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), WORKING);

        // Nor is a missing walg.env created.
        std::fs::remove_file(&path).unwrap();
        assert!(run_script(&walg_env_restore_script(path_str, ATTEMPT), ""));
        assert!(!path.exists());
    }

    #[test]
    fn a_walg_env_that_cannot_be_written_is_left_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("walg.env");
        let path_str = path.to_str().unwrap();
        std::fs::write(&path, WORKING).unwrap();
        // A directory where the new file would be written makes the write fail.
        std::fs::create_dir_all(dir.path().join("walg.env.tmp").join("busy")).unwrap();

        assert!(!run_script(
            &walg_env_replace_script(path_str),
            &new_walg_env()
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), WORKING);
        // Putting it back after that failure changes nothing.
        assert!(run_script(&walg_env_restore_script(path_str, ATTEMPT), ""));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), WORKING);
    }
}
