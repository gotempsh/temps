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
//! | `"postgres"`   | standalone, on a worker | yes (asked of its agent) | `"postgres_walg"` |
//! | `"postgres"`   | standalone, on a worker | no      | `"postgres_workload_job"` |
//! | `"postgres"`   | other        | yes              | `"postgres_walg"`   |
//! | `"postgres"`   | other        | no               | `"postgres_pgdump"` |
//! | `"redis"` / `"mariadb"` / storage | standalone, on a worker | – | `"worker_agent"` |
//! | `"redis"`      | any          | –                | `"redis"`           |
//! | `"mongodb"`    | any, here or on a worker | checked by the engine in the container | `"mongodb"` |
//! | `"mariadb"`    | any          | yes (PITR tools) | `"mariadb_physical"` |
//! | `"mariadb"`    | any          | no               | `"mariadb_dump"`    |
//! | `"s3"` / `"minio"` / `"blob"` | any | –       | `"s3_mirror"`       |
//! | anything else  | –            | –                | `Err(Unsupported)`  |
//!
//! Every service on a worker node is backed up on that node. A PostgreSQL
//! service on a WAL-G image takes the same engine as on the control plane,
//! whose container steps its node's agent runs (ADR-099); without WAL-G it
//! takes a dump job (`workload_job`). A MongoDB takes the same `mongodb`
//! engine everywhere: its WAL-G check runs in the container, on the node
//! that has it. The others are backed up by the node's agent
//! (`worker_agent`).
//!
//! The WAL-G and PITR-tool probes inspect a container on this host's Docker,
//! or, for a worker's PostgreSQL, ask its node's agent.
//! [`resolve_engine_key_on_this_host`] opens the local daemon only for a
//! local probe, so a server without one (`--profile control-plane`) still
//! resolves every worker service.

use async_trait::async_trait;
use thiserror::Error;

/// Asks a worker node whether a tool exists in one of its containers.
#[async_trait]
pub trait WorkerToolProbe: Send + Sync {
    /// Whether `wal-g` runs in `container_name` on worker `node_id`: the
    /// check [`container_has_walg`] runs on this host. `false` on any error,
    /// like that one.
    async fn worker_has_walg(&self, node_id: i32, container_name: &str) -> bool;
}

/// How long the agent's WAL-G check may take, as the local one.
const WORKER_WALG_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[async_trait]
impl WorkerToolProbe for temps_providers::ExternalServiceManager {
    async fn worker_has_walg(&self, node_id: i32, container_name: &str) -> bool {
        let probe = self.exec_in_worker_service(
            node_id,
            container_name,
            vec!["which".to_string(), "wal-g".to_string()],
        );
        match tokio::time::timeout(WORKER_WALG_PROBE_TIMEOUT, probe).await {
            Ok(Ok(exit_code)) => exit_code == 0,
            Ok(Err(error)) => {
                tracing::warn!(
                    node_id,
                    container = container_name,
                    %error,
                    "Could not ask the node's agent whether WAL-G is installed"
                );
                false
            }
            Err(_) => {
                tracing::warn!(
                    node_id,
                    container = container_name,
                    "The node's agent did not answer whether WAL-G is installed in time"
                );
                false
            }
        }
    }
}

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
}

/// Whether the engine choice for `service` inspects a local container, or is
/// known from the row alone.
enum EngineChoice {
    Known(&'static str),
    ProbePostgres,
    /// A standalone PostgreSQL on worker `node_id`: its agent is asked.
    ProbeWorkerPostgres {
        node_id: i32,
    },
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
            // A standalone database on a worker node is backed up on that
            // node. Its container is not on this host's Docker, so the
            // WAL-G check asks the node's agent.
            if super::workload_job::takes_job_path(service) {
                if let Some(node_id) = super::workload_job::worker_node(service) {
                    return Ok(EngineChoice::ProbeWorkerPostgres { node_id });
                }
            }
            Ok(EngineChoice::ProbePostgres)
        }
        // Every other service on a worker node is backed up by its node's
        // agent, with the engine's own tool on that node.
        _ if super::worker_agent::takes_agent_path(service) => {
            Ok(EngineChoice::Known(super::worker_agent::ENGINE_KEY))
        }
        "redis" => Ok(EngineChoice::Known("redis")),
        "mongodb" | "mongo" => Ok(EngineChoice::Known("mongodb")),
        "mariadb" => Ok(EngineChoice::ProbeMariadb),
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
    workers: &dyn WorkerToolProbe,
) -> Result<&'static str, ResolveEngineError> {
    match engine_choice(service)? {
        EngineChoice::Known(key) => return Ok(key),
        EngineChoice::ProbeWorkerPostgres { node_id } => {
            return Ok(worker_postgres_engine(service, node_id, workers).await)
        }
        EngineChoice::ProbePostgres | EngineChoice::ProbeMariadb => {}
    }
    let docker = bollard::Docker::connect_with_local_defaults().map_err(|error| {
        ResolveEngineError::LocalDockerUnavailable {
            service_id: service.id,
            service_type: service.service_type.clone(),
            reason: error.to_string(),
        }
    })?;
    resolve_engine_key(service, &docker, workers).await
}

/// The engine of a standalone PostgreSQL on a worker: WAL-G as on the
/// control plane when its image has it, else the node's dump job.
async fn worker_postgres_engine(
    service: &temps_entities::external_services::Model,
    node_id: i32,
    workers: &dyn WorkerToolProbe,
) -> &'static str {
    if workers
        .worker_has_walg(node_id, &service_container_name(service))
        .await
    {
        super::postgres_walg::ENGINE_KEY
    } else {
        super::workload_job::ENGINE_KEY
    }
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
    workers: &dyn WorkerToolProbe,
) -> Result<&'static str, ResolveEngineError> {
    match engine_choice(service)? {
        EngineChoice::Known(key) => Ok(key),
        EngineChoice::ProbeWorkerPostgres { node_id } => {
            Ok(worker_postgres_engine(service, node_id, workers).await)
        }
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

    /// A fleet whose agents find no WAL-G, or cannot be asked.
    struct NoWorkers;

    #[async_trait]
    impl WorkerToolProbe for NoWorkers {
        async fn worker_has_walg(&self, _node_id: i32, _container_name: &str) -> bool {
            false
        }
    }

    /// A fleet whose agents find WAL-G, recording what they were asked.
    #[derive(Default)]
    struct WalgWorkers(std::sync::Mutex<Vec<(i32, String)>>);

    #[async_trait]
    impl WorkerToolProbe for WalgWorkers {
        async fn worker_has_walg(&self, node_id: i32, container_name: &str) -> bool {
            self.0
                .lock()
                .expect("asked")
                .push((node_id, container_name.to_string()));
            true
        }
    }

    /// Parity (ADR-099): a PostgreSQL on a WAL-G image takes the same engine
    /// on a worker as on the control plane, decided by the same check run by
    /// its node's agent in the service's container; without WAL-G, the
    /// worker's dump job. No local Docker is opened either way.
    #[tokio::test]
    async fn a_worker_postgres_with_walg_takes_the_control_plane_engine() {
        let mut service = make_service("postgres", "standalone");
        service.node_id = Some(3);
        let workers = WalgWorkers::default();
        assert_eq!(
            resolve_engine_key_on_this_host(&service, &workers)
                .await
                .ok(),
            Some(super::super::postgres_walg::ENGINE_KEY)
        );
        assert_eq!(
            *workers.0.lock().expect("asked"),
            vec![(3, "postgres-test-svc".to_string())]
        );
        assert_eq!(
            resolve_engine_key_on_this_host(&service, &NoWorkers)
                .await
                .ok(),
            Some(super::super::workload_job::ENGINE_KEY)
        );
        // A cluster member is never asked: clusters keep their engine.
        let mut cluster = make_service("postgres", "cluster");
        cluster.node_id = Some(3);
        let workers = WalgWorkers::default();
        assert_eq!(
            resolve_engine_key_on_this_host(&cluster, &workers)
                .await
                .ok(),
            Some("postgres_cluster")
        );
        assert!(workers.0.lock().expect("asked").is_empty());
    }

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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
                assert!(
                    matches!(result, Ok("postgres_workload_job")),
                    "got: {:?}",
                    result
                );
                // So is one in the default mode: its container is on the
                // worker, never on this host's Docker.
                svc.workload_mode = ServiceWorkloadMode::Legacy;
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
        assert!(matches!(
            engine_choice(&worker_postgres),
            Ok(EngineChoice::ProbeWorkerPostgres { node_id: 3 })
        ));
        assert_eq!(
            resolve_engine_key_on_this_host(&worker_postgres, &NoWorkers)
                .await
                .ok(),
            Some("postgres_workload_job")
        );
        for (service, expected) in [
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
                resolve_engine_key_on_this_host(&service, &NoWorkers)
                    .await
                    .ok(),
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

    /// Every non-PostgreSQL service on a worker is backed up by its node's
    /// agent, resolved without this host's Docker; MariaDB is no longer
    /// refused. The same types on the control plane keep their engines.
    #[tokio::test]
    async fn worker_services_resolve_to_the_agent_engine_without_docker() {
        for service_type in ["mariadb", "redis", "s3", "rustfs", "blob"] {
            let mut service = make_service(service_type, "standalone");
            service.node_id = Some(3);
            assert_eq!(
                resolve_engine_key_on_this_host(&service, &NoWorkers)
                    .await
                    .ok(),
                Some(super::super::worker_agent::ENGINE_KEY),
                "{service_type} on a worker"
            );
        }
        assert!(matches!(
            engine_choice(&make_service("mariadb", "standalone")),
            Ok(EngineChoice::ProbeMariadb)
        ));
        assert!(matches!(
            engine_choice(&make_service("redis", "standalone")),
            Ok(EngineChoice::Known("redis"))
        ));
    }

    /// Parity (ADR-099): a MongoDB takes the same engine on a worker as on
    /// the control plane, without this host's Docker; the engine checks for
    /// WAL-G in the container, on the node that runs it.
    #[tokio::test]
    async fn a_worker_mongodb_takes_the_control_plane_engine() {
        for service_type in ["mongodb", "mongo"] {
            let local = make_service(service_type, "standalone");
            let mut worker = local.clone();
            worker.node_id = Some(3);
            for service in [&local, &worker] {
                assert!(matches!(
                    engine_choice(service),
                    Ok(EngineChoice::Known(key)) if key == super::super::mongodb::ENGINE_KEY
                ));
                assert_eq!(
                    resolve_engine_key_on_this_host(service, &NoWorkers)
                        .await
                        .ok(),
                    Some(super::super::mongodb::ENGINE_KEY),
                    "{service_type} on node {:?}",
                    service.node_id
                );
            }
        }
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
                let result = resolve_engine_key(&svc, &docker, &NoWorkers).await;
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
