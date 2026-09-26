// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Geo Plugin implementation for the Temps plugin system
//!
//! This plugin provides geolocation services including:
//! - GeoIpService for IP geolocation
//! - IpAddressService for IP address management and tracking
//! - GeoSettingsService for the refresh policy, encrypted MaxMind key and
//!   recorded freshness metadata, all read from `AppSettings::geo`
//! - A scheduled job that re-downloads the database and hot-swaps it in

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::OpenApi;

use crate::settings_service::GeoSettingsService;
use crate::{handlers, refresh, AppState, GeoIpService, IpAddressService};

/// Geo Plugin for managing geolocation and IP address services
pub struct GeoPlugin;

impl GeoPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GeoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for GeoPlugin {
    fn name(&self) -> &'static str {
        "geo"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // `temps serve`'s console-API startup path downloads
            // GeoLite2-City.mmdb in the background (see
            // `validate_geolite2_database` in temps-cli's serve/console.rs)
            // concurrently with plugin registration. `GeoIpService::new()`
            // itself only opens whatever's on disk right now -- it doesn't
            // download -- so on a fresh instance with no pre-provisioned
            // database, registration can race the download and fail even
            // though the file appears moments later. Wait for it (bounded)
            // before handing off to the sync constructor, unless mock mode
            // is enabled (which never touches the filesystem).
            let use_mock = std::env::var("TEMPS_GEO_MOCK")
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false);
            if !use_mock {
                let city_db_path = crate::geoip_service::resolve_mmdb_path("GeoLite2-City.mmdb");
                if !city_db_path.exists() {
                    tracing::info!(
                        path = %city_db_path.display(),
                        "GeoLite2-City.mmdb not present yet -- waiting up to 30s for the \
                         concurrent startup download to finish before registering the geo plugin",
                    );
                    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                    while !city_db_path.exists() && tokio::time::Instant::now() < deadline {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                }
            }

            // Create GeoIpService
            let geo_ip_service = Arc::new(GeoIpService::new().map_err(|e| {
                PluginError::PluginRegistrationFailed {
                    plugin_name: "geo".to_string(),
                    error: e.to_string(),
                }
            })?);
            context.register_service(geo_ip_service.clone());

            // Unconditional, and deliberately outside the match below: this is
            // the only thing that keeps a **split-role** deployment (ADR-017)
            // current. There, `temps proxy` is its own OS process with its own
            // reader, and it has no `EncryptionService`, so it never spawns the
            // refresh job and never learns that the console process replaced
            // the `.mmdb` -- it served the database it opened at boot until
            // restarted. The watcher needs nothing but the filesystem: no
            // license key, no network, no settings. In the monolith it is a
            // no-op, because there the refresh already swapped the one reader
            // both registries share. Skipped in mock mode, which has no file.
            if !use_mock {
                refresh::spawn_db_file_watcher(geo_ip_service.clone());
            }

            // Every geo knob lives on the settings row, so the refresh policy,
            // the encrypted MaxMind key and the recorded freshness metadata
            // are all reached through these two services rather than the
            // environment. `EncryptionService` is genuinely optional here:
            // `temps-proxy`'s `setup_proxy_plugins` registers only
            // ConfigPlugin+GeoPlugin in a deliberately minimal context (the
            // hot-path proxy never touches settings/encryption), so this
            // plugin must keep working -- with plain GeoIpService lookups and
            // no refresh job -- when it isn't present, rather than panicking
            // the proxy on every startup. The full console context always has
            // it, so `require_service` still applies wherever the settings
            // service is actually used downstream.
            let config_service = context.require_service::<temps_config::ConfigService>();
            let encryption_service = context.get_service::<temps_core::EncryptionService>();

            let ip_address_service = match encryption_service {
                Some(encryption_service) => {
                    let settings_service =
                        Arc::new(GeoSettingsService::new(config_service, encryption_service));
                    context.register_service(settings_service.clone());

                    // Keep the database current without a restart. Skipped in
                    // mock mode, which has no file to replace.
                    // `spawn_refresh_job` is idempotent per process, so the
                    // console API and proxy registries do not each start a
                    // downloader. The job re-reads the settings each tick, so
                    // nothing is captured here.
                    if !use_mock {
                        refresh::spawn_refresh_job(
                            geo_ip_service.clone(),
                            settings_service.clone(),
                        );
                    }

                    Arc::new(IpAddressService::with_settings(
                        db.clone(),
                        geo_ip_service.clone(),
                        settings_service,
                    ))
                }
                None => {
                    tracing::debug!(
                        "EncryptionService not available in this plugin context; geo settings, \
                         the scheduled MaxMind refresh and GeoSettingsService are disabled here \
                         (expected in the proxy's minimal context, not the console's)"
                    );
                    Arc::new(IpAddressService::new(db.clone(), geo_ip_service.clone()))
                }
            };
            context.register_service(ip_address_service);

            tracing::debug!("Geo plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get GeoIpService from service registry
        let geo_ip_service = context.require_service::<GeoIpService>();
        let geo_settings_service = context.require_service::<GeoSettingsService>();

        // Create AppState for handlers
        let app_state = Arc::new(AppState {
            geo_ip_service: geo_ip_service.clone(),
            geo_settings_service,
        });

        // Configure routes (plugin system adds /api prefix)
        let routes = handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(handlers::ApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_geo_plugin_name() {
        let geo_plugin = GeoPlugin::new();
        assert_eq!(geo_plugin.name(), "geo");
    }

    #[tokio::test]
    async fn test_geo_plugin_default() {
        let geo_plugin = GeoPlugin;
        assert_eq!(geo_plugin.name(), "geo");
    }
}
