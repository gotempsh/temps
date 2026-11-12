// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A managed service's Docker healthcheck overrides: what its container
//! would be created with and where each value comes from, saving an
//! override, and recreating the container to apply it now.
//!
//! Docker cannot change a running container's healthcheck. Saving an
//! override changes nothing running: it reaches the container when Temps
//! next creates or recreates it (an upgrade, a restore, a recreate for a
//! create-time setting), or when the operator asks for it with
//! [`ExternalServiceManager::apply_service_healthcheck`].

use std::time::Duration;

use bollard::models::HealthConfig;
use bollard::query_parameters::InspectContainerOptions;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, Set,
};
use serde::Serialize;
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
use temps_entities::external_services;
use temps_providers_core::externalsvc::recreate_guard::ManagedEngine;
use temps_providers_core::healthcheck::{
    self, registry, HealthcheckOverrides, HealthcheckTimings, ResolvedHealthcheck,
    StartIntervalSupport,
};
use tracing::{info, warn};
use utoipa::ToSchema;

use super::super::engine_selection::{EngineKind, EngineSelection};
use super::runtime_limits::ContainerRecreate;
use crate::externalsvc::ServiceType;
use crate::services::*;

/// Bound on reading the running container's healthcheck for a status read.
const INSPECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether Temps can give this service's container a healthcheck, and
/// whether it can recreate it from here to apply one now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HealthcheckCapability {
    /// The container has a Docker healthcheck Temps builds, so overrides
    /// apply to it.
    pub configurable: bool,
    /// Why not, when `configurable` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Temps can recreate the container from here to apply the settings.
    pub can_apply_now: bool,
    /// Why not, when `can_apply_now` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_now_reason: Option<String>,
}

impl HealthcheckCapability {
    fn unsupported(reason: String) -> Self {
        Self {
            configurable: false,
            reason: Some(reason.clone()),
            can_apply_now: false,
            apply_now_reason: Some(reason),
        }
    }
}

/// A service's healthcheck as the console shows it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ServiceHealthcheck {
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    /// The engine whose built-in timings apply. Absent for a service whose
    /// container has no healthcheck Temps builds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<ManagedEngine>,
    pub capability: HealthcheckCapability,
    /// This service's own overrides (null = inherit).
    pub overrides: HealthcheckOverrides,
    /// What a new or recreated container gets, and where each value comes
    /// from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<ResolvedHealthcheck>,
    /// What the container would get without this service's overrides: what
    /// a cleared field falls back to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited: Option<ResolvedHealthcheck>,
    /// The running container's healthcheck, when it could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<HealthcheckTimings>,
    /// The running container differs from `effective` (as this host's
    /// Docker would store it) until it is recreated.
    pub pending_recreate: bool,
    /// `false`: this host's Docker ignores the start interval, so the
    /// steady-state interval stays at the start interval. Absent when not
    /// known yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_interval_supported: Option<bool>,
}

/// What [`ExternalServiceManager::set_service_healthcheck`] changed, read
/// under the row lock that carried the write.
#[derive(Debug, Clone)]
pub struct ServiceHealthcheckUpdate {
    pub service_id: i32,
    pub service_name: String,
    pub previous: HealthcheckOverrides,
    pub current: HealthcheckOverrides,
}

/// The container was recreated with these timings.
#[derive(Debug, Clone)]
pub struct HealthcheckApplied {
    pub service_id: i32,
    pub service_name: String,
    pub timings: HealthcheckTimings,
}

/// A column value as the override it stores. A value written by this module
/// is always within bounds; anything else (negative, edited by hand) is
/// treated as unset rather than reaching Docker.
fn column(value: Option<i32>) -> Option<u32> {
    value.and_then(|v| u32::try_from(v).ok())
}

fn to_column(value: Option<u32>) -> Option<i32> {
    value.and_then(|v| i32::try_from(v).ok())
}

/// The overrides stored on a service row.
pub fn service_overrides(service: &external_services::Model) -> HealthcheckOverrides {
    HealthcheckOverrides {
        interval_secs: column(service.healthcheck_interval_secs),
        start_interval_secs: column(service.healthcheck_start_interval_secs),
        start_period_secs: column(service.healthcheck_start_period_secs),
        timeout_secs: column(service.healthcheck_timeout_secs),
        retries: column(service.healthcheck_retries),
    }
}

fn managed_engine(kind: EngineKind) -> Option<ManagedEngine> {
    match kind {
        EngineKind::Postgres => Some(ManagedEngine::Postgres),
        EngineKind::Redis => Some(ManagedEngine::Redis),
        EngineKind::Mongodb => Some(ManagedEngine::MongoDb),
        EngineKind::Mariadb => Some(ManagedEngine::MariaDb),
        EngineKind::Rustfs => Some(ManagedEngine::Rustfs),
        EngineKind::Minio => Some(ManagedEngine::Minio),
        // Checked through its S3 API by the readiness gate instead.
        EngineKind::Garage => None,
    }
}

/// The healthcheck as Docker stored it, in whole seconds.
fn timings_of(health: &HealthConfig) -> Option<HealthcheckTimings> {
    let secs = |nanos: Option<i64>| -> Option<u32> { u32::try_from(nanos? / 1_000_000_000).ok() };
    Some(HealthcheckTimings {
        interval_secs: secs(health.interval)?,
        start_interval_secs: secs(health.start_interval).unwrap_or(0),
        start_period_secs: secs(health.start_period).unwrap_or(0),
        timeout_secs: secs(health.timeout)?,
        retries: u32::try_from(health.retries?).ok()?,
    })
}

/// Where a service's container is, as far as its healthcheck is concerned.
struct HealthcheckTarget {
    engine: Option<ManagedEngine>,
    /// The control-plane container the engine creates, when there is one.
    container_name: Option<String>,
    capability: HealthcheckCapability,
}

impl ExternalServiceManager {
    /// Classify `service` (pure apart from decrypting its parameters).
    fn healthcheck_target(
        &self,
        service: &external_services::Model,
    ) -> Result<HealthcheckTarget, ExternalServiceError> {
        let unsupported = |reason: String| HealthcheckTarget {
            engine: None,
            container_name: None,
            capability: HealthcheckCapability::unsupported(reason),
        };
        if service.topology != "standalone" {
            return Ok(unsupported(format!(
                "its {} members run without a Docker healthcheck; the cluster's monitor and \
                 Temps' health checks watch them",
                service.topology
            )));
        }
        if let Some(container) = &service.container_name {
            return Ok(unsupported(format!(
                "it runs the imported container '{container}', which Temps never recreates"
            )));
        }
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.parameters_of(service)?;
        let selection = EngineSelection::for_parameter_map(
            service.id,
            &service.name,
            service_type,
            &parameters,
        )?;
        let Some(engine) = managed_engine(selection.kind) else {
            return Ok(unsupported(
                "its engine's container has no Docker healthcheck; Temps checks it through its \
                 S3 API"
                    .to_string(),
            ));
        };
        match service.node_id {
            None => Ok(HealthcheckTarget {
                engine: Some(engine),
                container_name: Some(selection.container_name()),
                capability: HealthcheckCapability {
                    configurable: true,
                    reason: None,
                    can_apply_now: true,
                    apply_now_reason: None,
                },
            }),
            // A worker's agent gives a database a healthcheck only when it
            // creates it from its workload spec.
            Some(node_id) if service.workload_mode == ServiceWorkloadMode::Enforce => {
                Ok(HealthcheckTarget {
                    engine: Some(engine),
                    container_name: None,
                    capability: HealthcheckCapability {
                        configurable: true,
                        reason: None,
                        can_apply_now: false,
                        apply_now_reason: Some(format!(
                            "it runs on worker node {node_id}; the settings apply when its \
                             container is next recreated from its workload spec"
                        )),
                    },
                })
            }
            Some(node_id) => Ok(unsupported(format!(
                "it runs on worker node {node_id}, whose agent creates its container without a \
                 Docker healthcheck"
            ))),
        }
    }

    /// The running control-plane container's healthcheck, best-effort.
    async fn running_healthcheck(&self, container_name: &str) -> Option<HealthcheckTimings> {
        let docker = self.docker.get()?;
        match tokio::time::timeout(
            INSPECT_TIMEOUT,
            docker.inspect_container(container_name, None::<InspectContainerOptions>),
        )
        .await
        {
            Ok(Ok(inspect)) => inspect
                .config
                .and_then(|config| config.healthcheck)
                .as_ref()
                .and_then(timings_of),
            Ok(Err(error)) => {
                tracing::debug!(container = container_name, %error, "Could not read the container's healthcheck");
                None
            }
            Err(_) => {
                warn!(
                    container = container_name,
                    timeout_secs = INSPECT_TIMEOUT.as_secs(),
                    "Reading the container's healthcheck timed out"
                );
                None
            }
        }
    }

    /// The service's overrides, what a recreated container would get, and
    /// what its running container has.
    pub async fn get_service_healthcheck(
        &self,
        service_id: i32,
    ) -> Result<ServiceHealthcheck, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let target = self.healthcheck_target(&service)?;
        let overrides = service_overrides(&service);
        let effective = target
            .engine
            .map(|engine| healthcheck::resolve_for(engine, &overrides));
        let inherited = target
            .engine
            .map(|engine| healthcheck::resolve_for(engine, &HealthcheckOverrides::default()));
        let support = match &target.container_name {
            Some(_) => match self.docker.get() {
                Some(docker) => Some(healthcheck::start_interval_support(docker).await),
                None => None,
            },
            None => None,
        };
        let running = match &target.container_name {
            Some(container) => self.running_healthcheck(container).await,
            None => None,
        };
        let pending_recreate = match (&running, &effective, support) {
            (Some(running), Some(effective), Some(support)) => {
                *running != healthcheck::effective_timings(effective.timings, support)
            }
            _ => false,
        };
        Ok(ServiceHealthcheck {
            service_id,
            service_name: service.name,
            service_type: service.service_type,
            engine: target.engine,
            capability: target.capability,
            overrides,
            effective,
            inherited,
            running,
            pending_recreate,
            start_interval_supported: support.map(|s| s == StartIntervalSupport::Supported),
        })
    }

    /// Replace the service's overrides. Changes nothing running: a new or
    /// recreated container gets them.
    ///
    /// Each value is checked against its bounds, then the combination with
    /// the instance default and the engine's built-in values (the timeout
    /// must stay shorter than the interval). Clearing every override is
    /// always accepted.
    pub async fn set_service_healthcheck(
        &self,
        service_id: i32,
        overrides: HealthcheckOverrides,
    ) -> Result<ServiceHealthcheckUpdate, ExternalServiceError> {
        overrides
            .validate()
            .map_err(|source| ExternalServiceError::InvalidHealthcheck {
                service_id: Some(service_id),
                source,
            })?;
        let service = self.get_service(service_id).await?;
        let target = self.healthcheck_target(&service)?;
        if !overrides.is_empty() {
            let Some(engine) = target.engine else {
                return Err(ExternalServiceError::HealthcheckUnsupported {
                    service_id,
                    name: service.name.clone(),
                    operation: "override the healthcheck",
                    reason: target
                        .capability
                        .reason
                        .unwrap_or_else(|| "it has no Docker healthcheck".to_string()),
                });
            };
            let builtin = HealthcheckTimings::builtin(engine);
            healthcheck::resolve(&overrides, &registry::instance_default(), &builtin)
                .validate()
                .map_err(|source| ExternalServiceError::InvalidHealthcheck {
                    service_id: Some(service_id),
                    source,
                })?;
        }

        const OPERATION: &str = "saving the healthcheck overrides of";
        let txn = begin_bounded_transaction(&self.db, service_id, OPERATION, SERVICE_LOCK_TIMEOUT)
            .await?;
        let (previous, service_name) =
            match Self::write_healthcheck_overrides(&txn, service_id, &overrides).await {
                Ok(saved) => saved,
                Err(error) => {
                    let error = lock_timeout_or_database_error(
                        service_id,
                        OPERATION,
                        SERVICE_LOCK_TIMEOUT,
                        error,
                    );
                    return Err(rollback_before_error(txn, error).await);
                }
            };
        txn.commit().await?;

        registry::set_service(registry::ServiceHealthcheckEntry {
            service_id,
            container_name: target.container_name,
            overrides,
        });
        info!(
            service_id,
            interval_secs = ?overrides.interval_secs,
            start_interval_secs = ?overrides.start_interval_secs,
            start_period_secs = ?overrides.start_period_secs,
            timeout_secs = ?overrides.timeout_secs,
            retries = ?overrides.retries,
            "Healthcheck overrides saved for external service {} ({}); applied when its \
             container is next recreated",
            service_id,
            service_name
        );
        Ok(ServiceHealthcheckUpdate {
            service_id,
            service_name,
            previous,
            current: overrides,
        })
    }

    /// The locked read-compare-write behind [`Self::set_service_healthcheck`].
    async fn write_healthcheck_overrides(
        txn: &sea_orm::DatabaseTransaction,
        service_id: i32,
        overrides: &HealthcheckOverrides,
    ) -> Result<(HealthcheckOverrides, String), ExternalServiceError> {
        let service = external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })?;
        let previous = service_overrides(&service);
        let service_name = service.name.clone();
        if previous != *overrides {
            let mut active: external_services::ActiveModel = service.into();
            active.healthcheck_interval_secs = Set(to_column(overrides.interval_secs));
            active.healthcheck_start_interval_secs = Set(to_column(overrides.start_interval_secs));
            active.healthcheck_start_period_secs = Set(to_column(overrides.start_period_secs));
            active.healthcheck_timeout_secs = Set(to_column(overrides.timeout_secs));
            active.healthcheck_retries = Set(to_column(overrides.retries));
            active.updated_at = Set(Utc::now());
            active.update(txn).await?;
        }
        Ok((previous, service_name))
    }

    /// Recreate a standalone control-plane service's container so it gets
    /// its effective healthcheck now. Its volumes and configuration are kept
    /// by the recreate guard; the service restarts briefly.
    pub async fn apply_service_healthcheck(
        &self,
        service_id: i32,
    ) -> Result<HealthcheckApplied, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let target = self.healthcheck_target(&service)?;
        let (Some(engine), true) = (target.engine, target.capability.can_apply_now) else {
            return Err(ExternalServiceError::HealthcheckUnsupported {
                service_id,
                name: service.name.clone(),
                operation: "recreate the container to apply the healthcheck",
                reason: target
                    .capability
                    .apply_now_reason
                    .unwrap_or_else(|| "Temps cannot recreate it from here".to_string()),
            });
        };
        let timings = healthcheck::resolve_for(engine, &service_overrides(&service)).timings;
        self.recreate_service_container_preserving_data(&service, ContainerRecreate::HEALTHCHECK)
            .await?;
        info!(
            service_id,
            interval_secs = timings.interval_secs,
            start_interval_secs = timings.start_interval_secs,
            "Recreated external service {} ({}) to apply its healthcheck",
            service_id,
            service.name
        );
        Ok(HealthcheckApplied {
            service_id,
            service_name: service.name,
            timings,
        })
    }

    /// Load the instance default and every service that overrides its
    /// healthcheck, so the engines create containers with them. Run once at
    /// startup, before routes serve. Reads only rows with an override.
    pub async fn load_healthcheck_registry(
        &self,
        instance_default: HealthcheckOverrides,
    ) -> Result<usize, ExternalServiceError> {
        let rows = external_services::Entity::find()
            .filter(
                Condition::any()
                    .add(external_services::Column::HealthcheckIntervalSecs.is_not_null())
                    .add(external_services::Column::HealthcheckStartIntervalSecs.is_not_null())
                    .add(external_services::Column::HealthcheckStartPeriodSecs.is_not_null())
                    .add(external_services::Column::HealthcheckTimeoutSecs.is_not_null())
                    .add(external_services::Column::HealthcheckRetries.is_not_null()),
            )
            .all(self.db.as_ref())
            .await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            let container_name = match self.healthcheck_target(row) {
                Ok(target) => target.container_name,
                Err(error) => {
                    warn!(
                        service_id = row.id,
                        %error,
                        "Could not resolve the container of a service with healthcheck \
                         overrides; its control-plane container keeps the defaults until the \
                         next start"
                    );
                    None
                }
            };
            entries.push(registry::ServiceHealthcheckEntry {
                service_id: row.id,
                container_name,
                overrides: service_overrides(row),
            });
        }
        let count = entries.len();
        registry::replace_all(instance_default, entries);
        info!(
            services = count,
            "Loaded managed service healthcheck overrides"
        );
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_healthcheck_reads_back_in_seconds() {
        let health = temps_providers_core::healthcheck::health_config(
            "true",
            &HealthcheckTimings::builtin(ManagedEngine::Redis),
        );
        assert_eq!(
            timings_of(&health),
            Some(HealthcheckTimings::builtin(ManagedEngine::Redis))
        );
        // A container from before StartInterval existed.
        let old = HealthConfig {
            start_interval: None,
            start_period: None,
            ..health
        };
        assert_eq!(timings_of(&old).map(|t| t.start_interval_secs), Some(0));
        assert_eq!(timings_of(&HealthConfig::default()), None);
    }

    #[test]
    fn a_column_out_of_range_is_inherit() {
        assert_eq!(column(Some(-1)), None);
        assert_eq!(column(Some(30)), Some(30));
        assert_eq!(to_column(Some(u32::MAX)), None);
        assert_eq!(to_column(Some(60)), Some(60));
    }

    #[test]
    fn garage_has_no_docker_healthcheck() {
        assert_eq!(managed_engine(EngineKind::Garage), None);
        assert_eq!(
            managed_engine(EngineKind::Rustfs),
            Some(ManagedEngine::Rustfs)
        );
    }
}
