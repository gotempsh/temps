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
use tracing::{debug, info, warn};
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
    /// Why this service's overrides are set aside: they no longer combine
    /// with the instance default, so `effective` is what the container gets
    /// without them. Change or clear them to use them again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides_ignored_reason: Option<String>,
    /// How long Temps waits for a new container of this engine to become
    /// healthy; a check has to land inside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_wait_secs: Option<u32>,
    /// The running container's healthcheck, when it could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<HealthcheckTimings>,
    /// The running container differs from `effective` (as this host's
    /// Docker would store it) until it is recreated. `false` when either is
    /// unknown.
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
    /// As this host's Docker got them: below API 1.44 the interval is the
    /// start interval ([`healthcheck::effective_timings`]).
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

/// Whether recreating the container would change its healthcheck: the
/// running one differs from what this daemon would store for `effective`.
/// Unknown (no running container, no configurable check, or a daemon whose
/// start-interval support could not be read) is never pending.
fn pending_recreate(
    running: Option<&HealthcheckTimings>,
    effective: Option<HealthcheckTimings>,
    support: Option<StartIntervalSupport>,
) -> bool {
    match (running, effective, support) {
        (Some(running), Some(effective), Some(support)) => {
            !healthcheck::matches_running(running, effective, support)
        }
        _ => false,
    }
}

/// Where a service's container is, as far as its healthcheck is concerned.
struct HealthcheckTarget {
    engine: Option<ManagedEngine>,
    /// The control-plane container the engine creates, when there is one.
    container_name: Option<String>,
    capability: HealthcheckCapability,
}

/// Serializes healthcheck writes in this process (a service's overrides,
/// the instance default) from the stored write through publishing it to
/// the registry, so the registry ends with the value committed last, and an
/// instance default is checked against the service overrides it will be
/// combined with. Writes are rare operator actions.
pub(crate) static HEALTHCHECK_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl ExternalServiceManager {
    /// Why Temps will not recreate a control-plane service's container from
    /// here right now, if it will not. Recreating starts the container, so a
    /// service the operator stopped (or that is still starting, failed or
    /// being created) is left alone: its next start or recreate applies the
    /// healthcheck.
    fn local_apply_blocker(&self, service: &external_services::Model) -> Option<String> {
        if !self.local_workloads_enabled {
            return Some(
                "local workloads are disabled on this process, so Temps does not create or \
                 recreate containers here"
                    .to_string(),
            );
        }
        if service.status != "running" {
            return Some(format!(
                "it is {}, and recreating its container would start it; the settings apply \
                 when its container is next created",
                service.status
            ));
        }
        None
    }

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
                    can_apply_now: self.local_apply_blocker(service).is_none(),
                    apply_now_reason: self.local_apply_blocker(service),
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
        let instance = registry::instance_default();
        let checked = target
            .engine
            .map(|engine| healthcheck::resolve_checked(engine, &overrides, &instance));
        let inherited = target.engine.map(|engine| {
            healthcheck::resolve_checked(engine, &HealthcheckOverrides::default(), &instance)
                .resolved
        });
        // The running container is read only on a process that runs local
        // containers; elsewhere there is nothing here to compare.
        let local_container = target
            .container_name
            .as_deref()
            .filter(|_| self.local_workloads_enabled);
        let support = match (local_container, self.docker.get()) {
            (Some(_), Some(docker)) => healthcheck::known_start_interval_support(docker).await,
            _ => None,
        };
        let running = match local_container {
            Some(container) => self.running_healthcheck(container).await,
            None => None,
        };
        let effective = checked.as_ref().map(|c| c.resolved);
        let pending_recreate = pending_recreate(
            running.as_ref(),
            effective.as_ref().map(|e| e.timings),
            support,
        );
        Ok(ServiceHealthcheck {
            service_id,
            service_name: service.name,
            service_type: service.service_type,
            engine: target.engine,
            capability: target.capability,
            overrides,
            effective,
            inherited,
            overrides_ignored_reason: checked
                .as_ref()
                .and_then(|c| c.ignored.as_ref())
                .map(ToString::to_string),
            readiness_wait_secs: target.engine.map(healthcheck::readiness_wait_secs),
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
        // Held through the commit and the registry publish; the combination
        // check reads the instance default under it too.
        let _writes = HEALTHCHECK_WRITES.lock().await;
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
                .validate(healthcheck::readiness_wait_secs(engine))
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
        let engine = self.apply_now_engine(&service)?;
        let timings = healthcheck::resolve_for(engine, &service_overrides(&service)).timings;
        // Checked again once the service's lock is held: a stop that
        // finished while this waited must not be undone by the recreate.
        self.recreate_service_container_preserving_data(
            &service,
            ContainerRecreate::HEALTHCHECK,
            |current| self.apply_now_engine(current).map(|_| ()),
        )
        .await?;
        let support = match self.docker.get() {
            Some(docker) => healthcheck::start_interval_support(docker).await,
            None => StartIntervalSupport::Supported,
        };
        let timings = healthcheck::effective_timings(timings, support);
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

    /// Forget a deleted service's overrides. Under [`HEALTHCHECK_WRITES`],
    /// which an override save holds through its commit and publish, so a
    /// save that committed before the delete cannot put the entry back after
    /// it (and hand it to a later service reusing the container name).
    pub(crate) async fn forget_healthcheck_overrides(service_id: i32) {
        let _writes = HEALTHCHECK_WRITES.lock().await;
        registry::clear_service(service_id);
    }

    /// Publish a service's overrides again under its container's current
    /// name, after a rename: the engines look them up by container name, so
    /// the old name would keep them and the new one would not.
    pub(crate) async fn republish_healthcheck_overrides(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        let _writes = HEALTHCHECK_WRITES.lock().await;
        let service = self.get_service(service_id).await?;
        let target = self.healthcheck_target(&service)?;
        registry::set_service(registry::ServiceHealthcheckEntry {
            service_id,
            container_name: target.container_name,
            overrides: service_overrides(&service),
        });
        Ok(())
    }

    /// The engine whose container Apply now recreates, or why it may not:
    /// local workloads off (before anything reaches Docker, so the operator
    /// sees the policy rather than a missing-daemon error), not running, a
    /// cluster, a worker or an imported container.
    fn apply_now_engine(
        &self,
        service: &external_services::Model,
    ) -> Result<ManagedEngine, ExternalServiceError> {
        if service.node_id.is_none() && !self.local_workloads_enabled {
            return Err(ExternalServiceError::LocalWorkloadsDisabled {
                name: service.name.clone(),
            });
        }
        let target = self.healthcheck_target(service)?;
        match (target.engine, target.capability.can_apply_now) {
            (Some(engine), true) => Ok(engine),
            _ => Err(ExternalServiceError::HealthcheckUnsupported {
                service_id: service.id,
                name: service.name.clone(),
                operation: "recreate the container to apply the healthcheck",
                reason: target
                    .capability
                    .apply_now_reason
                    .unwrap_or_else(|| "Temps cannot recreate it from here".to_string()),
            }),
        }
    }

    /// Load the instance default and every service that overrides its
    /// healthcheck, so the engines create containers with them. Run at
    /// startup, before routes serve, and then periodically by
    /// [`crate::services::HealthcheckDefaultsService::reload_registry`] to
    /// pick up what other processes saved. Reads only rows with an override.
    ///
    /// The instance default is published first: when the services cannot be
    /// read, new containers still get it rather than the built-in timings.
    pub async fn load_healthcheck_registry(
        &self,
        instance_default: HealthcheckOverrides,
    ) -> Result<usize, ExternalServiceError> {
        registry::set_instance_default(instance_default);
        let rows = self.services_with_healthcheck_overrides().await?;
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
        debug!(
            services = count,
            "Loaded managed service healthcheck overrides"
        );
        Ok(count)
    }

    /// Every service row with at least one healthcheck override (none on a
    /// default install).
    async fn services_with_healthcheck_overrides(
        &self,
    ) -> Result<Vec<external_services::Model>, ExternalServiceError> {
        Ok(external_services::Entity::find()
            .filter(
                Condition::any()
                    .add(external_services::Column::HealthcheckIntervalSecs.is_not_null())
                    .add(external_services::Column::HealthcheckStartIntervalSecs.is_not_null())
                    .add(external_services::Column::HealthcheckStartPeriodSecs.is_not_null())
                    .add(external_services::Column::HealthcheckTimeoutSecs.is_not_null())
                    .add(external_services::Column::HealthcheckRetries.is_not_null()),
            )
            .all(self.db.as_ref())
            .await?)
    }

    /// Refuse an instance default that would no longer combine with the
    /// overrides a service already has: the service's next container would
    /// otherwise set them aside. Call it holding [`HEALTHCHECK_WRITES`], so
    /// no override is saved between this check and the default's save.
    pub(crate) async fn check_instance_default_against_services(
        &self,
        instance: &HealthcheckOverrides,
    ) -> Result<(), ExternalServiceError> {
        let mut conflicts = Vec::new();
        for row in self.services_with_healthcheck_overrides().await? {
            let engine = match self.healthcheck_target(&row) {
                Ok(target) => target.engine,
                Err(error) => {
                    warn!(
                        service_id = row.id,
                        %error,
                        "Could not classify a service with healthcheck overrides; not checking \
                         the instance default against them"
                    );
                    None
                }
            };
            let Some(engine) = engine else { continue };
            let resolved = healthcheck::resolve(
                &service_overrides(&row),
                instance,
                &HealthcheckTimings::builtin(engine),
            );
            if let Err(source) = resolved.validate(healthcheck::readiness_wait_secs(engine)) {
                conflicts.push((row.id, row.name, source));
            }
        }
        let others = conflicts.len().saturating_sub(1);
        match conflicts.into_iter().next() {
            None => Ok(()),
            Some((service_id, name, source)) => Err(
                ExternalServiceError::HealthcheckDefaultConflictsWithService {
                    service_id,
                    name,
                    source,
                    others,
                },
            ),
        }
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

    /// What a daemon stores for the healthcheck Temps creates a container
    /// with, read back the way `get_service_healthcheck` reads it.
    fn stored_by(
        support: StartIntervalSupport,
        effective: &HealthcheckTimings,
    ) -> HealthcheckTimings {
        let sent = healthcheck::effective_timings(*effective, support);
        let mut health = healthcheck::health_config("true", &sent);
        if support == StartIntervalSupport::Unsupported {
            // API < 1.44 has no StartInterval: it is dropped, not stored.
            health.start_interval = None;
        }
        timings_of(&health).expect("a created healthcheck reads back")
    }

    #[test]
    fn a_just_applied_container_is_not_pending_on_any_daemon() {
        for engine in [
            ManagedEngine::Postgres,
            ManagedEngine::Redis,
            ManagedEngine::MongoDb,
            ManagedEngine::MariaDb,
            ManagedEngine::Rustfs,
            ManagedEngine::Minio,
        ] {
            let effective = HealthcheckTimings::builtin(engine);
            for support in [
                StartIntervalSupport::Supported,
                StartIntervalSupport::Unsupported,
            ] {
                let running = stored_by(support, &effective);
                assert!(
                    !pending_recreate(Some(&running), Some(effective), Some(support)),
                    "{engine:?} on {support:?}: running {running:?}, effective {effective:?}"
                );
            }
        }
    }

    #[test]
    fn a_container_from_before_a_change_is_pending_on_any_daemon() {
        let effective = HealthcheckTimings::builtin(ManagedEngine::Redis);
        let changed = HealthcheckTimings {
            timeout_secs: 10,
            retries: 5,
            ..effective
        };
        for support in [
            StartIntervalSupport::Supported,
            StartIntervalSupport::Unsupported,
        ] {
            let running = stored_by(support, &effective);
            assert!(pending_recreate(
                Some(&running),
                Some(changed),
                Some(support)
            ));
        }
        // An old daemon only ever runs the start interval, so a new interval
        // would not change its container: recreating it is not pending.
        let slower = HealthcheckTimings {
            interval_secs: 120,
            ..effective
        };
        let old = StartIntervalSupport::Unsupported;
        assert!(!pending_recreate(
            Some(&stored_by(old, &effective)),
            Some(slower),
            Some(old)
        ));
        let modern = StartIntervalSupport::Supported;
        assert!(pending_recreate(
            Some(&stored_by(modern, &effective)),
            Some(slower),
            Some(modern)
        ));
        // The every-second check of a container created before #1877.
        let legacy = HealthConfig {
            start_interval: None,
            start_period: None,
            ..healthcheck::health_config(
                "true",
                &HealthcheckTimings {
                    interval_secs: 1,
                    start_interval_secs: 1,
                    start_period_secs: 0,
                    timeout_secs: 3,
                    retries: 30,
                },
            )
        };
        let legacy = timings_of(&legacy).expect("reads back");
        assert!(pending_recreate(
            Some(&legacy),
            Some(effective),
            Some(StartIntervalSupport::Unsupported)
        ));
    }

    #[test]
    fn unknown_is_never_pending() {
        let effective = HealthcheckTimings::builtin(ManagedEngine::Redis);
        let different = HealthcheckTimings {
            interval_secs: 1,
            ..effective
        };
        // The daemon's version could not be read.
        assert!(!pending_recreate(Some(&different), Some(effective), None));
        assert!(!pending_recreate(
            None,
            Some(effective),
            Some(StartIntervalSupport::Supported)
        ));
        assert!(!pending_recreate(
            Some(&different),
            None,
            Some(StartIntervalSupport::Supported)
        ));
    }

    /// A manager over `rows` with no Docker daemon: nothing here can reach a
    /// real container.
    fn manager_over(
        rows: Vec<external_services::Model>,
    ) -> crate::services::ExternalServiceManager {
        let db = std::sync::Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(rows.into_iter().map(|row| vec![row]))
                .into_connection(),
        );
        crate::services::ExternalServiceManager::new_with_handle(
            db.clone(),
            std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
                "service-parameter-reveal-test",
            )),
            std::sync::Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "apply now test",
            )),
            true,
            std::sync::Arc::new(temps_dns::DnsRegistry::new(db)),
        )
    }

    #[tokio::test]
    async fn apply_now_does_not_undo_a_stop_that_finished_while_it_waited() {
        use crate::services::test_support::encrypted_service_model;
        // What Apply now read before waiting for the service's lock...
        let seen = encrypted_service_model(7, serde_json::json!({}));
        // ...and the row once it holds it: stopped meanwhile.
        let stopped = external_services::Model {
            status: "stopped".to_string(),
            ..seen.clone()
        };
        let manager = manager_over(vec![stopped]);
        let error = manager
            .recreate_service_container_preserving_data(
                &seen,
                ContainerRecreate::HEALTHCHECK,
                |current| manager.apply_now_engine(current).map(|_| ()),
            )
            .await
            .unwrap_err();
        match error {
            ExternalServiceError::HealthcheckUnsupported { reason, .. } => {
                assert!(reason.contains("it is stopped"), "{reason}");
            }
            other => panic!("expected the stop to be respected, got {other:?}"),
        }

        // Still running: the check passes and the recreate goes on (to the
        // missing Docker daemon here).
        let manager = manager_over(vec![seen.clone(); 4]);
        let error = manager
            .recreate_service_container_preserving_data(
                &seen,
                ContainerRecreate::HEALTHCHECK,
                |current| manager.apply_now_engine(current).map(|_| ()),
            )
            .await
            .unwrap_err();
        assert!(
            !matches!(error, ExternalServiceError::HealthcheckUnsupported { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_renamed_service_keeps_its_overrides_under_its_new_container_name() {
        use crate::services::test_support::encrypted_service_model;
        // Ids and names no other test uses: the table is process-wide.
        let before = external_services::Model {
            id: 990_101,
            name: "hc-rename-before".to_string(),
            healthcheck_interval_secs: Some(60),
            ..encrypted_service_model(990_101, serde_json::json!({}))
        };
        let after = external_services::Model {
            name: "hc-rename-after".to_string(),
            ..before.clone()
        };
        let manager = manager_over(vec![after.clone()]);
        let old_container = manager
            .healthcheck_target(&before)
            .unwrap()
            .container_name
            .unwrap();
        let new_container = manager
            .healthcheck_target(&after)
            .unwrap()
            .container_name
            .unwrap();
        assert_ne!(old_container, new_container);
        registry::set_service(registry::ServiceHealthcheckEntry {
            service_id: before.id,
            container_name: Some(old_container.clone()),
            overrides: service_overrides(&before),
        });

        manager
            .republish_healthcheck_overrides(before.id)
            .await
            .unwrap();
        let on_new = registry::for_container(&new_container);
        let on_old = registry::for_container(&old_container);
        registry::clear_service(before.id);
        assert_eq!(on_new.interval_secs, Some(60));
        assert!(on_old.is_empty(), "{on_old:?}");
    }

    #[tokio::test]
    async fn a_delete_forgets_overrides_only_after_a_save_in_flight_published() {
        let service_id = 990_102;
        let container = "hc-delete-in-flight";
        let held = HEALTHCHECK_WRITES.lock().await;
        // As a save does while it holds the lock: commit, then publish.
        let forgetting = tokio::spawn(
            crate::services::ExternalServiceManager::forget_healthcheck_overrides(service_id),
        );
        tokio::task::yield_now().await;
        registry::set_service(registry::ServiceHealthcheckEntry {
            service_id,
            container_name: Some(container.to_string()),
            overrides: HealthcheckOverrides {
                interval_secs: Some(60),
                ..Default::default()
            },
        });
        assert!(!forgetting.is_finished());
        drop(held);
        forgetting.await.unwrap();
        assert!(registry::for_container(container).is_empty());
        assert!(registry::for_service(service_id).is_empty());
    }

    #[tokio::test]
    async fn the_instance_default_is_kept_when_the_services_cannot_be_read() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
        let db = std::sync::Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("connection reset".into())])
                .into_connection(),
        );
        let manager = crate::services::ExternalServiceManager::new_with_handle(
            db.clone(),
            std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
                "healthcheck registry test",
            )),
            std::sync::Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "healthcheck registry test",
            )),
            true,
            std::sync::Arc::new(temps_dns::DnsRegistry::new(db)),
        );
        let saved = HealthcheckOverrides {
            interval_secs: Some(45),
            ..Default::default()
        };
        let result = manager.load_healthcheck_registry(saved).await;
        let published = registry::instance_default();
        // The process-wide default reaches every container this test binary
        // creates; put it back before asserting.
        registry::set_instance_default(HealthcheckOverrides::default());
        assert!(result.is_err(), "the failed query is still reported");
        assert_eq!(published, saved);
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
