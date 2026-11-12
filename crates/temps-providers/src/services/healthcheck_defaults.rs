// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The instance-wide default of the managed-service healthcheck, kept in
//! platform settings (`AppSettings.managed_service_healthchecks`) and
//! mirrored into the registry the engines read when they create a container.

use std::sync::Arc;

use serde::Serialize;
use temps_config::{ConfigService, ManagedServiceHealthcheckSettings};
use temps_providers_core::externalsvc::recreate_guard::ManagedEngine;
use temps_providers_core::healthcheck::{
    self, registry, HealthcheckOverrides, HealthcheckTimings, ResolvedHealthcheck,
};
use tracing::info;
use utoipa::ToSchema;

use super::manager::HEALTHCHECK_WRITES;
use super::{ExternalServiceError, ExternalServiceManager};

/// Every engine whose containers get a Temps-built healthcheck.
pub const HEALTHCHECK_ENGINES: [ManagedEngine; 6] = [
    ManagedEngine::Postgres,
    ManagedEngine::Redis,
    ManagedEngine::MongoDb,
    ManagedEngine::MariaDb,
    ManagedEngine::Rustfs,
    ManagedEngine::Minio,
];

pub fn overrides_from_settings(
    settings: ManagedServiceHealthcheckSettings,
) -> HealthcheckOverrides {
    HealthcheckOverrides {
        interval_secs: settings.interval_secs,
        start_interval_secs: settings.start_interval_secs,
        start_period_secs: settings.start_period_secs,
        timeout_secs: settings.timeout_secs,
        retries: settings.retries,
    }
}

fn settings_from_overrides(overrides: HealthcheckOverrides) -> ManagedServiceHealthcheckSettings {
    ManagedServiceHealthcheckSettings {
        interval_secs: overrides.interval_secs,
        start_interval_secs: overrides.start_interval_secs,
        start_period_secs: overrides.start_period_secs,
        timeout_secs: overrides.timeout_secs,
        retries: overrides.retries,
    }
}

/// What a service of one engine inherits.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct EngineHealthcheckDefault {
    pub engine: ManagedEngine,
    /// Temps' own timings for the engine.
    pub builtin: HealthcheckTimings,
    /// What a service without overrides gets, and where each value comes from.
    pub effective: ResolvedHealthcheck,
    /// How long Temps waits for a new container of this engine to become
    /// healthy; a check has to land inside it.
    pub readiness_wait_secs: u32,
}

/// The instance default and what it gives each engine.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct HealthcheckDefaults {
    /// The instance default (null = the engine's built-in value).
    pub overrides: HealthcheckOverrides,
    pub engines: Vec<EngineHealthcheckDefault>,
}

impl HealthcheckDefaults {
    pub fn of(overrides: HealthcheckOverrides) -> Self {
        Self {
            overrides,
            engines: HEALTHCHECK_ENGINES
                .iter()
                .map(|&engine| {
                    let builtin = HealthcheckTimings::builtin(engine);
                    EngineHealthcheckDefault {
                        engine,
                        builtin,
                        effective: healthcheck::resolve(&Default::default(), &overrides, &builtin),
                        readiness_wait_secs: healthcheck::readiness_wait_secs(engine),
                    }
                })
                .collect(),
        }
    }
}

/// What [`HealthcheckDefaultsService::update`] changed.
#[derive(Debug, Clone)]
pub struct HealthcheckDefaultsUpdate {
    pub previous: HealthcheckOverrides,
    pub current: HealthcheckOverrides,
}

/// Validate an instance default: each value within its bounds, and the
/// combination with every engine's built-in values valid (the timeout
/// shorter than the interval, a check inside the engine's readiness wait).
/// Pure; the stored service overrides are checked by
/// [`HealthcheckDefaultsService::update`].
pub fn validate_instance_default(
    overrides: &HealthcheckOverrides,
) -> Result<(), ExternalServiceError> {
    let invalid = |source| ExternalServiceError::InvalidHealthcheck {
        service_id: None,
        source,
    };
    overrides.validate().map_err(invalid)?;
    for engine in HEALTHCHECK_ENGINES {
        healthcheck::resolve(
            &Default::default(),
            overrides,
            &HealthcheckTimings::builtin(engine),
        )
        .validate(healthcheck::readiness_wait_secs(engine))
        .map_err(invalid)?;
    }
    Ok(())
}

/// Reads and writes the instance default.
pub struct HealthcheckDefaultsService {
    config: Arc<ConfigService>,
    /// Checks a new default against the overrides services already have.
    services: Arc<ExternalServiceManager>,
}

impl HealthcheckDefaultsService {
    pub fn new(config: Arc<ConfigService>, services: Arc<ExternalServiceManager>) -> Self {
        Self { config, services }
    }

    /// The stored instance default.
    pub async fn get(&self) -> Result<HealthcheckDefaults, ExternalServiceError> {
        let settings =
            self.config
                .get_settings()
                .await
                .map_err(|e| ExternalServiceError::DatabaseError {
                    reason: format!("failed to read the managed service healthcheck defaults: {e}"),
                })?;
        Ok(HealthcheckDefaults::of(overrides_from_settings(
            settings.managed_service_healthchecks,
        )))
    }

    /// Replace the instance default. New and recreated containers of every
    /// service that does not override a value get it; nothing running
    /// changes.
    pub async fn update(
        &self,
        overrides: HealthcheckOverrides,
    ) -> Result<HealthcheckDefaultsUpdate, ExternalServiceError> {
        validate_instance_default(&overrides)?;
        // Held through the save and the registry publish, so no service
        // override is saved against the old default in between, and the
        // registry ends with the default saved last.
        let _writes = HEALTHCHECK_WRITES.lock().await;
        self.services
            .check_instance_default_against_services(&overrides)
            .await?;
        let mut previous = HealthcheckOverrides::default();
        let saved = self
            .config
            .update_managed_service_healthcheck_settings(|section| {
                previous = overrides_from_settings(*section);
                *section = settings_from_overrides(overrides);
            })
            .await
            .map_err(|e| ExternalServiceError::DatabaseError {
                reason: format!("failed to save the managed service healthcheck defaults: {e}"),
            })?;
        let current = overrides_from_settings(saved);
        registry::set_instance_default(current);
        info!(
            interval_secs = ?current.interval_secs,
            start_interval_secs = ?current.start_interval_secs,
            start_period_secs = ?current.start_period_secs,
            timeout_secs = ?current.timeout_secs,
            retries = ?current.retries,
            "Managed service healthcheck defaults saved; applied when containers are next created"
        );
        Ok(HealthcheckDefaultsUpdate { previous, current })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_providers_core::healthcheck::{HealthcheckSource, HealthcheckValidationError};

    #[test]
    fn an_instance_default_is_checked_against_every_engine() {
        assert!(validate_instance_default(&HealthcheckOverrides::default()).is_ok());
        assert!(validate_instance_default(&HealthcheckOverrides {
            interval_secs: Some(60),
            ..Default::default()
        })
        .is_ok());
        // Shorter than MongoDB's 10 s built-in timeout.
        let error = validate_instance_default(&HealthcheckOverrides {
            interval_secs: Some(8),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            matches!(
                error,
                ExternalServiceError::InvalidHealthcheck {
                    service_id: None,
                    source: HealthcheckValidationError::TimeoutNotShorterThanInterval {
                        timeout_secs: 10,
                        timeout_source: HealthcheckSource::Builtin,
                        ..
                    }
                }
            ),
            "{error}"
        );
        assert!(
            error.to_string().contains("the instance default"),
            "{error}"
        );
        let error = validate_instance_default(&HealthcheckOverrides {
            retries: Some(11),
            ..Default::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("retries"), "{error}");
    }

    #[test]
    fn the_defaults_show_what_each_engine_inherits() {
        let defaults = HealthcheckDefaults::of(HealthcheckOverrides {
            interval_secs: Some(60),
            ..Default::default()
        });
        assert_eq!(defaults.engines.len(), HEALTHCHECK_ENGINES.len());
        for engine in &defaults.engines {
            assert_eq!(engine.effective.timings.interval_secs, 60);
            assert_eq!(
                engine.effective.sources.interval_secs,
                HealthcheckSource::InstanceDefault
            );
            assert_eq!(
                engine.effective.timings.start_period_secs,
                engine.builtin.start_period_secs
            );
            assert_eq!(
                engine.effective.sources.start_period_secs,
                HealthcheckSource::Builtin
            );
        }
    }

    #[test]
    fn settings_and_overrides_convert_both_ways() {
        let overrides = HealthcheckOverrides {
            interval_secs: Some(60),
            start_interval_secs: Some(2),
            start_period_secs: Some(10),
            timeout_secs: Some(5),
            retries: Some(3),
        };
        assert_eq!(
            overrides_from_settings(settings_from_overrides(overrides)),
            overrides
        );
    }
}
