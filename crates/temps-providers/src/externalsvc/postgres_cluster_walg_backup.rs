// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of a WAL-G base backup of a PostgreSQL HA cluster
//! (pg_auto_failover): everything the backup does to the primary's
//! container. The control plane runs it with its own Docker client for a
//! member it hosts; the agent of the worker that runs the primary runs the
//! same function with that node's client (the cluster's members are placed
//! on any node, and only that node's Docker can reach the container).
//!
//! 1. Refuse to start while a `wal-g backup-push` still runs in the
//!    container.
//! 2. `wal-g backup-push` of the member's data directory, against the
//!    member's own port.
//! 3. Read `pg_current_wal_lsn()`.
//!
//! Unlike a standalone service, the member is never recreated or restarted
//! and its archiving is never changed: pg_auto_failover owns it.

use std::sync::Arc;

use bollard::Docker;
use temps_service_protocol::{PostgresClusterWalgBackupRequest, PostgresWalgBackupResult};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::postgres_walg_backup::{
    backup_push_env_on_port, current_wal_lsn, node_s3, refuse_overlapping_backup_push,
    run_walg_exec, walg_prefix, PostgresLogin, WalgBaseBackupError, WalgBaseBackupOutcome,
};
use super::S3Credentials;

/// Where every cluster data member keeps its data. The member containers
/// run pg_autoctl with this path, which is NOT the image's `PGDATA` env
/// (`/var/lib/postgresql/<major>/docker`), so the image's cannot be trusted.
pub const CLUSTER_PGDATA: &str = "/var/lib/postgresql/pgdata";

/// One WAL-G base backup of an HA cluster, from one of its data members.
/// No `Debug`: it holds the login's password and the S3 secret key.
#[derive(Clone)]
pub struct ClusterWalgBaseBackup {
    pub service_id: i32,
    /// The cluster service's name: the WAL-G prefix follows from it.
    pub service: String,
    /// The member's container.
    pub container_name: String,
    /// The port PostgreSQL listens on inside the member's container.
    pub pg_port: i32,
    pub login: PostgresLogin,
    pub s3: S3Credentials,
    pub backup_uuid: String,
}

/// The environment of a cluster member's `wal-g backup-push`: the
/// standalone one, against the member's port and data directory.
pub fn cluster_backup_push_env(
    prefix: &str,
    s3: &S3Credentials,
    login: &PostgresLogin,
    pg_port: i32,
    backup_uuid: &str,
    container_endpoint: Option<String>,
) -> Vec<String> {
    let mut env =
        backup_push_env_on_port(prefix, s3, login, pg_port, backup_uuid, container_endpoint);
    env.push(format!("PGDATA={CLUSTER_PGDATA}"));
    env
}

/// Take a WAL-G base backup of a cluster from the member `request` names, in
/// its container on `docker` (see the module documentation).
pub async fn cluster_walg_base_backup(
    docker: &Docker,
    request: ClusterWalgBaseBackup,
    cancel: &CancellationToken,
) -> Result<WalgBaseBackupOutcome, WalgBaseBackupError> {
    let ClusterWalgBaseBackup {
        service_id,
        service,
        container_name,
        pg_port,
        login,
        s3,
        backup_uuid,
    } = request;
    refuse_overlapping_backup_push(
        docker,
        service_id,
        &container_name,
        "PostgreSQL cluster",
        cancel,
    )
    .await?;

    let prefix = walg_prefix(&s3.bucket_name, &s3.bucket_path, &service);
    let endpoint = s3
        .resolve_endpoint_for_container(docker, &container_name)
        .await;
    let env = cluster_backup_push_env(&prefix, &s3, &login, pg_port, &backup_uuid, endpoint);
    info!(
        service_id,
        service = %service,
        container = %container_name,
        pg_port,
        prefix = %prefix,
        "Running wal-g backup-push on the cluster's primary"
    );
    let output = run_walg_exec(
        docker,
        &container_name,
        "wal-g backup-push $PGDATA",
        &env,
        cancel,
    )
    .await?;
    if output.exit_code != 0 {
        let or_empty = |text: &str| match text.trim() {
            "" => "<empty>".to_string(),
            text => text.to_string(),
        };
        return Err(WalgBaseBackupError::BackupPush {
            service_id,
            container: container_name,
            exit_code: output.exit_code,
            stderr: or_empty(&output.stderr),
            stdout: or_empty(&output.stdout),
        });
    }
    if !output.stderr.trim().is_empty() {
        info!(
            service_id,
            container = %container_name,
            "wal-g (cluster) stderr (warnings): {}",
            output.stderr.trim()
        );
    }
    let lsn = match current_wal_lsn(docker, &container_name, &login, Some(pg_port)).await {
        Ok(lsn) => Some(lsn),
        Err(error) => {
            warn!(service_id, %error, "Could not read the WAL LSN after a cluster WAL-G base backup");
            None
        }
    };
    Ok(WalgBaseBackupOutcome {
        restarted: false,
        stdout: output.stdout,
        stderr: output.stderr,
        lsn,
    })
}

/// The member a node is asked to back up, or why it refuses: a request names
/// a member by ordinal, never a container, and a port PostgreSQL can listen
/// on.
fn node_cluster_target(request: &PostgresClusterWalgBackupRequest) -> Result<String, String> {
    let PostgresClusterWalgBackupRequest {
        service_id,
        service,
        ordinal,
        pg_port,
        ..
    } = request;
    if service.trim().is_empty() {
        return Err(format!(
            "the cluster backup request for service {service_id} names no service"
        ));
    }
    if *ordinal < 1 {
        return Err(format!(
            "cluster service {service_id} ('{service}') has no data member with ordinal \
             {ordinal}; data members are numbered from 1"
        ));
    }
    if !(1..=65535).contains(pg_port) {
        return Err(format!(
            "port {pg_port} of member {ordinal} of cluster service {service_id} ('{service}') \
             is not a TCP port"
        ));
    }
    Ok(super::naming::postgres_cluster_member_container_name(
        service, *ordinal,
    ))
}

/// The agent's side of `POST /agent/services/postgres/cluster-walg-backup`:
/// the request as [`cluster_walg_base_backup`] takes it, run against this
/// node's Docker.
pub async fn cluster_walg_base_backup_on_node(
    request: PostgresClusterWalgBackupRequest,
    docker: Arc<Docker>,
) -> PostgresWalgBackupResult {
    let container_name = match node_cluster_target(&request) {
        Ok(container_name) => container_name,
        Err(reason) => return PostgresWalgBackupResult::Refused { reason },
    };
    let PostgresClusterWalgBackupRequest {
        service_id,
        service,
        ordinal: _,
        pg_port,
        username,
        password,
        database,
        destination,
        bucket_path,
        backup_uuid,
    } = request;
    // Dropping the request (the control plane gave up) drops this future,
    // and the exec with it; the token only serves the shared signature.
    let cancel = CancellationToken::new();
    match cluster_walg_base_backup(
        &docker,
        ClusterWalgBaseBackup {
            service_id,
            service,
            container_name,
            pg_port,
            login: PostgresLogin {
                username,
                password,
                database,
            },
            s3: node_s3(destination, bucket_path),
            backup_uuid,
        },
        &cancel,
    )
    .await
    {
        Ok(outcome) => PostgresWalgBackupResult::Completed {
            restarted: outcome.restarted,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            lsn: outcome.lsn,
        },
        Err(error) if error.is_refusal() => PostgresWalgBackupResult::Refused {
            reason: error.to_string(),
        },
        Err(error) => PostgresWalgBackupResult::Failed {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_service_protocol::WorkerS3Destination;

    fn request() -> PostgresClusterWalgBackupRequest {
        PostgresClusterWalgBackupRequest {
            service_id: 4,
            service: "orders-ha".to_string(),
            ordinal: 2,
            pg_port: 6012,
            username: "app".to_string(),
            password: "p'w".to_string(),
            database: "appdb".to_string(),
            destination: WorkerS3Destination {
                bucket: "backups".to_string(),
                region: "us-east-1".to_string(),
                endpoint: Some("s3.example.test".to_string()),
                force_path_style: true,
                access_key_id: "AKID".to_string(),
                secret_access_key: "SECRET".to_string(),
                session_token: None,
            },
            bucket_path: "base".to_string(),
            backup_uuid: "uuid-1".to_string(),
        }
    }

    /// The node backs up the member the request names by ordinal, in the
    /// container the cluster engine created for it.
    #[test]
    fn the_node_targets_the_members_own_container() {
        assert_eq!(
            node_cluster_target(&request()).as_deref(),
            Ok("postgres-orders-ha-2")
        );
    }

    #[test]
    fn the_node_refuses_a_request_that_names_no_member() {
        let mut no_service = request();
        no_service.service = " ".to_string();
        let mut monitor = request();
        monitor.ordinal = 0;
        let mut port = request();
        port.pg_port = 70000;
        for (request, expected) in [
            (no_service, "names no service"),
            (monitor, "no data member with ordinal 0"),
            (port, "port 70000"),
        ] {
            let Err(reason) = node_cluster_target(&request) else {
                panic!("{request:?} must be refused");
            };
            assert!(reason.contains(expected), "{reason}");
        }
    }

    /// A refusal is answered before the node's Docker is touched.
    #[tokio::test]
    async fn a_refused_request_never_reaches_docker() {
        let docker = Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("an HTTP Docker client is built without connecting"),
        );
        let mut monitor = request();
        monitor.ordinal = 0;
        let result = cluster_walg_base_backup_on_node(monitor, docker).await;
        assert!(
            matches!(result, PostgresWalgBackupResult::Refused { .. }),
            "{result:?}"
        );
    }

    /// The exact environment the cluster engine always ran `backup-push`
    /// with: the member's own port and pg_autoctl's data directory.
    #[test]
    fn backup_push_runs_against_the_members_port_and_data_directory() {
        let login = PostgresLogin {
            username: "app".to_string(),
            password: "p'w".to_string(),
            database: "appdb".to_string(),
        };
        let s3 = node_s3(request().destination, "base".to_string());
        let env = cluster_backup_push_env(
            "s3://backups/base/external_services/postgres/orders-ha/walg",
            &s3,
            &login,
            6012,
            "uuid-1",
            None,
        );
        assert!(env.contains(&"PGPORT=6012".to_string()), "{env:?}");
        assert!(!env.contains(&"PGPORT=5432".to_string()), "{env:?}");
        assert!(env.contains(&"PGDATA=/var/lib/postgresql/pgdata".to_string()));
        assert!(env.contains(&"PGUSER=app".to_string()));
        assert!(env.contains(&"PGPASSWORD=p'w".to_string()));
        assert!(env.contains(&"AWS_ACCESS_KEY_ID=AKID".to_string()));
        assert!(env.contains(&"AWS_S3_FORCE_PATH_STYLE=true".to_string()));
        assert!(env.contains(&"WALG_DELTA_MAX_STEPS=0".to_string()));
    }

    /// The request's password never reaches a log line.
    #[test]
    fn the_requests_debug_hides_the_password_and_secret_key() {
        let printed = format!("{:?}", request());
        assert!(!printed.contains("p'w"), "{printed}");
        assert!(!printed.contains("SECRET"), "{printed}");
        assert!(printed.contains("orders-ha"), "{printed}");
    }
}
