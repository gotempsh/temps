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
//! | `"postgres"`   | other        | yes              | `"postgres_walg"`   |
//! | `"postgres"`   | other        | no               | `"postgres_pgdump"` |
//! | `"redis"`      | any          | –                | `"redis"`           |
//! | `"mongodb"`    | any          | –                | `"mongodb"`         |
//! | `"s3"` / `"minio"` / `"blob"` | any | –       | `"s3_mirror"`       |
//! | anything else  | –            | –                | `Err(Unsupported)`  |

use thiserror::Error;

/// Error returned by [`resolve_engine_key`] when no engine can be selected.
#[derive(Error, Debug)]
pub enum ResolveEngineError {
    /// The service's `service_type` is not supported by any registered engine.
    #[error(
        "Service type '{service_type}' (service_id={service_id}) is not supported by any backup engine. \
         Supported types: postgres, redis, mongodb, s3, minio, blob"
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
    match service.service_type.as_str() {
        "postgres" => {
            if service.topology.as_str() == "cluster" {
                return Ok("postgres_cluster");
            }
            // Probe for WAL-G in the running container. Container naming
            // must match the legacy provider's `get_container_name()` —
            // `postgres-{name}` for standalone Postgres
            // (see temps-providers/src/externalsvc/postgres.rs:269-271).
            // Using a different prefix here makes the probe miss every
            // container and silently fall back to pg_dump.
            let container_name = format!("postgres-{}", service.name);
            if container_has_walg(docker, &container_name).await {
                Ok("postgres_walg")
            } else {
                Ok("postgres_pgdump")
            }
        }
        "redis" => Ok("redis"),
        "mongodb" => Ok("mongodb"),
        "s3" | "minio" | "blob" => Ok("s3_mirror"),
        other => Err(ResolveEngineError::Unsupported {
            service_id: service.id,
            service_type: other.to_string(),
        }),
    }
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
async fn container_has_walg(docker: &bollard::Docker, container_name: &str) -> bool {
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
}
