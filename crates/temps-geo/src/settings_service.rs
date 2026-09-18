// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reading and writing the geolocation section of the platform settings.
//!
//! Everything this crate needs at runtime -- how often to refresh, how long a
//! cached lookup stays valid, which MaxMind account to download with, and what
//! the last refresh did -- lives in `AppSettings::geo` on the singleton
//! `settings` row. That makes all of it changeable by an admin through the
//! existing settings API/UI with audit logging and no restart, per CLAUDE.md's
//! prohibition on environment variables for runtime configuration.
//!
//! The license key is the only sensitive value here. It is stored as
//! AES-256-GCM ciphertext (`maxmind_license_key_encrypted`) and decrypted
//! exactly once per refresh attempt, at the point the download URL is built.
//! Nothing in this module logs, returns, or persists the plaintext.

use std::sync::Arc;

use chrono::Utc;
use temps_config::ConfigService;
use temps_core::{
    EncryptionService, GeoSettings, GEO_CHECK_STATUS_ERROR, GEO_CHECK_STATUS_OK,
    GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY,
};

use crate::refresh::DbSource;
use crate::GeoIpError;

/// Reads and writes `AppSettings::geo`.
///
/// Holds its own `Arc` clones of the config and encryption services rather
/// than reaching into another service's internals, per the dependency rules in
/// CLAUDE.md.
pub struct GeoSettingsService {
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
}

impl GeoSettingsService {
    pub fn new(
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            config_service,
            encryption_service,
        }
    }

    /// The current geo settings.
    ///
    /// `ConfigService::get_settings` serves a 5-second in-memory snapshot and
    /// is invalidated by the settings writer, so this is cheap enough for the
    /// per-lookup staleness check in `IpAddressService` while still picking up
    /// an admin's change within seconds.
    pub async fn load(&self) -> Result<GeoSettings, GeoIpError> {
        Ok(self.config_service.get_settings().await?.geo)
    }

    /// The MaxMind license key in plaintext, for the download path only.
    ///
    /// `None` means no key is configured, which is not an error: downloads
    /// fall back to the copy committed to the Temps repository.
    pub fn license_key(&self, settings: &GeoSettings) -> Result<Option<String>, GeoIpError> {
        Ok(settings.decrypt_license_key(self.encryption_service.as_ref())?)
    }

    /// Resolve an incoming settings write against what is stored: encrypt a
    /// newly submitted license key, preserve the stored one when the field was
    /// left blank, and restore the refresh metadata no client may write.
    ///
    /// This is the encrypt-on-write seam for the geo section. It lives here,
    /// beside the code that decrypts, rather than inside `ConfigService`,
    /// which has no business knowing which of its fields are geo secrets.
    pub fn apply_update(
        &self,
        incoming: &mut GeoSettings,
        current: &GeoSettings,
    ) -> Result<(), GeoIpError> {
        incoming.preserve_recorded_state(current);
        incoming.apply_license_key_update(current, self.encryption_service.as_ref())?;
        Ok(())
    }

    /// Record a successful check.
    ///
    /// `refreshed` distinguishes "new bytes installed" from "the download
    /// matched what we already had", so the refresh timestamp only moves when
    /// the database actually changed.
    pub async fn record_check_success(
        &self,
        source: DbSource,
        build_epoch: u64,
        refreshed: bool,
    ) -> Result<(), GeoIpError> {
        let now = Utc::now();
        self.config_service
            .update_geo_settings(move |geo| {
                geo.source = Some(source.as_str().to_string());
                geo.build_epoch = Some(build_epoch);
                geo.last_check_at = Some(now);
                geo.last_check_status = Some(GEO_CHECK_STATUS_OK.to_string());
                geo.last_error = None;
                if refreshed {
                    geo.last_refreshed_at = Some(now);
                }
            })
            .await?;
        Ok(())
    }

    /// Record a scheduled check that deliberately made no network request
    /// because no MaxMind license key is configured.
    ///
    /// Recorded rather than left blank on purpose: "last checked: never" reads
    /// as a broken job, and an operator debugging alone has no way to tell
    /// that apart from "this instance is not licensed to refresh". The status
    /// endpoint, `temps doctor` and the settings page all turn this status
    /// into the concrete next step (add a license key).
    ///
    /// `last_error` is cleared -- not refreshing by configuration is not a
    /// failure, and leaving an old download error behind would make the status
    /// page keep blaming a network problem that no longer applies.
    pub async fn record_check_skipped_no_license_key(&self) -> Result<(), GeoIpError> {
        let now = Utc::now();
        self.config_service
            .update_geo_settings(move |geo| {
                geo.last_check_at = Some(now);
                geo.last_check_status = Some(GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY.to_string());
                geo.last_error = None;
            })
            .await?;
        Ok(())
    }

    /// Record a failed check. `reason` must already be redacted by the caller
    /// (see [`crate::refresh::redact_license_key`]) because it is persisted and
    /// served through the status API.
    ///
    /// The last successful refresh is deliberately left untouched so an
    /// operator can still see what data is loaded while a refresh is failing.
    pub async fn record_check_failure(&self, reason: &str) -> Result<(), GeoIpError> {
        let now = Utc::now();
        let reason = reason.to_string();
        self.config_service
            .update_geo_settings(move |geo| {
                geo.last_check_at = Some(now);
                geo.last_check_status = Some(GEO_CHECK_STATUS_ERROR.to_string());
                geo.last_error = Some(reason);
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::AppSettings;

    fn test_service() -> GeoSettingsService {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::settings::Model>::new()])
                .into_connection(),
        );
        let config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .expect("valid test server config"),
        );
        GeoSettingsService::new(
            Arc::new(ConfigService::new(config, db)),
            Arc::new(EncryptionService::new(&"b".repeat(64)).expect("encryption service")),
        )
    }

    #[tokio::test]
    async fn missing_settings_row_loads_the_defaults_instead_of_failing() {
        let service = test_service();
        let settings = service.load().await.expect("load geo settings");
        assert_eq!(settings, GeoSettings::default());
        assert!(!settings.license_key_configured());
        assert_eq!(
            service.license_key(&settings).expect("no key configured"),
            None
        );
    }

    #[test]
    fn a_submitted_license_key_is_encrypted_and_decrypts_back() {
        let service = test_service();
        let mut incoming = GeoSettings {
            maxmind_license_key: Some("submitted-key".to_string()),
            refresh_interval_hours: Some(6),
            ..GeoSettings::default()
        };

        service
            .apply_update(&mut incoming, &GeoSettings::default())
            .expect("apply the update");

        assert_eq!(incoming.maxmind_license_key, None);
        assert_eq!(incoming.refresh_interval_hours, Some(6));
        assert_eq!(
            service.license_key(&incoming).expect("decrypt").as_deref(),
            Some("submitted-key")
        );
    }

    #[test]
    fn an_update_cannot_forge_refresh_metadata_or_wipe_the_stored_key() {
        let service = test_service();
        let now = Utc::now();
        let current = GeoSettings {
            maxmind_license_key_encrypted: Some("stored-ciphertext".to_string()),
            source: Some(temps_core::GEO_SOURCE_MAXMIND_OFFICIAL.to_string()),
            build_epoch: Some(1_767_225_600),
            last_refreshed_at: Some(now),
            last_check_status: Some(GEO_CHECK_STATUS_OK.to_string()),
            ..GeoSettings::default()
        };
        let mut incoming = GeoSettings {
            stale_lookup_days: Some(14),
            source: Some("forged".to_string()),
            build_epoch: Some(0),
            last_check_status: Some(GEO_CHECK_STATUS_ERROR.to_string()),
            ..GeoSettings::default()
        };

        service
            .apply_update(&mut incoming, &current)
            .expect("apply the update");

        assert_eq!(incoming.stale_lookup_days, Some(14));
        assert_eq!(
            incoming.maxmind_license_key_encrypted.as_deref(),
            Some("stored-ciphertext")
        );
        assert_eq!(
            incoming.source.as_deref(),
            Some(temps_core::GEO_SOURCE_MAXMIND_OFFICIAL)
        );
        assert_eq!(incoming.build_epoch, Some(1_767_225_600));
        assert!(!incoming.last_check_failed());
    }

    /// The whole point of consolidating the metadata into the typed struct: a
    /// full `AppSettings` deserialize/reserialize (what the generic settings
    /// endpoint does on every save) must not drop any geo field.
    #[test]
    fn every_geo_field_survives_a_full_settings_round_trip() {
        let now = Utc::now();
        let settings = AppSettings {
            // Every field named explicitly, so adding one to `GeoSettings`
            // without covering it here is a compile error rather than a
            // silently untested round trip.
            geo: GeoSettings {
                refresh_interval_hours: Some(8),
                stale_lookup_days: Some(21),
                maxmind_license_key: None,
                clear_maxmind_license_key: false,
                maxmind_license_key_encrypted: Some("ciphertext".to_string()),
                last_refreshed_at: Some(now),
                source: Some(temps_core::GEO_SOURCE_BUNDLED_GITHUB.to_string()),
                build_epoch: Some(1_767_225_600),
                last_check_at: Some(now),
                last_check_status: Some(GEO_CHECK_STATUS_ERROR.to_string()),
                last_error: Some("HTTP 401".to_string()),
            },
            ..AppSettings::default()
        };

        let back = AppSettings::from_json(settings.to_json());
        assert_eq!(back.geo, settings.geo);
    }
}
