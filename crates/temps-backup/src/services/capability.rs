// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::engines::dispatch::{
    container_has_mariadb_pitr_tools, container_has_walg, service_container_name,
    MARIADB_PITR_TOOLS_PROBE,
};
use crate::engines::workload_job::worker_node;
use bollard::query_parameters::InspectContainerOptions;
use std::time::Duration;
use temps_entities::external_services;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackupCapabilityError {
    #[error("External service {service_id} was not found while checking backup capability")]
    ServiceNotFound { service_id: i32 },
    #[error(
        "Failed to load external service {service_id} while checking backup capability: {source}"
    )]
    LoadService {
        service_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalServiceBackupCapability {
    pub cloud_backup_compatible: bool,
    pub verified: bool,
    pub wal_g_installed: bool,
    pub engine: String,
    pub artifact: String,
    pub reason: Option<String>,
    pub remediation: Option<String>,
    pub recommended_image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolProbe {
    Available,
    Missing,
    Unverified(UnverifiedCause),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnverifiedCause {
    Unavailable,
    TimedOut { stage: &'static str },
}

const DOCKER_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// Determine Cloud mirror compatibility from the same live container and tool
/// probes used by backup engine dispatch. Docker failures are represented as
/// an unverified capability instead of silently becoming a local-only engine.
pub async fn probe_external_service_backup_capability(
    service: &external_services::Model,
    manager: &temps_providers::ExternalServiceManager,
) -> ExternalServiceBackupCapability {
    // A standalone PostgreSQL on a worker takes the control plane's WAL-G
    // engine when its image has WAL-G (ADR-099): the same check, run by its
    // node's agent.
    if crate::engines::workload_job::takes_job_path(service) {
        if let Some(node_id) = worker_node(service) {
            let probe = probe_on_worker(service, "postgres", node_id, manager).await;
            return worker_postgres_capability(service, node_id, probe);
        }
    }
    // The other services on a worker node are backed up by its agent: what
    // that writes does not depend on the tools in its image.
    if let Some(capability) = worker_capability(service) {
        return capability;
    }
    let service_type = service.service_type.to_ascii_lowercase();
    if !requires_live_tool_probe(&service_type, &service.topology) {
        return classify_capability(service, ToolProbe::Available);
    }
    // A worker-hosted service's container is on that worker's Docker, not
    // this host's: ask its agent, or a running WAL-G service would read as
    // "container unavailable".
    if let Some(node_id) = worker_node(service) {
        let probe = probe_on_worker(service, &service_type, node_id, manager).await;
        return classify_capability(service, probe);
    }

    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(_) => {
            return classify_capability(
                service,
                ToolProbe::Unverified(UnverifiedCause::Unavailable),
            )
        }
    };
    match tokio::time::timeout(DOCKER_PROBE_TIMEOUT, docker.ping()).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {
            return classify_capability(
                service,
                ToolProbe::Unverified(UnverifiedCause::Unavailable),
            )
        }
        Err(_) => {
            return classify_capability(
                service,
                ToolProbe::Unverified(UnverifiedCause::TimedOut {
                    stage: "Docker availability check",
                }),
            )
        }
    }

    let container_name = service_container_name(service);
    let inspect = match tokio::time::timeout(
        DOCKER_PROBE_TIMEOUT,
        docker.inspect_container(&container_name, None::<InspectContainerOptions>),
    )
    .await
    {
        Ok(Ok(inspect)) => inspect,
        Ok(Err(_)) => {
            return classify_capability(
                service,
                ToolProbe::Unverified(UnverifiedCause::Unavailable),
            )
        }
        Err(_) => {
            return classify_capability(
                service,
                ToolProbe::Unverified(UnverifiedCause::TimedOut {
                    stage: "container inspection",
                }),
            )
        }
    };
    if inspect.state.and_then(|state| state.running) != Some(true) {
        return classify_capability(service, ToolProbe::Unverified(UnverifiedCause::Unavailable));
    }

    let tool_probe = async {
        if service_type == "mariadb" {
            container_has_mariadb_pitr_tools(&docker, &container_name).await
        } else {
            container_has_walg(&docker, &container_name).await
        }
    };
    let available = match tokio::time::timeout(TOOL_PROBE_TIMEOUT, tool_probe).await {
        Ok(available) => available,
        Err(_) => {
            return classify_capability(
                service,
                ToolProbe::Unverified(UnverifiedCause::TimedOut {
                    stage: "backup tool check",
                }),
            )
        }
    };
    classify_capability(
        service,
        if available {
            ToolProbe::Available
        } else {
            ToolProbe::Missing
        },
    )
}

/// The tool probe of [`probe_external_service_backup_capability`], run in
/// the service's container on worker `node_id` through its agent.
async fn probe_on_worker(
    service: &external_services::Model,
    service_type: &str,
    node_id: i32,
    manager: &temps_providers::ExternalServiceManager,
) -> ToolProbe {
    let container_name = service_container_name(service);
    let running = tokio::time::timeout(
        DOCKER_PROBE_TIMEOUT,
        manager.worker_service_running(node_id, &container_name),
    )
    .await;
    let running = match running {
        Ok(Ok(running)) => running,
        Ok(Err(_)) => return ToolProbe::Unverified(UnverifiedCause::Unavailable),
        Err(_) => {
            return ToolProbe::Unverified(UnverifiedCause::TimedOut {
                stage: "container inspection on its node",
            })
        }
    };
    if !running {
        return ToolProbe::Unverified(UnverifiedCause::Unavailable);
    }
    let exit_code = tokio::time::timeout(
        TOOL_PROBE_TIMEOUT,
        manager.exec_in_worker_service(node_id, &container_name, tool_probe_command(service_type)),
    )
    .await;
    match exit_code {
        Ok(Ok(code)) => tool_probe_from_exit_code(code),
        Ok(Err(_)) => ToolProbe::Unverified(UnverifiedCause::Unavailable),
        Err(_) => ToolProbe::Unverified(UnverifiedCause::TimedOut {
            stage: "backup tool check",
        }),
    }
}

/// The command whose exit status tells whether the backup tools exist in a
/// container; the same checks `container_has_walg` and
/// `container_has_mariadb_pitr_tools` run locally.
fn tool_probe_command(service_type: &str) -> Vec<String> {
    if service_type == "mariadb" {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            MARIADB_PITR_TOOLS_PROBE.to_string(),
        ]
    } else {
        vec!["which".to_string(), "wal-g".to_string()]
    }
}

fn tool_probe_from_exit_code(exit_code: i64) -> ToolProbe {
    if exit_code == 0 {
        ToolProbe::Available
    } else {
        ToolProbe::Missing
    }
}

fn requires_live_tool_probe(service_type: &str, topology: &str) -> bool {
    match service_type {
        "postgres" | "postgresql" | "timescale" | "timescaledb" => topology != "cluster",
        "mariadb" | "mongodb" | "mongo" | "redis" => true,
        _ => false,
    }
}

fn classify_capability(
    service: &external_services::Model,
    probe: ToolProbe,
) -> ExternalServiceBackupCapability {
    let service_type = service.service_type.to_ascii_lowercase();
    match service_type.as_str() {
        "postgres" | "postgresql" | "timescale" | "timescaledb" => {
            if service.topology == "cluster" || probe == ToolProbe::Available {
                compatible(
                    if service.topology == "cluster" {
                        "postgres_cluster"
                    } else {
                        "postgres_walg"
                    },
                    "walg_repository",
                    true,
                )
            } else if let ToolProbe::Unverified(cause) = probe {
                unverified(service, "postgres_pgdump", "logical_pg_dump", cause)
            } else {
                local_only(
                    service,
                    "postgres_pgdump",
                    "logical_pg_dump",
                    "PostgreSQL WAL-G is not installed; pg_dump backups remain local-only and cannot be mirrored for managed PITR.",
                    "Upgrade to a WAL-G-enabled PostgreSQL image, restart the service, and run a new backup.",
                    "gotempsh/postgres-walg:18-bookworm",
                )
            }
        }
        "mariadb" => match probe {
            ToolProbe::Available => compatible(
                "mariadb_physical",
                "mariadb_physical_walg_repository",
                true,
            ),
            ToolProbe::Unverified(cause) => {
                unverified(service, "mariadb_dump", "logical_mariadb_dump", cause)
            }
            ToolProbe::Missing => local_only(
                service,
                "mariadb_dump",
                "logical_mariadb_dump",
                "MariaDB physical WAL-G PITR tools are not installed; logical mariadb-dump backups remain local-only.",
                "Upgrade to the MariaDB WAL-G image, restart the service, and run a new physical backup.",
                "ghcr.io/gotempsh/mariadb-walg:11.4",
            ),
        },
        "mongodb" | "mongo" => match probe {
            ToolProbe::Available => compatible("mongodb", "mongodb_walg_stream", true),
            ToolProbe::Unverified(cause) => {
                unverified(service, "mongodb", "mongodump_archive", cause)
            }
            ToolProbe::Missing => local_only(
                service,
                "mongodb",
                "mongodump_archive",
                "MongoDB WAL-G streaming is not installed; mongodump archives remain local-only.",
                "Upgrade to the MongoDB WAL-G image, restart the service, and run a new backup.",
                "gotempsh/mongodb-walg:8.0",
            ),
        },
        "redis" => match probe {
            ToolProbe::Available => compatible("redis", "redis_walg_stream", true),
            ToolProbe::Unverified(cause) => unverified(service, "redis", "redis_rdb", cause),
            ToolProbe::Missing => local_only(
                service,
                "redis",
                "redis_rdb",
                "Redis WAL-G streaming is not installed; standalone RDB backups remain local-only.",
                "Upgrade to the Redis WAL-G image, restart the service, and run a new backup.",
                "ghcr.io/gotempsh/redis-walg:8-trixie",
            ),
        },
        "rustfs" | "s3" | "minio" | "blob" => compatible("s3_mirror", "object_set", false),
        _ => ExternalServiceBackupCapability {
            cloud_backup_compatible: false,
            verified: true,
            wal_g_installed: false,
            engine: "unsupported".to_string(),
            artifact: "unsupported".to_string(),
            reason: Some(format!(
                "Service type '{}' does not have a Cloud mirror adapter.",
                service.service_type
            )),
            remediation: Some(
                "Use a supported source type: PostgreSQL/TimescaleDB with WAL-G, MariaDB physical WAL-G, MongoDB or Redis WAL-G streams, or RustFS/S3/MinIO/blob object storage."
                    .to_string(),
            ),
            recommended_image: None,
        },
    }
}

/// What a backup of a storage service on a worker node writes: a bucket
/// mirror taken by its agent. `None` for a service the local engines back
/// up: MongoDB, Redis and MariaDB take the control plane's engines on a
/// worker, checked in their container like local ones (`worker_service_kind`
/// never returns them).
fn worker_capability(
    service: &external_services::Model,
) -> Option<ExternalServiceBackupCapability> {
    use crate::engines::worker_agent::{self, WorkerServiceKind};
    worker_node(service)?;
    if !worker_agent::takes_agent_path(service) {
        return None;
    }
    match worker_agent::worker_service_kind(&service.service_type)? {
        WorkerServiceKind::Storage => {
            Some(compatible(worker_agent::ENGINE_KEY, "object_set", false))
        }
        WorkerServiceKind::Dump(_) => None,
    }
}

/// What a backup of a standalone PostgreSQL on worker `node` writes, given
/// its agent's WAL-G check: the control plane's WAL-G repository when its
/// image has WAL-G, else a dump taken on the node.
fn worker_postgres_capability(
    service: &external_services::Model,
    node: i32,
    probe: ToolProbe,
) -> ExternalServiceBackupCapability {
    let dump =
        |verified: bool, reason: String, remediation: String| ExternalServiceBackupCapability {
            cloud_backup_compatible: false,
            verified,
            wal_g_installed: false,
            engine: crate::engines::workload_job::ENGINE_KEY.to_string(),
            artifact: "logical_pg_dumpall".to_string(),
            reason: Some(reason),
            remediation: Some(remediation),
            recommended_image: None,
        };
    match probe {
        ToolProbe::Available => compatible("postgres_walg", "walg_repository", true),
        ToolProbe::Missing => dump(
            true,
            format!(
                "Service {} on worker node {node} runs a PostgreSQL image without WAL-G, so its \
                 backups are logical dumps taken on the node; WAL-G repositories and \
                 point-in-time recovery need WAL-G in the image.",
                service.id
            ),
            format!(
                "Backups and in-place restores work as they are. For WAL-G and point-in-time \
                 recovery, switch the service to a WAL-G PostgreSQL image (for example {}) and \
                 run a new backup.",
                recommended_image("postgres").unwrap_or("a gotempsh/postgres-walg image")
            ),
        ),
        ToolProbe::Unverified(cause) => dump(
            false,
            match cause {
                UnverifiedCause::Unavailable => format!(
                    "Backup compatibility for service {} could not be verified because its \
                     container on worker node {node} is unavailable, not running, or its agent \
                     could not be reached.",
                    service.id
                ),
                UnverifiedCause::TimedOut { stage } => format!(
                    "Backup compatibility for service {} could not be verified because the \
                     {stage} timed out.",
                    service.id
                ),
            },
            format!(
                "Check that worker node {node} and its agent are online and the service is \
                 running, then check capability again."
            ),
        ),
    }
}

fn compatible(
    engine: &str,
    artifact: &str,
    wal_g_installed: bool,
) -> ExternalServiceBackupCapability {
    ExternalServiceBackupCapability {
        cloud_backup_compatible: true,
        verified: true,
        wal_g_installed,
        engine: engine.to_string(),
        artifact: artifact.to_string(),
        reason: None,
        remediation: None,
        recommended_image: None,
    }
}

fn local_only(
    _service: &external_services::Model,
    engine: &str,
    artifact: &str,
    reason: &str,
    remediation: &str,
    recommended_image: &str,
) -> ExternalServiceBackupCapability {
    ExternalServiceBackupCapability {
        cloud_backup_compatible: false,
        verified: true,
        wal_g_installed: false,
        engine: engine.to_string(),
        artifact: artifact.to_string(),
        reason: Some(reason.to_string()),
        remediation: Some(remediation.to_string()),
        recommended_image: Some(recommended_image.to_string()),
    }
}

fn unverified(
    service: &external_services::Model,
    fallback_engine: &str,
    fallback_artifact: &str,
    cause: UnverifiedCause,
) -> ExternalServiceBackupCapability {
    let reason = match cause {
        UnverifiedCause::Unavailable => format!(
            "Backup compatibility for external service {} could not be verified because its Docker container is unavailable or not running.",
            service.id
        ),
        UnverifiedCause::TimedOut { stage } => format!(
            "Backup compatibility for external service {} could not be verified because the {stage} timed out.",
            service.id
        ),
    };
    ExternalServiceBackupCapability {
        cloud_backup_compatible: false,
        verified: false,
        wal_g_installed: false,
        engine: fallback_engine.to_string(),
        artifact: fallback_artifact.to_string(),
        reason: Some(reason),
        remediation: Some(
            "Start Docker and the external service container, then check capability again. Local backup configuration remains discoverable."
                .to_string(),
        ),
        recommended_image: recommended_image(&service.service_type).map(str::to_string),
    }
}

fn recommended_image(service_type: &str) -> Option<&'static str> {
    match service_type.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "timescale" | "timescaledb" => {
            Some("gotempsh/postgres-walg:18-bookworm")
        }
        "mariadb" => Some("ghcr.io/gotempsh/mariadb-walg:11.4"),
        "mongodb" | "mongo" => Some("gotempsh/mongodb-walg:8.0"),
        "redis" => Some("ghcr.io/gotempsh/redis-walg:8-trixie"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    /// Only a worker's storage service is described without a probe, its
    /// mirror an object set. Every database on a worker takes the control
    /// plane's engines (ADR-099) and is probed through its agent like a
    /// local one, so none is described here.
    #[test]
    fn only_worker_storage_is_described_without_a_probe() {
        for service_type in ["postgres", "mongodb", "redis", "mariadb"] {
            let mut worker = service(service_type, "standalone");
            worker.node_id = Some(3);
            assert!(worker_capability(&worker).is_none(), "{service_type}");
        }

        let mut storage = service("rustfs", "standalone");
        storage.node_id = Some(3);
        let capability = worker_capability(&storage).expect("a worker storage service");
        assert!(capability.cloud_backup_compatible);
        assert_eq!(capability.artifact, "object_set");

        // A service on this host is the local engines' to describe.
        assert!(worker_capability(&service("rustfs", "standalone")).is_none());
    }

    /// Parity (ADR-099): a worker Redis is probed through its agent and
    /// reports exactly what a local Redis on the same image reports, for
    /// every outcome of the probe.
    #[test]
    fn a_worker_redis_reports_what_a_local_redis_reports() {
        let local = service("redis", "standalone");
        let mut worker = local.clone();
        worker.node_id = Some(3);
        assert!(
            worker_capability(&worker).is_none(),
            "probed, not described"
        );
        for probe in [
            ToolProbe::Available,
            ToolProbe::Missing,
            ToolProbe::Unverified(UnverifiedCause::Unavailable),
            ToolProbe::Unverified(UnverifiedCause::TimedOut {
                stage: "backup tool check",
            }),
        ] {
            let on_worker = classify_capability(&worker, probe);
            let here = classify_capability(&local, probe);
            assert_eq!(on_worker, here);
        }
        assert_eq!(
            classify_capability(&worker, ToolProbe::Available).engine,
            "redis"
        );
    }

    /// Parity (ADR-099): a worker PostgreSQL whose agent finds WAL-G reports
    /// exactly what a local one does, so the console's "not available"
    /// notice goes away; without WAL-G it says what to change on the worker,
    /// never to move the service.
    #[test]
    fn a_worker_postgres_reports_the_local_engine_when_its_agent_finds_walg() {
        let mut worker = service("postgres", "standalone");
        worker.node_id = Some(3);
        let local = service("postgres", "standalone");
        assert_eq!(
            worker_postgres_capability(&worker, 3, ToolProbe::Available),
            classify_capability(&local, ToolProbe::Available)
        );

        let missing = worker_postgres_capability(&worker, 3, ToolProbe::Missing);
        assert!(!missing.cloud_backup_compatible);
        assert!(missing.verified);
        assert_eq!(missing.engine, "postgres_workload_job");
        let remediation = missing.remediation.unwrap_or_default();
        assert!(
            remediation.contains("WAL-G PostgreSQL image"),
            "{remediation}"
        );
        assert!(!remediation.contains("control plane"), "{remediation}");
        assert_eq!(missing.recommended_image, None);

        let unverified = worker_postgres_capability(
            &worker,
            3,
            ToolProbe::Unverified(UnverifiedCause::Unavailable),
        );
        assert!(!unverified.verified);
        assert!(unverified
            .reason
            .unwrap_or_default()
            .contains("worker node 3"));
        assert!(!unverified
            .remediation
            .unwrap_or_default()
            .contains("control plane"));
    }

    /// Parity (ADR-099): a worker MongoDB is not described as a node dump;
    /// it is probed in its container like a local one (the same `which
    /// wal-g`, asked of its agent) and classified by the same function, so
    /// it reports exactly what a local MongoDB on the same image reports.
    #[test]
    fn a_worker_mongodb_reports_exactly_what_a_local_one_does() {
        for service_type in ["mongodb", "mongo"] {
            let local = service(service_type, "standalone");
            let mut worker = local.clone();
            worker.node_id = Some(3);
            assert!(worker_capability(&worker).is_none(), "{service_type}");
            assert!(requires_live_tool_probe(service_type, "standalone"));
            assert_eq!(tool_probe_command(service_type), vec!["which", "wal-g"]);
            for probe in [
                ToolProbe::Available,
                ToolProbe::Missing,
                ToolProbe::Unverified(UnverifiedCause::Unavailable),
            ] {
                assert_eq!(
                    classify_capability(&worker, probe),
                    classify_capability(&local, probe),
                    "{service_type} {probe:?}"
                );
            }
            let walg = classify_capability(&worker, ToolProbe::Available);
            assert_eq!(walg.engine, "mongodb");
            assert_eq!(walg.artifact, "mongodb_walg_stream");
            assert!(walg.cloud_backup_compatible && walg.wal_g_installed);
        }
    }

    #[test]
    fn a_worker_probe_reads_the_tool_check_exit_code() {
        assert_eq!(tool_probe_from_exit_code(0), ToolProbe::Available);
        assert_eq!(tool_probe_from_exit_code(1), ToolProbe::Missing);
        assert_eq!(tool_probe_from_exit_code(127), ToolProbe::Missing);
    }

    #[test]
    fn a_worker_probe_runs_the_same_tool_checks_as_a_local_one() {
        assert_eq!(tool_probe_command("postgres"), vec!["which", "wal-g"]);
        assert_eq!(tool_probe_command("redis"), vec!["which", "wal-g"]);
        assert_eq!(
            tool_probe_command("mariadb"),
            vec!["sh", "-c", MARIADB_PITR_TOOLS_PROBE]
        );
    }

    use super::*;

    fn service(service_type: &str, topology: &str) -> external_services::Model {
        external_services::Model {
            id: 42,
            name: "test-service".to_string(),
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
            created_by_user_id: None,
            container_name: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
            healthcheck_interval_secs: None,
            healthcheck_start_interval_secs: None,
            healthcheck_start_period_secs: None,
            healthcheck_timeout_secs: None,
            healthcheck_retries: None,
        }
    }

    #[test]
    fn postgres_and_timescale_require_walg_except_clusters() {
        for service_type in ["postgres", "postgresql", "timescale", "timescaledb"] {
            let standalone = service(service_type, "standalone");
            let compatible = classify_capability(&standalone, ToolProbe::Available);
            assert!(compatible.cloud_backup_compatible);
            assert_eq!(compatible.engine, "postgres_walg");
            assert_eq!(compatible.artifact, "walg_repository");

            let local = classify_capability(&standalone, ToolProbe::Missing);
            assert!(!local.cloud_backup_compatible);
            assert_eq!(local.engine, "postgres_pgdump");

            let cluster =
                classify_capability(&service(service_type, "cluster"), ToolProbe::Missing);
            assert!(cluster.cloud_backup_compatible);
            assert_eq!(cluster.engine, "postgres_cluster");
        }
    }

    #[test]
    fn mariadb_distinguishes_physical_pitr_from_logical_dump() {
        let service = service("mariadb", "standalone");
        let physical = classify_capability(&service, ToolProbe::Available);
        assert!(physical.cloud_backup_compatible);
        assert_eq!(physical.engine, "mariadb_physical");
        assert_eq!(physical.artifact, "mariadb_physical_walg_repository");

        let logical = classify_capability(&service, ToolProbe::Missing);
        assert!(!logical.cloud_backup_compatible);
        assert_eq!(logical.engine, "mariadb_dump");
        assert!(logical.remediation.is_some());
    }

    #[test]
    fn mongodb_and_redis_require_walg_stream_artifacts() {
        for (service_type, stream, fallback) in [
            ("mongodb", "mongodb_walg_stream", "mongodump_archive"),
            ("mongo", "mongodb_walg_stream", "mongodump_archive"),
            ("redis", "redis_walg_stream", "redis_rdb"),
        ] {
            let service = service(service_type, "standalone");
            let compatible = classify_capability(&service, ToolProbe::Available);
            assert!(compatible.cloud_backup_compatible);
            assert_eq!(compatible.artifact, stream);

            let local = classify_capability(&service, ToolProbe::Missing);
            assert!(!local.cloud_backup_compatible);
            assert_eq!(local.artifact, fallback);
            assert!(local.remediation.is_some());
        }
    }

    #[test]
    fn every_object_source_is_native_mirror_compatible() {
        for service_type in ["rustfs", "s3", "minio", "blob"] {
            let capability =
                classify_capability(&service(service_type, "standalone"), ToolProbe::Available);
            assert!(capability.cloud_backup_compatible);
            assert!(capability.verified);
            assert!(!capability.wal_g_installed);
            assert_eq!(capability.engine, "s3_mirror");
            assert_eq!(capability.artifact, "object_set");
        }
    }

    #[test]
    fn unsupported_source_has_actionable_remediation() {
        let capability = classify_capability(
            &service("elasticsearch", "standalone"),
            ToolProbe::Available,
        );
        assert!(!capability.cloud_backup_compatible);
        assert!(capability.verified);
        assert_eq!(capability.engine, "unsupported");
        assert!(capability
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("elasticsearch")));
        assert!(capability
            .remediation
            .as_deref()
            .is_some_and(|remediation| remediation.contains("supported source type")));
    }

    #[test]
    fn unavailable_docker_is_explicitly_unverified() {
        for service_type in ["postgres", "mariadb", "mongodb", "redis"] {
            let capability = classify_capability(
                &service(service_type, "standalone"),
                ToolProbe::Unverified(UnverifiedCause::Unavailable),
            );
            assert!(!capability.cloud_backup_compatible);
            assert!(!capability.verified);
            assert!(capability.reason.as_deref().is_some_and(|reason| {
                reason.contains("Docker container is unavailable or not running")
            }));
            assert!(capability.remediation.is_some());
        }
    }

    #[test]
    fn timed_out_probe_is_unverified_actionable_and_sanitized() {
        let capability = classify_capability(
            &service("postgres", "standalone"),
            ToolProbe::Unverified(UnverifiedCause::TimedOut {
                stage: "container inspection",
            }),
        );

        assert!(!capability.verified);
        let reason = capability.reason.expect("timeout reason");
        assert!(reason.contains("container inspection timed out"));
        assert!(reason.contains("external service 42"));
        assert!(!reason.contains("socket"));
        assert!(!reason.contains("daemon"));
    }
}
