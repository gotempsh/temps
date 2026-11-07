// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Engine-key resolution for external service backups (ADR-014 Phase 2–4).
//!
//! [`resolve_engine_key`] maps a `external_services` row to the correct engine
//! key string. Handlers call this before enqueuing a `backup_jobs` row so the
//! runner knows which `BackupEngine` to dispatch.
//!
//! ## Routing rules
//!
//! | `service_type` | `topology`   | WAL-G available? | engine key          |
//! |----------------|--------------|------------------|---------------------|
//! | `"postgres"`   | `"cluster"`  | (always WAL-G)   | `"postgres_cluster"` |
//! | `"postgres"`   | standalone, on a worker | –       | `"postgres_workload_job"` |
//! | `"postgres"`   | other        | yes              | `"postgres_walg"`   |
//! | `"postgres"`   | other        | no               | `"postgres_pgdump"` |
//! | `"redis"`      | any          | –                | `"redis"`           |
//! | `"mongodb"`    | any          | –                | `"mongodb"`         |
//! | `"mariadb"`    | any          | yes (PITR tools) | `"mariadb_physical"` |
//! | `"mariadb"`    | any          | no               | `"mariadb_dump"`    |
//! | `"s3"` / `"minio"` / `"blob"` | any | –       | `"s3_mirror"`       |
//! | anything else  | –            | –                | `Err(Unsupported)`  |
//!
//! A MariaDB on a worker node is refused (`WorkerNodeUnsupported`): its
//! engines exec into the container through this host's Docker.
//!
//! The WAL-G and PITR-tool probes inspect a container on this host's Docker.
//! [`resolve_engine_key_on_this_host`] opens the daemon only for those, so a
//! server without one (`--profile control-plane`) still resolves every engine
//! that needs no local probe, including a worker database's job.

use thiserror::Error;

/// Error returned by [`resolve_engine_key`] when no engine can be selected.
#[derive(Error, Debug)]
pub enum ResolveEngineError {
    /// The service's `service_type` is not supported by any registered engine.
    #[error(
        "Service type '{service_type}' (service_id={service_id}) is not supported by any backup engine. \
         Supported types: postgres, redis, mongodb, mariadb, s3, minio, blob"
    )]
    Unsupported {
        service_id: i32,
        service_type: String,
    },

    /// Docker probe failed (non-fatal: fall back to pg_dump).
    #[error(
        "WAL-G probe for service_id={service_id} failed (will use pg_dump fallback): {reason}"
    )]
    WalgProbeFailed { service_id: i32, reason: String },

    /// Choosing the engine needs a look inside the service's local container,
    /// and this server cannot reach a Docker daemon.
    #[error(
        "Choosing the backup engine of {service_type} service {service_id} needs this server's \
         Docker daemon to inspect its container, and it cannot be reached: {reason}"
    )]
    LocalDockerUnavailable {
        service_id: i32,
        service_type: String,
        reason: String,
    },

    /// The service runs on a worker node, and its engines only work on a
    /// container on this host's Docker.
    #[error(
        "Backups of {service_type} service {service_id} are not supported yet: it runs on worker \
         node {node_id}, and its backup engines only reach containers on this server's Docker. \
         Standalone PostgreSQL databases on worker nodes are backed up by a job on their node"
    )]
    WorkerNodeUnsupported {
        service_id: i32,
        service_type: String,
        node_id: i32,
    },
}

/// Whether the engine choice for `service` inspects a local container, or is
/// known from the row alone.
enum EngineChoice {
    Known(&'static str),
    ProbePostgres,
    ProbeMariadb,
}

fn engine_choice(
    service: &temps_entities::external_services::Model,
) -> Result<EngineChoice, ResolveEngineError> {
    match service.service_type.as_str() {
        "postgres" | "postgresql" | "timescale" | "timescaledb" => {
            if service.topology.as_str() == "cluster" {
                return Ok(EngineChoice::Known("postgres_cluster"));
            }
            // ADR-055 Phase 9: a standalone database on a worker node is
            // backed up by a job on that node, through its agent. Its
            // container is not on this host's Docker, so probing it here
            // could only miss.
            if super::workload_job::takes_job_path(service) {
                return Ok(EngineChoice::Known(super::workload_job::ENGINE_KEY));
            }
            Ok(EngineChoice::ProbePostgres)
        }
        "redis" => Ok(EngineChoice::Known("redis")),
        "mongodb" | "mongo" => Ok(EngineChoice::Known("mongodb")),
        "mariadb" => {
            if let Some(node_id) = super::workload_job::worker_node(service) {
                return Err(ResolveEngineError::WorkerNodeUnsupported {
                    service_id: service.id,
                    service_type: service.service_type.clone(),
                    node_id,
                });
            }
            Ok(EngineChoice::ProbeMariadb)
        }
        "rustfs" | "s3" | "minio" | "blob" => Ok(EngineChoice::Known("s3_mirror")),
        other => Err(ResolveEngineError::Unsupported {
            service_id: service.id,
            service_type: other.to_string(),
        }),
    }
}

/// [`resolve_engine_key`], opening this host's Docker daemon only when the
/// choice inspects a local container. What the handlers and the scheduler
/// call: a server without Docker resolves everything else, and says why
/// when it cannot.
pub async fn resolve_engine_key_on_this_host(
    service: &temps_entities::external_services::Model,
) -> Result<&'static str, ResolveEngineError> {
    if let EngineChoice::Known(key) = engine_choice(service)? {
        return Ok(key);
    }
    let docker = bollard::Docker::connect_with_local_defaults().map_err(|error| {
        ResolveEngineError::LocalDockerUnavailable {
            service_id: service.id,
            service_type: service.service_type.clone(),
            reason: error.to_string(),
        }
    })?;
    resolve_engine_key(service, &docker).await
}

/// Resolve the engine key for a given external service.
///
/// The function is `async` because the Postgres routing requires a Docker probe
/// to check whether WAL-G is installed in the running container. All other
/// service types resolve synchronously (the `async` wrapper has no overhead
/// since the futures are immediately resolved).
///
/// Returns a `'static str` that matches a registered `BackupEngine::engine()`.
pub async fn resolve_engine_key(
    service: &temps_entities::external_services::Model,
    docker: &bollard::Docker,
) -> Result<&'static str, ResolveEngineError> {
    match engine_choice(service)? {
        EngineChoice::Known(key) => Ok(key),
        // Probe for WAL-G in the running container. Container naming must
        // match the legacy provider's `get_container_name()` —
        // `postgres-{name}` for standalone Postgres. Using a different
        // prefix here makes the probe miss every container and silently fall
        // back to pg_dump.
        EngineChoice::ProbePostgres => {
            let container_name = service_container_name(service);
            if container_has_walg(docker, &container_name).await {
                Ok("postgres_walg")
            } else {
                Ok("postgres_pgdump")
            }
        }
        // Cloud-grade PITR needs WAL-G, mariadb-backup (physical base), and
        // mariadb-binlog (binary-log replay) inside the container. Container
        // naming must match the provider's `get_container_name()` —
        // `mariadb-{name}`. Using a different prefix here makes the probe
        // miss every container and silently fall back to the logical dump.
        EngineChoice::ProbeMariadb => {
            let container_name = service_container_name(service);
            if container_has_mariadb_pitr_tools(docker, &container_name).await {
                Ok("mariadb_physical")
            } else {
                Ok("mariadb_dump")
            }
        }
    }
}

/// Resolve the actual container targeted by backup execution and capability
/// probes. Imported services persist their original Docker container name;
/// synthesizing a Temps-managed name for them probes and backs up the wrong
/// container.
pub(crate) fn service_container_name(service: &temps_entities::external_services::Model) -> String {
    service.container_name.clone().unwrap_or_else(|| {
        let prefix = match service.service_type.as_str() {
            "mongodb" | "mongo" => "temps-mongodb",
            "postgres" | "postgresql" | "timescale" | "timescaledb" => "postgres",
            "mariadb" => "mariadb",
            "redis" => "redis",
            _ => service.service_type.as_str(),
        };
        format!("{prefix}-{}", service.name)
    })
}

/// Probe whether `wal-g` is available in `container_name`.
///
/// Uses `which wal-g` via docker exec (detach=true). Returns `false` on any
/// error (container not running, exec failure, etc.) so the caller can fall
/// back to pg_dump gracefully.
///
/// Mirrors the implementation in `temps-providers/src/externalsvc/postgres.rs:536`
/// but is a standalone free function so `temps-backup` does not need to depend
/// on the full `ExternalService` trait.
pub(crate) async fn container_has_walg(docker: &bollard::Docker, container_name: &str) -> bool {
    use bollard::exec::{CreateExecOptions, StartExecOptions};

    let exec = match docker
        .create_exec(
            container_name,
            CreateExecOptions {
                cmd: Some(vec!["which", "wal-g"]),
                attach_stdout: Some(false),
                attach_stderr: Some(false),
                ..Default::default()
            },
        )
        .await
    {
        Ok(e) => e,
        Err(_) => return false,
    };

    match docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach: true,
                ..Default::default()
            }),
        )
        .await
    {
        Ok(_) => {}
        Err(_) => return false,
    }

    // Poll the exec for up to 5 seconds to check the exit code.
    for _ in 0..5u32 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        match docker.inspect_exec(&exec.id).await {
            Ok(info) if info.running == Some(false) => {
                return info.exit_code == Some(0);
            }
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
    false
}

/// Probe whether the MariaDB PITR tools (`wal-g`, `mariadb-backup` and `mariadb-binlog`)
/// are available in `container_name`.
///
/// Both are required: `mariadb-backup` for the physical base and
/// `mariadb-binlog`/`mysqlbinlog` for replay. Returns `false` on any error or
/// if any tool is missing, so existing stock MariaDB containers keep the
/// logical `mariadb_dump` fallback while new managed images use WAL-G.
/// Shell test that exits 0 only if every MariaDB physical PITR tool resolves.
/// `mariadb-binlog` and `mysqlbinlog` are the same tool (symlink); either
/// counts, as do `mariadb-backup` and `mariabackup`.
pub(crate) const MARIADB_PITR_TOOLS_PROBE: &str = "command -v wal-g >/dev/null 2>&1; w=$?; \
     command -v mariadb-backup >/dev/null 2>&1 || command -v mariabackup >/dev/null 2>&1; \
     a=$?; \
     command -v mariadb-binlog >/dev/null 2>&1 || command -v mysqlbinlog >/dev/null 2>&1; \
     b=$?; \
     [ $w -eq 0 ] && [ $a -eq 0 ] && [ $b -eq 0 ]";

pub(crate) async fn container_has_mariadb_pitr_tools(
    docker: &bollard::Docker,
    container_name: &str,
) -> bool {
    use bollard::exec::{CreateExecOptions, StartExecOptions};

    // Single shell test: exit 0 only if ALL tools resolve. `mariadb-binlog`
    // and `mysqlbinlog` are the same tool (symlink); accept either.
    let probe = MARIADB_PITR_TOOLS_PROBE;

    let exec = match docker
        .create_exec(
            container_name,
            CreateExecOptions {
                cmd: Some(vec!["sh", "-c", probe]),
                attach_stdout: Some(false),
                attach_stderr: Some(false),
                ..Default::default()
            },
        )
        .await
    {
        Ok(e) => e,
        Err(_) => return false,
    };

    if docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach: true,
                ..Default::default()
            }),
        )
        .await
        .is_err()
    {
        return false;
    }

    for _ in 0..5u32 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        match docker.inspect_exec(&exec.id).await {
            Ok(info) if info.running == Some(false) => {
                return info.exit_code == Some(0);
            }
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
    false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service(
        service_type: &str,
        topology: &str,
    ) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 42,
            name: "test-svc".to_string(),
            service_type: service_type.to_string(),
            topology: topology.to_string(),
            status: "running".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            node_id: None,
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

    #[test]
    fn a_postgres_on_a_worker_in_enforce_is_backed_up_as_a_job() {
        use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                // Connecting is lazy: the job path never probes Docker.
                let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
                    return;
                };
                let mut svc = make_service("postgres", "standalone");
                svc.node_id = Some(3);
                svc.workload_mode = ServiceWorkloadMode::Enforce;
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(
                    matches!(result, Ok("postgres_workload_job")),
                    "got: {:?}",
                    result
                );
                // So is one in the default mode: its container is on the
                // worker, never on this host's Docker.
                svc.workload_mode = ServiceWorkloadMode::Legacy;
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(
                    matches!(result, Ok("postgres_workload_job")),
                    "got: {:?}",
                    result
                );
            });
    }

    /// The live failure: on a `--profile control-plane` server, triggering a
    /// backup of a worker's Postgres failed before anything ran, because
    /// choosing the engine opened this host's Docker socket first. A
    /// choice that inspects no local container needs no daemon.
    #[tokio::test]
    async fn engines_that_inspect_no_local_container_resolve_without_docker() {
        let mut worker_postgres = make_service("postgres", "standalone");
        worker_postgres.node_id = Some(3);
        let mut cluster = make_service("postgres", "cluster");
        cluster.node_id = Some(3);
        for (service, expected) in [
            (worker_postgres, "postgres_workload_job"),
            (cluster, "postgres_cluster"),
            (make_service("redis", "standalone"), "redis"),
            (make_service("mongodb", "standalone"), "mongodb"),
            (make_service("s3", "standalone"), "s3_mirror"),
        ] {
            let result = engine_choice(&service);
            assert!(
                matches!(result, Ok(EngineChoice::Known(key)) if key == expected),
                "{} on node {:?}",
                service.service_type,
                service.node_id
            );
            assert_eq!(
                resolve_engine_key_on_this_host(&service).await.ok(),
                Some(expected)
            );
        }

        // A local Postgres or MariaDB still inspects its container.
        assert!(matches!(
            engine_choice(&make_service("postgres", "standalone")),
            Ok(EngineChoice::ProbePostgres)
        ));
        assert!(matches!(
            engine_choice(&make_service("mariadb", "standalone")),
            Ok(EngineChoice::ProbeMariadb)
        ));
    }

    /// A MariaDB on a worker cannot be backed up by engines that exec into a
    /// container on this host: refused up front, naming the node, instead of
    /// a missing Docker socket or a backup that fails later.
    #[tokio::test]
    async fn a_mariadb_on_a_worker_is_refused_with_its_node() {
        let mut service = make_service("mariadb", "standalone");
        service.node_id = Some(3);
        let error = resolve_engine_key_on_this_host(&service)
            .await
            .expect_err("no engine reaches a worker's MariaDB");
        assert!(
            matches!(
                error,
                ResolveEngineError::WorkerNodeUnsupported { node_id: 3, .. }
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("worker node 3"), "{error}");
    }

    #[test]
    fn test_redis_resolves_to_redis() {
        let svc = make_service("redis", "standalone");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() {
                    return; // no Docker available in test env, skip
                }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(matches!(result, Ok("redis")), "got: {:?}", result);
            });
    }

    #[test]
    fn test_mongodb_resolves_to_mongodb() {
        let svc = make_service("mongodb", "standalone");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() {
                    return;
                }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(matches!(result, Ok("mongodb")), "got: {:?}", result);
            });
    }

    #[test]
    fn test_s3_resolves_to_s3_mirror() {
        let svc = make_service("s3", "standalone");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() {
                    return;
                }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(matches!(result, Ok("s3_mirror")), "got: {:?}", result);
            });
    }

    #[test]
    fn test_minio_resolves_to_s3_mirror() {
        let svc = make_service("minio", "standalone");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() {
                    return;
                }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(matches!(result, Ok("s3_mirror")), "got: {:?}", result);
            });
    }

    #[test]
    fn test_postgres_cluster_resolves_to_postgres_cluster() {
        let svc = make_service("postgres", "cluster");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() {
                    return;
                }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(
                    matches!(result, Ok("postgres_cluster")),
                    "got: {:?}",
                    result
                );
            });
    }

    #[test]
    fn test_mariadb_resolves_to_dump_when_pitr_tools_absent() {
        // With no running `mariadb-test-svc` container, the PITR-tools probe
        // fails and dispatch must fall back to the logical dump engine.
        let svc = make_service("mariadb", "standalone");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() {
                    return;
                }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(matches!(result, Ok("mariadb_dump")), "got: {:?}", result);
            });
    }

    #[test]
    fn test_unsupported_service_type() {
        let svc = make_service("elasticsearch", "standalone");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let docker = bollard::Docker::connect_with_local_defaults();
                if docker.is_err() { return; }
                let docker = docker.unwrap();
                let result = resolve_engine_key(&svc, &docker).await;
                assert!(matches!(result, Err(ResolveEngineError::Unsupported { service_type, .. }) if service_type == "elasticsearch"));
            });
    }

    #[test]
    fn imported_service_uses_persisted_container_name() {
        let mut service = make_service("postgres", "standalone");
        service.container_name = Some("imported-production-db".to_string());

        assert_eq!(service_container_name(&service), "imported-production-db");
    }

    #[test]
    fn managed_service_uses_engine_specific_container_name() {
        assert_eq!(
            service_container_name(&make_service("mongodb", "standalone")),
            "temps-mongodb-test-svc"
        );
        assert_eq!(
            service_container_name(&make_service("redis", "standalone")),
            "redis-test-svc"
        );
    }
}
