// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin that registers the anonymous telemetry reporter.
//!
//! Registers an `Arc<dyn TelemetryReporter>` in the service registry so any
//! feature crate can `require_service::<dyn TelemetryReporter>()` (or accept it
//! via constructor) without depending on this crate directly — mirroring the
//! `AuditLogger` wiring.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_config::ServerConfig;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::telemetry::{NoopTelemetryReporter, TelemetryReporter};
use utoipa::{openapi::OpenApi, OpenApi as OpenApiTrait};

use crate::handlers::{configure_routes, TelemetryApiDoc, TelemetryAppState};
use crate::settings::{ConfigTelemetryPreferenceStore, TelemetrySettingsService};
use crate::TelemetryService;

/// Plugin for anonymous product telemetry.
pub struct TelemetryPlugin {
    server_config: Arc<ServerConfig>,
    /// Version string stamped onto every event (typically the server's
    /// git-describe `TEMPS_VERSION`, not the static `CARGO_PKG_VERSION` --
    /// the latter is identical across nightly/beta/release builds).
    temps_version: String,
}

impl TelemetryPlugin {
    pub fn new(server_config: Arc<ServerConfig>, temps_version: impl Into<String>) -> Self {
        Self {
            server_config,
            temps_version: temps_version.into(),
        }
    }
}

impl TempsPlugin for TelemetryPlugin {
    fn name(&self) -> &'static str {
        "telemetry"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            // Telemetry must never block startup. If the reporter can't be
            // built (e.g. the anonymous-id file can't be written), fall back to
            // a no-op reporter and log, rather than failing the server. The
            // settings page then reports it as unavailable.
            let service: Option<TelemetryService> =
                match temps_config::stateless_telemetry_anonymous_id(db.as_ref()).await {
                    Ok(stateless_anonymous_id) => match TelemetryService::new_for_installation(
                        &self.server_config.data_dir,
                        self.temps_version.clone(),
                        stateless_anonymous_id.as_deref(),
                    ) {
                        Ok(svc) => {
                            svc.set_db(db);
                            Some(svc)
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                "Failed to initialize telemetry reporter; telemetry disabled for this run"
                            );
                            None
                        }
                    },
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "Failed to resolve persisted telemetry identity; telemetry disabled for this run"
                        );
                        None
                    }
                };

            let settings_service = Arc::new(TelemetrySettingsService::new(
                service.clone(),
                Arc::new(ConfigTelemetryPreferenceStore::new(config_service)),
            ));
            // Apply the admin's stored choice before any event can be sent,
            // so an instance an admin opted out never emits `instance_started`.
            if let Some(svc) = &service {
                match settings_service.refresh().await {
                    Ok(_) => {}
                    Err(error) => {
                        // Fail closed: a preference we cannot read may be an
                        // opt-out. The sync loop retries and re-enables.
                        tracing::warn!(
                            error = %error,
                            "Could not read the stored telemetry preference; telemetry paused until it can be read"
                        );
                        svc.apply_admin_preference(Some(false));
                    }
                }
                svc.log_effective_state();
            }
            settings_service.start_preference_sync();

            let reporter: Arc<dyn TelemetryReporter> = match service {
                Some(svc) => Arc::new(svc),
                None => Arc::new(NoopTelemetryReporter),
            };
            context.register_service(reporter);
            context.register_service(settings_service);
            tracing::debug!("Telemetry plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let settings_service = context.require_service::<TelemetrySettingsService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let state = TelemetryAppState {
            settings_service,
            audit_service,
        };
        Some(PluginRoutes::new(configure_routes().with_state(state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(TelemetryApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_telemetry() {
        // Construct with a throwaway config; we only assert the name.
        // ServerConfig::new touches the data dir, so use a temp dir.
        let dir = std::env::temp_dir().join(format!(
            "temps-telemetry-plugin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::env::set_var("TEMPS_DATA_DIR", &dir);
        let cfg = ServerConfig::new(
            "127.0.0.1:0".to_string(),
            "postgres://localhost/none".to_string(),
            None,
            None,
        )
        .unwrap();
        std::env::remove_var("TEMPS_DATA_DIR");

        let plugin = TelemetryPlugin::new(Arc::new(cfg), "0.0.0-test");
        assert_eq!(plugin.name(), "telemetry");
        std::fs::remove_dir_all(&dir).ok();
    }
}
