// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::engines::dispatch::{
    container_has_mariadb_pitr_tools, container_has_walg, service_container_name,
};
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
) -> ExternalServiceBackupCapability {
    let service_type = service.service_type.to_ascii_lowercase();
    if !requires_live_tool_probe(&service_type, &service.topology) {
        return classify_capability(service, ToolProbe::Available);
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
                "gotempsh/redis-walg:8-bookworm",
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
        "redis" => Some("gotempsh/redis-walg:8-bookworm"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
            ai_data_access: false,
            created_by_user_id: None,
            container_name: None,
            walg_archive_s3_source_id: None,
            walg_archive_pinned_at: None,
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
