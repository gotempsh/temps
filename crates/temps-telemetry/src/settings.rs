// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Operator-facing control of anonymous product telemetry.
//!
//! Telemetry is decided by three inputs, in this order of precedence:
//!
//! 1. `TEMPS_TELEMETRY` set to an opt-out value in the server environment — a
//!    host-level kill switch read once at startup. Nothing in the console can
//!    override it.
//! 2. The admin preference stored in the settings row
//!    (`anonymous_telemetry_enabled`), changed from Settings › Telemetry and
//!    applied at runtime without a restart.
//! 3. [`DEFAULT_TELEMETRY_ENABLED`].
//!
//! [`TelemetrySettingsService`] reads and writes the preference through a
//! [`TelemetryPreferenceStore`], keeps the process's reporter in sync with it,
//! and reports the resolved state (including *why* it is on or off) for the
//! console.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use crate::service::{effective_enabled, DEFAULT_TELEMETRY_ENABLED};
use crate::TelemetryService;

/// How often each process re-reads the stored preference. The process that
/// served the admin's change applies it immediately; this bounds how long
/// other processes sharing the database (split proxy/console roles, stateless
/// replicas) keep the old value. Consent is read directly from its own JSON key so unrelated settings
/// decoding and cache snapshots cannot undo an opt-out.
pub const PREFERENCE_SYNC_INTERVAL: Duration = Duration::from_secs(30);

/// Errors reading or writing the stored telemetry preference.
#[derive(Error, Debug)]
pub enum TelemetrySettingsError {
    #[error("Failed to read the anonymous telemetry preference from settings row 1: {reason}")]
    PreferenceRead { reason: String },

    #[error(
        "Failed to save anonymous telemetry preference (enabled={enabled}) to settings row 1: {reason}"
    )]
    PreferenceWrite { enabled: bool, reason: String },
}

/// Durable home of the admin's telemetry preference.
#[async_trait]
pub trait TelemetryPreferenceStore: Send + Sync {
    /// The stored preference; `None` when no admin has chosen.
    async fn load(&self) -> Result<Option<bool>, TelemetrySettingsError>;
    /// Persist a preference and return what was stored.
    async fn save(&self, enabled: bool) -> Result<Option<bool>, TelemetrySettingsError>;
}

/// [`TelemetryPreferenceStore`] backed by the shared `settings` row via
/// [`temps_config::ConfigService`].
pub struct ConfigTelemetryPreferenceStore {
    config_service: Arc<temps_config::ConfigService>,
}

impl ConfigTelemetryPreferenceStore {
    pub fn new(config_service: Arc<temps_config::ConfigService>) -> Self {
        Self { config_service }
    }
}

#[async_trait]
impl TelemetryPreferenceStore for ConfigTelemetryPreferenceStore {
    async fn load(&self) -> Result<Option<bool>, TelemetrySettingsError> {
        self.config_service
            .anonymous_telemetry_preference()
            .await
            .map_err(|error| TelemetrySettingsError::PreferenceRead {
                reason: error.to_string(),
            })
    }

    async fn save(&self, enabled: bool) -> Result<Option<bool>, TelemetrySettingsError> {
        self.config_service
            .set_anonymous_telemetry_enabled(enabled)
            .await
            .map_err(|error| TelemetrySettingsError::PreferenceWrite {
                enabled,
                reason: error.to_string(),
            })
    }
}

/// Read the stored admin preference straight from the settings row, without
/// a [`temps_config::ConfigService`]. For one-shot paths that run outside the
/// plugin system (a failed upgrade reporting before the process exits) and
/// must still honour an admin's opt-out.
pub async fn stored_preference(
    db: &sea_orm::DatabaseConnection,
) -> Result<Option<bool>, TelemetrySettingsError> {
    temps_config::anonymous_telemetry_preference(db)
        .await
        .map_err(|error| TelemetrySettingsError::PreferenceRead {
            reason: error.to_string(),
        })
}

/// What decided the current telemetry state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryStatusSource {
    /// `TEMPS_TELEMETRY` forced it off in the server environment.
    Environment,
    /// An admin turned it on or off in Settings › Telemetry.
    AdminSetting,
    /// Nobody has chosen; the built-in default applies.
    Default,
    /// The reporter could not start (for example the anonymous ID file is not
    /// writable), so nothing is sent this run. See the server log.
    Unavailable,
}

/// Resolved telemetry state for this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryStatus {
    pub enabled: bool,
    pub source: TelemetryStatusSource,
    pub env_opted_out: bool,
    pub admin_preference: Option<bool>,
    pub anonymous_id: Option<String>,
    pub endpoint: Option<String>,
    pub temps_version: Option<String>,
}

/// Resolve the state from its inputs. Pure, so the precedence is testable on
/// its own.
pub fn resolve_status(
    reporter_available: bool,
    env_opted_out: bool,
    admin_preference: Option<bool>,
) -> (bool, TelemetryStatusSource) {
    if !reporter_available {
        return (false, TelemetryStatusSource::Unavailable);
    }
    if env_opted_out {
        return (false, TelemetryStatusSource::Environment);
    }
    match admin_preference {
        Some(enabled) => (enabled, TelemetryStatusSource::AdminSetting),
        None => (
            effective_enabled(false, None),
            TelemetryStatusSource::Default,
        ),
    }
}

/// Reads/writes the admin preference and keeps the reporter in sync with it.
pub struct TelemetrySettingsService {
    /// `None` when the reporter failed to initialize; status then reports
    /// [`TelemetryStatusSource::Unavailable`] instead of pretending.
    reporter: Option<TelemetryService>,
    store: Arc<dyn TelemetryPreferenceStore>,
    /// Environment opt-out, kept separately so it is reported even when the
    /// reporter is unavailable.
    env_opted_out: bool,
    preference_lock: tokio::sync::Mutex<()>,
}

impl TelemetrySettingsService {
    pub fn new(
        reporter: Option<TelemetryService>,
        store: Arc<dyn TelemetryPreferenceStore>,
    ) -> Self {
        let env_opted_out = reporter
            .as_ref()
            .map(TelemetryService::env_opted_out)
            .unwrap_or_else(|| !TelemetryService::enabled_from_env());
        Self {
            reporter,
            store,
            env_opted_out,
            preference_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The built-in default, exposed for the console.
    pub fn default_enabled(&self) -> bool {
        DEFAULT_TELEMETRY_ENABLED
    }

    /// Re-read the stored preference and apply it to this process's reporter.
    pub async fn refresh(&self) -> Result<Option<bool>, TelemetrySettingsError> {
        let _guard = self.preference_lock.lock().await;
        let preference = self.store.load().await?;
        if let Some(reporter) = &self.reporter {
            reporter.apply_admin_preference(preference);
        }
        Ok(preference)
    }

    /// Current state, read fresh from the store so the console never shows a
    /// stale preference.
    pub async fn status(&self) -> Result<TelemetryStatus, TelemetrySettingsError> {
        let preference = self.refresh().await?;
        Ok(self.status_for(preference))
    }

    /// Persist an admin's choice and apply it to this process immediately.
    /// Other processes pick it up within [`PREFERENCE_SYNC_INTERVAL`].
    pub async fn set_enabled(
        &self,
        enabled: bool,
    ) -> Result<TelemetryStatus, TelemetrySettingsError> {
        let _guard = self.preference_lock.lock().await;
        let stored = self.store.save(enabled).await?;
        if let Some(reporter) = &self.reporter {
            reporter.apply_admin_preference(stored);
        }
        Ok(self.status_for(stored))
    }

    fn status_for(&self, preference: Option<bool>) -> TelemetryStatus {
        let (enabled, source) =
            resolve_status(self.reporter.is_some(), self.env_opted_out, preference);
        TelemetryStatus {
            enabled,
            source,
            env_opted_out: self.env_opted_out,
            admin_preference: preference,
            anonymous_id: self
                .reporter
                .as_ref()
                .map(|reporter| reporter.anonymous_id().to_string()),
            endpoint: self
                .reporter
                .as_ref()
                .map(|reporter| reporter.endpoint().to_string()),
            temps_version: self
                .reporter
                .as_ref()
                .map(|reporter| reporter.temps_version().to_string())
                .filter(|version| !version.is_empty()),
        }
    }

    /// Spawn the background loop that keeps this process in step with
    /// preference changes made through another process. One cached settings
    /// read per [`PREFERENCE_SYNC_INTERVAL`]; failures keep the last value.
    pub fn start_preference_sync(self: &Arc<Self>) {
        if self.reporter.is_none() {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PREFERENCE_SYNC_INTERVAL);
            // The plugin already applied the stored value at startup.
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = service.refresh().await {
                    tracing::debug!(
                        error = %error,
                        "Could not refresh the anonymous telemetry preference; keeping the last value"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    use temps_core::TelemetryReporter;

    /// In-memory store for tests.
    #[derive(Default)]
    pub(crate) struct MemoryStore {
        pub value: Mutex<Option<bool>>,
        pub fail_writes: bool,
    }

    #[async_trait]
    impl TelemetryPreferenceStore for MemoryStore {
        async fn load(&self) -> Result<Option<bool>, TelemetrySettingsError> {
            Ok(*self.value.lock().expect("store lock"))
        }

        async fn save(&self, enabled: bool) -> Result<Option<bool>, TelemetrySettingsError> {
            if self.fail_writes {
                return Err(TelemetrySettingsError::PreferenceWrite {
                    enabled,
                    reason: "simulated write failure".to_string(),
                });
            }
            *self.value.lock().expect("store lock") = Some(enabled);
            Ok(Some(enabled))
        }
    }

    #[tokio::test]
    async fn stale_refresh_cannot_apply_after_a_saved_opt_out() {
        struct PausedStore {
            value: Mutex<Option<bool>>,
            loaded: tokio::sync::Notify,
            release: tokio::sync::Notify,
        }
        #[async_trait]
        impl TelemetryPreferenceStore for PausedStore {
            async fn load(&self) -> Result<Option<bool>, TelemetrySettingsError> {
                let snapshot = *self.value.lock().unwrap();
                self.loaded.notify_one();
                self.release.notified().await;
                Ok(snapshot)
            }
            async fn save(&self, enabled: bool) -> Result<Option<bool>, TelemetrySettingsError> {
                *self.value.lock().unwrap() = Some(enabled);
                Ok(Some(enabled))
            }
        }
        let store = Arc::new(PausedStore {
            value: Mutex::new(Some(true)),
            loaded: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let dir = std::env::temp_dir().join(format!("temps-consent-race-{}", uuid::Uuid::new_v4()));
        let reporter = TelemetryService::new(&dir, "0.0.0-test").unwrap();
        let service = Arc::new(TelemetrySettingsService::new(
            Some(reporter.clone()),
            store.clone(),
        ));
        let refresh = tokio::spawn({
            let service = service.clone();
            async move { service.refresh().await }
        });
        store.loaded.notified().await;
        let save = tokio::spawn({
            let service = service.clone();
            async move { service.set_enabled(false).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !save.is_finished(),
            "save must wait for the older read/apply operation"
        );
        store.release.notify_one();
        refresh.await.unwrap().unwrap();
        save.await.unwrap().unwrap();
        assert!(!reporter.is_enabled());
        assert_eq!(*store.value.lock().unwrap(), Some(false));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolve_status_precedence() {
        assert_eq!(
            resolve_status(true, false, None),
            (DEFAULT_TELEMETRY_ENABLED, TelemetryStatusSource::Default)
        );
        assert_eq!(
            resolve_status(true, false, Some(false)),
            (false, TelemetryStatusSource::AdminSetting)
        );
        assert_eq!(
            resolve_status(true, false, Some(true)),
            (true, TelemetryStatusSource::AdminSetting)
        );
        // Environment wins over an admin opt-in.
        assert_eq!(
            resolve_status(true, true, Some(true)),
            (false, TelemetryStatusSource::Environment)
        );
        // A reporter that never started sends nothing, whatever the inputs.
        assert_eq!(
            resolve_status(false, false, Some(true)),
            (false, TelemetryStatusSource::Unavailable)
        );
    }

    #[tokio::test]
    async fn set_enabled_persists_and_reports_admin_source() {
        let store = Arc::new(MemoryStore::default());
        let service = TelemetrySettingsService::new(None, store.clone());

        let status = service.set_enabled(false).await.expect("save preference");
        assert_eq!(*store.value.lock().unwrap(), Some(false));
        assert_eq!(status.admin_preference, Some(false));
        // No reporter in this test, so it is reported as unavailable rather
        // than as enabled.
        assert!(!status.enabled);
        assert_eq!(status.source, TelemetryStatusSource::Unavailable);
    }

    #[tokio::test]
    async fn set_enabled_surfaces_write_failures() {
        let store = Arc::new(MemoryStore {
            fail_writes: true,
            ..Default::default()
        });
        let service = TelemetrySettingsService::new(None, store);
        let error = service
            .set_enabled(true)
            .await
            .expect_err("write failure must propagate");
        assert!(matches!(
            error,
            TelemetrySettingsError::PreferenceWrite { enabled: true, .. }
        ));
    }
}
