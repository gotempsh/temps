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

/// Publish the instance default `defaults` holds. Read under
/// [`HEALTHCHECK_WRITES`], not before it: a save in this process holds it
/// through its publish and has updated the watch before that (its settings
/// write reloads the cache), so the value read here is never older than one
/// that save published.
async fn publish_watched_default(
    defaults: &mut tokio::sync::watch::Receiver<Option<ManagedServiceHealthcheckSettings>>,
) {
    let _writes = HEALTHCHECK_WRITES.lock().await;
    let current = *defaults.borrow_and_update();
    if let Some(settings) = current {
        registry::set_instance_default(overrides_from_settings(settings));
    }
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
        self.update_within(overrides, super::SERVICE_LOCK_TIMEOUT)
            .await
    }

    /// [`Self::update`], waiting at most `lock_timeout` for another
    /// healthcheck settings save.
    async fn update_within(
        &self,
        overrides: HealthcheckOverrides,
        lock_timeout: Duration,
    ) -> Result<HealthcheckDefaultsUpdate, ExternalServiceError> {
        validate_instance_default(&overrides)?;
        // Held through the save and the registry publish, so the registry
        // ends with the default saved last.
        let _writes = HEALTHCHECK_WRITES.lock().await;
        // Held until the default is saved, so no service override is saved
        // against the old default in between, by any process.
        let lock = self
            .services
            .lock_healthcheck_settings_for("saving the healthcheck defaults", lock_timeout)
            .await?;
        let saved = match self.save_checked(overrides).await {
            Ok(saved) => saved,
            Err(error) => return Err(super::rollback_before_error(lock, error).await),
        };
        if let Err(error) = lock.commit().await {
            // The default is saved; only the lock's release failed, and its
            // connection's end releases it.
            warn!(%error, "Could not release the healthcheck settings lock after saving the defaults");
        }
        let (previous, saved) = saved;
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

    /// Check `overrides` against every service's saved overrides and save
    /// them, under the healthcheck settings lock: the previous and the saved
    /// default.
    async fn save_checked(
        &self,
        overrides: HealthcheckOverrides,
    ) -> Result<(HealthcheckOverrides, ManagedServiceHealthcheckSettings), ExternalServiceError>
    {
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
        Ok((previous, saved))
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
                     loaded instance default until they change"
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
                publish_watched_default(&mut defaults).await;
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
        // A failed read is a database error (a service that cannot be
        // classified is logged and skipped): reconnect and read again.
        if let Err(error) = self.reload_registry().await {
            warn!(
                %error,
                retry_secs,
                "Could not reload the managed service healthcheck overrides after \
                 subscribing; reconnecting to read them again"
            );
            return;
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
                        // A database error, likely transient. The
                        // notification is consumed, so reconnect: the reload
                        // on resubscribing reads this service again.
                        warn!(
                            %error,
                            payload = notification.payload(),
                            retry_secs,
                            "Could not reload the healthcheck overrides of a changed service; \
                             reconnecting to read them again"
                        );
                        return;
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

    /// A default saved here while an older one waited for the lock is not
    /// overwritten by it.
    #[tokio::test]
    async fn a_watched_default_never_overwrites_a_newer_save() {
        let _registry = crate::services::HEALTHCHECK_REGISTRY_TEST_LOCK.lock().await;
        let interval = |secs| ManagedServiceHealthcheckSettings {
            interval_secs: Some(secs),
            ..Default::default()
        };
        let (watch, mut defaults) = tokio::sync::watch::channel(None);
        watch.send_replace(Some(interval(60)));

        // A save in this process holds the lock; the older value waits.
        let save = HEALTHCHECK_WRITES.lock().await;
        let waiting = tokio::spawn(async move {
            publish_watched_default(&mut defaults).await;
        });
        tokio::task::yield_now().await;
        watch.send_replace(Some(interval(90)));
        registry::set_instance_default(overrides_from_settings(interval(90)));
        drop(save);
        waiting.await.expect("publish task");

        let published = registry::instance_default();
        registry::set_instance_default(HealthcheckOverrides::default());
        assert_eq!(published.interval_secs, Some(90));
    }

    /// The service reading and writing through `db`.
    fn defaults_on(db: sea_orm::DatabaseConnection) -> HealthcheckDefaultsService {
        defaults_on_shared(Arc::new(db))
    }

    /// [`defaults_on`] for a connection shared with the test.
    fn defaults_on_shared(db: Arc<sea_orm::DatabaseConnection>) -> HealthcheckDefaultsService {
        let manager = Arc::new(ExternalServiceManager::new_with_handle(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "healthcheck defaults test",
            )),
            Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "healthcheck defaults test",
            )),
            true,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        ));
        let config = Arc::new(ConfigService::new(
            Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .expect("ServerConfig::new"),
            ),
            db,
        ));
        HealthcheckDefaultsService::new(config, manager)
    }

    /// A failed read is retried: the listener gives up its subscription, so
    /// the sync loop reconnects and reloads everything, rather than staying
    /// subscribed with what it failed to read. Listens on a real database;
    /// the reads go to a mock that fails.
    #[tokio::test]
    async fn a_failed_reload_reconnects_the_listener() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, MockDatabase};
        let _registry = crate::services::HEALTHCHECK_REGISTRY_TEST_LOCK.lock().await;
        let database = match temps_test_utils::TestDatabase::new().await {
            Ok(database) => database,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping a_failed_reload_reconnects_the_listener: {error}");
                return;
            }
            Err(error) => panic!("test database setup failed: {error}"),
        };
        let gives_up = |defaults: HealthcheckDefaultsService| {
            let url = database.database_url.clone();
            tokio::spawn(async move { defaults.listen_for_registry_changes(&url).await })
        };

        // The reload after subscribing fails.
        let listening = gives_up(defaults_on(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));
        tokio::time::timeout(Duration::from_secs(10), listening)
            .await
            .expect("a failed reload after subscribing gives the subscription up")
            .expect("listener task");

        // The reload succeeds (settings unreadable, no overrides), one
        // notification is read, and the next one fails.
        let service_id = 990_311;
        let named = temps_entities::external_services::Model {
            service_type: "not-an-engine".to_string(),
            healthcheck_interval_secs: Some(60),
            ..crate::services::test_support::encrypted_service_model(
                service_id,
                serde_json::json!({}),
            )
        };
        let mut listening = gives_up(defaults_on(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("settings unavailable".into())])
                .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
                .append_query_results([vec![named]])
                .append_query_errors([DbErr::Custom("connection reset".into())])
                .into_connection(),
        ));
        let notifier = database.connection_arc();
        // Sent until it gives up: one sent before it subscribed is lost.
        let gave_up = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                notifier
                    .execute(sea_orm::Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Postgres,
                        "SELECT pg_notify($1, $2)",
                        [HEALTHCHECK_CHANNEL.into(), service_id.to_string().into()],
                    ))
                    .await
                    .expect("notify");
                tokio::select! {
                    result = &mut listening => return result,
                    () = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
        })
        .await;
        let read = registry::for_service(service_id);
        registry::clear_service(service_id);
        gave_up
            .expect("a failed notification read gives the subscription up")
            .expect("listener task");
        assert_eq!(
            read.interval_secs,
            Some(60),
            "the first notification was read"
        );
    }

    /// A default save waits for every other healthcheck settings save, in
    /// any process, and says so when the wait runs out.
    #[tokio::test]
    async fn a_default_save_waits_for_another_healthcheck_settings_save() {
        use sea_orm::{ConnectionTrait, TransactionTrait};
        let _registry = crate::services::HEALTHCHECK_REGISTRY_TEST_LOCK.lock().await;
        let database = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!(
                    "Skipping a_default_save_waits_for_another_healthcheck_settings_save: {error}"
                );
                return;
            }
            Err(error) => panic!("test database setup failed: {error}"),
        };
        let db = database.connection_arc();
        let defaults = defaults_on_shared(db.clone());
        let interval = |secs| HealthcheckOverrides {
            interval_secs: Some(secs),
            ..Default::default()
        };

        // Another process's save holds the lock.
        let other = db.begin().await.expect("begin");
        other
            .execute_unprepared(
                "SELECT pg_advisory_xact_lock(hashtextextended('external-service-healthcheck-settings', 0))",
            )
            .await
            .expect("lock");
        let waited = defaults
            .update_within(interval(60), Duration::from_millis(300))
            .await;
        other.rollback().await.expect("release");
        let saved = defaults
            .update_within(interval(60), Duration::from_secs(10))
            .await;
        registry::set_instance_default(HealthcheckOverrides::default());

        assert!(
            matches!(
                waited,
                Err(ExternalServiceError::HealthcheckSettingsBusy {
                    operation: "saving the healthcheck defaults",
                    ..
                })
            ),
            "{waited:?}"
        );
        // What the 409 tells the operator.
        assert_eq!(
            waited.unwrap_err().to_string(),
            "Timed out after 1s waiting for another healthcheck settings save while saving the \
             healthcheck defaults. Nothing was saved; retry once the other save finishes"
        );
        assert_eq!(
            saved.expect("saved once the other save finished").current,
            interval(60)
        );
    }

    /// The listener reconnects (and reloads) on an error, so a failed read
    /// must be reported, while a service that cannot be classified must not
    /// be, or it would reconnect forever.
    #[tokio::test]
    async fn only_a_database_error_fails_a_notification() {
        use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
        let _registry = crate::services::HEALTHCHECK_REGISTRY_TEST_LOCK.lock().await;
        let unclassifiable = temps_entities::external_services::Model {
            service_type: "not-an-engine".to_string(),
            healthcheck_interval_secs: Some(60),
            ..crate::services::test_support::encrypted_service_model(990_301, serde_json::json!({}))
        };
        let defaults = defaults_on(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![unclassifiable]])
                .append_query_errors([DbErr::Custom("connection reset".into())])
                .into_connection(),
        );

        let classified = defaults.handle_registry_notification("990301").await;
        let published = registry::for_service(990_301);
        registry::clear_service(990_301);
        assert!(classified.is_ok(), "{classified:?}");
        assert_eq!(published.interval_secs, Some(60), "kept by id");

        let failed = defaults.handle_registry_notification("990302").await;
        assert!(
            matches!(failed, Err(ExternalServiceError::DatabaseError { .. })),
            "{failed:?}"
        );
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
