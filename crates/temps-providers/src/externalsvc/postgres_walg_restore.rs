// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node half of a PostgreSQL WAL-G restore (ADR-099): the control
//! plane's own `restore_from_walg`, run by a worker's agent against the
//! container it hosts.
//!
//! The control plane decides everything that needs its database (which
//! backup, whose login the restored data answers to, the recovery target,
//! admission and confirmation) and sends it sealed. The node checks that the
//! request names its own managed PostgreSQL and a well-formed target, then
//! runs the restore: fetch into a staging directory while the server runs,
//! write the recovery settings, stop, swap keeping the original aside, start
//! the restored server and wait for it to promote, then delete the original.
//! When the restored server does not come up the original is put back.

use std::net::IpAddr;
use std::sync::Arc;

use bollard::Docker;
use temps_service_protocol::{
    PostgresWalgRestoreRequest, PostgresWalgRestoreResult, RecoveryTarget, TargetWriteBoundary,
};
use tracing::info;

use super::postgres::{NodeRecreate, PostgresService};
use super::{EngineError, S3Credentials, ServiceConfig, ServiceType};

/// Why a restore request cannot run on this node, before anything is
/// touched.
fn refusal(
    service_id: i32,
    service: &str,
    reason: impl std::fmt::Display,
) -> PostgresWalgRestoreResult {
    PostgresWalgRestoreResult::Refused {
        reason: format!(
            "PostgreSQL service {service_id} ('{service}') cannot be restored on this node: {reason}"
        ),
    }
}

/// A recovery target PostgreSQL reads as one setting: the values are
/// written into `postgresql.auto.conf`, so one that could end the line or
/// the quoted value is refused rather than altered.
pub fn validate_recovery_target(target: &RecoveryTarget) -> Result<(), String> {
    match target {
        RecoveryTarget::Time { .. } => Ok(()),
        RecoveryTarget::Xid { xid } => {
            if !xid.is_empty() && xid.len() <= 20 && xid.bytes().all(|b| b.is_ascii_digit()) {
                Ok(())
            } else {
                Err(format!("the transaction id '{xid}' is not a number"))
            }
        }
        RecoveryTarget::Lsn { lsn } => {
            let valid = lsn.split_once('/').is_some_and(|(high, low)| {
                [high, low].iter().all(|part| {
                    !part.is_empty()
                        && part.len() <= 8
                        && part.bytes().all(|b| b.is_ascii_hexdigit())
                })
            });
            if valid {
                Ok(())
            } else {
                Err(format!("'{lsn}' is not a WAL location like 0/16B3748"))
            }
        }
        RecoveryTarget::Name { name } => {
            if !name.is_empty()
                && name.len() <= 63
                && !name
                    .chars()
                    .any(|c| c.is_control() || c == '\'' || c == '\\')
            {
                Ok(())
            } else {
                Err(
                    "a restore point name is 1 to 63 characters without quotes, backslashes or \
                     control characters"
                        .to_string(),
                )
            }
        }
    }
}

/// A WAL-G repository in the source's bucket: `s3://<bucket>/…/walg`.
fn validate_walg_prefix(prefix: &str, bucket: &str) -> Result<(), String> {
    let path = prefix
        .strip_prefix("s3://")
        .and_then(|rest| rest.strip_prefix(bucket))
        .and_then(|rest| rest.strip_prefix('/'));
    let Some(path) = path else {
        return Err(format!(
            "the backup '{prefix}' is not in the S3 source's bucket '{bucket}'"
        ));
    };
    if !path.trim_end_matches('/').ends_with("walg")
        || path
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\'' || c == '"')
    {
        return Err(format!("'{prefix}' is not a WAL-G repository"));
    }
    Ok(())
}

/// A worker service records its container's name, which is the managed
/// name; only an imported database's differs. Kept, the provider would treat
/// this managed container as imported.
fn forget_managed_container_name(config: &mut ServiceConfig, container_name: &str) {
    if let Some(parameters) = config.parameters.as_object_mut() {
        if parameters.get("container_name").and_then(|v| v.as_str()) == Some(container_name) {
            parameters.remove("container_name");
        }
    }
}

/// The agent's side of `POST /agent/services/postgres/walg-restore`: the
/// request run against this node's Docker, recreating with `node`.
///
/// The stored host of a worker service is loopback, which is not where the
/// node publishes its ports; the readiness and settings checks connect
/// through `published_address`, like the health and WAL probes.
pub async fn walg_restore_on_node(
    request: PostgresWalgRestoreRequest,
    docker: Arc<Docker>,
    node: NodeRecreate,
    published_address: Option<IpAddr>,
    writes: Arc<TargetWriteBoundary>,
) -> PostgresWalgRestoreResult {
    let PostgresWalgRestoreRequest {
        service_id,
        node_id: _,
        mut service_config,
        mut live_service_config,
        source,
        walg_prefix,
        recovery_target,
        target_user_data,
    } = request;
    let service = live_service_config.name.clone();
    if live_service_config.service_type != ServiceType::Postgres
        || service_config.service_type != ServiceType::Postgres
    {
        return refusal(
            service_id,
            &service,
            format!(
                "WAL-G restores are for PostgreSQL services, and this one is {}",
                live_service_config.service_type
            ),
        );
    }
    if service_config.name != service {
        return refusal(
            service_id,
            &service,
            format!(
                "the restored login names another service ('{}')",
                service_config.name
            ),
        );
    }
    for config in [&mut service_config, &mut live_service_config] {
        if let Err(error) = super::resolve_managed_probe_target(config, published_address) {
            return refusal(service_id, &service, error);
        }
    }
    if let Err(reason) = validate_walg_prefix(&walg_prefix, &source.bucket) {
        return refusal(service_id, &service, reason);
    }
    if let Some(target) = &recovery_target {
        if let Err(reason) = validate_recovery_target(target) {
            return refusal(service_id, &service, reason);
        }
    }
    let container_name = super::naming::postgres_container_name(&service);
    forget_managed_container_name(&mut service_config, &container_name);
    forget_managed_container_name(&mut live_service_config, &container_name);
    let s3 = S3Credentials {
        access_key_id: source.access_key_id,
        secret_key: source.secret_access_key,
        session_token: source.session_token,
        region: source.region,
        endpoint: source.endpoint,
        bucket_name: source.bucket,
        bucket_path: String::new(),
        force_path_style: source.force_path_style,
    };
    info!(
        service_id,
        service = %service,
        pitr = recovery_target.is_some(),
        "Restoring a worker's PostgreSQL from WAL-G"
    );
    let postgres = PostgresService::new(service.clone(), docker).on_node(node);
    match postgres
        .restore_walg_in_place(
            &s3,
            &walg_prefix,
            service_config,
            &live_service_config,
            recovery_target.as_ref(),
            target_user_data.as_deref(),
            writes.as_ref(),
        )
        .await
    {
        Ok(()) => PostgresWalgRestoreResult::Completed,
        Err(error @ (EngineError::Conflict { .. } | EngineError::InvalidInput { .. })) => {
            PostgresWalgRestoreResult::Refused {
                reason: error.to_string(),
            }
        }
        Err(EngineError::RecreateGuard(guard)) if guard.is_refusal() => {
            PostgresWalgRestoreResult::Refused {
                reason: guard.to_string(),
            }
        }
        Err(error) => PostgresWalgRestoreResult::Failed {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_targets_postgresql_reads_as_one_setting_are_accepted() {
        let ok = [
            RecoveryTarget::Time {
                time: chrono::Utc::now(),
            },
            RecoveryTarget::Xid {
                xid: "7421".to_string(),
            },
            RecoveryTarget::Lsn {
                lsn: "0/16B3748".to_string(),
            },
            RecoveryTarget::Name {
                name: "before-migration".to_string(),
            },
        ];
        for target in ok {
            assert!(validate_recovery_target(&target).is_ok(), "{target:?}");
        }
        let refused = [
            RecoveryTarget::Xid {
                xid: "1' restore_command".to_string(),
            },
            RecoveryTarget::Lsn {
                lsn: "0/1\nrestore_command = 'x'".to_string(),
            },
            RecoveryTarget::Lsn {
                lsn: "16B3748".to_string(),
            },
            RecoveryTarget::Name {
                name: "x\nrestore_command = 'sh'".to_string(),
            },
            RecoveryTarget::Name {
                name: String::new(),
            },
        ];
        for target in refused {
            assert!(validate_recovery_target(&target).is_err(), "{target:?}");
        }
    }

    #[test]
    fn the_backup_must_be_a_walg_repository_in_the_sources_bucket() {
        assert!(validate_walg_prefix(
            "s3://backups/base/external_services/postgres/orders/walg",
            "backups"
        )
        .is_ok());
        for (prefix, bucket) in [
            (
                "s3://other/external_services/postgres/orders/walg",
                "backups",
            ),
            (
                "s3://backupsx/external_services/postgres/orders/walg",
                "backups",
            ),
            (
                "s3://backups/external_services/postgres/orders/dump.sql.gz",
                "backups",
            ),
            ("s3://backups/a b/walg", "backups"),
            ("/var/lib/walg", "backups"),
        ] {
            assert!(validate_walg_prefix(prefix, bucket).is_err(), "{prefix}");
        }
    }

    /// A request that does not name this node's managed PostgreSQL, or a
    /// target that could change the recovery settings, is refused before the
    /// node's Docker is touched.
    #[tokio::test]
    async fn the_node_refuses_what_it_must_not_restore() {
        let docker = Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("an HTTP Docker client is built without connecting"),
        );
        let node = NodeRecreate {
            service_id: 3,
            node_id: 2,
            attachment: Default::default(),
            after_create: Arc::new(|_| Box::pin(async {})),
        };
        let config = |name: &str, service_type, host: &str| ServiceConfig {
            name: name.to_string(),
            service_type,
            version: None,
            parameters: serde_json::json!({"host": host, "port": "5432"}),
        };
        let request = |restored: ServiceConfig, live: ServiceConfig| PostgresWalgRestoreRequest {
            service_id: 3,
            node_id: 2,
            service_config: restored,
            live_service_config: live,
            source: temps_service_protocol::WorkerS3Destination {
                bucket: "backups".to_string(),
                region: "us-east-1".to_string(),
                endpoint: None,
                force_path_style: true,
                access_key_id: "a".to_string(),
                secret_access_key: "s".to_string(),
                session_token: None,
            },
            walg_prefix: "s3://backups/external_services/postgres/orders/walg".to_string(),
            recovery_target: None,
            target_user_data: None,
        };
        let pg = |host: &str| config("orders", ServiceType::Postgres, host);
        let cases = [
            (
                request(
                    config("orders", ServiceType::Redis, "localhost"),
                    config("orders", ServiceType::Redis, "localhost"),
                ),
                "is redis",
            ),
            (request(pg("10.9.8.7"), pg("localhost")), "10.9.8.7"),
            (
                request(
                    config("billing", ServiceType::Postgres, "localhost"),
                    pg("localhost"),
                ),
                "another service",
            ),
            (
                PostgresWalgRestoreRequest {
                    walg_prefix: "s3://elsewhere/walg".to_string(),
                    ..request(pg("localhost"), pg("localhost"))
                },
                "not in the S3 source's bucket",
            ),
            (
                PostgresWalgRestoreRequest {
                    recovery_target: Some(RecoveryTarget::Name {
                        name: "a\nrestore_command = 'sh'".to_string(),
                    }),
                    ..request(pg("localhost"), pg("localhost"))
                },
                "restore point name",
            ),
        ];
        for (request, expected) in cases {
            let result = walg_restore_on_node(
                request,
                docker.clone(),
                node.clone(),
                None,
                Arc::new(TargetWriteBoundary::new("orders")),
            )
            .await;
            assert!(
                matches!(&result, PostgresWalgRestoreResult::Refused { reason } if reason.contains(expected)),
                "{expected}: {result:?}"
            );
        }
    }
}
