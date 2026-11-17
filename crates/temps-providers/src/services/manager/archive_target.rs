// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Test archive target": check, on demand, that WAL-G can write to the S3
//! source a PostgreSQL service archives its WAL to, with exactly the
//! settings `walg.env` holds.
//!
//! The same check runs before archiving is turned on or re-pointed (see
//! `PostgresService::write_wal_archiving_config`); this is the operator's
//! way to run it any time, for example after fixing an endpoint.

use crate::externalsvc::postgres::{ArchiveTargetCheck, ArchiveTargetCheckError};
use crate::services::*;
use chrono::{DateTime, Utc};

/// The outcome of an archive target check, success or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveTargetTestOutcome {
    pub ok: bool,
    pub s3_source_id: i32,
    pub s3_source_name: String,
    /// `AWS_ENDPOINT` as WAL-G receives it, or "AWS S3 (region …)".
    pub endpoint: String,
    /// `http`, `https`, or `aws`.
    pub scheme: String,
    pub force_path_style: bool,
    pub path_style_reason: String,
    /// What WAL-G said: a confirmation, or its error (credentials redacted).
    pub message: String,
    /// How the running container's `walg.env` differs from these
    /// settings, and how to fix it. PostgreSQL archives with that file, so
    /// a target that accepts these settings can still be failing.
    pub live_settings_drift: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// Why an archive target could not be checked at all.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveTargetTestError {
    #[error("External service {service_id} not found")]
    ServiceNotFound { service_id: i32 },
    #[error("External service {service_id} is a '{service_type}' service; only PostgreSQL archives WAL with WAL-G")]
    UnsupportedEngine {
        service_id: i32,
        service_type: String,
    },
    #[error("PostgreSQL service {service_id} is a cluster; its backups push base backups only, without continuous WAL archiving, so there is no archive target to test")]
    Cluster { service_id: i32 },
    #[error("PostgreSQL service {service_id} runs on this host, and this Temps runs with local workloads disabled, so it does not run commands in local containers; enable local workloads to test its archive target")]
    LocalWorkloadsDisabled { service_id: i32 },
    #[error("PostgreSQL service {service_id} has no WAL archive target yet; run a backup of it to turn continuous archiving on")]
    NotConfigured { service_id: i32 },
    #[error("WAL archive target of PostgreSQL service {service_id} is S3 source {s3_source_id}, which no longer exists")]
    SourceNotFound { service_id: i32, s3_source_id: i32 },
    #[error("PostgreSQL service {service_id} runs on worker node {node_id}; checking its archive target from the control plane is not supported yet")]
    WorkerNode { service_id: i32, node_id: i32 },
    #[error("Could not decrypt the {field} of S3 source {s3_source_id} for PostgreSQL service {service_id}: {reason}")]
    Decryption {
        service_id: i32,
        s3_source_id: i32,
        field: &'static str,
        reason: String,
    },
    #[error("Could not read PostgreSQL service {service_id} or its archive target: {source}")]
    Database {
        service_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Could not run the archive target check of PostgreSQL service {service_id}: {reason}")]
    Engine { service_id: i32, reason: String },
}

impl ExternalServiceManager {
    /// Run WAL-G's write check against the archive target of PostgreSQL
    /// service `service_id`. A target that refuses the write is a
    /// successful call with `ok == false`, naming the endpoint, scheme,
    /// path style and WAL-G's error.
    pub async fn test_wal_archive_target(
        &self,
        service_id: i32,
    ) -> Result<ArchiveTargetTestOutcome, ArchiveTargetTestError> {
        let service = external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ArchiveTargetTestError::Database { service_id, source })?
            .ok_or(ArchiveTargetTestError::ServiceNotFound { service_id })?;
        if !matches!(
            service.service_type.as_str(),
            "postgres" | "postgresql" | "timescale" | "timescaledb"
        ) {
            return Err(ArchiveTargetTestError::UnsupportedEngine {
                service_id,
                service_type: service.service_type.clone(),
            });
        }
        // A cluster's members are `postgres-{name}-{ordinal}`, never the
        // standalone container this check runs in, and its backups do not
        // set up continuous archiving at all.
        if service.topology == "cluster" {
            return Err(ArchiveTargetTestError::Cluster { service_id });
        }
        if let crate::services::cluster_topology::ServiceExecutionRoute::Remote(node_id) =
            crate::services::cluster_topology::service_execution_route(service.node_id)
        {
            return Err(ArchiveTargetTestError::WorkerNode {
                service_id,
                node_id,
            });
        }
        // The check runs in a local container: a profile that forbids local
        // workloads refuses it as policy, before Docker is ever asked for.
        if !self.local_workloads_enabled {
            return Err(ArchiveTargetTestError::LocalWorkloadsDisabled { service_id });
        }
        let s3_source_id = service
            .continuous_archive_s3_source_id
            .ok_or(ArchiveTargetTestError::NotConfigured { service_id })?;
        let source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ArchiveTargetTestError::Database { service_id, source })?
            .ok_or(ArchiveTargetTestError::SourceNotFound {
                service_id,
                s3_source_id,
            })?;

        let decrypt = |field: &'static str, value: &str| {
            self.encryption_service
                .decrypt_string(value)
                .map_err(|error| ArchiveTargetTestError::Decryption {
                    service_id,
                    s3_source_id,
                    field,
                    reason: error.to_string(),
                })
        };
        let s3_credentials = crate::S3Credentials {
            access_key_id: decrypt("access key", &source.access_key_id)?,
            secret_key: decrypt("secret key", &source.secret_key)?,
            session_token: source
                .session_token
                .as_deref()
                .filter(|token| !token.is_empty())
                .map(|token| decrypt("session token", token))
                .transpose()?,
            region: source.region.clone(),
            endpoint: source.endpoint.clone(),
            bucket_name: source.bucket_name.clone(),
            bucket_path: source.bucket_path.clone(),
            // The same default the archiving path applies.
            force_path_style: source.effective_path_style(true).force_path_style,
        };
        let walg_prefix = crate::externalsvc::postgres_walg_backup::walg_prefix(
            &source.bucket_name,
            &source.bucket_path,
            &service.name,
        );
        let config_json = service
            .config
            .as_deref()
            .map(|encrypted| self.encryption_service.decrypt_string(encrypted))
            .transpose()
            .map_err(|error| ArchiveTargetTestError::Decryption {
                service_id,
                s3_source_id,
                field: "service configuration",
                reason: error.to_string(),
            })?
            .unwrap_or_else(|| "{}".to_string());
        let service_config = crate::externalsvc::ServiceConfig {
            name: service.name.clone(),
            service_type: crate::externalsvc::ServiceType::Postgres,
            version: None,
            parameters: serde_json::from_str(&config_json).unwrap_or(serde_json::Value::Null),
        };
        let docker = self
            .require_docker()
            .map_err(|error| ArchiveTargetTestError::Engine {
                service_id,
                reason: error.to_string(),
            })?;
        let postgres = PostgresService::new(service.name.clone(), docker);
        let checked = postgres
            .check_archive_target(service_config.clone(), &s3_credentials, &walg_prefix)
            .await
            .map_err(|error| ArchiveTargetTestError::Engine {
                service_id,
                reason: error.to_string(),
            })?;
        let drift = postgres
            .live_walg_env_drift(service_config, &s3_credentials, &walg_prefix)
            .await;
        let mut result = outcome(s3_source_id, &source.name, checked, drift, Utc::now());
        explain_forced_path_style(&mut result, source.effective_path_style(true));
        Ok(result)
    }
}

/// The check only sees the boolean it hands WAL-G; the source knows whether
/// that was the operator's choice or forced by its endpoint, so a forced
/// value is reported with its reason.
fn explain_forced_path_style(
    result: &mut ArchiveTargetTestOutcome,
    source: temps_core::s3_endpoint::EffectivePathStyle,
) {
    if result.force_path_style
        && source.reason != temps_core::s3_endpoint::PathStyleReason::Configured
    {
        result.path_style_reason = source.reason.describe().to_string();
    }
}

fn outcome(
    s3_source_id: i32,
    s3_source_name: &str,
    checked: Result<ArchiveTargetCheck, Box<ArchiveTargetCheckError>>,
    live_settings_drift: Option<String>,
    checked_at: DateTime<Utc>,
) -> ArchiveTargetTestOutcome {
    match checked {
        Ok(check) => ArchiveTargetTestOutcome {
            ok: true,
            s3_source_id,
            s3_source_name: s3_source_name.to_string(),
            endpoint: check.endpoint,
            scheme: check.scheme,
            force_path_style: check.force_path_style,
            path_style_reason: check.path_style_reason,
            message: "WAL-G wrote a probe object to the archive target".to_string(),
            live_settings_drift,
            checked_at,
        },
        Err(error) => ArchiveTargetTestOutcome {
            ok: false,
            s3_source_id,
            s3_source_name: s3_source_name.to_string(),
            endpoint: error.endpoint,
            scheme: error.scheme,
            force_path_style: error.force_path_style,
            path_style_reason: error.path_style_reason,
            message: error.message,
            live_settings_drift,
            checked_at,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_check_is_reported_with_its_settings_and_cause() {
        let at = Utc::now();
        let result = outcome(
            7,
            "archive",
            Err(Box::new(ArchiveTargetCheckError {
                service_name: "orders".to_string(),
                endpoint: "http://10.0.0.5:9000".to_string(),
                scheme: "http".to_string(),
                force_path_style: true,
                path_style_reason: "forced: the endpoint is an IP address".to_string(),
                message: "connection refused".to_string(),
            })),
            None,
            at,
        );
        assert!(!result.ok);
        assert_eq!(result.s3_source_id, 7);
        assert_eq!(result.endpoint, "http://10.0.0.5:9000");
        assert_eq!(result.message, "connection refused");
        assert_eq!(result.checked_at, at);
    }

    #[test]
    fn an_accepted_check_is_ok() {
        let result = outcome(
            7,
            "archive",
            Ok(ArchiveTargetCheck {
                endpoint: "https://s3.example.com".to_string(),
                scheme: "https".to_string(),
                force_path_style: false,
                path_style_reason: "as configured on the S3 source".to_string(),
            }),
            Some("The running container's walg.env sends WAL to 10.0.0.5:9000".to_string()),
            Utc::now(),
        );
        assert!(result.ok);
        assert!(result
            .live_settings_drift
            .as_deref()
            .is_some_and(|drift| drift.contains("walg.env")));
        assert_eq!(result.scheme, "https");
    }

    #[test]
    fn path_style_forced_by_the_endpoint_says_so() {
        use temps_core::s3_endpoint::{EffectivePathStyle, PathStyleReason};
        let checked = || ArchiveTargetCheck {
            endpoint: "http://10.0.0.5:9000".to_string(),
            scheme: "http".to_string(),
            force_path_style: true,
            path_style_reason: PathStyleReason::Configured.describe().to_string(),
        };
        let mut result = outcome(7, "archive", Ok(checked()), None, Utc::now());
        explain_forced_path_style(
            &mut result,
            EffectivePathStyle {
                force_path_style: true,
                reason: PathStyleReason::IpAddressEndpoint,
            },
        );
        assert_eq!(
            result.path_style_reason,
            "forced: the endpoint is an IP address"
        );

        // The operator's own choice stays "as configured".
        let mut result = outcome(7, "archive", Ok(checked()), None, Utc::now());
        explain_forced_path_style(
            &mut result,
            EffectivePathStyle {
                force_path_style: true,
                reason: PathStyleReason::Configured,
            },
        );
        assert_eq!(result.path_style_reason, "as configured on the S3 source");
    }

    #[test]
    fn errors_name_the_service_and_what_to_do() {
        assert!(ArchiveTargetTestError::NotConfigured { service_id: 3 }
            .to_string()
            .contains("run a backup"));
        assert!(ArchiveTargetTestError::SourceNotFound {
            service_id: 3,
            s3_source_id: 9
        }
        .to_string()
        .contains("S3 source 9"));
    }
}
