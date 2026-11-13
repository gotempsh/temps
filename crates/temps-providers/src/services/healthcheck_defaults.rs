// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The instance-wide default of the managed-service healthcheck, kept in
//! platform settings (`AppSettings.managed_service_healthchecks`) and
//! mirrored into the registry the engines read when they create a container.

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgListener;

use serde::Serialize;
use temps_config::{ConfigService, ManagedServiceHealthcheckSettings};
use temps_providers_core::externalsvc::recreate_guard::ManagedEngine;
use temps_providers_core::healthcheck::{
    self, registry, HealthcheckOverrides, HealthcheckTimings, ResolvedHealthcheck,
};
use tracing::{debug, info, warn};
use utoipa::ToSchema;

use super::manager::{HEALTHCHECK_CHANNEL, HEALTHCHECK_WRITES};
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

/// How long the listener for other processes' healthcheck changes waits
/// before connecting again after it lost (or could not open) its connection.
const LISTENER_RETRY: Duration = Duration::from_secs(10);

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

    /// The saved instance default, or the one the registry has when the
    /// settings cannot be read.
    async fn saved_instance_default(&self) -> HealthcheckOverrides {
        match self.config.get_settings().await {
            Ok(settings) => overrides_from_settings(settings.managed_service_healthchecks),
            Err(error) => {
                warn!(
                    %error,
                    "Could not read the managed service healthcheck defaults; keeping the last \
                     loaded instance default until the next refresh"
                );
                registry::instance_default()
            }
        }
    }

    /// Replace this process's registry with the saved instance default and
    /// every service's saved overrides, including what another process
    /// saved. Under [`HEALTHCHECK_WRITES`], so a save in this process cannot
    /// be overwritten by a read taken before it committed.
    ///
    /// When the settings cannot be read, the registry keeps the instance
    /// default it has, and the service overrides are still read.
    pub async fn reload_registry(&self) -> Result<usize, ExternalServiceError> {
        let _writes = HEALTHCHECK_WRITES.lock().await;
        let instance = self.saved_instance_default().await;
        self.services.load_healthcheck_registry(instance).await
    }

    /// Reload the one service a notification on [`HEALTHCHECK_CHANNEL`]
    /// names. A payload that is not a service id is logged and ignored.
    pub async fn handle_registry_notification(
        &self,
        payload: &str,
    ) -> Result<(), ExternalServiceError> {
        let Ok(service_id) = payload.parse::<i32>() else {
            warn!(
                payload,
                "Ignoring a healthcheck change notification that names no service id"
            );
            return Ok(());
        };
        let _writes = HEALTHCHECK_WRITES.lock().await;
        self.services.reload_service_healthcheck(service_id).await
    }

    /// Keep the registry current with what other processes save, for the
    /// life of the process. Nothing is polled:
    ///
    /// - the instance default arrives through
    ///   [`ConfigService::subscribe_managed_service_healthchecks`], fed by
    ///   the settings `NOTIFY` listener;
    /// - a service's overrides, rename or deletion arrive as its id on
    ///   [`HEALTHCHECK_CHANNEL`], and only that service is read again. The
    ///   listener reloads everything each time it (re)subscribes, since a
    ///   change sent while it was not listening is not delivered again.
    pub fn spawn_registry_sync(self: &Arc<Self>) {
        let service = Arc::clone(self);
        let mut defaults = self.config.subscribe_managed_service_healthchecks();
        tokio::spawn(async move {
            while defaults.changed().await.is_ok() {
                let current = *defaults.borrow_and_update();
                if let Some(settings) = current {
                    // After a save in this process, which holds it through
                    // its own publish: the registry ends with the latest.
                    let _writes = HEALTHCHECK_WRITES.lock().await;
                    registry::set_instance_default(overrides_from_settings(settings));
                }
            }
        });

        let service_listener = Arc::clone(&service);
        let database_url = self.config.get_database_url();
        tokio::spawn(async move {
            loop {
                service_listener
                    .listen_for_registry_changes(&database_url)
                    .await;
                tokio::time::sleep(LISTENER_RETRY).await;
            }
        });
    }

    /// Subscribe to [`HEALTHCHECK_CHANNEL`], reload everything, then reload
    /// each service named until the connection is lost.
    async fn listen_for_registry_changes(&self, database_url: &str) {
        let retry_secs = LISTENER_RETRY.as_secs();
        let mut listener = match PgListener::connect(database_url).await {
            Ok(listener) => listener,
            Err(error) => {
                warn!(
                    %error,
                    retry_secs,
                    "Could not connect to listen for other Temps processes' healthcheck \
                     changes; retrying"
                );
                return;
            }
        };
        if let Err(error) = listener.listen(HEALTHCHECK_CHANNEL).await {
            warn!(
                %error,
                retry_secs,
                "Could not listen for other Temps processes' healthcheck changes; retrying"
            );
            return;
        }
        if let Err(error) = self.reload_registry().await {
            warn!(
                %error,
                "Could not reload the managed service healthcheck overrides after \
                 subscribing; keeping the last loaded ones"
            );
        }
        debug!("Listening for managed service healthcheck changes");
        loop {
            // `try_recv` rather than `recv`, which reconnects silently and
            // would drop what was sent in between without a reload.
            match listener.try_recv().await {
                Ok(Some(notification)) => {
                    if let Err(error) = self
                        .handle_registry_notification(notification.payload())
                        .await
                    {
                        warn!(
                            %error,
                            payload = notification.payload(),
                            "Could not reload the healthcheck overrides of a changed service; \
                             it keeps the last loaded ones until the next change or reconnect"
                        );
                    }
                }
                Ok(None) => {
                    warn!(
                        retry_secs,
                        "Lost the connection listening for healthcheck changes; reconnecting"
                    );
                    return;
                }
                Err(error) => {
                    warn!(
                        %error,
                        retry_secs,
                        "Error listening for healthcheck changes; reconnecting"
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_providers_core::healthcheck::{HealthcheckSource, HealthcheckValidationError};

    /// The settings section publishes the bounds the instance default is
    /// validated against.
    #[test]
    fn the_settings_schema_publishes_the_accepted_bounds() {
        use temps_providers_core::healthcheck::HealthcheckField;
        use utoipa::PartialSchema;
        let schema = serde_json::to_value(
            <temps_config::ManagedServiceHealthcheckSettings as PartialSchema>::schema(),
        )
        .expect("serialize the schema");
        for field in HealthcheckField::ALL {
            let property = &schema["properties"][field.name()];
            let (min, max) = field.bounds();
            assert_eq!(property["minimum"], min, "{}: {property}", field.name());
            assert_eq!(property["maximum"], max, "{}: {property}", field.name());
        }
    }

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
